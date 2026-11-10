//! Image files into textures for the grid and the viewer.

use std::path::Path;

use gtk::gdk;

/// The most pixels a file is decoded at: 400 MB as RGBA, room for any
/// camera's photo (a 48 MP iPhone photo is 8064 × 6048).
pub const MAX_PIXELS: u64 = 100_000_000;

/// Whether an image `w` × `h` (as its header says) may be decoded.
pub fn fits(w: i32, h: i32, max_pixels: u64) -> bool {
    match (u64::try_from(w), u64::try_from(h)) {
        (Ok(w @ 1..), Ok(h @ 1..)) => w * h <= max_pixels,
        _ => false,
    }
}

/// Decode `path` at full size, unless its header says it is too big to hold
/// in memory (a few-MB file can claim 60000 × 60000, 14 GB decoded). Such a
/// file is refused, not scaled: gdk-pixbuf decodes it whole before scaling.
pub fn texture(path: &Path) -> Result<gdk::Texture, String> {
    texture_within(path, MAX_PIXELS)
}

fn texture_within(path: &Path, max_pixels: u64) -> Result<gdk::Texture, String> {
    match gtk::gdk_pixbuf::Pixbuf::file_info(path) {
        Some((_, w, h)) if fits(w, h, max_pixels) => gdk::Texture::from_filename(path).map_err(|e| e.to_string()),
        Some((_, w, h)) => Err(format!("the image is too large to show ({w} × {h})")),
        None => Err("not an image that can be shown".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    #[test]
    fn fits_camera_photos_but_not_a_decompression_bomb() {
        assert!(fits(8064, 6048, MAX_PIXELS));
        assert!(fits(10_000, 10_000, MAX_PIXELS));
        assert!(!fits(10_001, 10_000, MAX_PIXELS));
        assert!(!fits(60_000, 60_000, MAX_PIXELS));
        assert!(!fits(i32::MAX, i32::MAX, MAX_PIXELS));
        assert!(!fits(0, 100, MAX_PIXELS));
        assert!(!fits(-1, -1, MAX_PIXELS));
    }

    #[test]
    fn texture_refuses_a_file_over_the_cap_before_decoding() {
        let file = tempfile::Builder::new().suffix(".jpg").tempfile().unwrap();
        let px = vec![128u8; 400 * 300 * 3];
        jpeg_encoder::Encoder::new_file(file.path(), 80)
            .unwrap()
            .encode(&px, 400, 300, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let tex = texture_within(file.path(), 120_000).unwrap();
        assert_eq!((tex.width(), tex.height()), (400, 300));
        assert!(texture_within(file.path(), 119_999).unwrap_err().contains("too large"));
        // A 65-byte PNG that says it is 60000 × 60000 is refused from its header.
        let png = b"\x89PNG\r\n\x1a\n\
            \0\0\0\x0dIHDR\0\0\xea\x60\0\0\xea\x60\x08\x02\0\0\0\x0f\xb0\xe2\x15\
            \0\0\0\x08IDAT\x78\x9c\x03\0\0\0\0\x01\x48\x06\x89\xd2\
            \0\0\0\0IEND\xae\x42\x60\x82";
        std::fs::write(file.path(), png).unwrap();
        assert_eq!(
            texture(file.path()).unwrap_err(),
            "the image is too large to show (60000 × 60000)"
        );
        // Not an image at all.
        std::fs::write(file.path(), b"not a picture").unwrap();
        assert!(texture_within(file.path(), MAX_PIXELS).is_err());
    }
}
