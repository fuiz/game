//! Corkboard image processing library.

use image::{
    Frame, ImageFormat, ImageReader, ImageReaderOptions,
    codecs::{png::PngDecoder, webp::WebPDecoder},
    error::UnsupportedError,
};
use num_integer::Integer;

pub use png::EncodingError as PngEncodingError;

pub use image::ImageError as ImageDecodingError;

/// Builds an error describing an image whose frames could not be read.
fn unsupported_image() -> ImageDecodingError {
    ImageDecodingError::Unsupported(UnsupportedError::from_format_and_kind(
        image::error::ImageFormatHint::Unknown,
        image::error::UnsupportedErrorKind::Format(image::error::ImageFormatHint::Unknown),
    ))
}

/// Reads an image from bytes and returns its frames.
///
/// # Errors
///
/// Returns an [`ImageDecodingError`] if the image format is unsupported, decoding fails, or the
/// image contains no frames.
pub fn read_image_as_frames(bytes: &[u8]) -> Result<Vec<Frame>, ImageDecodingError> {
    let options = ImageReaderOptions::new(std::io::Cursor::new(bytes)).with_guessed_format()?;

    let frames = match options.format().ok_or_else(unsupported_image)? {
        ImageFormat::Png => {
            let mut decoder = PngDecoder::new(options.into_inner());
            if decoder.is_apng()? {
                ImageReader::from_decoder(Box::new(decoder.apng()?))
                    .into_frames()
                    .collect_frames()?
            } else {
                let image = ImageReader::from_decoder(Box::new(decoder)).decode()?.0;
                vec![Frame::new(image.into_rgba8())]
            }
        }
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(options.into_inner())?;
            let is_animated = decoder.has_animation();
            let mut reader = ImageReader::from_decoder(Box::new(decoder));
            if is_animated {
                reader.into_frames().collect_frames()?
            } else {
                vec![Frame::new(reader.decode()?.0.into_rgba8())]
            }
        }
        ImageFormat::Gif => options.into_reader()?.into_frames().collect_frames()?,
        _ => {
            let image = options.into_reader()?.decode()?.0;
            vec![Frame::new(image.into_rgba8())]
        }
    };

    if frames.is_empty() {
        return Err(unsupported_image());
    }

    Ok(frames)
}

/// Encodes a sequence of frames as an animated PNG.
///
/// # Errors
///
/// Returns a [`PngEncodingError`] if encoding fails.
pub fn encode_frames_as_png(frames: Vec<Frame>) -> Result<Vec<u8>, PngEncodingError> {
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

/// Computes a thumbnail of an image from bytes.
///
/// # Errors
///
/// Returns an [`ImageDecodingError`] if the image format is unsupported or decoding fails.
pub fn generate_thumbnail(data: &[u8]) -> Result<Vec<u8>, ImageDecodingError> {
    let mut decoded_image = image::ImageReaderOptions::new(std::io::Cursor::new(data))
        .with_guessed_format()?
        .into_reader()?
        .decode()?
        .0;

    decoded_image.resize(400, 400, image::imageops::FilterType::Nearest);

    let mut thumbnail_bytes: Vec<u8> = Vec::new();
    decoded_image.write_to(&mut std::io::Cursor::new(&mut thumbnail_bytes), image::ImageFormat::Png)?;

    Ok(thumbnail_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, RgbaImage};
    use std::time::Duration;

    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, image::Rgba(color))
    }

    fn frame(color: [u8; 4], millis: u64) -> Frame {
        Frame::from_parts(
            solid(16, 16, color),
            0,
            0,
            Delay::from_saturating_duration(Duration::from_millis(millis)),
        )
    }

    fn encode_static(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image.clone())
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn encode_gif(frames: Vec<Frame>) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(std::io::Cursor::new(&mut bytes));
        encoder.encode_frames(frames).unwrap();
        drop(encoder);
        bytes
    }

    #[test]
    fn static_png_reads_as_one_frame() {
        let bytes = encode_static(&solid(16, 16, [255, 0, 0, 255]), ImageFormat::Png);
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].buffer().dimensions(), (16, 16));
        assert_eq!(frames[0].buffer().get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn static_jpeg_reads_as_one_frame() {
        let bytes = encode_static(&solid(16, 16, [0, 0, 255, 255]), ImageFormat::Jpeg);
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].buffer().dimensions(), (16, 16));
    }

    #[test]
    fn static_webp_reads_as_one_frame() {
        let bytes = encode_static(&solid(16, 16, [0, 255, 255, 255]), ImageFormat::WebP);
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].buffer().dimensions(), (16, 16));
    }

    #[test]
    fn animated_webp_reads_every_frame() {
        let bytes = include_bytes!("../tests/images/animated.webp");
        let frames = read_image_as_frames(bytes).unwrap();
        assert_eq!(frames.len(), 6);
        for frame in &frames {
            assert_eq!(frame.buffer().dimensions(), (200, 200));
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            assert_eq!(numerator / denominator, 100);
        }
    }

    #[test]
    fn animated_webp_converts_to_animated_png() {
        let frames = read_image_as_frames(include_bytes!("../tests/images/animated.webp")).unwrap();
        let apng = encode_frames_as_png(frames).unwrap();
        assert_eq!(&apng[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(read_image_as_frames(&apng).unwrap().len(), 6);
    }

    #[test]
    fn static_tiff_reads_as_one_frame() {
        let bytes = encode_static(&solid(16, 16, [7, 8, 9, 255]), ImageFormat::Tiff);
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn animated_gif_reads_every_frame() {
        let bytes = encode_gif(vec![
            frame([255, 0, 0, 255], 100),
            frame([0, 255, 0, 255], 100),
            frame([0, 0, 255, 255], 100),
        ]);
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 3);
        for frame in &frames {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            assert_eq!(numerator / denominator, 100);
        }
    }

    #[test]
    fn apng_reads_every_frame() {
        let bytes = encode_frames_as_png(vec![frame([255, 0, 0, 255], 50), frame([0, 255, 0, 255], 50)]).unwrap();
        let frames = read_image_as_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].buffer().get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(frames[1].buffer().get_pixel(0, 0).0, [0, 255, 0, 255]);
    }

    #[test]
    fn gif_converts_to_animated_png() {
        let gif = encode_gif(vec![frame([255, 0, 0, 255], 100), frame([0, 255, 0, 255], 100)]);
        let apng = encode_frames_as_png(read_image_as_frames(&gif).unwrap()).unwrap();
        assert_eq!(&apng[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(read_image_as_frames(&apng).unwrap().len(), 2);
    }

    #[test]
    fn thumbnail_scales_to_fit_and_encodes_png() {
        let bytes = encode_static(&solid(1000, 500, [12, 34, 56, 255]), ImageFormat::Png);
        let thumbnail = generate_thumbnail(&bytes).unwrap();
        assert_eq!(&thumbnail[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&thumbnail).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (400, 200));
    }

    #[test]
    fn thumbnail_accepts_jpeg() {
        let bytes = encode_static(&solid(1000, 500, [30, 60, 90, 255]), ImageFormat::Jpeg);
        let thumbnail = generate_thumbnail(&bytes).unwrap();
        let decoded = image::load_from_memory(&thumbnail).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (400, 200));
    }

    #[test]
    fn garbage_bytes_are_rejected() {
        assert!(read_image_as_frames(b"not an image at all").is_err());
        assert!(generate_thumbnail(b"not an image at all").is_err());
    }

    #[test]
    fn empty_bytes_are_rejected() {
        assert!(read_image_as_frames(&[]).is_err());
        assert!(generate_thumbnail(&[]).is_err());
    }
}
