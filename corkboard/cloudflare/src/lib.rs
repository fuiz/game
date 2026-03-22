use serde_hex::{SerHex, Strict};
use serde_json::json;
use worker::*;

const IMAGE_EXPIRATION: std::time::Duration = std::time::Duration::from_hours(24);

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    let router = Router::new();

    router
        .get("/hello", |_, _| Response::ok("Hello World!"))
        .post_async("/thumbnail", |mut req, _ctx| async move {
            let Some(image) = req.form_data().await?.get("image") else {
                return Response::error("no image in request", 400);
            };

            let data = match image {
                FormEntry::File(f) => f.bytes().await?,
                FormEntry::Field(_) => return Response::error("image has to be a file", 400),
            };

            let Some(decoded_image) = image::ImageReader::new(std::io::Cursor::new(data))
                .with_guessed_format()?
                .decode()
                .ok()
                .map(|image| image.resize(400, 400, image::imageops::FilterType::Nearest))
            else {
                return Response::error("image format not supported", 400);
            };

            let mut thumbnail_bytes: Vec<u8> = Vec::new();
            if decoded_image
                .write_to(&mut std::io::Cursor::new(&mut thumbnail_bytes), image::ImageFormat::Png)
                .is_err()
            {
                return Response::error("failed to encode image", 500);
            }

            Ok(
                Response::from_body(worker::ResponseBody::Body(thumbnail_bytes))?.with_headers({
                    let headers = Headers::new();
                    headers.append("content-type", "image/png")?;
                    headers
                }),
            )
        })
        .post_async("/upload", |mut req, ctx| async move {
            let Some(image) = req.form_data().await?.get("image") else {
                return Response::error("no image in request", 400);
            };

            let data = match image {
                FormEntry::File(f) => f.bytes().await?,
                FormEntry::Field(_) => return Response::error("image has to be a file", 400),
            };

            let frames = match corkboard::read_image_as_frames(&data) {
                Ok(frames) => frames,
                Err(e) => {
                    return Response::error(
                        format!(
                            "Failed to decode image format, please use a supported format. Internal error: {}",
                            e
                        ),
                        400,
                    );
                }
            };

            let Ok(bytes) = corkboard::encode_frames_as_png(frames) else {
                return Response::error("failed to encode image", 500);
            };

            let content_type = image::ImageFormat::Png.to_mime_type();

            let kv = ctx.kv("IMAGES")?;

            let Ok(random_key) = getrandom::u64() else {
                return Response::error("couldn't generate random key", 500);
            };

            let Ok(key) = <u64 as SerHex<Strict>>::into_hex(&random_key) else {
                return Response::error("couldn't convert hash to hex", 500);
            };

            kv.put_bytes(&key, &bytes)?
                .metadata(content_type)?
                .expiration_ttl(IMAGE_EXPIRATION.as_secs())
                .execute()
                .await?;

            Response::from_json(&json!(key))
        })
        .get_async("/get/:media_id", |_req, ctx| async move {
            let Some(media_id) = ctx.param("media_id") else {
                return Response::error("missing media_id", 400);
            };

            let kv = ctx.kv("IMAGES")?;

            let (bytes, content_type) = kv.get(media_id).bytes_with_metadata().await?;

            Ok(Response::from_bytes(bytes.unwrap_or_default())?.with_headers({
                Headers::from_iter([(
                    "content-type".to_string(),
                    content_type.unwrap_or(image::ImageFormat::Png.to_mime_type().to_string()),
                )])
            }))
        })
        .run(req, env)
        .await?
        .with_cors(
            &Cors::default()
                .with_max_age(86400)
                .with_allowed_headers(["*"])
                .with_origins(vec!["https://fuiz.org"])
                .with_methods(vec![Method::Get, Method::Post, Method::Options]),
        )
}
