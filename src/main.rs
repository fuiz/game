use actix_multipart::form::{MultipartForm, MultipartFormConfig};
use actix_web::{
    error::ErrorNotFound, get, http::StatusCode, post, web, App, HttpResponse, HttpServer,
    Responder,
};
use image::{codecs::gif, AnimationDecoder, ImageError};
use media_manager::{MediaId, MediaManager};
use storage::Memory;
use thiserror::Error;

mod media_manager;
mod storage;

#[derive(Default)]
struct AppState {
    media_manager: MediaManager<u64, Memory>,
}

#[get("/get/{media_id}")]
async fn get_full(data: web::Data<AppState>, path: web::Path<MediaId>) -> impl Responder {
    match data.media_manager.retrieve(path.into_inner()) {
        Some((bytes, content_type)) => Ok(HttpResponse::build(StatusCode::OK)
            .content_type(content_type)
            .body(bytes)),
        None => Err(ErrorNotFound("This file was not found")),
    }
}

#[get("/exists/{media_id}")]
async fn exists(data: web::Data<AppState>, path: web::Path<MediaId>) -> impl Responder {
    web::Json(data.media_manager.contains(path.into_inner()))
}

#[derive(MultipartForm)]
struct ImageUpload {
    #[multipart(limit = "25 MiB")]
    image: actix_multipart::form::bytes::Bytes,
}

#[derive(Error, Debug)]
enum ImageDecodingError {
    #[error("type is not recognizable")]
    NotRecognizable(#[from] std::io::Error),
    #[error("image decoding error")]
    ImageError(#[from] ImageError),
    #[error("missing content type")]
    NoContentType,
}

impl actix_web::error::ResponseError for ImageDecodingError {}

#[post("/thumbnail")]
async fn thumbnail(image_upload: MultipartForm<ImageUpload>) -> impl Responder {
    let ImageUpload { image } = image_upload.into_inner();

    let decoded_image = image::io::Reader::new(std::io::Cursor::new(image.data))
        .with_guessed_format()?
        .decode()?
        .resize(400, 400, image::imageops::FilterType::Nearest);

    let mut thumbnail_bytes: Vec<u8> = Vec::new();
    decoded_image.write_to(
        &mut std::io::Cursor::new(&mut thumbnail_bytes),
        image::ImageOutputFormat::Png,
    )?;

    Ok::<HttpResponse, ImageDecodingError>(
        HttpResponse::build(StatusCode::OK)
            .content_type("image/png")
            .body(thumbnail_bytes),
    )
}

#[post("/upload")]
async fn upload(
    data: web::Data<AppState>,
    image_upload: MultipartForm<ImageUpload>,
) -> impl Responder {
    let ImageUpload { image } = image_upload.into_inner();

    let content_type = image
        .content_type
        .ok_or(ImageDecodingError::NoContentType)?;

    let (bytes, content_type) = match content_type {
        gif if gif == mime::IMAGE_GIF => {
            let decoded_image = gif::GifDecoder::new(std::io::Cursor::new(image.data))?;
            let mut bytes: Vec<u8> = Vec::new();

            {
                let mut encoded_image = gif::GifEncoder::new(&mut bytes);
                encoded_image.set_repeat(gif::Repeat::Infinite)?;
                encoded_image
                    .encode_frames(decoded_image.into_frames().collect::<Result<Vec<_>, _>>()?)?;
            }

            (bytes, gif)
        }
        _ => {
            let decoded_image = image::io::Reader::new(std::io::Cursor::new(image.data))
                .with_guessed_format()?
                .decode()?;

            let mut bytes: Vec<u8> = Vec::new();
            decoded_image.write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageOutputFormat::Png,
            )?;

            (bytes, mime::IMAGE_PNG)
        }
    };

    let media_id = data
        .media_manager
        .store(actix_web::web::Bytes::from(bytes), content_type);

    actix_web::rt::spawn(async move {
        actix_web::rt::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        data.media_manager.delete(media_id);
    });

    Ok::<actix_web::web::Json<MediaId>, ImageDecodingError>(web::Json(media_id))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    pretty_env_logger::init();

    let app_data = web::Data::new(AppState::default());

    HttpServer::new(move || {
        let app = App::new()
            .app_data(web::PayloadConfig::new(250_000_000))
            .app_data(MultipartFormConfig::default().memory_limit(20_000_000))
            .app_data(app_data.clone())
            .route("/hello", web::get().to(|| async { "Hello World!" }))
            .service(get_full)
            .service(thumbnail)
            .service(exists)
            .service(upload);

        #[cfg(feature = "https")]
        {
            let cors = actix_cors::Cors::default()
                .allowed_origin_fn(|origin, _| origin.as_bytes().ends_with(b".fuiz.pages.dev"))
                .allowed_origin("https://fuiz.us")
                .allowed_methods(vec!["GET", "POST"])
                .allowed_headers(vec![
                    actix_web::http::header::AUTHORIZATION,
                    actix_web::http::header::ACCEPT,
                ])
                .supports_credentials()
                .allowed_header(actix_web::http::header::CONTENT_TYPE);
            app.wrap(cors)
        }
        #[cfg(not(feature = "https"))]
        {
            let cors = actix_cors::Cors::permissive();
            app.wrap(cors)
        }
    })
    .bind((
        if cfg!(feature = "https") {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        },
        5040,
    ))?
    .run()
    .await
}
