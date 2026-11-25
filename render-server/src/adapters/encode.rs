//! A whole frame as a PNG file's bytes (pure Rust, from the `image` crate). Ported from SolidZ's
//! render server `adapters/encode.rs`; its JPEG/stripe encoders arrive with the stream mode.

use std::io::Cursor;

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

/// `frame` (tight RGBA rows, `w`x`h`) as a PNG, every pixel opaque.
pub fn png(frame: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut opaque = frame.to_vec();
    for px in opaque.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&opaque, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("png: {e}"))?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_becomes_an_opaque_png() {
        let frame: Vec<u8> = (0..4u32 * 3).flat_map(|i| [i as u8, 7, 9, 0]).collect();
        let bytes = png(&frame, 4, 3).unwrap_or_default();
        assert_eq!(bytes.get(1..4), Some(&b"PNG"[..]));
    }
}
