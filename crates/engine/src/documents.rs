//! Document conversions through pandoc (an external binary); PDFs are rendered by typst
//! (`--pdf-engine=typst`, no LaTeX).
//!
//! Operation ids are `doc.to_<format>`: `doc.to_md`, `doc.to_docx`, `doc.to_html`, `doc.to_odt`,
//! `doc.to_epub`, `doc.to_pdf`, `doc.to_slides` (PowerPoint), `doc.to_text` (plain text, `.txt`).
//! Offered: DOCX -> MD / HTML / EPUB / text, MD -> DOCX / HTML / EPUB / PDF / slides / text,
//! HTML -> MD / DOCX / PDF / text. Every operation is listed but marked unavailable when pandoc
//! is missing; PDF also needs typst. DOCX -> PDF / ODT is never done here: pandoc drops the page
//! layout (headers, floating images, shapes), so those go through LibreOffice (`libreoffice`).
//!
//! DOCX -> MD extracts embedded images into a `name_media` folder next to the output (pandoc
//! nests them as `name_media/media/…`) and the Markdown links to them relatively; the folder is
//! removed again when the document has no images. Markdown output is GitHub-flavoured, unwrapped.
//! HTML output is a standalone page; from DOCX its images are embedded as `data:` URIs, so the
//! page is one self-contained file. Text output is unwrapped (one line per paragraph).
//!
//! No network: remote images (`https://…`) become plain links via a Lua filter before any
//! writer would fetch them, and `tools::run` gives pandoc and typst a dead proxy as a backstop
//! (raw HTML `<img>` in EPUB input, typst `@preview` package imports). Local images next to
//! the input still embed, because pandoc runs in the input's directory.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::mime::{self, DOCX, HTML, MARKDOWN};
use crate::{catalog, create_unique_sibling, tools, Converter, EngineConfig, EngineError, Operation, Options, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Docx,
    Markdown,
    Html,
    Odt,
    Epub,
    Pdf,
    Pptx,
    Text,
}

use Format::*;

impl Format {
    fn of_mime(mime: &str) -> Option<Format> {
        match mime {
            DOCX => Some(Docx),
            MARKDOWN => Some(Markdown),
            HTML => Some(Html),
            _ => None,
        }
    }

    fn targets(self) -> &'static [Format] {
        match self {
            // Not PDF or ODT: those keep the page layout, which pandoc drops (LibreOffice does them).
            Docx => &[Markdown, Html, Epub, Text],
            Markdown => &[Docx, Html, Epub, Pdf, Pptx, Text],
            Html => &[Markdown, Docx, Pdf, Text],
            Odt | Epub | Pdf | Pptx | Text => &[],
        }
    }

    /// `(op id, format name for messages, file extension)`. Menu labels are in the catalog.
    fn meta(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Markdown => ("doc.to_md", "Markdown", "md"),
            Docx => ("doc.to_docx", "Word", "docx"),
            Html => ("doc.to_html", "HTML", "html"),
            Odt => ("doc.to_odt", "ODT", "odt"),
            Epub => ("doc.to_epub", "EPUB", "epub"),
            Pdf => ("doc.to_pdf", "PDF", "pdf"),
            Pptx => ("doc.to_slides", "PowerPoint", "pptx"),
            Text => ("doc.to_text", "text", "txt"),
        }
    }

    fn from_id(id: &str) -> Option<Format> {
        [Markdown, Docx, Html, Odt, Epub, Pdf, Pptx, Text].into_iter().find(|f| f.meta().0 == id)
    }

    /// pandoc's reader / writer name. PDF is written as typst source and compiled (`to_pdf`).
    fn pandoc_name(self) -> &'static str {
        match self {
            Docx => "docx",
            Markdown => "markdown",
            Html => "html",
            Odt => "odt",
            Epub => "epub",
            Pdf => "typst",
            Pptx => "pptx",
            Text => "plain",
        }
    }
}

/// Turns remote images into links so no writer tries to download them. `C:\…` is not a
/// scheme (needs two or more letters); `data:` and `file:` are local.
const OFFLINE_FILTER: &str = r#"
local function is_remote(src)
  if src:match("^//") then return true end
  local scheme = src:match("^(%a[%w+.-]+):")
  if not scheme then return false end
  scheme = scheme:lower()
  return scheme ~= "data" and scheme ~= "file"
end

function Image(el)
  if is_remote(el.src) then
    local text = #el.caption > 0 and el.caption or { pandoc.Str(el.src) }
    return pandoc.Link(text, el.src)
  end
end
"#;

pub struct DocumentConverter {
    pandoc: Option<PathBuf>,
    typst: Option<PathBuf>,
    timeout: Duration,
}

impl DocumentConverter {
    pub fn new(config: &EngineConfig) -> Self {
        let usable = |p: &Option<PathBuf>| p.clone().filter(|p| tools::is_executable(p));
        Self { pandoc: usable(&config.pandoc_path), typst: usable(&config.typst_path), timeout: config.tool_timeout }
    }

    fn available(&self, to: Format) -> bool {
        self.pandoc.is_some() && (to != Pdf || self.typst.is_some())
    }

    fn convert(&self, pandoc: &Path, input: &Path, from: Format, to: Format) -> Result<PathBuf> {
        let input = std::path::absolute(input)?;
        let (file, output) = create_unique_sibling(&input, "converted", Some(to.meta().2))?;
        drop(file); // the tool writes the file itself; we only reserved the name
        let result = WorkDir::create().and_then(|work| match to {
            Pdf => self.to_pdf(pandoc, &work, &input, from, &output),
            _ => self.to_document(pandoc, &work, &input, from, to, &output),
        });
        match result {
            Ok(()) => Ok(output),
            Err(e) => {
                let _ = std::fs::remove_file(&output);
                Err(e)
            }
        }
    }

    /// pandoc straight to the output, run in the input's folder so relative images resolve.
    fn to_document(&self, pandoc: &Path, work: &WorkDir, input: &Path, from: Format, to: Format, output: &Path) -> Result<()> {
        let dir = input.parent().unwrap_or_else(|| Path::new("/"));
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
        let mut args = work.pandoc_args(from)?;
        match to {
            Markdown => args.extend(["--to".into(), "gfm".into(), "--wrap=none".into()]),
            Text => args.extend(["--to".into(), "plain".into(), "--wrap=none".into()]),
            Html => args.extend([
                "--to".into(),
                "html5".into(),
                "--standalone".into(),
                "--metadata".into(),
                format!("pagetitle={stem}").into(),
            ]),
            _ => args.extend(["--to".into(), to.pandoc_name().into()]),
        }
        if from == Docx && to == Html {
            // A DOCX's images only exist inside it, so they go into the page as data: URIs
            // (remote ones are links by now, so nothing is fetched).
            args.push("--embed-resources".into());
        }
        let media = if from == Docx && to == Markdown {
            Some(create_unique_dir(dir, |n| match n {
                1 => format!("{stem}_media"),
                n => format!("{stem}_media ({n})"),
            })?)
        } else {
            None
        };
        if let Some(m) = &media {
            // Relative, so the Markdown links stay valid when the folder is moved with the file.
            let mut arg = OsString::from("--extract-media=");
            arg.push(m.file_name().unwrap_or_default());
            args.push(arg);
        }
        args.extend(["--output".into(), output.into(), input.into()]);

        let result = tools::run("pandoc", pandoc, &args, Some(dir), self.timeout);
        if let Some(m) = &media {
            if result.is_ok() {
                let _ = std::fs::remove_dir(m); // only succeeds if nothing was extracted
            } else {
                let _ = std::fs::remove_dir_all(m);
            }
        }
        result.map(drop)
    }

    /// pandoc -> typst source in the work folder (images extracted beside it, linked
    /// relatively), then `typst compile` with that folder as its root. Letting pandoc drive
    /// typst instead breaks images: it hands typst absolute temp paths, which typst resolves
    /// inside its project root.
    fn to_pdf(&self, pandoc: &Path, work: &WorkDir, input: &Path, from: Format, output: &Path) -> Result<()> {
        let typst = self.typst.as_deref().ok_or_else(|| EngineError::ToolMissing("typst".into()))?;
        let mut resources = OsString::from("--resource-path=");
        resources.push(input.parent().unwrap_or_else(|| Path::new("/")));
        let mut args = work.pandoc_args(from)?;
        args.extend([
            "--to".into(),
            "typst".into(),
            "--standalone".into(),
            resources,
            "--extract-media=media".into(),
            "--output".into(),
            "document.typ".into(),
            input.into(),
        ]);
        tools::run("pandoc", pandoc, &args, Some(&work.0), self.timeout)?;
        let args: [OsString; 5] = ["compile".into(), "--root".into(), ".".into(), "document.typ".into(), output.into()];
        tools::run("typst", typst, &args, Some(&work.0), self.timeout).map(drop)
    }
}

/// Atomically creates the first free `dir/name(n)` for n = 1, 2, ... (never reuses a folder).
fn create_unique_dir(dir: &Path, name: impl Fn(u32) -> String) -> Result<PathBuf> {
    for n in 1..10_000 {
        let path = dir.join(name(n));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no free folder name").into())
}

/// A private folder in the temp dir for one conversion (filter, intermediate files); removed on drop.
pub(crate) struct WorkDir(pub(crate) PathBuf);

impl WorkDir {
    pub(crate) fn create() -> Result<Self> {
        let pid = std::process::id();
        create_unique_dir(&std::env::temp_dir(), |n| format!("rupantar-{pid}-{n}")).map(WorkDir)
    }

    /// `--from <reader> --lua-filter <offline filter>`.
    fn pandoc_args(&self, from: Format) -> Result<Vec<OsString>> {
        let filter = self.0.join("offline.lua");
        std::fs::write(&filter, OFFLINE_FILTER)?;
        Ok(vec!["--from".into(), from.pandoc_name().into(), "--lua-filter".into(), filter.into()])
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Converter for DocumentConverter {
    fn name(&self) -> &'static str {
        "pandoc"
    }

    fn can_handle(&self, mime: &str) -> bool {
        Format::of_mime(mime).is_some()
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        let Some(from) = Format::of_mime(mime) else { return Vec::new() };
        from.targets()
            .iter()
            .map(|&to| {
                catalog::op(to.meta().0).available(self.available(to))
            })
            .collect()
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        let to = Format::from_id(&op.id).ok_or_else(|| EngineError::UnknownOperation(op.id.clone()))?;
        // Check the whole batch before converting anything.
        let mut jobs = Vec::new();
        for input in inputs {
            if !input.is_file() {
                return Err(EngineError::InputNotFound(input.clone()));
            }
            let mime = mime::detect(input);
            let from = Format::of_mime(&mime).ok_or_else(|| EngineError::UnsupportedFormat(mime.clone()))?;
            if !from.targets().contains(&to) {
                return Err(EngineError::InvalidOption(format!(
                    "{} can't be converted to {}",
                    from.meta().1,
                    to.meta().1
                )));
            }
            jobs.push((input, from));
        }
        let pandoc = self.pandoc.as_deref().ok_or_else(|| EngineError::ToolMissing("pandoc".into()))?;
        if to == Pdf && self.typst.is_none() {
            return Err(EngineError::ToolMissing("typst".into()));
        }
        jobs.into_iter().map(|(input, from)| self.convert(pandoc, input, from, to)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn converter(pandoc: Option<PathBuf>, typst: Option<PathBuf>) -> DocumentConverter {
        DocumentConverter::new(&EngineConfig { pandoc_path: pandoc, typst_path: typst, ..EngineConfig::default() })
    }

    /// pandoc (and typst) from PATH or the fetched sidecars, or `None` to skip: these tests
    /// need the real tools.
    fn real() -> Option<DocumentConverter> {
        let c = converter(tools::for_tests("pandoc"), tools::for_tests("typst"));
        if c.pandoc.is_none() {
            eprintln!("skipping: pandoc not found");
            return None;
        }
        Some(c)
    }

    fn run(c: &DocumentConverter, id: &str, inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        c.run(&Operation::new(id, id, "x"), inputs, &Options::new())
    }

    fn zip_names(path: &Path) -> Vec<String> {
        let z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        z.file_names().map(|n| n.unwrap().into_owned()).collect()
    }

    fn zip_text(path: &Path, name: &str) -> String {
        let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        image::RgbImage::from_pixel(4, 4, image::Rgb([200, 30, 30])).save(&path).unwrap();
        path
    }

    /// A Markdown file with a heading, a table, a local image and a remote one.
    fn markdown(dir: &Path, name: &str) -> PathBuf {
        png(dir, "dot.png");
        let path = dir.join(name);
        std::fs::write(
            &path,
            "# Quarterly notes\n\nSome *emphasis* and a [link](https://example.com).\n\n\
             | a | b |\n|---|---|\n| 1 | 2 |\n\n\
             ![local dot](dot.png)\n\n![remote](https://example.com/x.png)\n",
        )
        .unwrap();
        path
    }

    /// A DOCX with an embedded image, made by pandoc from `markdown`.
    fn docx(c: &DocumentConverter, dir: &Path) -> PathBuf {
        let out = run(c, "doc.to_docx", &[markdown(dir, "report.md")]).unwrap().remove(0);
        let path = dir.join("report.docx");
        std::fs::rename(out, &path).unwrap();
        path
    }

    // ---- without tools ----------------------------------------------------------------

    #[test]
    fn operations_per_input_type() {
        let c = converter(None, None);
        let ids = |m: &str| c.operations(m).into_iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(ids(DOCX), ["doc.to_md", "doc.to_html", "doc.to_epub", "doc.to_text"], "no PDF / ODT: LibreOffice's");
        assert_eq!(
            ids(MARKDOWN),
            ["doc.to_docx", "doc.to_html", "doc.to_epub", "doc.to_pdf", "doc.to_slides", "doc.to_text"]
        );
        assert_eq!(ids(HTML), ["doc.to_md", "doc.to_docx", "doc.to_pdf", "doc.to_text"]);
        assert!(ids("image/png").is_empty());
        assert!(!c.can_handle("application/pdf"));
    }

    #[test]
    fn missing_tools_mark_operations_unavailable() {
        // Any existing executable stands in for a tool; availability only checks presence.
        let exe = std::env::current_exe().unwrap();
        let avail = |c: &DocumentConverter| {
            c.operations(MARKDOWN).into_iter().map(|o| (o.id, o.available)).collect::<Vec<_>>()
        };
        assert!(avail(&converter(None, Some(exe.clone()))).iter().all(|(_, a)| !a));
        assert_eq!(
            avail(&converter(Some(exe.clone()), None)),
            [
                ("doc.to_docx".into(), true),
                ("doc.to_html".into(), true),
                ("doc.to_epub".into(), true),
                ("doc.to_pdf".into(), false),
                ("doc.to_slides".into(), true),
                ("doc.to_text".into(), true),
            ]
        );
        assert!(avail(&converter(Some(exe.clone()), Some(exe))).iter().all(|(_, a)| *a));
        let gone = PathBuf::from("/definitely/not/pandoc");
        assert!(avail(&converter(Some(gone), None)).iter().all(|(_, a)| !a), "a configured path that doesn't exist");
    }

    #[test]
    fn errors_without_tools_or_with_bad_input() {
        let d = tempfile::tempdir().unwrap();
        let md = d.path().join("a.md");
        std::fs::write(&md, "# hi").unwrap();
        let none = converter(None, None);
        assert!(matches!(run(&none, "doc.to_docx", std::slice::from_ref(&md)), Err(EngineError::ToolMissing(t)) if t == "pandoc"));
        let exe = std::env::current_exe().unwrap();
        let no_typst = converter(Some(exe), None);
        assert!(matches!(run(&no_typst, "doc.to_pdf", std::slice::from_ref(&md)), Err(EngineError::ToolMissing(t)) if t == "typst"));

        let err = run(&none, "doc.to_odt", std::slice::from_ref(&md)).unwrap_err();
        assert_eq!(err.to_string(), "invalid option: Markdown can't be converted to ODT");
        assert!(matches!(run(&none, "doc.to_md", &[d.path().join("x.md")]), Err(EngineError::InputNotFound(_))));
        let png = png(d.path(), "p.png");
        assert!(matches!(run(&none, "doc.to_md", &[png]), Err(EngineError::UnsupportedFormat(_))));
        assert!(matches!(run(&none, "image.rotate.90", &[md]), Err(EngineError::UnknownOperation(_))));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2, "nothing written");
    }

    #[test]
    fn offline_filter_classifies_sources() {
        // Mirrors the Lua `is_remote`, to pin down the cases it must get right.
        for (src, remote) in [
            ("https://example.com/a.png", true),
            ("http://x/a.png", true),
            ("//cdn.example.com/a.png", true),
            ("ftp://host/a.png", true),
            ("dot.png", false),
            ("img/dot.png", false),
            ("C:\\pics\\dot.png", false),
            ("data:image/png;base64,AAAA", false),
            ("file:///tmp/dot.png", false),
        ] {
            let scheme = src.split_once(':').map(|(s, _)| s).filter(|s| {
                s.len() >= 2 && s.starts_with(|c: char| c.is_ascii_alphabetic())
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            });
            let is_remote = src.starts_with("//")
                || scheme.is_some_and(|s| !s.eq_ignore_ascii_case("data") && !s.eq_ignore_ascii_case("file"));
            assert_eq!(is_remote, remote, "{src}");
        }
    }

    // ---- with pandoc (skipped when it isn't on PATH) ----------------------------------

    #[test]
    fn md_to_docx_embeds_local_images_and_links_remote_ones() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let out = run(&c, "doc.to_docx", &[markdown(d.path(), "notes.md")]).unwrap();
        assert_eq!(out, vec![d.path().join("notes (converted).docx")]);
        let names = zip_names(&out[0]);
        let media: Vec<_> = names.iter().filter(|n| n.starts_with("word/media/")).collect();
        assert_eq!(media.len(), 1, "only the local image is embedded: {names:?}");
        let body = zip_text(&out[0], "word/document.xml");
        assert!(body.contains("Quarterly notes"));
        let rels = zip_text(&out[0], "word/_rels/document.xml.rels");
        assert!(rels.contains("https://example.com/x.png"), "the remote image became a hyperlink");
    }

    #[test]
    fn docx_to_md_extracts_media_next_to_the_output() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = docx(&c, d.path());
        let out = run(&c, "doc.to_md", &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("report (converted).md")]);
        let md = std::fs::read_to_string(&out[0]).unwrap();
        assert!(md.starts_with("# Quarterly notes"), "{md}");
        assert!(md.contains("| a   | b   |") || md.contains("| a | b |"), "GFM table: {md}");

        let media_dir = d.path().join("report_media");
        let files: Vec<PathBuf> = walk(&media_dir);
        assert_eq!(files.len(), 1, "{files:?}");
        let rel = files[0].strip_prefix(d.path()).unwrap().to_string_lossy().replace('\\', "/");
        assert!(md.contains(&rel), "the Markdown links to {rel}: {md}");

        // A second conversion gets its own folder; the first is left alone.
        run(&c, "doc.to_md", &[d.path().join("report.docx")]).unwrap();
        assert!(d.path().join("report_media (2)").is_dir());
        assert_eq!(walk(&media_dir).len(), 1);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn docx_without_images_leaves_no_media_folder() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let md = d.path().join("plain.md");
        std::fs::write(&md, "Just text.\n").unwrap();
        let docx = run(&c, "doc.to_docx", &[md]).unwrap().remove(0);
        run(&c, "doc.to_md", &[docx]).unwrap();
        let dirs: Vec<_> = std::fs::read_dir(d.path()).unwrap().filter(|e| e.as_ref().unwrap().path().is_dir()).collect();
        assert!(dirs.is_empty());
    }

    #[test]
    fn md_to_html_is_standalone() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let out = run(&c, "doc.to_html", &[markdown(d.path(), "page.md")]).unwrap();
        let html = std::fs::read_to_string(&out[0]).unwrap();
        assert!(html.contains("<html") && html.contains("<title>page</title>"), "{html}");
        assert!(html.contains("<table"));
    }

    #[test]
    fn docx_to_html_embeds_its_images() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = docx(&c, d.path());
        std::fs::remove_file(d.path().join("dot.png")).unwrap(); // only the copy inside the DOCX is left
        let out = run(&c, "doc.to_html", &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("report (converted).html")]);
        let html = std::fs::read_to_string(&out[0]).unwrap();
        assert!(html.contains("<title>report</title>") && html.contains("Quarterly notes"), "{html}");
        assert!(html.contains("src=\"data:image/png;base64,"), "the image is inside the page");
        assert!(html.contains("https://example.com/x.png"), "the remote image stays a link");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 3, "no media folder: {:?}", walk(d.path()));
    }

    #[test]
    fn docx_markdown_and_html_to_text() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = docx(&c, d.path());
        let html = d.path().join("page.html");
        std::fs::write(&html, "<h1>Title</h1><p>One <b>bold</b> line that is long enough to have been wrapped by pandoc at seventy-two columns.</p>").unwrap();
        let out = run(&c, "doc.to_text", &[input, d.path().join("report.md"), html]).unwrap();
        assert_eq!(out[0], d.path().join("report (converted).txt"));
        let text = std::fs::read_to_string(&out[0]).unwrap();
        assert!(text.contains("Quarterly notes") && text.contains("emphasis"), "{text}");
        assert!(!text.contains("**") && !text.contains("<"), "plain, no markup: {text}");
        let page = std::fs::read_to_string(&out[2]).unwrap();
        assert!(page.contains("One bold line that is long enough to have been wrapped by pandoc at seventy-two columns."), "unwrapped: {page}");
    }

    #[test]
    fn html_to_md_and_docx() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("site.html");
        std::fs::write(&input, "<html><body><h1>Hello</h1><p>A <b>bold</b> claim.</p></body></html>").unwrap();
        let md = run(&c, "doc.to_md", std::slice::from_ref(&input)).unwrap();
        assert_eq!(std::fs::read_to_string(&md[0]).unwrap().trim(), "# Hello\n\nA **bold** claim.");
        let docx = run(&c, "doc.to_docx", &[input]).unwrap();
        assert!(zip_text(&docx[0], "word/document.xml").contains("claim."));
    }

    #[test]
    fn epubs_from_mixed_inputs_and_no_layout_formats_from_docx() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = docx(&c, d.path());
        for id in ["doc.to_odt", "doc.to_pdf"] {
            let err = run(&c, id, std::slice::from_ref(&input)).unwrap_err();
            assert!(matches!(err, EngineError::InvalidOption(_)), "{id}: {err}");
        }

        let md = d.path().join("other.md");
        std::fs::write(&md, "# Other\n").unwrap();
        let epubs = run(&c, "doc.to_epub", &[input, md]).unwrap();
        assert_eq!(epubs, vec![d.path().join("report (converted).epub"), d.path().join("other (converted).epub")]);
        for e in &epubs {
            assert_eq!(zip_text(e, "mimetype"), "application/epub+zip");
        }
    }

    #[test]
    fn markdown_to_slides() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let md = d.path().join("talk.md");
        std::fs::write(&md, "# Intro\n\nHello there\n\n# Plan\n\n- one\n- two\n").unwrap();
        let out = run(&c, "doc.to_slides", &[md]).unwrap().remove(0);
        assert_eq!(out, d.path().join("talk (converted).pptx"));
        assert!(zip_text(&out, "ppt/slides/slide2.xml").contains("Plan"));
        assert!(zip_text(&out, "ppt/slides/slide2.xml").contains("two"));
    }

    #[test]
    fn md_to_pdf_via_typst() {
        let Some(c) = real() else { return };
        if c.typst.is_none() {
            eprintln!("skipping: typst not found");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let md = markdown(d.path(), "print.md");
        let html = d.path().join("page.html");
        std::fs::write(&html, "<h1>Page</h1><p>Text</p>").unwrap();
        let out = run(&c, "doc.to_pdf", &[md, html]).unwrap();
        for pdf in &out {
            let bytes = std::fs::read(pdf).unwrap();
            assert!(bytes.starts_with(b"%PDF-") && bytes.len() > 1000, "{pdf:?}");
        }
    }

    #[test]
    fn pandoc_errors_are_readable_and_leave_nothing_behind() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let broken = d.path().join("broken.docx");
        std::fs::write(&broken, b"this is not a zip file").unwrap();
        let err = run(&c, "doc.to_md", &[broken]).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, EngineError::ToolFailed { .. }), "{msg}");
        assert!(msg.starts_with("pandoc failed (exit code"), "{msg}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no output or media folder left");
    }

    #[test]
    fn timeouts_stop_pandoc() {
        let Some(mut c) = real() else { return };
        c.timeout = Duration::ZERO;
        let d = tempfile::tempdir().unwrap();
        let err = run(&c, "doc.to_html", &[markdown(d.path(), "slow.md")]).unwrap_err();
        assert!(matches!(err, EngineError::ToolTimeout { .. }), "{err}");
        assert!(!d.path().join("slow (converted).html").exists());
    }

    #[test]
    fn outputs_never_overwrite() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let md = markdown(d.path(), "n.md");
        std::fs::write(d.path().join("n (converted).html"), "keep").unwrap();
        let out = run(&c, "doc.to_html", &[md]).unwrap();
        assert_eq!(out, vec![d.path().join("n (converted 2).html")]);
        assert_eq!(std::fs::read_to_string(d.path().join("n (converted).html")).unwrap(), "keep");
    }
}
