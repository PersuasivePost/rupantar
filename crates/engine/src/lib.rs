//! Rupantar conversion engine. Pure Rust: no Tauri or UI dependencies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod copy;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("input not found: {0}")]
    InputNotFound(PathBuf),
    #[error("no converter handles mime type `{0}`")]
    Unsupported(String),
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
}

pub type Options = HashMap<String, String>;

pub trait Converter: Send + Sync {
    fn can_handle(&self, mime: &str) -> bool;
    fn operations(&self, mime: &str) -> Vec<Operation>;
    /// Returns the paths of the files written, next to their inputs.
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

    pub fn run(
        &self,
        mime: &str,
        op: &Operation,
        inputs: &[PathBuf],
        options: &Options,
    ) -> Result<Vec<PathBuf>> {
        self.converters
            .iter()
            .filter(|c| c.can_handle(mime))
            .find(|c| c.operations(mime).iter().any(|o| o.id == op.id))
            .ok_or_else(|| EngineError::Unsupported(mime.to_string()))?
            .run(op, inputs, options)
    }
}

/// Picks `<stem>-<suffix>.<ext>` next to `input`, adding ` (n)` so an existing file is never overwritten.
pub fn unique_sibling_path(input: &Path, suffix: &str, ext: Option<&str>) -> PathBuf {
    let dir = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let ext = ext
        .map(str::to_owned)
        .or_else(|| input.extension().and_then(|e| e.to_str()).map(str::to_owned));
    let name = |n: u32| {
        let counter = if n == 0 { String::new() } else { format!(" ({n})") };
        match &ext {
            Some(e) => format!("{stem}-{suffix}{counter}.{e}"),
            None => format!("{stem}-{suffix}{counter}"),
        }
    };
    (0..)
        .map(|n| dir.join(name(n)))
        .find(|p| !p.exists())
        .expect("unbounded range always yields a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rupantar-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unique_path_avoids_existing_files() {
        let d = tmpdir("unique");
        let input = d.join("a.txt");
        let first = unique_sibling_path(&input, "copy", None);
        assert_eq!(first, d.join("a-copy.txt"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(unique_sibling_path(&input, "copy", None), d.join("a-copy (1).txt"));
    }

    #[test]
    fn registry_dispatches_by_mime() {
        let r = Registry::with_defaults();
        assert!(!r.operations("text/plain").is_empty());
        assert!(Registry::new().operations("text/plain").is_empty());
    }

    #[test]
    fn run_rejects_unsupported_mime() {
        let r = Registry::new();
        let op = Operation { id: "copy".into(), label: "Copy".into(), icon: "Copy".into() };
        let err = r.run("text/plain", &op, &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::Unsupported(_)));
    }
}
