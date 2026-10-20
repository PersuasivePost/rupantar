//! Operations that read Office Open XML packages directly (they are zip files), no tools needed.
//!
//! - `doc.extract_images` (DOCX): copies every file under `word/media/` into a new folder
//!   `name (images)` next to the input (`name (images 2)`, ... if taken), keeping the original
//!   file names. A document without images is an `Empty` error and leaves nothing behind.
//! - `doc.compress.{light,balanced,strong}` (DOCX): the PDF compress levels applied to the
//!   images in `word/media/`: PNG/JPEG over 2400 / 1600 / 1100 px are downsampled (their size on
//!   the page is set in the document, so the layout doesn't change), JPEGs re-encoded at q85 /
//!   70 / 50, and with balanced/strong an opaque PNG becomes a JPEG (`image1.png` ->
//!   `image1.jpeg`, its relationships and `[Content_Types].xml` overrides updated). Transparent PNGs stay
//!   PNG; CMYK JPEGs, GIF, EMF/WMF, SVG etc. are left alone, and an image is only replaced when
//!   the result is smaller. Every other part is recompressed at the best Deflate level. The
//!   output (`name (compressed).docx`) is never bigger than the input.
//!
//! Zip entries are untrusted: only the file-name part of each entry is used (no paths), and
//! sizes are capped so a zip bomb can't fill the disk.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use image::ImageFormat;

use crate::mime::{self, DOCX};
use crate::pdf::{Preset, PRESETS};
use crate::{catalog, write_siblings, Converter, EngineError, Operation, Options, Result};

pub struct OfficeConverter;

const EXTRACT_IMAGES: &str = "doc.extract_images";
const COMPRESS: &str = "doc.compress";
const MAX_ENTRY_BYTES: u64 = 512 << 20;
const MAX_TOTAL_BYTES: u64 = 2 << 30;
/// Images above this many pixels are left alone rather than decoded.
const MAX_IMAGE_PIXELS: u64 = 60_000_000;

enum Action {
    ExtractImages,
    Compress(Preset),
}

fn parse_action(id: &str) -> Result<Action> {
    if id == EXTRACT_IMAGES {
        return Ok(Action::ExtractImages);
    }
    let level = id.strip_prefix(COMPRESS).ok_or_else(|| EngineError::UnknownOperation(id.to_string()))?;
    let level = match level.strip_prefix('.') {
        Some(l) => l,
        None if level.is_empty() => "balanced",
        None => return Err(EngineError::UnknownOperation(id.to_string())),
    };
    PRESETS
        .iter()
        .find(|p| p.0 == level)
        .map(|p| Action::Compress(p.3))
        .ok_or_else(|| EngineError::InvalidOption(format!("`level` is not valid: `{level}`")))
}

impl Converter for OfficeConverter {
    fn name(&self) -> &'static str {
        "DOCX package"
    }

    fn can_handle(&self, mime: &str) -> bool {
        mime == DOCX
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if mime != DOCX {
            return Vec::new();
        }
        let levels = PRESETS.iter().map(|(id, label, icon, _)| Operation::new(&format!("{COMPRESS}.{id}"), label, icon)).collect();
        vec![catalog::op(COMPRESS).with_children(levels), catalog::op(EXTRACT_IMAGES)]
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        let action = parse_action(&op.id)?;
        for input in inputs {
            if !input.is_file() {
                return Err(EngineError::InputNotFound(input.clone()));
            }
            let m = mime::detect(input);
            if m != DOCX {
                return Err(EngineError::UnsupportedFormat(m));
            }
        }
        let mut out = Vec::new();
        for input in inputs {
            let result = match action {
                Action::ExtractImages => extract_images(input),
                Action::Compress(preset) => compress(input, preset).map(|p| vec![p]),
            };
            match result {
                Ok(files) => out.extend(files),
                Err(e) => {
                    for f in &out {
                        let _ = std::fs::remove_dir_all(f).or_else(|_| std::fs::remove_file(f));
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }
}

/// A file name that is safe on every OS, or `None` if nothing usable is left.
fn safe_name(name: &str) -> Option<String> {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    (!cleaned.is_empty() && cleaned != "..").then_some(cleaned)
}

/// Creates `dir/stem (tag)`, then `stem (tag 2)`, ... and returns the first that was free.
fn create_unique_dir(dir: &Path, stem: &str, tag: &str) -> Result<PathBuf> {
    for n in 1..10_000 {
        let path = dir.join(crate::sibling_name(stem, tag, n, None));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free folder name").into())
}

fn extract_images(input: &Path) -> Result<Vec<PathBuf>> {
    let bad = |e: zip::result::ZipError| EngineError::UnsupportedFormat(format!("not a readable DOCX: {e}"));
    let mut archive = zip::ZipArchive::new(File::open(input)?).map_err(bad)?;
    let media: Vec<usize> = (0..archive.len())
        .filter(|&i| {
            archive.by_index_raw(i).is_ok_and(|e| e.is_file() && e.name().is_ok_and(|n| n.starts_with("word/media/")))
        })
        .collect();
    if media.is_empty() {
        return Err(EngineError::Empty("the document has no images".into()));
    }

    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    let folder = create_unique_dir(dir, stem, "images")?;
    let result = (|| {
        let mut written = Vec::new();
        let mut total = 0u64;
        for (k, i) in media.into_iter().enumerate() {
            let mut entry = archive.by_index(i).map_err(bad)?;
            let full = entry.name().map(|n| n.into_owned()).unwrap_or_default();
            let base = full.rsplit('/').next().unwrap_or_default().to_string();
            let name = safe_name(&base).unwrap_or_else(|| format!("image{}", k + 1));
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
                _ => (name.clone(), None),
            };
            let (mut file, path) = crate::create_unique(&folder, |n| match (n, &ext) {
                (1, Some(e)) => format!("{stem}.{e}"),
                (1, None) => stem.clone(),
                (n, Some(e)) => format!("{stem} ({n}).{e}"),
                (n, None) => format!("{stem} ({n})"),
            })?;
            written.push(path);
            let mut buf = Vec::new();
            (&mut entry).take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf)?;
            total += buf.len() as u64;
            if buf.len() as u64 > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
                return Err(EngineError::UnsupportedFormat("the document's images are implausibly large".into()));
            }
            file.write_all(&buf)?;
        }
        Ok(written)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&folder); // all or nothing; the folder is ours
    }
    result
}

// ---- compress ----------------------------------------------------------------------------

fn not_docx(e: zip::result::ZipError) -> EngineError {
    EngineError::UnsupportedFormat(format!("not a readable DOCX: {e}"))
}

/// A smaller encoding of one media image, and whether it became a JPEG (from a PNG).
fn smaller_media(name: &str, bytes: &[u8], preset: Preset) -> Option<(Vec<u8>, bool)> {
    let format = image::guess_format(bytes).ok()?;
    let lower = name.to_ascii_lowercase();
    let named_png = lower.ends_with(".png");
    match format {
        ImageFormat::Png if named_png => {}
        ImageFormat::Jpeg if lower.ends_with(".jpg") || lower.ends_with(".jpeg") => {
            if !matches!(crate::pdf::jpeg_components(bytes), Some(1 | 3)) {
                return None; // CMYK (or unreadable): re-encoding would shift its colours
            }
        }
        _ => return None,
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.no_limits();
    let (w, h) = reader.into_dimensions().ok()?;
    if u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS {
        return None;
    }
    let img = image::load_from_memory_with_format(bytes, format).ok()?;
    let transparent = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255);
    let resize = w.max(h) > preset.max_side;
    let to_jpeg = format == ImageFormat::Jpeg || (preset.flate_to_jpeg && !transparent);
    if !resize && !to_jpeg {
        return None; // a PNG at its size stays as it is
    }
    let img = if resize { img.resize(preset.max_side, preset.max_side, image::imageops::FilterType::Lanczos3) } else { img };
    let out = if to_jpeg {
        crate::images::encode(&img, ImageFormat::Jpeg, Some(preset.quality)).ok()?
    } else {
        crate::images::encode(&img, ImageFormat::Png, None).ok()?
    };
    (out.len() < bytes.len()).then_some((out, to_jpeg && format == ImageFormat::Png))
}

/// `word/media/image1.png` -> `word/media/image1.jpeg`, avoiding names already in the package.
fn jpeg_name(name: &str, taken: &BTreeMap<String, Vec<u8>>) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    (1..).map(|n| if n == 1 { format!("{stem}.jpeg") } else { format!("{stem}_{n}.jpeg") }).find(|c| !taken.contains_key(c)).unwrap_or_default()
}

/// `[Content_Types].xml` after renaming parts to `.jpeg`: an `<Override>` for a renamed part
/// (pandoc writes one per image) is pointed at the new name and type, and a
/// `<Default Extension="jpeg" .../>` is added if there is none.
fn with_jpeg_types(types: &str, renamed: &[(String, String)]) -> String {
    let mut out = types.to_string();
    for (old, new) in renamed {
        let Some(at) = out.find(&format!("PartName=\"/{old}\"")) else { continue };
        let (Some(start), Some(len)) = (out[..at].rfind('<'), out[at..].find('>')) else { continue };
        out.replace_range(start..at + len + 1, &format!("<Override PartName=\"/{new}\" ContentType=\"image/jpeg\"/>"));
    }
    let has = out.to_ascii_lowercase().contains("extension=\"jpeg\"");
    match out.rfind("</Types>") {
        Some(end) if !has => format!("{}<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>{}", &out[..end], &out[end..]),
        _ => out,
    }
}

/// Every entry of a package, in order, with its bytes (capped like `extract_images`).
fn read_package(input: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = zip::ZipArchive::new(File::open(input)?).map_err(not_docx)?;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(not_docx)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(not_docx)?.to_string();
        let mut buf = Vec::new();
        (&mut entry).take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf)?;
        total += buf.len() as u64;
        if buf.len() as u64 > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
            return Err(EngineError::UnsupportedFormat("the document is implausibly large".into()));
        }
        entries.push((name, buf));
    }
    if !entries.iter().any(|(n, _)| n == "[Content_Types].xml") {
        return Err(EngineError::UnsupportedFormat("not a DOCX: no [Content_Types].xml".into()));
    }
    Ok(entries)
}

fn compress(input: &Path, preset: Preset) -> Result<PathBuf> {
    let original = std::fs::read(input)?;
    let entries = read_package(input)?;
    let mut parts: BTreeMap<String, Vec<u8>> = entries.iter().cloned().collect();

    // Re-encode the media; remember PNGs that became JPEGs (old name -> new name).
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut replaced: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (name, bytes) in &entries {
        if !name.starts_with("word/media/") {
            continue;
        }
        let Some((out, became_jpeg)) = smaller_media(name, bytes, preset) else { continue };
        if became_jpeg {
            let new = jpeg_name(name, &parts);
            parts.insert(new.clone(), Vec::new()); // reserve the name
            renamed.push((name.clone(), new.clone()));
            replaced.insert(new, out);
        } else {
            replaced.insert(name.clone(), out);
        }
    }

    // Relationship targets are relative ("media/image1.png") or absolute ("/word/media/...").
    let rename_in = |text: String| {
        renamed.iter().fold(text, |t, (old, new)| {
            let (old, new) = (old.trim_start_matches("word/"), new.trim_start_matches("word/"));
            t.replace(&format!("{old}\""), &format!("{new}\""))
        })
    };
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let write_err = |e: zip::result::ZipError| EngineError::Io(std::io::Error::other(e));
    for (name, bytes) in &entries {
        let (name, bytes) = match renamed.iter().find(|(old, _)| old == name) {
            Some((_, new)) => (new.as_str(), replaced.get(new).unwrap_or(bytes)),
            None => (name.as_str(), replaced.get(name).unwrap_or(bytes)),
        };
        let edited;
        let bytes: &[u8] = if renamed.is_empty() {
            bytes
        } else if name == "[Content_Types].xml" {
            edited = with_jpeg_types(&String::from_utf8_lossy(bytes), &renamed).into_bytes();
            &edited
        } else if name.ends_with(".rels") {
            edited = rename_in(String::from_utf8_lossy(bytes).into_owned()).into_bytes();
            &edited
        } else {
            bytes
        };
        let jpeg = image::guess_format(bytes).is_ok_and(|f| f == ImageFormat::Jpeg);
        writer.start_file(name, if jpeg { stored } else { deflate }).map_err(write_err)?;
        writer.write_all(bytes)?;
    }
    let bytes = writer.finish().map_err(write_err)?.into_inner();
    let bytes = if bytes.len() < original.len() { bytes } else { original };
    Ok(write_siblings(input, "docx", vec![("compressed".into(), bytes)])?.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;
    use zip::write::SimpleFileOptions;

    /// A DOCX-shaped zip with `media` entries (name, bytes) under `word/media/` plus the usual parts.
    fn docx(path: &Path, media: &[(&str, &[u8])]) -> PathBuf {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("[Content_Types].xml", opts).unwrap();
        w.write_all(b"<Types/>").unwrap();
        w.start_file("word/document.xml", opts).unwrap();
        w.write_all(b"<w:document/>").unwrap();
        for (name, bytes) in media {
            w.start_file(format!("word/media/{name}"), opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
        path.to_path_buf()
    }

    fn run(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        OfficeConverter.run(&Operation::new(EXTRACT_IMAGES, "", ""), inputs, &Options::new())
    }

    #[test]
    fn extracts_media_into_a_new_folder() {
        let d = tempfile::tempdir().unwrap();
        let input = docx(&d.path().join("report.docx"), &[("image1.png", b"png bytes"), ("image2.jpeg", b"jpeg bytes")]);
        let out = run(std::slice::from_ref(&input)).unwrap();
        let folder = d.path().join("report (images)");
        assert_eq!(out, [folder.join("image1.png"), folder.join("image2.jpeg")]);
        assert_eq!(std::fs::read(&out[1]).unwrap(), b"jpeg bytes");

        let again = run(&[input]).unwrap();
        assert_eq!(again[0], d.path().join("report (images 2)").join("image1.png"), "never reuses a folder");
    }

    #[test]
    fn entry_paths_cannot_escape_the_folder() {
        let d = tempfile::tempdir().unwrap();
        let input = docx(&d.path().join("evil.docx"), &[("../../escape.png", b"x"), ("..", b"y")]);
        let out = run(&[input]).unwrap();
        let folder = d.path().join("evil (images)");
        assert!(out.iter().all(|p| p.parent() == Some(folder.as_path())), "{out:?}");
        assert!(!d.path().join("escape.png").exists());
    }

    #[test]
    fn no_images_or_not_a_docx_is_an_error_without_leftovers() {
        let d = tempfile::tempdir().unwrap();
        let plain = docx(&d.path().join("plain.docx"), &[]);
        assert!(matches!(run(&[plain]), Err(EngineError::Empty(_))));
        let fake = d.path().join("fake.docx");
        std::fs::write(&fake, b"not a zip").unwrap();
        assert!(matches!(run(&[fake]), Err(EngineError::UnsupportedFormat(_))));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2, "only the two inputs");
        assert!(matches!(run(&[d.path().join("gone.docx")]), Err(EngineError::InputNotFound(_))));
    }

    /// Pixels that don't compress well, so encodings and sizes actually differ.
    fn noisy(w: u32, h: u32, alpha: bool) -> DynamicImage {
        let mut seed = 0x2545_f491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            let n = (next() % 48) as u8;
            let a = if alpha && x < w / 2 { 0 } else { 255 };
            image::Rgba([(x * 255 / w) as u8 ^ n, (y * 255 / h) as u8, 128 + n, a])
        });
        DynamicImage::ImageRgba8(img)
    }

    fn encoded(img: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        crate::images::encode(img, format, Some(95)).unwrap()
    }

    const TYPES: &str = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="png" ContentType="image/png"/><Default Extension="xml" ContentType="application/xml"/></Types>"#;
    const RELS: &str = r#"<Relationships><Relationship Id="rId1" Target="media/photo.png"/><Relationship Id="rId2" Target="/word/media/logo.png"/><Relationship Id="rId3" Target="media/scan.jpg"/></Relationships>"#;

    /// A DOCX with an opaque PNG photo (2000 px), a transparent PNG logo and a JPEG scan.
    fn picture_docx(path: &Path) -> PathBuf {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in [
            ("[Content_Types].xml", TYPES.as_bytes().to_vec()),
            ("word/document.xml", b"<w:document><w:body>Hello</w:body></w:document>".to_vec()),
            ("word/_rels/document.xml.rels", RELS.as_bytes().to_vec()),
            ("word/media/photo.png", encoded(&noisy(2000, 1000, false), ImageFormat::Png)),
            ("word/media/logo.png", encoded(&noisy(300, 200, true), ImageFormat::Png)),
            ("word/media/scan.jpg", encoded(&noisy(1800, 1200, false), ImageFormat::Jpeg)),
        ] {
            w.start_file(name, stored).unwrap();
            w.write_all(&bytes).unwrap();
        }
        w.finish().unwrap();
        path.to_path_buf()
    }

    fn part(path: &Path, name: &str) -> Option<Vec<u8>> {
        let mut z = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut buf = Vec::new();
        z.by_name(name).ok()?.read_to_end(&mut buf).unwrap();
        Some(buf)
    }

    fn dims(bytes: &[u8]) -> (u32, u32, ImageFormat) {
        let img = image::load_from_memory(bytes).unwrap();
        (img.width(), img.height(), image::guess_format(bytes).unwrap())
    }

    fn compress_run(level: &str, input: &Path) -> PathBuf {
        let op = Operation::new(&format!("doc.compress.{level}"), "", "");
        OfficeConverter.run(&op, &[input.to_path_buf()], &Options::new()).unwrap().remove(0)
    }

    #[test]
    fn compress_balanced_downsamples_and_turns_opaque_pngs_into_jpegs() {
        let d = tempfile::tempdir().unwrap();
        let input = picture_docx(&d.path().join("report.docx"));
        let out = compress_run("balanced", &input);
        assert_eq!(out, d.path().join("report (compressed).docx"));
        let size = |p: &Path| std::fs::metadata(p).unwrap().len();
        assert!(size(&out) * 3 < size(&input), "{} vs {}", size(&out), size(&input));

        assert!(part(&out, "word/media/photo.png").is_none(), "renamed");
        assert_eq!(dims(&part(&out, "word/media/photo.jpeg").unwrap()), (1600, 800, ImageFormat::Jpeg));
        assert_eq!(dims(&part(&out, "word/media/logo.png").unwrap()).2, ImageFormat::Png, "transparent: stays PNG");
        assert_eq!(dims(&part(&out, "word/media/scan.jpg").unwrap()), (1600, 1067, ImageFormat::Jpeg));

        let rels = String::from_utf8(part(&out, "word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="media/photo.jpeg""#) && rels.contains(r#"Target="/word/media/logo.png""#), "{rels}");
        let types = String::from_utf8(part(&out, "[Content_Types].xml").unwrap()).unwrap();
        assert!(types.contains(r#"<Default Extension="jpeg" ContentType="image/jpeg"/></Types>"#), "{types}");
        assert_eq!(part(&out, "word/document.xml").unwrap(), part(&input, "word/document.xml").unwrap());
    }

    #[test]
    fn compress_light_keeps_pngs_lossless() {
        let d = tempfile::tempdir().unwrap();
        let input = picture_docx(&d.path().join("report.docx"));
        let out = compress_run("light", &input);
        let photo = part(&out, "word/media/photo.png").expect("not renamed");
        assert_eq!(dims(&photo), (2000, 1000, ImageFormat::Png), "under 2400 px and lossless: untouched");
        assert_eq!(dims(&part(&out, "word/media/scan.jpg").unwrap()), (1800, 1200, ImageFormat::Jpeg), "re-encoded at q85");
        assert!(part(&out, "word/media/scan.jpg").unwrap().len() < part(&input, "word/media/scan.jpg").unwrap().len());
        assert_eq!(part(&out, "[Content_Types].xml").unwrap(), TYPES.as_bytes(), "nothing renamed, types untouched");
    }

    #[test]
    fn compress_never_makes_a_docx_bigger_and_rejects_non_docx() {
        let d = tempfile::tempdir().unwrap();
        let input = docx(&d.path().join("plain.docx"), &[]);
        let out = compress_run("strong", &input);
        assert!(std::fs::metadata(&out).unwrap().len() <= std::fs::metadata(&input).unwrap().len());
        assert!(part(&out, "word/document.xml").is_some());

        let fake = d.path().join("fake.docx");
        std::fs::write(&fake, b"not a zip").unwrap();
        let op = Operation::new("doc.compress.light", "", "");
        assert!(matches!(OfficeConverter.run(&op, &[fake], &Options::new()), Err(EngineError::UnsupportedFormat(_))));
        let bad = Operation::new("doc.compress.max", "", "");
        assert!(matches!(OfficeConverter.run(&bad, &[input], &Options::new()), Err(EngineError::InvalidOption(_))));
    }

    #[test]
    fn jpeg_names_avoid_existing_parts() {
        let mut taken = BTreeMap::new();
        assert_eq!(jpeg_name("word/media/a.png", &taken), "word/media/a.jpeg");
        taken.insert("word/media/a.jpeg".to_string(), Vec::new());
        assert_eq!(jpeg_name("word/media/a.png", &taken), "word/media/a_2.jpeg");
        assert_eq!(with_jpeg_types(r#"<Types><Default Extension="JPEG" ContentType="image/jpeg"/></Types>"#, &[]).matches("jpeg\"").count(), 1);
        // pandoc's per-image overrides follow the rename.
        let pandoc = r#"<Types><Override PartName="/word/document.xml" ContentType="x"/><Override PartName="/word/media/rId9.png" ContentType="image/png" /></Types>"#;
        let renamed = [("word/media/rId9.png".to_string(), "word/media/rId9.jpeg".to_string())];
        assert_eq!(
            with_jpeg_types(pandoc, &renamed),
            r#"<Types><Override PartName="/word/document.xml" ContentType="x"/><Override PartName="/word/media/rId9.jpeg" ContentType="image/jpeg"/><Default Extension="jpeg" ContentType="image/jpeg"/></Types>"#
        );
    }

    #[test]
    fn lists_only_for_docx() {
        let ids: Vec<String> = OfficeConverter.operations(DOCX).into_iter().map(|o| o.id).collect();
        assert_eq!(ids, ["doc.compress", "doc.extract_images"]);
        assert!(OfficeConverter.operations(mime::PDF).is_empty());
        let err = OfficeConverter.run(&Operation::new("doc.to_pdf", "", ""), &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)));
    }
}
