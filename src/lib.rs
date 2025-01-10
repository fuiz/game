use std::hash::{DefaultHasher, Hash, Hasher};

use serde_hex::{SerHex, Strict};
use serde_json::json;
use worker::*;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    console_error_panic_hook::set_once();
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
                .write_to(
                    &mut std::io::Cursor::new(&mut thumbnail_bytes),
                    image::ImageFormat::Png,
                )
                .is_err()
            {
                return Response::error("failed to encode image", 500);
            }

            Ok(
                Response::from_body(worker::ResponseBody::Body(thumbnail_bytes))?.with_headers({
                    let mut headers = Headers::new();
                    headers.append("content-type", "image/png")?;
                    headers
                }),
            )
        })
        .post_async("/upload", |mut req, ctx| async move {
            let Some(image) = req.form_data().await?.get("image") else {
                return Response::error("no image in request", 400);
            };

            let (data, content_type) = match image {
                FormEntry::File(f) => (f.bytes().await?, f.type_()),
                FormEntry::Field(_) => return Response::error("image has to be a file", 400),
            };

            let (bytes, content_type) = match content_type {
                gif if gif == "image/gif" => {
                    let Ok(mut decoded_image) = gif::Decoder::new(std::io::Cursor::new(data))
                    else {
                        return Response::error("data couldn't be decoded", 500);
                    };

                    let Some(palette) = decoded_image.global_palette() else {
                        return Response::error("couldn't determine GIF palette", 500);
                    };

                    let mut bytes: Vec<u8> = Vec::new();

                    {
                        let Ok(mut encoded_image) = gif::Encoder::new(
                            &mut bytes,
                            decoded_image.width(),
                            decoded_image.height(),
                            palette,
                        ) else {
                            return Response::error("data couldn't be encoded", 500);
                        };
                        let _ = encoded_image.set_repeat(decoded_image.repeat());
                        while let Ok(Some(frame)) = decoded_image.read_next_frame() {
                            if encoded_image.write_frame(frame).is_err() {
                                return Response::error("couldn't encode frame", 500);
                            }
                        }
                    }

                    (bytes, "image/gif")
                }
                _ => {
                    let Ok(decoded_image) = image::ImageReader::new(std::io::Cursor::new(data))
                        .with_guessed_format()?
                        .decode()
                    else {
                        return Response::error("data couldn't be decoded", 500);
                    };

                    let mut bytes: Vec<u8> = Vec::new();

                    if decoded_image
                        .write_to(
                            &mut std::io::Cursor::new(&mut bytes),
                            image::ImageFormat::Png,
                        )
                        .is_err()
                    {
                        return Response::error("image format not supported", 400);
                    }

                    (bytes, "image/png")
                }
            };

            let kv = ctx.kv("IMAGES")?;

            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            let hash = hasher.finish();

            let Ok(key) = <u64 as SerHex<Strict>>::into_hex(&hash) else {
                return Response::error("couldn't convert hash to hex", 500);
            };

            kv.put_bytes(&key, &bytes)?
                .metadata(content_type)?
                .expiration_ttl({
                    const SECONDS_IN_A_DAY: u64 = 60 * 60 * 24;
                    SECONDS_IN_A_DAY
                })
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

            Ok(
                Response::from_bytes(bytes.unwrap_or_default())?.with_headers({
                    let mut headers = Headers::new();
                    headers.append(
                        "content-type",
                        &content_type.unwrap_or("image/png".to_owned()),
                    )?;
                    headers
                }),
            )
        })
        .run(req, env)
        .await?
        .with_cors(
            &Cors::default()
                .with_max_age(86400)
                .with_allowed_headers(["*"])
                .with_origins(vec!["https://fuiz.us"])
                .with_methods(vec![Method::Get, Method::Post, Method::Options]),
        )
}
