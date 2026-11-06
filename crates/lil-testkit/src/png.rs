//! A small hand-written PNG builder for tests (QA-031, QA-066): images are generated in the test,
//! never committed, and the spec tests and the test kit don't depend on an image crate.
//!
//! [`rgba`] writes an 8-bit RGBA PNG whose `IDAT` is a zlib stream of stored (uncompressed)
//! deflate blocks, with each chunk's CRC-32 and the stream's Adler-32; [`with_text`] adds a
//! `tEXt` chunk (as ComfyUI stores its `prompt`); [`header_only`] writes a PNG with an `IHDR` and
//! no image data (a header that claims any size); [`long_side`] reads the size back from `IHDR`.

/// The eight bytes every PNG starts with.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The largest stored deflate block (RFC 1951 §3.2.4: `LEN` is 16 bits).
const STORED_MAX: usize = 65_535;

/// An 8-bit RGBA PNG of `w` × `h` pixels; `pixels` holds `w * h * 4` bytes, row by row, top to
/// bottom, each pixel `R G B A`.
///
/// # Panics
/// If `w` or `h` is 0, or `pixels` isn't `w * h * 4` bytes long.
pub fn rgba(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
    assert!(
        w > 0 && h > 0,
        "a PNG is at least 1 x 1 pixels, not {w} x {h}"
    );
    let row = w as usize * 4;
    assert_eq!(
        pixels.len(),
        row * h as usize,
        "{w} x {h} RGBA pixels are {} bytes",
        row * h as usize
    );
    // Each scanline starts with its filter type: 0, none.
    let mut raw = Vec::with_capacity(pixels.len() + h as usize);
    for line in pixels.chunks(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut png = SIGNATURE.to_vec();
    push_chunk(&mut png, b"IHDR", &ihdr(w, h));
    push_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut png, b"IEND", &[]);
    png
}

/// `png` with a `tEXt` chunk `key` = `value` inserted right after its `IHDR`.
///
/// # Panics
/// If `png` doesn't start with the signature and an `IHDR`, if `key` isn't 1–79 Latin-1
/// printable characters without leading, trailing or consecutive spaces, or if either string
/// holds a character outside Latin-1 or a NUL.
pub fn with_text(png: &[u8], key: &str, value: &str) -> Vec<u8> {
    assert!(
        (1..=79).contains(&key.chars().count())
            && key
                .chars()
                .all(|c| matches!(c as u32, 0x20..=0x7e | 0xa1..=0xff)),
        "a tEXt keyword is 1-79 printable Latin-1 characters: {key:?}"
    );
    assert!(
        !key.starts_with(' ') && !key.ends_with(' ') && !key.contains("  "),
        "a tEXt keyword has no leading, trailing or consecutive spaces: {key:?}"
    );
    let ihdr_end = SIGNATURE.len() + 12 + 13;
    assert!(
        png.len() >= ihdr_end && png[..8] == SIGNATURE && &png[12..16] == b"IHDR",
        "not a PNG that starts with its IHDR"
    );
    let mut data = latin1(key);
    data.push(0);
    data.extend(latin1(value));
    let mut out = png[..ihdr_end].to_vec();
    push_chunk(&mut out, b"tEXt", &data);
    out.extend_from_slice(&png[ihdr_end..]);
    out
}

/// A PNG of the signature, an `IHDR` claiming `w` × `h` 8-bit RGBA pixels, and `IEND`: no image
/// data, so a reader that trusts the header before decoding sees any size it is told.
pub fn header_only(w: u32, h: u32) -> Vec<u8> {
    let mut png = SIGNATURE.to_vec();
    push_chunk(&mut png, b"IHDR", &ihdr(w, h));
    push_chunk(&mut png, b"IEND", &[]);
    png
}

/// The longer of the width and height that `png`'s `IHDR` gives.
///
/// # Panics
/// If `png` doesn't start with the signature and an `IHDR`.
pub fn long_side(png: &[u8]) -> u32 {
    let (w, h) = size(png);
    w.max(h)
}

/// The `(width, height)` that `png`'s `IHDR` gives.
///
/// # Panics
/// If `png` doesn't start with the signature and an `IHDR`.
pub fn size(png: &[u8]) -> (u32, u32) {
    assert!(
        png.len() >= 24 && png[..8] == SIGNATURE && &png[12..16] == b"IHDR",
        "not a PNG that starts with its IHDR ({} bytes)",
        png.len()
    );
    let be = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    (be(16), be(20))
}

/// CRC-32 (ISO 3309, as PNG uses it: polynomial `0xEDB88320` reflected, initial and final XOR
/// `0xFFFFFFFF`).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32 (RFC 1950 §8).
pub fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1_u32, 0_u32);
    for chunk in bytes.chunks(5_552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// `IHDR`'s 13 bytes: the size, bit depth 8, colour type 6 (RGBA), deflate, filter method 0,
/// no interlace.
fn ihdr(w: u32, h: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&w.to_be_bytes());
    data.extend_from_slice(&h.to_be_bytes());
    data.extend_from_slice(&[8, 6, 0, 0, 0]);
    data
}

/// Appends a chunk: length, type, data, and the CRC-32 of type and data.
fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).expect("a PNG chunk is under 2^31 bytes");
    png.extend_from_slice(&len.to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream (RFC 1950) holding `raw` in stored deflate blocks (RFC 1951 §3.2.4), then the
/// Adler-32 of `raw`.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    // CMF 0x78 (deflate, 32 KiB window), FLG 0x01 (no dictionary, fastest; 0x7801 % 31 == 0).
    let mut out = vec![0x78, 0x01];
    let mut blocks = raw.chunks(STORED_MAX).peekable();
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        // BFINAL in bit 0, BTYPE 00 (stored); the rest of the byte pads to the boundary.
        out.push(u8::from(last));
        let len = u16::try_from(block.len()).expect("a stored block is at most 65,535 bytes");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// `text` as Latin-1 bytes.
fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| {
            u8::try_from(u32::from(c))
                .ok()
                .filter(|b| *b != 0)
                .unwrap_or_else(|| panic!("{c:?} is not a Latin-1 character a tEXt chunk holds"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chunk of `png` after the signature, as `(type, data)`, with each CRC checked.
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(png[..8], SIGNATURE);
        let mut at = 8;
        let mut out = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
            let data = png[at + 8..at + 8 + len].to_vec();
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(
                crc,
                crc32(&png[at + 4..at + 8 + len]),
                "the CRC of {}",
                String::from_utf8_lossy(&kind)
            );
            out.push((kind, data));
            at += 12 + len;
        }
        assert_eq!(at, png.len(), "the last chunk ends the file");
        out
    }

    /// Reads a zlib stream of stored blocks back, checking its header and Adler-32: the data
    /// and the number of blocks.
    fn inflate_stored(z: &[u8]) -> (Vec<u8>, usize) {
        assert_eq!(z[0] & 0x0f, 8, "deflate");
        assert_eq!((u16::from(z[0]) << 8 | u16::from(z[1])) % 31, 0, "FCHECK");
        let mut at = 2;
        let mut out = Vec::new();
        let mut blocks = 0;
        loop {
            blocks += 1;
            let header = z[at];
            assert_eq!(header >> 1 & 3, 0, "a stored block");
            let len = u16::from_le_bytes([z[at + 1], z[at + 2]]);
            let nlen = u16::from_le_bytes([z[at + 3], z[at + 4]]);
            assert_eq!(len, !nlen, "NLEN is the complement of LEN");
            out.extend_from_slice(&z[at + 5..at + 5 + len as usize]);
            at += 5 + len as usize;
            if header & 1 == 1 {
                break;
            }
        }
        let adler = u32::from_be_bytes(z[at..at + 4].try_into().unwrap());
        assert_eq!(adler, adler32(&out), "the Adler-32 trailer");
        assert_eq!(at + 4, z.len(), "the trailer ends the stream");
        (out, blocks)
    }

    fn pattern(w: u32, h: u32) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn checksums_match_their_published_check_values() {
        // CRC-32/ISO-HDLC and Adler-32 check values for "123456789" (and Adler's "Wikipedia").
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(adler32(b""), 1);
        // The IEND chunk every PNG ends with: its CRC is AE 42 60 82.
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn rgba_builds_valid_chunks_and_a_stored_zlib_stream() {
        // 200 x 100 RGBA is 80,100 bytes with filter bytes: two stored blocks.
        let (w, h) = (200, 100);
        let pixels = pattern(w, h);
        let png = rgba(w, h, &pixels);
        let chunks = chunks(&png);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);
        assert_eq!(chunks[0].1, [0, 0, 0, 200, 0, 0, 0, 100, 8, 6, 0, 0, 0]);
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]));

        let (raw, blocks) = inflate_stored(&chunks[1].1);
        assert_eq!(blocks, 2, "80,100 bytes need two stored blocks");
        assert_eq!(raw.len(), pixels.len() + h as usize);
        for (y, line) in raw.chunks(w as usize * 4 + 1).enumerate() {
            assert_eq!(line[0], 0, "row {y}'s filter byte");
            let row = &pixels[y * w as usize * 4..(y + 1) * w as usize * 4];
            assert_eq!(&line[1..], row, "row {y}'s pixels");
        }
    }

    #[test]
    fn long_side_reads_ihdr_back() {
        let png = rgba(1024, 512, &vec![0x80; 1024 * 512 * 4]);
        assert_eq!(long_side(&png), 1024);
        assert_eq!(size(&png), (1024, 512));
        assert_eq!(long_side(&rgba(3, 7, &pattern(3, 7))), 7);
        assert_eq!(long_side(&header_only(60_000, 60_000)), 60_000);
    }

    #[test]
    fn header_only_has_no_image_data() {
        let png = header_only(60_000, 40_000);
        let chunks = chunks(&png);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [b"IHDR", b"IEND"]);
        assert_eq!(size(&png), (60_000, 40_000));
    }

    #[test]
    fn with_text_adds_a_text_chunk_after_ihdr() {
        let png = rgba(2, 2, &pattern(2, 2));
        let value = r#"{"3": {"class_type": "KSampler", "inputs": {"seed": 7}}}"#;
        let tagged = with_text(&png, "prompt", value);
        let chunks = chunks(&tagged);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [b"IHDR", b"tEXt", b"IDAT", b"IEND"]);
        let mut expected = b"prompt\0".to_vec();
        expected.extend_from_slice(value.as_bytes());
        assert_eq!(chunks[1].1, expected);
        assert_eq!(long_side(&tagged), 2);
        // The image data is unchanged.
        assert_eq!(chunks[2].1, self::chunks(&png)[1].1);
    }

    #[test]
    #[should_panic(expected = "no leading, trailing or consecutive spaces")]
    fn with_text_refuses_a_keyword_with_double_spaces() {
        with_text(&rgba(1, 1, &[0; 4]), "two  spaces", "x");
    }

    #[test]
    fn a_one_pixel_image_is_one_final_block() {
        let png = rgba(1, 1, &[1, 2, 3, 4]);
        let idat = &chunks(&png)[1].1;
        assert_eq!(idat[2], 1, "BFINAL set, BTYPE stored");
        let (raw, blocks) = inflate_stored(idat);
        assert_eq!((raw.as_slice(), blocks), ([0, 1, 2, 3, 4].as_slice(), 1));
    }
}
