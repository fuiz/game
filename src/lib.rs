use image::{AnimationDecoder, Frame, ImageFormat, ImageReader, ImageResult};
use num_integer::Integer;
use serde_hex::{SerHex, Strict};
use serde_json::json;
use worker::*;

const IMAGE_EXPIRATION: std::time::Duration = std::time::Duration::from_hours(24);

fn read_image_as_frames(bytes: &[u8]) -> ImageResult<Vec<Frame>> {
    let reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    match reader.format().ok_or_else(|| {
        image::ImageError::Unsupported(image::error::UnsupportedError::from_format_and_kind(
            image::error::ImageFormatHint::Unknown,
            image::error::UnsupportedErrorKind::Format(image::error::ImageFormatHint::Unknown),
        ))
    })? {
        ImageFormat::Gif => image::codecs::gif::GifDecoder::new(reader.into_inner())?
            .into_frames()
            .collect_frames(),
        ImageFormat::Png => {
            let png_decoder = image::codecs::png::PngDecoder::new(reader.into_inner())?;
            if png_decoder.is_apng()? {
                png_decoder.apng()?.into_frames().collect_frames()
            } else {
                let mut reader = ImageReader::new(std::io::Cursor::new(bytes));
                reader.set_format(ImageFormat::Png);
                let image = reader.decode()?.to_rgba8();
                Ok(vec![Frame::new(image)])
            }
        }
        ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(reader.into_inner())?
            .into_frames()
            .collect_frames(),
        _ => {
            let image = reader.decode()?.to_rgba8();
            Ok(vec![Frame::new(image)])
        }
    }
}

fn encode_frames_as_png(frames: Vec<Frame>) -> Result<Vec<u8>, png::EncodingError> {
    let mut output: Vec<u8> = Vec::new();

    let (width, height) = frames[0].buffer().dimensions();

    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(frames.len() as u32, 0)?; // 0 = loop forever

    let mut writer = encoder.write_header()?;

    for frame in frames {
        let buf = frame.buffer();

        let (num_ms, denom_ms) = frame.delay().numer_denom_ms();
        let (num_sec, denom_sec) = (num_ms, denom_ms * 1000);
        let gcd = num_sec.gcd(&denom_sec);
        let num_sec_simple = num_sec / gcd;
        let denom_sec_simple = denom_sec / gcd;

        writer.set_frame_delay(num_sec_simple as u16, denom_sec_simple as u16)?;
        writer.set_frame_position(frame.left(), frame.top())?;

        writer.write_image_data(buf.as_raw())?;
    }

    writer.finish()?;

    Ok(output)
}

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

            let frames = match read_image_as_frames(&data) {
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

            let Ok(bytes) = encode_frames_as_png(frames) else {
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

            Ok(
                Response::from_bytes(bytes.unwrap_or_default())?.with_headers({
                    Headers::from_iter([(
                        "content-type".to_string(),
                        content_type.unwrap_or(image::ImageFormat::Png.to_mime_type().to_string()),
                    )])
                }),
            )
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
