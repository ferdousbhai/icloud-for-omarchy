//! A file added to a note from this computer: its type, and an image's
//! size, as the note's new `Attachment` record states them
//! (docs/DESIGN.md, "Adding an attachment").

use crate::js::posix;

/// The largest file push uploads as a new attachment.
pub const MAX_NEW_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// The type of a file added as an attachment, from its extension
/// (`public.data` when it says nothing). Agrees with
/// `vault::attachments::extension_for_uti` and `embeds::is_image_uti`: a
/// type that names an extension there gets that extension's files here.
pub fn uti_for_file_name(name: &str) -> &'static str {
    let extension = posix::extname(name).to_ascii_lowercase();
    match extension.trim_start_matches('.') {
        "png" => "public.png",
        "jpg" | "jpeg" => "public.jpeg",
        "heic" => "public.heic",
        "gif" => "com.compuserve.gif",
        "tiff" | "tif" => "public.tiff",
        "webp" => "org.webmproject.webp",
        "pdf" => "com.adobe.pdf",
        "m4a" => "com.apple.m4a-audio",
        "mp3" => "public.mp3",
        "mp4" => "public.mpeg-4",
        "mov" => "com.apple.quicktime-movie",
        "txt" => "public.plain-text",
        "zip" => "public.zip-archive",
        _ => "public.data",
    }
}

/// The pixel size (width, height) a PNG, JPEG, GIF or WebP file states in
/// its header; `None` for anything else, or a header cut short.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let size = png_dimensions(bytes)
        .or_else(|| gif_dimensions(bytes))
        .or_else(|| webp_dimensions(bytes))
        .or_else(|| jpeg_dimensions(bytes))?;
    (size.0 > 0 && size.1 > 0).then_some(size)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}

/// The signature, then the IHDR chunk: length, "IHDR", width, height.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    Some((be32(bytes, 16)?, be32(bytes, 20)?))
}

/// "GIF87a" or "GIF89a", then the logical screen's width and height.
fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"GIF87a") && !bytes.starts_with(b"GIF89a") {
        return None;
    }
    Some((le16(bytes, 6)?, le16(bytes, 8)?))
}

/// "RIFF", a length, "WEBP", then the first chunk: the canvas of an
/// extended file (`VP8X`), or the frame of a lossy (`VP8 `) or lossless
/// (`VP8L`) one.
fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let le24 = |at: usize| Some(u32::from(*bytes.get(at)?) | le16(bytes, at + 1)? << 8);
    match bytes.get(12..16)? {
        b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
        b"VP8 " if bytes.get(23..26)? == [0x9d, 0x01, 0x2a] => {
            Some((le16(bytes, 26)? & 0x3fff, le16(bytes, 28)? & 0x3fff))
        }
        b"VP8L" if *bytes.get(20)? == 0x2f => {
            let bits = le16(bytes, 21)? | le16(bytes, 23)? << 16;
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        _ => None,
    }
}

/// The first start-of-frame segment: length, precision, height, width.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xff {
            return None;
        }
        // Fill bytes before a marker.
        while *bytes.get(at + 1)? == 0xff {
            at += 1;
        }
        let marker = bytes[at + 1];
        at += 2;
        match marker {
            // Markers that stand alone.
            0x01 | 0xd0..=0xd8 => continue,
            // The image data, or the end: no frame header came first.
            0xd9 | 0xda => return None,
            // SOF0-SOF15, but for DHT (c4), JPG (c8) and DAC (cc).
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                return Some((be16(bytes, at + 5)?, be16(bytes, at + 3)?));
            }
            _ => at += be16(bytes, at)?.max(2) as usize,
        }
    }
}
