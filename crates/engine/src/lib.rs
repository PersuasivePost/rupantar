//! Rupantar conversion engine. Pure Rust: no Tauri or UI dependencies.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod copy;
pub mod images;
pub mod mime;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("input not found: {0}")]
    InputNotFound(PathBuf),
    #[error("no converter handles mime type `{0}`")]
    Unsupported(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("unknown operation `{0}`")]
    UnknownOperation(String),
    #[error("invalid option: {0}")]
    InvalidOption(String),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// A conversion offered for a mime type; shown as a wedge in the radial menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub label: String,
    /// lucide-react icon name.
    pub icon: String,
    /// Sub-options (e.g. Resize -> 50% / 25%); shown as an outer ring in the menu.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Operation>,
}

impl Operation {
    pub fn new(id: &str, label: &str, icon: &str) -> Self {
        Self { id: id.into(), label: label.into(), icon: icon.into(), children: Vec::new() }
    }

    pub fn with_children(mut self, children: Vec<Operation>) -> Self {
        self.children = children;
        self
    }
}

pub type Options = HashMap<String, String>;

pub trait Converter: Send + Sync {
    fn can_handle(&self, mime: &str) -> bool;
    fn operations(&self, mime: &str) -> Vec<Operation>;
    /// Returns the paths of the files written, next to their inputs.
    /// Must return `EngineError::UnknownOperation` for an op id it does not own, so the
    /// registry can try the next converter.
    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>>;
}

#[derive(Default)]
pub struct Registry {
    converters: Vec<Box<dyn Converter>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with every built-in converter.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Box::new(images::ImageConverter));
        r.register(Box::new(copy::CopyConverter));
        r
    }

    pub fn register(&mut self, c: Box<dyn Converter>) {
        self.converters.push(c);
    }

    pub fn operations(&self, mime: &str) -> Vec<Operation> {
        self.converters
            .iter()
            .filter(|c| c.can_handle(mime))
            .flat_map(|c| c.operations(mime))
            .collect()
    }

    /// Runs `op` with the first converter for `mime` that owns it.
    pub fn run(
        &self,
        mime: &str,
        op: &Operation,
        inputs: &[PathBuf],
        options: &Options,
    ) -> Result<Vec<PathBuf>> {
        let mut tried = false;
        for c in self.converters.iter().filter(|c| c.can_handle(mime)) {
            tried = true;
            match c.run(op, inputs, options) {
                Err(EngineError::UnknownOperation(_)) => continue,
                other => return other,
            }
        }
        Err(if tried {
            EngineError::UnknownOperation(op.id.clone())
        } else {
            EngineError::Unsupported(mime.to_string())
        })
    }
}

/// `stem (tag).ext`, then `stem (tag 2).ext`, `stem (tag 3).ext`, ...
pub fn sibling_name(stem: &str, tag: &str, n: u32, ext: Option<&str>) -> String {
    let label = if n <= 1 { tag.to_string() } else { format!("{tag} {n}") };
    match ext {
        Some(e) => format!("{stem} ({label}).{e}"),
        None => format!("{stem} ({label})"),
    }
}

/// Atomically creates a new file next to `input` named per [`sibling_name`]. Uses `create_new`,
/// so an existing file is never overwritten, even if another process races us for the name.
/// `ext` defaults to the input's extension.
pub fn create_unique_sibling(input: &Path, tag: &str, ext: Option<&str>) -> Result<(File, PathBuf)> {
    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let ext = ext.or_else(|| input.extension().and_then(|e| e.to_str()));
    for n in 1..10_000 {
        let path = dir.join(sibling_name(stem, tag, n, ext));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no free output name").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_names_follow_the_pattern() {
        assert_eq!(sibling_name("a", "converted", 1, Some("jpg")), "a (converted).jpg");
        assert_eq!(sibling_name("a", "converted", 2, Some("jpg")), "a (converted 2).jpg");
        assert_eq!(sibling_name("a", "copy", 1, None), "a (copy)");
    }

    #[test]
    fn create_unique_sibling_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("a.txt");
        let (_, first) = create_unique_sibling(&input, "converted", None).unwrap();
        assert_eq!(first, d.path().join("a (converted).txt"));
        std::fs::write(&first, b"keep me").unwrap();
        let (_, second) = create_unique_sibling(&input, "converted", None).unwrap();
        assert_eq!(second, d.path().join("a (converted 2).txt"));
        assert_eq!(std::fs::read(&first).unwrap(), b"keep me");
    }

    #[test]
    fn registry_lists_ops_by_mime() {
        let r = Registry::with_defaults();
        assert!(!r.operations("text/plain").is_empty());
        assert!(Registry::new().operations("text/plain").is_empty());
        let ids: Vec<_> = r.operations("image/png").into_iter().map(|o| o.id).collect();
        assert!(ids.contains(&"image.convert".to_string()));
        assert!(ids.contains(&"copy".to_string()));
    }

    #[test]
    fn run_rejects_unsupported_mime() {
        let op = Operation::new("copy", "Copy", "Copy");
        let err = Registry::new().run("text/plain", &op, &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::Unsupported(_)));
    }

    #[test]
    fn run_rejects_unknown_operation() {
        let op = Operation::new("nope", "Nope", "Circle");
        let err = Registry::with_defaults().run("image/png", &op, &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(id) if id == "nope"));
    }
}
