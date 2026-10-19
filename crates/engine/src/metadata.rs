//! Lossless metadata removal for JPEG, PNG and WebP. Image data is copied byte for byte; only
//! metadata containers are dropped (EXIF, XMP, IPTC, comments, text chunks, embedded
//! thumbnails and anything after the end of the image). What affects how the image looks is
//! kept: ICC profiles, colour/gamma chunks, JFIF/Adobe headers, transparency, animation.
//!
//! EXIF orientation is the one tag carried over (as a minimal EXIF block), because dropping it
//! would turn portrait photos sideways.

use image::ImageFormat;

use crate::{EngineError, Result};

pub(crate) fn strip(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>> {
    match format {
        ImageFormat::Jpeg => strip_jpeg(bytes),
        ImageFormat::Png => strip_png(bytes),
        ImageFormat::WebP => strip_webp(bytes),
        other => Err(EngineError::UnsupportedFormat(format!("{other:?}"))),
    }
}

fn damaged(what: &str) -> EngineError {
    EngineError::UnsupportedFormat(format!("damaged {what}"))
}

// ---- EXIF orientation ----------------------------------------------------------------------

/// The orientation tag (2..=8) of an EXIF TIFF block; `None` if absent, normal (1) or unreadable.
fn orientation(tiff: &[u8]) -> Option<u16> {
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |i: usize| {
        let b: [u8; 2] = tiff.get(i..i.checked_add(2)?)?.try_into().ok()?;
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |i: usize| {
        let b: [u8; 4] = tiff.get(i..i.checked_add(4)?)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    for k in 0..usize::from(u16_at(ifd)?) {
        let entry = ifd + 2 + k * 12;
        if u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 {
            return Some(u16_at(entry + 8)?).filter(|v| (2..=8).contains(v));
        }
    }
    None
}

/// A big-endian TIFF block whose only tag is Orientation.
fn minimal_exif(orientation: u16) -> Vec<u8> {
    let mut t = b"MM\0\x2a\0\0\0\x08".to_vec();
    t.extend(1u16.to_be_bytes()); // one entry
    t.extend(0x0112u16.to_be_bytes()); // Orientation
    t.extend(3u16.to_be_bytes()); // SHORT
    t.extend(1u32.to_be_bytes()); // count
    t.extend(orientation.to_be_bytes());
    t.extend([0, 0]); // value padding
    t.extend(0u32.to_be_bytes()); // no next IFD
    t
}

// ---- JPEG ----------------------------------------------------------------------------------

/// The index just past the EOI marker that ends the scans starting at `from`, or the end of
/// the data if it is truncated. In entropy-coded data 0xFF is always followed by 0x00 or a
/// restart marker, so the first other marker ends the scan; markers between progressive
/// scans are skipped by their length.
fn scans_end(b: &[u8], mut j: usize) -> usize {
    loop {
        while j + 1 < b.len() && !(b[j] == 0xFF && b[j + 1] != 0x00 && !(0xD0..=0xD7).contains(&b[j + 1])) {
            j += 1;
        }
        if j + 1 >= b.len() {
            return b.len();
        }
        match b[j + 1] {
            0xFF => j += 1, // fill byte
            0xD9 => return j + 2,
            _ => {
                let Some(len) = b.get(j + 2..j + 4) else { return b.len() };
                j += 2 + usize::from(u16::from_be_bytes([len[0], len[1]]));
            }
        }
    }
}

fn strip_jpeg(b: &[u8]) -> Result<Vec<u8>> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return Err(damaged("JPEG"));
    }
    let mut out = vec![0xFF, 0xD8];
    let mut orient = None;
    // EXIF goes right after SOI, or after a leading JFIF APP0 (both orders are accepted).
    let mut exif_at = out.len();
    let mut i = 2;
    loop {
        if b.get(i) != Some(&0xFF) {
            return Err(damaged("JPEG"));
        }
        let marker = *b.get(i + 1).ok_or_else(|| damaged("JPEG"))?;
        match marker {
            0xFF => {
                i += 1; // fill byte
                continue;
            }
            0xD9 => {
                out.extend([0xFF, 0xD9]);
                break;
            }
            0x01 | 0xD0..=0xD7 => {
                out.extend(&b[i..i + 2]); // no length
                i += 2;
                continue;
            }
            _ => {}
        }
        let len = b.get(i + 2..i + 4).map(|l| usize::from(u16::from_be_bytes([l[0], l[1]])));
        let end = len.filter(|l| *l >= 2).map(|l| i + 2 + l).ok_or_else(|| damaged("JPEG"))?;
        let segment = b.get(i..end).ok_or_else(|| damaged("JPEG"))?;
        let payload = &segment[4..];
        if marker == 0xDA {
            out.extend(&b[i..scans_end(b, end)]); // the image data, through EOI
            break;
        }
        let keep = match marker {
            0xE0 => payload.starts_with(b"JFIF\0"),
            0xE2 => payload.starts_with(b"ICC_PROFILE\0"),
            0xEE => payload.starts_with(b"Adobe"),
            0xE1 => {
                if let Some(tiff) = payload.strip_prefix(b"Exif\0\0") {
                    orient = orient.or(orientation(tiff));
                }
                false
            }
            0xE3..=0xEF | 0xFE => false, // other APPn, comments
            _ => true,                   // tables, frame headers, ...
        };
        if keep {
            let first_app0 = marker == 0xE0 && out.len() == 2;
            out.extend(segment);
            if first_app0 {
                exif_at = out.len();
            }
        }
        i = end;
    }
    if let Some(o) = orient {
        let tiff = minimal_exif(o);
        let mut app1 = vec![0xFF, 0xE1];
        app1.extend(u16::try_from(tiff.len() + 8).unwrap_or(u16::MAX).to_be_bytes()); // length + "Exif\0\0"
        app1.extend(b"Exif\0\0");
        app1.extend(tiff);
        out.splice(exif_at..exif_at, app1);
    }
    Ok(out)
}

// ---- PNG -----------------------------------------------------------------------------------

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Chunks that affect how the image looks (or animates); everything else is dropped.
const PNG_KEEP: [&[u8; 4]; 18] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"bKGD",
    b"pHYs", b"acTL", b"fcTL", b"fdAT", b"cICP", b"mDCV", b"cLLI",
];

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend(u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend(kind);
    out.extend(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend(crc.finalize().to_be_bytes());
}

fn strip_png(b: &[u8]) -> Result<Vec<u8>> {
    if !b.starts_with(PNG_SIGNATURE) {
        return Err(damaged("PNG"));
    }
    let mut kept: Vec<(&[u8; 4], &[u8])> = Vec::new();
    let mut orient = None;
    let mut i = PNG_SIGNATURE.len();
    loop {
        let header = b.get(i..i + 8).ok_or_else(|| damaged("PNG"))?;
        let len = usize::try_from(u32::from_be_bytes([header[0], header[1], header[2], header[3]])).unwrap_or(usize::MAX);
        let kind: &[u8; 4] = header[4..8].try_into().unwrap_or(b"????");
        let data = b.get(i + 8..(i + 8).saturating_add(len)).ok_or_else(|| damaged("PNG"))?;
        if kind == b"eXIf" {
            orient = orient.or(orientation(data));
        } else if PNG_KEEP.contains(&kind) {
            kept.push((kind, data));
        }
        if kind == b"IEND" {
            break; // anything after IEND is dropped too
        }
        i += 12 + len;
    }
    let mut out = PNG_SIGNATURE.to_vec();
    let exif = orient.map(minimal_exif);
    let mut exif_written = false;
    for (kind, data) in kept {
        // eXIf must come before the image data.
        if let Some(tiff) = exif.as_deref().filter(|_| !exif_written && matches!(kind, b"IDAT" | b"IEND")) {
            png_chunk(&mut out, b"eXIf", tiff);
            exif_written = true;
        }
        png_chunk(&mut out, kind, data);
    }
    Ok(out)
}

// ---- WebP ----------------------------------------------------------------------------------

const VP8X_EXIF: u8 = 0x08;
const VP8X_XMP: u8 = 0x04;

fn riff_chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
    out.extend(kind);
    out.extend(u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

fn strip_webp(b: &[u8]) -> Result<Vec<u8>> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return Err(damaged("WebP"));
    }
    let mut chunks: Vec<(&[u8], &[u8])> = Vec::new();
    let mut i = 12;
    while i + 8 <= b.len() {
        let kind = &b[i..i + 4];
        let len = usize::try_from(u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]])).unwrap_or(usize::MAX);
        let data = b.get(i + 8..(i + 8).saturating_add(len)).ok_or_else(|| damaged("WebP"))?;
        chunks.push((kind, data));
        i += 8 + len + len % 2;
    }
    // The simple format (one VP8/VP8L chunk) can't carry metadata; only VP8X files can.
    if chunks.first().is_none_or(|(kind, _)| *kind != b"VP8X") {
        return Ok(b.to_vec());
    }
    let mut orient = None;
    let mut body = Vec::new();
    let mut vp8x = Vec::new();
    for (kind, data) in chunks {
        match kind {
            b"VP8X" => vp8x = data.to_vec(),
            b"EXIF" => orient = orient.or(orientation(data.strip_prefix(b"Exif\0\0").unwrap_or(data))),
            b"ICCP" | b"ANIM" | b"ANMF" | b"ALPH" | b"VP8 " | b"VP8L" => riff_chunk(&mut body, kind, data),
            _ => {} // XMP and unknown chunks
        }
    }
    let flags = vp8x.first_mut().ok_or_else(|| damaged("WebP"))?;
    *flags &= !(VP8X_EXIF | VP8X_XMP);
    if let Some(o) = orient {
        *flags |= VP8X_EXIF;
        riff_chunk(&mut body, b"EXIF", &minimal_exif(o)); // EXIF follows the image data
    }
    let mut payload = b"WEBP".to_vec();
    riff_chunk(&mut payload, b"VP8X", &vp8x);
    payload.extend(body);
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend(payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{ExtendedColorType, RgbImage};

    const SECRET: &[u8] = b"SecretCam";

    fn picture() -> RgbImage {
        RgbImage::from_fn(16, 8, |x, y| image::Rgb([(x * 16) as u8, (y * 32) as u8, 128]))
    }

    fn pixels(bytes: &[u8]) -> Vec<u8> {
        image::load_from_memory(bytes).unwrap().to_rgb8().into_raw()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// EXIF with Orientation and a Make string (`SecretCam`), in either byte order.
    fn exif(orientation: u16, little: bool) -> Vec<u8> {
        let w16 = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut t: Vec<u8> = if little { b"II".to_vec() } else { b"MM".to_vec() };
        t.extend(w16(42));
        t.extend(w32(8));
        t.extend(w16(2));
        let make_at = 8 + 2 + 2 * 12 + 4;
        t.extend(w16(0x010F)); // Make, ASCII, at make_at
        t.extend(w16(2));
        t.extend(w32(SECRET.len() as u32 + 1));
        t.extend(w32(make_at as u32));
        t.extend(w16(0x0112)); // Orientation
        t.extend(w16(3));
        t.extend(w32(1));
        t.extend(w16(orientation));
        t.extend([0, 0]);
        t.extend(w32(0));
        t.extend(SECRET);
        t.push(0);
        t
    }

    fn jpeg_with_metadata(orientation: u16) -> Vec<u8> {
        let img = picture();
        let mut plain = Vec::new();
        JpegEncoder::new_with_quality(&mut plain, 90).encode(img.as_raw(), 16, 8, ExtendedColorType::Rgb8).unwrap();
        let segment = |marker: u8, payload: &[u8]| {
            let mut s = vec![0xFF, marker];
            s.extend(((payload.len() + 2) as u16).to_be_bytes());
            s.extend(payload);
            s
        };
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(exif(orientation, true));
        let mut out = plain[..2].to_vec(); // SOI; the encoder's JFIF APP0 follows below
        out.extend(segment(0xE1, &app1));
        out.extend(segment(0xED, b"Photoshop 3.0\0SecretCam"));
        out.extend(segment(0xFE, b"SecretCam comment"));
        out.extend(&plain[2..]);
        out.extend(b"trailing SecretCam thumbnail"); // e.g. an MPF preview after EOI
        out
    }

    #[test]
    fn jpeg_loses_metadata_but_keeps_pixels_and_orientation() {
        let input = jpeg_with_metadata(6);
        let out = strip(&input, ImageFormat::Jpeg).unwrap();
        assert!(!contains(&out, SECRET));
        assert_eq!(pixels(&out), pixels(&input), "image data copied, not re-encoded");
        let at = out.windows(6).position(|w| w == b"Exif\0\0").unwrap();
        assert_eq!(orientation(&out[at + 6..]), Some(6));
        assert!(out.ends_with(&[0xFF, 0xD9]));

        let upright = strip(&jpeg_with_metadata(1), ImageFormat::Jpeg).unwrap();
        assert!(!contains(&upright, b"Exif"), "orientation 1 is the default: no EXIF at all");
        assert_eq!(strip(&upright, ImageFormat::Jpeg).unwrap(), upright, "idempotent");
    }

    #[test]
    fn png_loses_text_and_exif_chunks() {
        let mut plain = Vec::new();
        picture().write_to(&mut std::io::Cursor::new(&mut plain), ImageFormat::Png).unwrap();
        let ihdr_end = PNG_SIGNATURE.len() + 8 + 13 + 4;
        let mut input = plain[..ihdr_end].to_vec();
        png_chunk(&mut input, b"tEXt", b"Author\0SecretCam");
        png_chunk(&mut input, b"eXIf", &exif(8, false));
        png_chunk(&mut input, b"gAMA", &45455u32.to_be_bytes());
        input.extend(&plain[ihdr_end..]);
        input.extend(b"after IEND SecretCam");

        let out = strip(&input, ImageFormat::Png).unwrap();
        assert!(!contains(&out, SECRET));
        assert!(contains(&out, b"gAMA"), "rendering chunks stay");
        let exif_at = out.windows(4).position(|w| w == b"eXIf").unwrap();
        assert!(exif_at < out.windows(4).position(|w| w == b"IDAT").unwrap());
        assert_eq!(orientation(&out[exif_at + 4..]), Some(8));
        assert_eq!(pixels(&out), pixels(&input));
    }

    #[test]
    fn webp_loses_exif_and_xmp_chunks() {
        let img = picture();
        let mut simple = Vec::new();
        WebPEncoder::new_lossless(&mut simple).encode(img.as_raw(), 16, 8, ExtendedColorType::Rgb8).unwrap();
        assert_eq!(strip(&simple, ImageFormat::WebP).unwrap(), simple, "simple WebP has no metadata");

        // Rebuild it as an extended (VP8X) file with EXIF and XMP.
        let vp8l = &simple[12..];
        let mut vp8x = vec![VP8X_EXIF | VP8X_XMP, 0, 0, 0];
        vp8x.extend(&15u32.to_le_bytes()[..3]);
        vp8x.extend(&7u32.to_le_bytes()[..3]);
        let mut payload = b"WEBP".to_vec();
        riff_chunk(&mut payload, b"VP8X", &vp8x);
        payload.extend(vp8l);
        riff_chunk(&mut payload, b"EXIF", &exif(3, true));
        riff_chunk(&mut payload, b"XMP ", b"<x:xmpmeta>SecretCam</x:xmpmeta>");
        let mut input = b"RIFF".to_vec();
        input.extend((payload.len() as u32).to_le_bytes());
        input.extend(payload);

        let out = strip(&input, ImageFormat::WebP).unwrap();
        assert!(!contains(&out, SECRET) && !contains(&out, b"XMP "));
        assert_eq!(out[20] & (VP8X_EXIF | VP8X_XMP), VP8X_EXIF, "flags match the chunks");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize, out.len() - 8);
        let exif_at = out.windows(4).position(|w| w == b"EXIF").unwrap();
        assert_eq!(orientation(&out[exif_at + 8..]), Some(3));
        assert_eq!(pixels(&out), pixels(&input));
    }

    #[test]
    fn damaged_files_are_errors() {
        for (bytes, format) in [
            (&b"not a jpeg"[..], ImageFormat::Jpeg),
            (&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF][..], ImageFormat::Jpeg),
            (&b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"[..], ImageFormat::Png),
            (&b"RIFF\0\0\0\0WEBX"[..], ImageFormat::WebP),
        ] {
            assert!(matches!(strip(bytes, format), Err(EngineError::UnsupportedFormat(_))), "{bytes:?}");
        }
    }

    #[test]
    fn orientation_reads_both_byte_orders() {
        assert_eq!(orientation(&exif(6, true)), Some(6));
        assert_eq!(orientation(&exif(5, false)), Some(5));
        assert_eq!(orientation(&exif(1, true)), None);
        assert_eq!(orientation(&minimal_exif(7)), Some(7));
        assert_eq!(orientation(b"MM\0\x2a\xff\xff\xff\xff"), None, "bad offsets don't panic");
    }
}
