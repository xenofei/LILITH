//! The renderer registry (APP-038; Appendix H "Thumbnails"; P4b-R12): `image@1` draws PNG,
//! JPEG and WebP files; `icon@1` draws nothing (the app draws the media kind's icon, never a
//! decode error).
//!
//! `image@1` decodes untrusted bytes, so before any decoder is made it checks, in this order
//! (PF-32): the file's size (at most [`MAX_FILE_BYTES`]), its magic bytes (a PNG, JPEG or WebP
//! signature, whatever the extension says), then the dimensions in its header, read here by
//! hand from the bytes already in memory (at most [`MAX_SIDE`] a side and [`MAX_PIXELS`]), and
//! for a JPEG its integrity ([`jpeg_complete`]). Only then does the `image` crate decode, with
//! its strict [`MAX_SIDE`] limit and the best-effort allocation limit of [`alloc_limit`]. The
//! decoded image is scaled with Lanczos3 so its long side is the requested size (never up),
//! and encoded as PNG.

use std::io::Cursor;
use std::sync::Arc;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use lil_schema::MediaKind;

/// `image@1`'s id.
pub const IMAGE: &str = "image";
/// `icon@1`'s id.
pub const ICON: &str = "icon";
/// The extensions `image@1` decodes; `lil_schema::display::renderable` says the same.
pub const DECODED_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// A file larger than this isn't read for a render (256 MiB).
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// An image wider or taller than this (per its header) isn't decoded.
pub const MAX_SIDE: u32 = 16_384;
/// An image of more pixels than this (per its header) isn't decoded: 256 Mpx, 16,384².
pub const MAX_PIXELS: u64 = 256 * 1024 * 1024;
/// The decoder's allocation limit for the tiers (up to 512 px).
pub const ALLOC_TIERS: u64 = 256 * 1024 * 1024;
/// The decoder's allocation limit above 512 px (`large_px`, or a tier at a high scale).
pub const ALLOC_LARGE: u64 = 512 * 1024 * 1024;

/// The decoder's allocation limit for a render to `px` (P4b-R36): [`ALLOC_TIERS`] up to 512 px,
/// [`ALLOC_LARGE`] above.
pub fn alloc_limit(px: u32) -> u64 {
    if px <= 512 { ALLOC_TIERS } else { ALLOC_LARGE }
}

/// Why a render gave no image. Every one of them means the `icon@1` state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The renderer draws nothing (`icon@1`).
    NoImage,
    /// The bytes carry no PNG, JPEG or WebP signature.
    NotAnImage,
    /// The file or the image its header describes is over a limit; nothing was decoded.
    TooLarge(String),
    /// The header can't be read, or a JPEG ends inside its scan.
    Damaged(String),
    /// The decoder or the encoder failed.
    Decode(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::NoImage => f.write_str("this renderer draws no image"),
            RenderError::NotAnImage => f.write_str("not a PNG, JPEG or WebP file"),
            RenderError::TooLarge(why) => write!(f, "too large: {why}"),
            RenderError::Damaged(why) => write!(f, "damaged: {why}"),
            RenderError::Decode(why) => write!(f, "can't be decoded: {why}"),
        }
    }
}

/// A thumbnail renderer: draws one media kind's thumbnails.
pub trait Renderer: Send + Sync {
    /// `image`, `icon`.
    fn id(&self) -> &'static str;
    /// Part of the cache key: a new version renders every thumbnail anew.
    fn version(&self) -> u32;
    /// Whether it draws files of `kind`.
    fn renders(&self, kind: MediaKind) -> bool;
    /// `bytes` drawn with its long side at most `px`, as PNG bytes.
    ///
    /// # Errors
    /// Why it can't: the item gets the `icon@1` state.
    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError>;
}

/// `image@1`: PNG, JPEG and WebP (module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageRenderer;

impl Renderer for ImageRenderer {
    fn id(&self) -> &'static str {
        IMAGE
    }

    fn version(&self) -> u32 {
        1
    }

    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }

    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
        render_image(bytes, px)
    }
}

/// `icon@1`: every kind without a renderer of its own. It draws nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct IconRenderer;

impl Renderer for IconRenderer {
    fn id(&self) -> &'static str {
        ICON
    }

    fn version(&self) -> u32 {
        1
    }

    fn renders(&self, kind: MediaKind) -> bool {
        !kind.has_renderer()
    }

    fn render(&self, _bytes: &[u8], _px: u32) -> Result<Vec<u8>, RenderError> {
        Err(RenderError::NoImage)
    }
}

/// Plan 4b's registry: `image@1`, then `icon@1`.
pub fn default_renderers() -> Vec<Arc<dyn Renderer>> {
    vec![Arc::new(ImageRenderer), Arc::new(IconRenderer)]
}

/// The image formats `image@1` decodes, as their magic bytes tell them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    Png,
    Jpeg,
    Webp,
}

impl Sniffed {
    fn format(self) -> ImageFormat {
        match self {
            Sniffed::Png => ImageFormat::Png,
            Sniffed::Jpeg => ImageFormat::Jpeg,
            Sniffed::Webp => ImageFormat::WebP,
        }
    }
}

/// The format `bytes` start with: the PNG signature, a JPEG's `FF D8 FF`, or a RIFF `WEBP`
/// header; `None` for anything else (an executable named `.png` among them).
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Sniffed::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Sniffed::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Sniffed::Webp)
    } else {
        None
    }
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

/// The width and height `bytes`' header claims, read without decoding anything: a PNG's
/// `IHDR` (which must be its first chunk), a JPEG's first start-of-frame marker before its
/// first scan, a WebP's `VP8 `, `VP8L` or `VP8X` chunk. `None` when there is none to read.
pub fn header_size(bytes: &[u8], format: Sniffed) -> Option<(u32, u32)> {
    match format {
        Sniffed::Png => {
            // Signature (8), then the first chunk: length (4), type (4), width, height.
            (bytes.get(12..16)? == b"IHDR").then_some(())?;
            Some((be32(bytes, 16)?, be32(bytes, 20)?))
        }
        Sniffed::Jpeg => jpeg_frame_size(bytes),
        Sniffed::Webp => {
            let chunk = bytes.get(12..16)?;
            let data = 20;
            match chunk {
                b"VP8 " => {
                    // Frame tag (3), start code 9d 01 2a, then 14-bit width and height.
                    (bytes.get(data + 3..data + 6)? == [0x9d, 0x01, 0x2a]).then_some(())?;
                    Some((
                        le16(bytes, data + 6)? & 0x3FFF,
                        le16(bytes, data + 8)? & 0x3FFF,
                    ))
                }
                b"VP8L" => {
                    // Signature 0x2f, then 14 bits of width − 1 and 14 of height − 1.
                    (*bytes.get(data)? == 0x2f).then_some(())?;
                    let bits = u32::from_le_bytes(bytes.get(data + 1..data + 5)?.try_into().ok()?);
                    Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
                }
                b"VP8X" => {
                    // Flags (1), reserved (3), then 24 bits each of canvas width − 1 and
                    // height − 1.
                    Some((le24(bytes, data + 4)? + 1, le24(bytes, data + 7)? + 1))
                }
                _ => None,
            }
        }
    }
}

/// A JPEG's frame size: the first start-of-frame marker (`FF C0`–`FF CF` but `C4`, `C8` and
/// `CC`), walked to segment by segment from the start; `None` if a scan (`FF DA`) or the end
/// comes first.
fn jpeg_frame_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2; // After `FF D8`.
    loop {
        // A marker: `FF`, fill bytes `FF`, then its code.
        (*bytes.get(at)? == 0xFF).then_some(())?;
        while *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        let code = *bytes.get(at + 1)?;
        at += 2;
        match code {
            // Standalone markers: no length.
            0x01 | 0xD0..=0xD7 => {}
            0xC0..=0xCF if !matches!(code, 0xC4 | 0xC8 | 0xCC) => {
                // Length (2), precision (1), height (2), width (2).
                let (height, width) = (be16(bytes, at + 3)?, be16(bytes, at + 5)?);
                return Some((width, height));
            }
            0xDA | 0xD9 => return None,
            _ => at += usize::try_from(be16(bytes, at)?).ok()?,
        }
    }
}

/// Whether a JPEG holds a whole scan (PF-24, PF-27): its last end-of-image marker `FF D9`
/// comes after its last start-of-scan marker `FF DA`. A JPEG cut inside its scan otherwise
/// decodes without an error, and an EXIF thumbnail's end-of-image marker sits in APP1 before
/// the main scan, so the last of each is compared. One with no `FF DA` is not whole either.
pub fn jpeg_complete(bytes: &[u8]) -> bool {
    let last = |marker: u8| bytes.windows(2).rposition(|w| w == [0xFF, marker]);
    match (last(0xDA), last(0xD9)) {
        (Some(scan), Some(end)) => end > scan,
        _ => false,
    }
}

/// The header checks (P4b-R12): at most [`MAX_SIDE`] a side and [`MAX_PIXELS`] in all, and not
/// empty.
///
/// # Errors
/// [`RenderError::TooLarge`] or, for a zero side, [`RenderError::Damaged`].
pub fn check_header(width: u32, height: u32) -> Result<(), RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::Damaged(format!(
            "the header claims {width} x {height} px"
        )));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(RenderError::TooLarge(format!(
            "the header claims {width} x {height} px; at most {MAX_SIDE} a side"
        )));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(RenderError::TooLarge(format!(
            "the header claims {width} x {height} px; at most 256 Mpx"
        )));
    }
    Ok(())
}

/// `image@1`'s render (module docs): the guards in order, then the decode under the `image`
/// crate's limits, the Lanczos3 scale and the PNG encode.
///
/// # Errors
/// The guard that refused, or the decoder's or encoder's failure.
pub fn render_image(bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
    // 1. The size.
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(RenderError::TooLarge(format!(
            "{} bytes; at most 256 MiB",
            bytes.len()
        )));
    }
    let format = sniff(bytes).ok_or(RenderError::NotAnImage)?;
    // 2. The header's dimensions, before any decoder exists.
    let (width, height) = header_size(bytes, format)
        .ok_or_else(|| RenderError::Damaged("its header can't be read".to_string()))?;
    check_header(width, height)?;
    if format == Sniffed::Jpeg && !jpeg_complete(bytes) {
        return Err(RenderError::Damaged(
            "the JPEG ends inside its scan".to_string(),
        ));
    }
    // 3. The decode, under the crate's limits.
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(alloc_limit(px));
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format.format());
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| RenderError::Decode(e.to_string()))?;
    let image = if image.width().max(image.height()) > px {
        image.resize(px, px, FilterType::Lanczos3)
    } else {
        image
    };
    let image = if image.color().has_alpha() {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    };
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| RenderError::Decode(e.to_string()))?;
    Ok(png)
}
