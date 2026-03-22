use actix_multipart::form::{MultipartForm, MultipartFormConfig};
use actix_web::{App, HttpResponse, HttpServer, Responder, error::ErrorNotFound, get, http::StatusCode, post, web};
use image::ImageError;
use storage::{MediaId, Memory};
use thiserror::Error;

mod storage;

#[derive(Default)]
struct AppState {
    storage: Memory,
}

#[get("/get/{media_id}")]
async fn get_full(data: web::Data<AppState>, path: web::Path<MediaId>) -> impl Responder {
    match data.storage.retrieve(&path.into_inner()) {
        Some((bytes, content_type)) => Ok(HttpResponse::build(StatusCode::OK)
            .content_type(content_type)
            .body(bytes)),
        None => Err(ErrorNotFound("This file was not found")),
    }
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
    #[error("encoding error")]
    EncodingError(#[from] corkboard::png::EncodingError),
}

impl actix_web::error::ResponseError for ImageDecodingError {}

#[post("/thumbnail")]
async fn thumbnail(image_upload: MultipartForm<ImageUpload>) -> impl Responder {
    let ImageUpload { image } = image_upload.into_inner();

    let decoded_image = image::ImageReader::new(std::io::Cursor::new(image.data))
        .with_guessed_format()?
        .decode()?
        .resize(400, 400, image::imageops::FilterType::Nearest);

    let mut thumbnail_bytes: Vec<u8> = Vec::new();
    decoded_image.write_to(&mut std::io::Cursor::new(&mut thumbnail_bytes), image::ImageFormat::Png)?;

    Ok::<HttpResponse, ImageDecodingError>(
        HttpResponse::build(StatusCode::OK)
            .content_type("image/png")
            .body(thumbnail_bytes),
    )
}

#[post("/upload")]
async fn upload(data: web::Data<AppState>, image_upload: MultipartForm<ImageUpload>) -> impl Responder {
    let ImageUpload { image } = image_upload.into_inner();

    let (bytes, content_type) = {
        let frames = corkboard::read_image_as_frames(&image.data)?;
        let bytes = corkboard::encode_frames_as_png(frames)?;
        (bytes, mime::IMAGE_PNG)
    };

    let media_id = MediaId::new();
    data.storage
        .store(media_id, actix_web::web::Bytes::from(bytes), content_type);

    actix_web::rt::spawn(async move {
        actix_web::rt::time::sleep(std::time::Duration::from_hours(1)).await;
        data.storage.delete(&media_id);
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
