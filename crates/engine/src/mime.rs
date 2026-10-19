//! File type detection for dropped files. Content sniffing wins over the extension for the
//! formats we can recognise, so a mis-named image is still handled as an image.

use std::io::Read;
use std::path::Path;

pub const UNKNOWN: &str = "application/octet-stream";
pub const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub const XLS: &str = "application/vnd.ms-excel";
pub const ODS: &str = "application/vnd.oasis.opendocument.spreadsheet";
pub const CSV: &str = "text/csv";
pub const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub const MARKDOWN: &str = "text/markdown";
pub const HTML: &str = "text/html";
pub const PDF: &str = "application/pdf";

const BY_EXTENSION: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("pdf", PDF),
    ("docx", DOCX),
    ("xlsx", XLSX),
    ("xls", XLS),
    ("ods", ODS),
    ("md", MARKDOWN),
    ("markdown", MARKDOWN),
    ("txt", "text/plain"),
    ("csv", CSV),
    ("json", "application/json"),
    ("html", HTML),
    ("htm", HTML),
    ("zip", "application/zip"),
];

fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"%PDF-") {
        Some(PDF)
    } else {
        None
    }
}

/// Best-effort mime type for a file; never fails (unreadable or unknown files are `UNKNOWN`).
pub fn detect(path: &Path) -> String {
    let mut head = [0u8; 16];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if let Some(m) = sniff(&head[..n]) {
        return m.to_string();
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    ext.and_then(|e| BY_EXTENSION.iter().find(|(k, _)| *k == e).map(|(_, m)| (*m).to_string()))
        .unwrap_or_else(|| UNKNOWN.to_string())
}

/// The mime shared by every file in a batch, or `UNKNOWN` if they differ (or there are none).
pub fn common(mimes: &[String]) -> String {
    match mimes.split_first() {
        Some((first, rest)) if rest.iter().all(|m| m == first) => first.clone(),
        _ => UNKNOWN.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn detects_by_extension_case_insensitively() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(detect(&file(d.path(), "a.PDF", b"")), "application/pdf");
        assert_eq!(detect(&file(d.path(), "b.md", b"# hi")), "text/markdown");
        assert!(detect(&file(d.path(), "c.docx", b"PK")).ends_with("wordprocessingml.document"));
    }

    #[test]
    fn content_beats_a_wrong_extension() {
        let d = tempfile::tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(detect(&file(d.path(), "photo.jpg", &png)), "image/png");
        assert_eq!(detect(&file(d.path(), "x.txt", b"%PDF-1.7")), "application/pdf");
        assert_eq!(detect(&file(d.path(), "y.png", b"RIFF\0\0\0\0WEBPVP8 ")), "image/webp");
        assert_eq!(detect(&file(d.path(), "z.bin", &[0xFF, 0xD8, 0xFF, 0xE0])), "image/jpeg");
    }

    #[test]
    fn unknown_and_unreadable_files_are_octet_stream() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(detect(&file(d.path(), "mystery", b"hello")), UNKNOWN);
        assert_eq!(detect(&d.path().join("missing.xyz")), UNKNOWN);
        assert_eq!(detect(&d.path().join("missing.png")), "image/png"); // falls back to the name
    }

    #[test]
    fn common_requires_agreement() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(common(&s(&["image/png", "image/png"])), "image/png");
        assert_eq!(common(&s(&["image/png", "image/jpeg"])), UNKNOWN);
        assert_eq!(common(&[]), UNKNOWN);
    }
}
