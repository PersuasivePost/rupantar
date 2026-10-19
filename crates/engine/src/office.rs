//! Operations that read Office Open XML packages directly (they are zip files), no tools needed.
//!
//! - `doc.extract_images` (DOCX): copies every file under `word/media/` into a new folder
//!   `name (images)` next to the input (`name (images 2)`, ... if taken), keeping the original
//!   file names. A document without images is an `Empty` error and leaves nothing behind.
//!
//! Zip entries are untrusted: only the file-name part of each entry is used (no paths), and
//! sizes are capped so a zip bomb can't fill the disk.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::mime::{self, DOCX};
use crate::{catalog, Converter, EngineError, Operation, Options, Result};

pub struct OfficeConverter;

const EXTRACT_IMAGES: &str = "doc.extract_images";
const MAX_ENTRY_BYTES: u64 = 512 << 20;
const MAX_TOTAL_BYTES: u64 = 2 << 30;

impl Converter for OfficeConverter {
    fn can_handle(&self, mime: &str) -> bool {
        mime == DOCX
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if mime == DOCX {
            vec![catalog::op(EXTRACT_IMAGES)]
        } else {
            Vec::new()
        }
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        if op.id != EXTRACT_IMAGES {
            return Err(EngineError::UnknownOperation(op.id.clone()));
        }
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
            out.extend(extract_images(input)?);
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

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn lists_only_for_docx() {
        assert_eq!(OfficeConverter.operations(DOCX).len(), 1);
        assert!(OfficeConverter.operations(mime::PDF).is_empty());
        let err = OfficeConverter.run(&Operation::new("doc.to_pdf", "", ""), &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)));
    }
}
