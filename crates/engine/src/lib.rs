//! Rupantar conversion engine. Pure Rust: no Tauri or UI dependencies.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod ai;
pub mod catalog;
pub mod copy;
#[cfg(test)]
mod fidelity;
pub mod documents;
pub mod images;
pub mod libreoffice;
mod metadata;
pub mod mime;
pub mod office;
pub mod pdf;
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
    /// The operation needs an optional download (`Operation::pack`) that isn't installed.
    #[error("this needs the {0} pack, which is not installed")]
    PackMissing(String),
    #[error("{tool} did not finish within {} s and was stopped", .timeout.as_secs())]
    ToolTimeout { tool: String, timeout: Duration },
    /// The user cancelled the run (`Job::cancel`); whatever was half written is gone.
    #[error("cancelled")]
    Cancelled,
    /// Upscaling runs on the GPU through Vulkan, and there is no usable Vulkan device.
    #[error("Upscale needs a graphics card with Vulkan support, and none was found ({0})")]
    NoGpu(String),
    /// ONNX Runtime could not load or run the model.
    #[error("background removal failed: {0}")]
    Inference(String),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("spreadsheet error: {0}")]
    Spreadsheet(#[from] calamine::Error),
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("xlsx error: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
    #[error("pdf error: {0}")]
    Pdf(#[from] lopdf::Error),
    #[error("pdf rendering failed: {0}")]
    Pdfium(String),
    /// The input has nothing the operation can produce (e.g. no images to extract).
    #[error("{0}")]
    Empty(String),
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
    /// Menu position for this selection, lower first (from the catalog; top level only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<catalog::Group>,
    /// The UI must ask for options before running this (e.g. `crop`: a rectangle). Such
    /// operations are offered for single-file selections only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The optional download this needs (`"office"`: LibreOffice). With `available: false` the
    /// menu still shows it (under More, as "needs Office pack") and offers the download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// Always listed under "More", never as one of the top wedges (Duplicate; Office-pack
    /// operations while the pack is missing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
}

fn yes() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Operation {
    pub fn new(id: &str, label: &str, icon: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: icon.into(),
            children: Vec::new(),
            available: true,
            priority: None,
            group: None,
            prompt: None,
            pack: None,
            more: false,
        }
    }

    pub fn with_children(mut self, children: Vec<Operation>) -> Self {
        self.children = children;
        self
    }

    pub fn available(mut self, available: bool) -> Self {
        self.available = available;
        self
    }

    pub fn with_prompt(mut self, prompt: &str) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    pub fn needs_pack(mut self, pack: &str) -> Self {
        self.pack = Some(pack.into());
        self
    }

    pub fn in_more(mut self) -> Self {
        self.more = true;
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
    /// The pdfium shared library file (`libpdfium.so` / `libpdfium.dylib` / `pdfium.dll`),
    /// loaded on first use to render PDF pages.
    pub pdfium_path: Option<PathBuf>,
    /// LibreOffice's `soffice` program from the installed Office pack. Never looked up on PATH:
    /// the shell sets it from the pack, so the version is the pinned one.
    pub soffice_path: Option<PathBuf>,
    /// From the installed AI pack (set by the shell, never looked up): the ONNX Runtime shared
    /// library, the BiRefNet lite model, and the Real-ESRGAN ncnn-vulkan program (its `models/`
    /// folder next to it).
    pub onnxruntime_path: Option<PathBuf>,
    pub birefnet_path: Option<PathBuf>,
    pub realesrgan_path: Option<PathBuf>,
    /// Upper bound for one external tool run; the process is killed after this.
    pub tool_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            pandoc_path: None,
            typst_path: None,
            pdfium_path: None,
            soffice_path: None,
            onnxruntime_path: None,
            birefnet_path: None,
            realesrgan_path: None,
            tool_timeout: Duration::from_secs(120),
        }
    }
}

impl EngineConfig {
    /// Finds each tool in `dirs` (e.g. next to the executable, where Tauri puts sidecars), then
    /// on PATH. pdfium is a library, so it is only looked for in `dirs`.
    pub fn locate(dirs: &[PathBuf]) -> Self {
        Self {
            pandoc_path: tools::find("pandoc", dirs),
            typst_path: tools::find("typst", dirs),
            pdfium_path: pdf::find_pdfium(dirs),
            ..Self::default()
        }
    }
}

pub trait Converter: Send + Sync {
    /// Which engine does the work ("LibreOffice", "pandoc", ...), for the shell's log.
    fn name(&self) -> &'static str {
        "built-in"
    }
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
    /// `run` with progress and cancellation. Converters whose work takes long (the AI pack's)
    /// override it, report through `job` and stop with `EngineError::Cancelled` once it is
    /// cancelled; the rest ignore the job.
    fn run_job(&self, op: &Operation, inputs: &[PathBuf], options: &Options, job: &Job) -> Result<Vec<PathBuf>> {
        let _ = job;
        self.run(op, inputs, options)
    }
}

/// How far a run has got: `fraction` in 0..=1 and what it is doing ("Removing background on
/// CPU…").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Progress {
    pub fraction: f32,
    pub label: String,
}

/// Progress reporting and cancellation for one run, shared by the caller (who may cancel from
/// another thread) and the converter. Also carries a note on how the work was done (e.g. which
/// execution provider), which ends up in the log next to the engine's name.
pub struct Job {
    cancelled: AtomicBool,
    progress: Box<dyn Fn(&Progress) + Send + Sync>,
    note: Mutex<Option<String>>,
}

impl Default for Job {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl Job {
    pub fn new(progress: impl Fn(&Progress) + Send + Sync + 'static) -> Self {
        Self { cancelled: AtomicBool::new(false), progress: Box::new(progress), note: Mutex::new(None) }
    }

    /// Asks the run to stop; it ends with `EngineError::Cancelled` at its next check.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// `Err(Cancelled)` once cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(EngineError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn report(&self, fraction: f32, label: &str) {
        (self.progress)(&Progress { fraction: fraction.clamp(0.0, 1.0), label: label.to_string() });
    }

    /// Records how the work was done ("CUDA", "CPU", ...), for the log.
    pub fn note(&self, note: impl Into<String>) {
        *self.note.lock().unwrap_or_else(|e| e.into_inner()) = Some(note.into());
    }

    fn take_note(&self) -> Option<String> {
        self.note.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// The result of [`Registry::run_job`]: the files written and the converter that wrote them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub outputs: Vec<PathBuf>,
    /// `Converter::name` of the converter that ran the operation, plus its `Job::note` in
    /// parentheses if it left one ("AI (ONNX Runtime on CPU)").
    pub engine: String,
}

#[derive(Default)]
pub struct Registry {
    converters: Vec<Box<dyn Converter>>,
}

/// The operations of `a` that `b` also offers (by id, recursively); a parent whose children
/// all drop out goes too. Available only if available in both; a pack either needs is kept,
/// and so is `more`.
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
            let pack = op.pack.clone().or_else(|| other.pack.clone());
            let more = op.more || other.more;
            Some(Operation { children, available, pack, more, ..op })
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
        // First: it owns every page-layout conversion (DOCX -> PDF / ODT included: never pandoc)
        // and `sheet.to_xlsx` for workbooks (the sheet converter's is CSV only).
        r.register(Box::new(libreoffice::LibreOfficeConverter::new(config)));
        r.register(Box::new(images::ImageConverter));
        r.register(Box::new(ai::AiConverter::new(config)));
        r.register(Box::new(sheets::SheetConverter));
        r.register(Box::new(pdf::PdfConverter::new(config)));
        r.register(Box::new(office::OfficeConverter));
        r.register(Box::new(documents::DocumentConverter::new(config)));
        r.register(Box::new(copy::CopyConverter));
        r
    }

    pub fn register(&mut self, c: Box<dyn Converter>) {
        self.converters.push(c);
    }

    /// The menu for one file of type `mime`, in priority order: every catalog entry for the
    /// type, with the ones no converter implements yet marked unavailable.
    pub fn operations(&self, mime: &str) -> Vec<Operation> {
        let mut implemented: Vec<Operation> =
            self.converters.iter().filter(|c| c.can_handle(mime)).flat_map(|c| c.operations(mime)).collect();
        let mut ops: Vec<Operation> = catalog::entries_for(mime)
            .map(|entry| {
                let op = match implemented.iter().position(|o| o.id == entry.id) {
                    Some(i) => implemented.remove(i),
                    None => entry.operation().available(false), // planned
                };
                Operation { priority: Some(entry.priority), ..op }
            })
            .collect();
        // Two converters may implement one id (`sheet.to_xlsx`: LibreOffice for workbooks, the
        // sheet converter for CSV); the first wins.
        let mut seen: Vec<String> = ops.iter().map(|o| o.id.clone()).collect();
        implemented.retain(|o| {
            let new = !seen.contains(&o.id);
            seen.push(o.id.clone());
            new
        });
        ops.append(&mut implemented); // types without a menu (tests keep the others complete)
        by_priority(&mut ops);
        ops
    }

    /// Operations that only exist for this selection (e.g. "Merge"), with the selection's
    /// priority from the first file's menu.
    pub fn batch_operations(&self, mimes: &[String]) -> Vec<Operation> {
        let menu = mimes.first().map_or(&[][..], |m| catalog::menu(m));
        self.converters
            .iter()
            .flat_map(|c| c.batch_operations(mimes))
            .map(|op| {
                let priority = menu.iter().find(|e| e.id == op.id).map(|e| e.priority_for(mimes.len()));
                Operation { priority, ..op }
            })
            .collect()
    }

    /// Operations for a selection (one mime per file): the batch operations plus the per-file
    /// operations every selected type offers, in priority order (ties: batch first). Operations
    /// with a `prompt` (their options are picked for one file) need a single-file selection.
    pub fn operations_for(&self, mimes: &[String]) -> Vec<Operation> {
        let Some((first, rest)) = mimes.split_first() else { return Vec::new() };
        let mut common = self.operations(first);
        for m in rest.iter().filter(|m| *m != first) {
            common = intersect(common, &self.operations(m));
        }
        if mimes.len() > 1 {
            common.retain(|o| o.prompt.is_none());
        }
        let mut ops = self.batch_operations(mimes);
        for op in common {
            if !ops.iter().any(|o| o.id == op.id) {
                ops.push(op);
            }
        }
        by_priority(&mut ops);
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
        self.run_traced(mimes, op, inputs, options).map(|r| r.outputs)
    }

    /// [`Registry::run_for`], also reporting which converter ran (e.g. DOCX -> PDF:
    /// "LibreOffice"; MD -> PDF: "pandoc"; PDF -> Word keeping layout: "PDF (LibreOffice)").
    pub fn run_traced(&self, mimes: &[String], op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Run> {
        self.run_job(mimes, op, inputs, options, &Job::default())
    }

    /// [`Registry::run_traced`] with progress and cancellation through `job`.
    pub fn run_job(&self, mimes: &[String], op: &Operation, inputs: &[PathBuf], options: &Options, job: &Job) -> Result<Run> {
        let mut tried = false;
        for c in self.converters.iter().filter(|c| mimes.iter().all(|m| c.can_handle(m))) {
            tried = true;
            match c.run_job(op, inputs, options, job) {
                Err(EngineError::UnknownOperation(_)) => continue,
                other => {
                    let engine = match job.take_note() {
                        Some(note) => format!("{} ({note})", c.name()),
                        None => c.name().to_string(),
                    };
                    return other.map(|outputs| Run { outputs, engine });
                }
            }
        }
        Err(if tried {
            EngineError::UnknownOperation(op.id.clone())
        } else {
            EngineError::Unsupported(mimes.join(", "))
        })
    }
}

/// Stable sort by priority; operations without one go last.
fn by_priority(ops: &mut [Operation]) {
    ops.sort_by_key(|o| o.priority.unwrap_or(u16::MAX));
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

/// Reserves `stem (tag).ext` next to `input` for each `(tag, bytes)` (`stem (tag 2).ext`, ...
/// when taken) and writes them all, or none.
pub(crate) fn write_siblings(input: &Path, ext: &str, outputs: Vec<(String, Vec<u8>)>) -> Result<Vec<PathBuf>> {
    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let mut files = Vec::new();
    for (tag, bytes) in outputs {
        match create_unique(dir, |n| sibling_name(stem, &tag, n, Some(ext))) {
            Ok((file, path)) => files.push((file, path, bytes)),
            Err(e) => {
                for (_, p, _) in &files {
                    let _ = std::fs::remove_file(p);
                }
                return Err(e);
            }
        }
    }
    write_all_or_nothing(files)
}

/// Creates every output or none: files written before a failure are removed.
pub(crate) fn write_all_or_nothing(outputs: Vec<(std::fs::File, PathBuf, Vec<u8>)>) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = outputs.iter().map(|(_, p, _)| p.clone()).collect();
    for (mut file, _, bytes) in outputs {
        if let Err(e) = file.write_all(&bytes) {
            for p in &paths {
                let _ = std::fs::remove_file(p);
            }
            return Err(e.into());
        }
    }
    Ok(paths)
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

    fn ids(ops: &[Operation]) -> Vec<&str> {
        ops.iter().map(|o| o.id.as_str()).collect()
    }

    /// What the menu would show: available operations only.
    fn visible(ops: &[Operation]) -> Vec<&str> {
        ops.iter().filter(|o| o.available).map(|o| o.id.as_str()).collect()
    }

    fn mimes(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// Registry with every tool "present" (any existing file stands in; only presence is checked).
    fn all_tools() -> Registry {
        let exe = std::env::current_exe().unwrap();
        Registry::with_config(&EngineConfig {
            pandoc_path: Some(exe.clone()),
            typst_path: Some(exe.clone()),
            pdfium_path: Some(exe.clone()),
            soffice_path: Some(exe.clone()),
            onnxruntime_path: Some(exe.clone()),
            birefnet_path: Some(exe.clone()),
            realesrgan_path: Some(exe),
            ..EngineConfig::default()
        })
    }

    #[test]
    fn registry_offers_spreadsheet_ops() {
        let r = Registry::with_defaults();
        assert_eq!(
            visible(&r.operations(mime::CSV)),
            ["sheet.to_xlsx", "sheet.to_json", "sheet.change_delimiter", "sheet.dedupe", "sheet.sort", "sheet.split_rows", "copy"]
        );
        for m in [mime::XLSX, mime::XLS, mime::ODS] {
            assert_eq!(visible(&r.operations(m)), ["sheet.to_csv", "sheet.to_json", "sheet.split", "sheet.to_html", "copy"]);
        }
    }

    #[test]
    fn menus_follow_the_catalog_with_planned_ops_unavailable() {
        let r = all_tools();
        for (mime_list, _) in catalog::MENUS {
            for &m in *mime_list {
                let ops = r.operations(m);
                let expected: Vec<&str> = catalog::entries_for(m).map(|e| e.id).collect();
                assert_eq!(ids(&ops), expected, "{m}: every entry, in order, and nothing else");
                for (op, entry) in ops.iter().zip(catalog::entries_for(m)) {
                    assert_eq!((op.label.as_str(), op.icon.as_str()), (entry.label, entry.icon));
                    assert_eq!((op.priority, op.group), (Some(entry.priority), Some(entry.group)));
                }
            }
        }
        // Implemented ops are available (all tools present); planned ones aren't and have no children.
        assert_eq!(
            visible(&r.operations(mime::PDF)),
            [
                "pdf.compress", "pdf.to_docx", "pdf.to_image", "pdf.split", "pdf.rotate", "pdf.extract", "pdf.to_pptx",
                "pdf.to_text", "pdf.watermark", "pdf.page_numbers", "copy",
            ]
        );
        assert_eq!(
            visible(&r.operations("image/png")),
            [
                "image.compress", "image.resize", "image.convert", "image.crop", "image.remove_bg", "image.rotate",
                "image.grayscale", "image.upscale", "image.strip_metadata", "copy",
            ]
        );
        assert_eq!(
            visible(&r.operations(mime::DOCX)),
            [
                "doc.to_pdf", "doc.to_md", "doc.to_html", "doc.compress", "doc.extract_images", "doc.to_odt", "doc.to_epub",
                "doc.to_text", "copy",
            ]
        );
        assert_eq!(
            visible(&r.operations(mime::MARKDOWN)),
            ["doc.to_pdf", "doc.to_docx", "doc.to_html", "doc.to_slides", "doc.to_epub", "doc.to_text", "copy"]
        );
        assert_eq!(visible(&r.operations(mime::HTML)), ["doc.to_pdf", "doc.to_docx", "doc.to_md", "doc.to_text", "copy"]);
        // Duplicate is last and always under More.
        for m in [mime::PDF, mime::DOCX, mime::CSV, "image/png", mime::UNKNOWN] {
            let ops = r.operations(m);
            assert!(ops.iter().filter(|o| o.more).map(|o| o.id.as_str()).eq(["copy"]), "{m}");
        }
        // LibreOffice ("present" here) handles the rest of the Office formats.
        assert_eq!(visible(&r.operations(mime::PPTX)), ["slides.to_pdf", "slides.to_images", "slides.to_odp", "copy"]);
        assert_eq!(visible(&r.operations(mime::PPT)), ["slides.to_pdf", "slides.to_pptx", "slides.to_images", "slides.to_odp", "copy"]);
        assert_eq!(visible(&r.operations(mime::DOC)), ["doc.to_pdf", "doc.to_docx", "doc.to_odt", "copy"]);
        assert_eq!(visible(&r.operations(mime::ODT)), ["doc.to_pdf", "doc.to_docx", "copy"]);
        assert_eq!(visible(&r.operations(mime::XLS)), ["sheet.to_csv", "sheet.to_xlsx", "sheet.to_pdf", "sheet.to_json", "sheet.split", "sheet.to_ods", "sheet.to_html", "copy"]);
        let planned = r.operations(mime::PDF).into_iter().find(|o| o.id == "pdf.ocr").unwrap();
        assert!(!planned.available && planned.children.is_empty());
        // Types without a menu get the converters' operations as they are.
        assert_eq!(ids(&r.operations(mime::UNKNOWN)), ["copy"]);
    }

    /// Everything a converter implements has a catalog entry, so nothing ends up unordered.
    #[test]
    fn every_implemented_operation_is_in_the_catalog() {
        let r = all_tools();
        for (mime_list, _) in catalog::MENUS {
            for &m in *mime_list {
                for op in r.converters.iter().filter(|c| c.can_handle(m)).flat_map(|c| c.operations(m)) {
                    assert!(catalog::entries_for(m).any(|e| e.id == op.id), "{m}: {} is not in the catalog", op.id);
                }
            }
        }
        for sel in [&[mime::PDF, mime::PDF][..], &["image/png"], &["image/png", "image/jpeg"]] {
            for op in r.converters.iter().flat_map(|c| c.batch_operations(&mimes(sel))) {
                assert!(catalog::menu(sel[0]).iter().any(|e| e.id == op.id && e.batch), "{} is not a batch entry", op.id);
            }
        }
    }

    #[test]
    fn selections_offer_what_every_type_offers() {
        let r = Registry::with_defaults();
        // One image: "To PDF" (batch) takes its catalog slot under More; two or more: first.
        let one = r.operations_for(&mimes(&["image/png"]));
        let at = one.iter().position(|o| o.id == "pdf.from_images").unwrap();
        assert_eq!(ids(&one[..at]), ids(&r.operations("image/png"))[..at]);
        assert_eq!(visible(&one)[..5], ["image.compress", "image.resize", "image.convert", "image.crop", "image.rotate"]);
        // Two or more: "To PDF" first, and crop (its rectangle is picked on one image) is gone.
        let two = r.operations_for(&mimes(&["image/png", "image/png"]));
        assert_eq!(two[0].id, "pdf.from_images");
        let single: Vec<Operation> = r.operations("image/png").into_iter().filter(|o| o.prompt.is_none()).collect();
        assert_eq!(two[1..], single);
        assert!(one.iter().any(|o| o.prompt.as_deref() == Some("crop")));
        assert_eq!(ids(&r.operations_for(&mimes(&[mime::PDF]))), ids(&r.operations(mime::PDF)), "merge needs two");
        let pdfs = r.operations_for(&mimes(&[mime::PDF, mime::PDF]));
        assert_eq!((pdfs[0].id.as_str(), pdfs[0].priority), ("pdf.merge", Some(0)));

        // PNG converts to JPG/WebP, JPEG to PNG/WebP: together only WebP is valid for both.
        let mixed = r.operations_for(&mimes(&["image/png", "image/jpeg"]));
        assert_eq!(
            visible(&mixed),
            [
                "pdf.from_images", "image.compress", "image.resize", "image.convert", "image.rotate", "image.grayscale",
                "image.strip_metadata", "copy",
            ]
        );
        let convert = mixed.iter().find(|o| o.id == "image.convert").unwrap();
        assert_eq!(ids(&convert.children), ["image.convert.webp"]);

        assert_eq!(visible(&r.operations_for(&mimes(&["image/png", mime::CSV]))), ["copy"]);
        assert!(r.operations_for(&[]).is_empty());

        // Availability is the AND across types: To image is only planned for Markdown, and a
        // DOCX + PDF selection has nothing in common but Duplicate (which stays under More).
        let r = all_tools();
        let docs = r.operations_for(&mimes(&[mime::DOCX, mime::MARKDOWN]));
        assert_eq!(ids(&docs), ["doc.to_pdf", "doc.to_html", "doc.to_epub", "doc.to_text", "copy"]);
        assert!(docs.iter().all(|o| o.available));
        let md = r.operations_for(&mimes(&[mime::MARKDOWN, mime::HTML]));
        let image = md.iter().find(|o| o.id == "doc.to_image").unwrap();
        assert!(!image.available, "planned for both");
        let copy = r.operations_for(&mimes(&[mime::DOCX, mime::PDF]));
        assert_eq!((ids(&copy), copy[0].more), (vec!["copy"], true));
    }

    #[test]
    fn office_pack_ops_are_flagged_and_carried_through_selections() {
        let r = Registry::with_defaults(); // no LibreOffice
        let pack = |ops: &[Operation], id: &str| ops.iter().find(|o| o.id == id).map(|o| (o.available, o.pack.clone()));
        let doc = r.operations(mime::DOC);
        assert_eq!(pack(&doc, "doc.to_pdf"), Some((false, Some("office".into()))));
        // Layout formats never fall back to pandoc: DOCX -> PDF / ODT need the pack too, and
        // "To PDF" keeps its wedge while the rest wait under More.
        let docx = r.operations(mime::DOCX);
        for id in ["doc.to_pdf", "doc.to_odt"] {
            assert_eq!(pack(&docx, id), Some((false, Some("office".into()))), "{id}");
        }
        let more = |ops: &[Operation], id: &str| ops.iter().find(|o| o.id == id).unwrap().more;
        assert!(!more(&docx, "doc.to_pdf") && more(&docx, "doc.to_odt"));
        let mixed = r.operations_for(&mimes(&[mime::DOCX, mime::DOC]));
        assert_eq!(pack(&mixed, "doc.to_pdf"), Some((false, Some("office".into()))));
        assert_eq!(ids(&docx).iter().filter(|id| **id == "doc.to_pdf").count(), 1, "listed once");

        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.docx"), d.path().join("b.doc"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let op = Operation::new("doc.to_pdf", "", "");
        let err = r.run_for(&mimes(&[mime::DOCX]), &op, &[a], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::PackMissing(_)), "never pandoc: {err}");
        let err = r.run_for(&mimes(&[mime::DOC]), &op, &[b], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::PackMissing(_)), "{err}");
    }

    /// Without the AI pack its operations keep their catalog places (Remove background is one
    /// of the image wedges), flagged as needing the pack, and so do their sub-options.
    #[test]
    fn ai_pack_ops_are_flagged_and_keep_their_places() {
        let r = Registry::with_defaults();
        let ops = r.operations_for(&mimes(&["image/png", "image/jpeg"]));
        let at = |id: &str| ops.iter().position(|o| o.id == id).unwrap();
        for id in ["image.remove_bg", "image.upscale"] {
            let op = &ops[at(id)];
            assert_eq!((op.available, op.pack.as_deref(), op.more), (false, Some(ai::PACK), false), "{id}");
            assert!(op.children.len() == 2 && op.children.iter().all(|c| c.pack.as_deref() == Some(ai::PACK)), "{id}");
        }
        assert!(ops.iter().all(|o| o.id != "image.crop"), "crop is single-file");
        assert!(at("image.remove_bg") < at("image.rotate") && at("image.rotate") < at("image.upscale"));
    }

    #[test]
    fn jobs_report_progress_cancel_and_name_the_engine() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let job = Job::new(move |p| log.lock().unwrap().push(p.clone()));
        job.report(1.5, "almost");
        assert_eq!(seen.lock().unwrap()[0], Progress { fraction: 1.0, label: "almost".into() }, "clamped");
        assert!(job.check().is_ok());
        job.cancel();
        assert!(matches!(job.check(), Err(EngineError::Cancelled)));

        // Converters that ignore the job still run through `run_job`; a note joins the name.
        let d = tempfile::tempdir().unwrap();
        let png = d.path().join("a.png");
        image::RgbImage::new(4, 2).save(&png).unwrap();
        let r = Registry::with_defaults();
        let job = Job::default();
        job.note("test note");
        let run = r.run_job(&mimes(&["image/png"]), &Operation::new("image.rotate.90", "", ""), &[png], &Options::new(), &job).unwrap();
        assert_eq!(run.engine, "image (test note)");
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
        let r = all_tools();
        let mimes = [
            mime::PDF, mime::DOCX, mime::DOC, mime::ODT, mime::RTF, mime::MARKDOWN, mime::HTML, "image/png", "image/jpeg",
            "image/webp", mime::XLSX, mime::XLS, mime::ODS, mime::CSV, mime::PPTX, mime::PPT, mime::ODP,
            mime::UNKNOWN,
        ];
        let operations: serde_json::Map<String, serde_json::Value> =
            mimes.iter().map(|m| (m.to_string(), serde_json::to_value(r.operations(m)).unwrap())).collect();
        // Batch operations for representative selections (key: the mimes, comma-joined).
        let selections: [&[&str]; 4] =
            [&[mime::PDF, mime::PDF], &["image/png"], &["image/png", "image/jpeg"], &[mime::PDF]];
        let batch: serde_json::Map<String, serde_json::Value> = selections
            .iter()
            .map(|sel| {
                let mimes: Vec<String> = sel.iter().map(|m| m.to_string()).collect();
                (mimes.join(","), serde_json::to_value(r.batch_operations(&mimes)).unwrap())
            })
            .collect();
        let snapshot = serde_json::json!({ "operations": operations, "batch": batch });
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
