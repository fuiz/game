//! Corkboard image processing library.

use image::{AnimationDecoder, Frame, ImageFormat, ImageReader, ImageResult};
use num_integer::Integer;

pub use png;

/// Reads an image from bytes and returns its frames.
pub fn read_image_as_frames(bytes: &[u8]) -> ImageResult<Vec<Frame>> {
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
        // TODO: Uncomment when this issue is resolved: https://github.com/image-rs/image/issues/2761
        // ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(reader.into_inner())?
        //     .into_frames()
        //     .collect_frames(),
        _ => {
            let image = reader.decode()?.to_rgba8();
            Ok(vec![Frame::new(image)])
        }
    }
}

/// Encodes a sequence of frames as an animated PNG.
pub fn encode_frames_as_png(frames: Vec<Frame>) -> Result<Vec<u8>, png::EncodingError> {
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
