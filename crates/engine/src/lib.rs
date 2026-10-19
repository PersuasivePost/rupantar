//! Rupantar conversion engine. Pure Rust: no Tauri or UI dependencies.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod copy;
pub mod documents;
pub mod images;
pub mod mime;
pub mod sheets;
pub mod tools;

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
    #[error("{0} is not available (it was not found next to Rupantar or on PATH)")]
    ToolMissing(String),
    #[error("{tool} failed ({status}): {stderr}")]
    ToolFailed { tool: String, status: String, stderr: String },
    #[error("{tool} did not finish within {} s and was stopped", .timeout.as_secs())]
    ToolTimeout { tool: String, timeout: Duration },
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("spreadsheet error: {0}")]
    Spreadsheet(#[from] calamine::Error),
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("xlsx error: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
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
    /// False when the operation needs an external tool that isn't installed. Still listed (so
    /// the UI can explain), but the menu does not render it.
    #[serde(default = "yes")]
    pub available: bool,
}

fn yes() -> bool {
    true
}

impl Operation {
    pub fn new(id: &str, label: &str, icon: &str) -> Self {
        Self { id: id.into(), label: label.into(), icon: icon.into(), children: Vec::new(), available: true }
    }

    pub fn with_children(mut self, children: Vec<Operation>) -> Self {
        self.children = children;
        self
    }

    pub fn available(mut self, available: bool) -> Self {
        self.available = available;
        self
    }
}

pub type Options = HashMap<String, String>;

/// Where the external tools live. Built by the shell at startup (bundled sidecars first, then
/// PATH); `None` means the tool is missing and the operations needing it are unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub pandoc_path: Option<PathBuf>,
    pub typst_path: Option<PathBuf>,
    /// Upper bound for one external tool run; the process is killed after this.
    pub tool_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { pandoc_path: None, typst_path: None, tool_timeout: Duration::from_secs(120) }
    }
}

impl EngineConfig {
    /// Finds each tool in `dirs` (e.g. next to the executable, where Tauri puts sidecars), then on PATH.
    pub fn locate(dirs: &[PathBuf]) -> Self {
        Self {
            pandoc_path: tools::find("pandoc", dirs),
            typst_path: tools::find("typst", dirs),
            ..Self::default()
        }
    }
}

pub trait Converter: Send + Sync {
    fn can_handle(&self, mime: &str) -> bool;
    fn operations(&self, mime: &str) -> Vec<Operation>;
    /// Operations that only make sense for a whole selection, e.g. merging several PDFs.
    /// `mimes` has one entry per selected file.
    fn batch_operations(&self, _mimes: &[String]) -> Vec<Operation> {
        Vec::new()
    }
    /// Returns the paths of the files written, next to their inputs. Inputs may be of
    /// different types (every one is a type this converter `can_handle`), so detect each
    /// file's format rather than assuming one for the batch.
    /// Must return `EngineError::UnknownOperation` for an op id it does not own, so the
    /// registry can try the next converter.
    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>>;
}

#[derive(Default)]
pub struct Registry {
    converters: Vec<Box<dyn Converter>>,
}

/// The operations of `a` that `b` also offers (by id, recursively); a parent whose children
/// all drop out goes too. Available only if available in both.
fn intersect(a: Vec<Operation>, b: &[Operation]) -> Vec<Operation> {
    a.into_iter()
        .filter_map(|op| {
            let other = b.iter().find(|o| o.id == op.id)?;
            let had_children = !op.children.is_empty();
            let available = op.available && other.available;
            let children = intersect(op.children, &other.children);
            if had_children && children.is_empty() {
                return None;
            }
            Some(Operation { children, available, ..op })
        })
        .collect()
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with every built-in converter and no external tools (document ops unavailable).
    pub fn with_defaults() -> Self {
        Self::with_config(&EngineConfig::default())
    }

    /// Registry with every built-in converter, using the tools in `config`.
    pub fn with_config(config: &EngineConfig) -> Self {
        let mut r = Self::new();
        r.register(Box::new(images::ImageConverter));
        r.register(Box::new(sheets::SheetConverter));
        r.register(Box::new(documents::DocumentConverter::new(config)));
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

    /// Operations for a selection (one mime per file): batch operations first (e.g. "Merge
    /// PDFs"), then the per-file operations every selected type offers.
    pub fn operations_for(&self, mimes: &[String]) -> Vec<Operation> {
        let Some((first, rest)) = mimes.split_first() else { return Vec::new() };
        let mut common = self.operations(first);
        for m in rest.iter().filter(|m| *m != first) {
            common = intersect(common, &self.operations(m));
        }
        let mut ops: Vec<Operation> = self.converters.iter().flat_map(|c| c.batch_operations(mimes)).collect();
        for op in common {
            if !ops.iter().any(|o| o.id == op.id) {
                ops.push(op);
            }
        }
        ops
    }

    /// Runs `op` with the first converter for `mime` that owns it.
    pub fn run(&self, mime: &str, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>> {
        self.run_for(&[mime.to_string()], op, inputs, options)
    }

    /// Runs `op` on a selection (one mime per file) with the first converter that handles
    /// every selected type and owns the op.
    pub fn run_for(
        &self,
        mimes: &[String],
        op: &Operation,
        inputs: &[PathBuf],
        options: &Options,
    ) -> Result<Vec<PathBuf>> {
        let mut tried = false;
        for c in self.converters.iter().filter(|c| mimes.iter().all(|m| c.can_handle(m))) {
            tried = true;
            match c.run(op, inputs, options) {
                Err(EngineError::UnknownOperation(_)) => continue,
                other => return other,
            }
        }
        Err(if tried {
            EngineError::UnknownOperation(op.id.clone())
        } else {
            EngineError::Unsupported(mimes.join(", "))
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
    create_unique(dir, |n| sibling_name(stem, tag, n, ext))
}

/// Atomically creates the first free `dir/name(n)` for n = 1, 2, ... with `create_new`.
pub fn create_unique(dir: &Path, name: impl Fn(u32) -> String) -> Result<(File, PathBuf)> {
    for n in 1..10_000 {
        let path = dir.join(name(n));
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
    fn registry_offers_spreadsheet_ops() {
        let r = Registry::with_defaults();
        let ids = |m: &str| r.operations(m).into_iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(ids(mime::CSV), ["sheet.to_xlsx", "copy"]);
        for m in [mime::XLSX, mime::XLS, mime::ODS] {
            assert_eq!(ids(m), ["sheet.to_csv", "copy"]);
        }
    }

    fn ids(ops: &[Operation]) -> Vec<&str> {
        ops.iter().map(|o| o.id.as_str()).collect()
    }

    #[test]
    fn selections_offer_what_every_type_offers() {
        let r = Registry::with_defaults();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(r.operations_for(&s(&["image/png"])), r.operations("image/png"));
        assert_eq!(r.operations_for(&s(&["image/png", "image/png"])), r.operations("image/png"));

        // PNG converts to JPG/WebP, JPEG to PNG/WebP: together only WebP is valid for both.
        let mixed = r.operations_for(&s(&["image/png", "image/jpeg"]));
        assert_eq!(ids(&mixed), ["image.convert", "image.resize", "image.compress", "image.rotate", "copy"]);
        assert_eq!(ids(&mixed[0].children), ["image.convert.webp"]);
        assert_eq!(mixed[1].children, r.operations("image/png")[1].children);

        assert_eq!(ids(&r.operations_for(&s(&["image/png", mime::CSV]))), ["copy"]);
        assert!(r.operations_for(&[]).is_empty());

        // Availability is the AND across types.
        let docs = r.operations_for(&s(&[mime::DOCX, mime::MARKDOWN]));
        assert_eq!(ids(&docs), ["doc.to_epub", "doc.to_pdf", "copy"]);
        assert_eq!(docs.iter().map(|o| o.available).collect::<Vec<_>>(), [false, false, true]);
    }

    #[test]
    fn intersect_drops_parents_without_common_children() {
        let a = vec![Operation::new("p", "P", "x").with_children(vec![Operation::new("p.1", "1", "x")])];
        let b = vec![Operation::new("p", "P", "x").with_children(vec![Operation::new("p.2", "2", "x")])];
        assert!(intersect(a.clone(), &b).is_empty());
        let b = vec![Operation::new("p", "P", "x").available(false).with_children(a[0].children.clone())];
        assert_eq!(intersect(a, &b), b);
    }

    struct Batchy;
    impl Converter for Batchy {
        fn can_handle(&self, mime: &str) -> bool {
            mime == "a/x"
        }
        fn operations(&self, _mime: &str) -> Vec<Operation> {
            vec![Operation::new("single", "S", "x")]
        }
        fn batch_operations(&self, mimes: &[String]) -> Vec<Operation> {
            if mimes.len() > 1 && mimes.iter().all(|m| m == "a/x") {
                vec![Operation::new("merge", "M", "x"), Operation::new("single", "dup", "x")]
            } else {
                Vec::new()
            }
        }
        fn run(&self, op: &Operation, inputs: &[PathBuf], _o: &Options) -> Result<Vec<PathBuf>> {
            match op.id.as_str() {
                "merge" => Ok(vec![PathBuf::from(format!("merged {}", inputs.len()))]),
                _ => Err(EngineError::UnknownOperation(op.id.clone())),
            }
        }
    }

    #[test]
    fn batch_operations_come_first_without_duplicates() {
        let mut r = Registry::new();
        r.register(Box::new(Batchy));
        let two = vec!["a/x".to_string(), "a/x".to_string()];
        let ops = r.operations_for(&two);
        assert_eq!(ids(&ops), ["merge", "single"]);
        assert_eq!(ops[1].label, "dup", "the batch version wins");
        assert_eq!(ids(&r.operations_for(&two[..1])), ["single"]);
        let merged = r.run_for(&two, &Operation::new("merge", "M", "x"), &[PathBuf::new(), PathBuf::new()], &Options::new());
        assert_eq!(merged.unwrap(), [PathBuf::from("merged 2")]);
    }

    #[test]
    fn run_for_handles_mixed_types() {
        let d = tempfile::tempdir().unwrap();
        let png = d.path().join("a.png");
        let jpg = d.path().join("b.jpg");
        image::RgbImage::new(4, 2).save(&png).unwrap();
        image::RgbImage::new(4, 2).save(&jpg).unwrap();
        let r = Registry::with_defaults();
        let mimes = vec!["image/png".to_string(), "image/jpeg".to_string()];
        let out = r.run_for(&mimes, &Operation::new("image.rotate.90", "", ""), &[png, jpg], &Options::new()).unwrap();
        assert_eq!(out, [d.path().join("a (rotated).png"), d.path().join("b (rotated).jpg")]);

        let mixed = vec!["image/png".to_string(), mime::CSV.to_string()];
        let err = r.run_for(&mixed, &Operation::new("image.rotate.90", "", ""), &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)), "only copy handles both, and it doesn't own rotate");
    }

    #[test]
    fn operations_deserialize_as_available_by_default() {
        let op: Operation = serde_json::from_str(r#"{"id":"copy","label":"Duplicate","icon":"Copy"}"#).unwrap();
        assert!(op.available);
        let json = serde_json::to_value(Operation::new("x", "X", "Y").available(false)).unwrap();
        assert_eq!(json["available"], false);
    }

    /// The browser-preview mocks in `src/api.ts` are built from this snapshot of the real
    /// operations (with every tool present), so mock ids can't drift from the engine's.
    /// Regenerate with `UPDATE_SNAPSHOT=1 cargo test -p engine`.
    #[test]
    fn frontend_operation_snapshot_is_current() {
        let exe = std::env::current_exe().unwrap();
        let config = EngineConfig { pandoc_path: Some(exe.clone()), typst_path: Some(exe), ..EngineConfig::default() };
        let r = Registry::with_config(&config);
        let mimes = [
            mime::PDF, mime::DOCX, mime::MARKDOWN, mime::HTML, "image/png", "image/jpeg", "image/webp",
            mime::XLSX, mime::XLS, mime::ODS, mime::CSV, mime::UNKNOWN,
        ];
        let snapshot: serde_json::Map<String, serde_json::Value> =
            mimes.iter().map(|m| (m.to_string(), serde_json::to_value(r.operations(m)).unwrap())).collect();
        let text = serde_json::to_string_pretty(&snapshot).unwrap() + "\n";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/generated/engine-operations.json");
        if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
        }
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(current == text, "{} is stale; run `UPDATE_SNAPSHOT=1 cargo test -p engine`", path.display());
    }

    #[test]
    fn errors_can_cross_threads() {
        // `run_operation` in the shell runs on `spawn_blocking` and returns these.
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<EngineError>();
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
