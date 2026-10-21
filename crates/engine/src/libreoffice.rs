//! Office formats through LibreOffice (the optional "Office pack"), run headless.
//!
//! Operations (top level, no sub-options):
//! - Word (DOCX / DOC / ODT / RTF): `doc.to_pdf`, `doc.to_docx`, `doc.to_odt`
//! - Excel (XLSX / XLS / ODS): `sheet.to_pdf`, `sheet.to_xlsx`, `sheet.to_ods`
//! - PowerPoint (PPTX / PPT / ODP): `slides.to_pdf`, `slides.to_pptx`, `slides.to_odp`, and
//!   `slides.to_images` (PDF via LibreOffice, then every slide rendered to PNG at 150 DPI with
//!   pdfium: `name (slide N).png`, `name (converted).png` for one slide)
//!
//! A conversion to the file's own type is never offered. Outputs are `name (converted).ext`.
//!
//! Page-layout formats never go through pandoc to PDF or ODF: pandoc drops headers, floating
//! images, shapes and positions. Without the pack these are listed with `pack: "office"` and
//! `available: false` (the menu offers the download), and running one is `PackMissing`. "To
//! PDF" keeps its place in the menu ("To PDF, needs Office pack"); the others wait under More.
//!
//! Also here: [`pdf_to_docx`], PDF -> Word keeping the layout (the PDF converter's
//! `pdf.to_docx.layout`).
//!
//! Each run copies the input into a private work dir as `document.<ext>` (odd file names can't
//! confuse LibreOffice's argument parsing), uses a fresh user profile there (no shared state, no
//! lock clashes between parallel runs, nothing left in the user's real profile), and is killed
//! after the tool timeout. A fresh profile makes LibreOffice exit with code 81 ("restart")
//! after creating it, so that exit is retried once. LibreOffice exits 0 even when it cannot
//! open a file; a missing output is reported as a readable `ToolFailed`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::documents::WorkDir;
use crate::mime::{self, DOC, DOCX, ODP, ODS, ODT, PPT, PPTX, RTF, XLS, XLSX};
use crate::{catalog, tools, write_siblings, Converter, EngineConfig, EngineError, Operation, Options, Result};

/// The pack these operations need (`Operation::pack`).
pub const PACK: &str = "office";
const TOOL: &str = "LibreOffice";
/// LibreOffice's "please restart me" exit code, used after it creates a new profile.
const EXIT_RESTART: &str = "exit code 81";
const SLIDE_DPI: u16 = 150;
const TO_IMAGES: &str = "slides.to_images";

/// One conversion: op id, output extension, output mime, `--convert-to` filter.
struct Target {
    id: &'static str,
    ext: &'static str,
    mime: &'static str,
    filter: &'static str,
}

const fn t(id: &'static str, ext: &'static str, mime: &'static str, filter: &'static str) -> Target {
    Target { id, ext, mime, filter }
}

const TEXT: &[Target] = &[
    t("doc.to_pdf", "pdf", mime::PDF, "pdf:writer_pdf_Export"),
    t("doc.to_docx", "docx", DOCX, "docx:MS Word 2007 XML"),
    t("doc.to_odt", "odt", ODT, "odt:writer8"),
];
const SHEETS: &[Target] = &[
    t("sheet.to_pdf", "pdf", mime::PDF, "pdf:calc_pdf_Export"),
    t("sheet.to_xlsx", "xlsx", XLSX, "xlsx:Calc MS Excel 2007 XML"),
    t("sheet.to_ods", "ods", ODS, "ods:calc8"),
];
const SLIDES: &[Target] = &[
    t("slides.to_pdf", "pdf", mime::PDF, "pdf:impress_pdf_Export"),
    t("slides.to_pptx", "pptx", PPTX, "pptx:Impress MS PowerPoint 2007 XML"),
    t("slides.to_odp", "odp", ODP, "odp:impress8"),
];

/// The conversions for a type, and the extension its input copy gets.
fn family(mime: &str) -> Option<(&'static [Target], &'static str)> {
    Some(match mime {
        DOCX => (TEXT, "docx"),
        DOC => (TEXT, "doc"),
        ODT => (TEXT, "odt"),
        RTF => (TEXT, "rtf"),
        XLSX => (SHEETS, "xlsx"),
        XLS => (SHEETS, "xls"),
        ODS => (SHEETS, "ods"),
        PPTX => (SLIDES, "pptx"),
        PPT => (SLIDES, "ppt"),
        ODP => (SLIDES, "odp"),
        _ => return None,
    })
}

pub struct LibreOfficeConverter {
    soffice: Option<PathBuf>,
    pdfium: Option<PathBuf>,
    timeout: Duration,
}

impl LibreOfficeConverter {
    pub fn new(config: &EngineConfig) -> Self {
        Self { soffice: config.soffice_path.clone(), pdfium: config.pdfium_path.clone(), timeout: config.tool_timeout }
    }

    /// Converts `input` (of type `mime`) with `filter`; returns the output's path in `work`.
    fn convert(&self, soffice: &Path, work: &WorkDir, input: &Path, mime: &str, target_ext: &str, filter: &str) -> Result<PathBuf> {
        let (_, in_ext) = family(mime).ok_or_else(|| EngineError::UnsupportedFormat(mime.to_string()))?;
        convert(soffice, self.timeout, work, input, in_ext, None, target_ext, filter)
    }

    fn run_one(&self, soffice: &Path, id: &str, input: &Path) -> Result<Vec<PathBuf>> {
        if !input.is_file() {
            return Err(EngineError::InputNotFound(input.to_path_buf()));
        }
        let mime = mime::detect(input);
        let (targets, _) = family(&mime).ok_or_else(|| EngineError::UnsupportedFormat(mime.clone()))?;
        let work = WorkDir::create()?;
        if id == TO_IMAGES {
            if targets.as_ptr() != SLIDES.as_ptr() {
                return Err(EngineError::UnsupportedFormat(mime));
            }
            let pdfium = self.pdfium.as_deref().ok_or_else(|| EngineError::ToolMissing("pdfium".into()))?;
            let pdf = self.convert(soffice, &work, input, &mime, "pdf", SLIDES[0].filter)?;
            let mut outputs = Vec::new();
            crate::pdf::render_pages(pdfium, &pdf, SLIDE_DPI, |i, count, image| {
                let tag = if count == 1 { "converted".to_string() } else { format!("slide {}", i + 1) };
                outputs.push((tag, crate::images::encode(&image, image::ImageFormat::Png, None)?));
                Ok(())
            })?;
            return write_siblings(input, "png", outputs);
        }
        let target = targets.iter().find(|t| t.id == id).ok_or_else(|| EngineError::UnknownOperation(id.to_string()))?;
        if target.mime == mime {
            return Err(EngineError::InvalidOption(format!("the file already is a {} file", target.ext.to_uppercase())));
        }
        let output = self.convert(soffice, &work, input, &mime, target.ext, target.filter)?;
        write_siblings(input, target.ext, vec![("converted".into(), std::fs::read(output)?)])
    }
}

/// Converts `input` (read as `document.<in_ext>`, with import filter `infilter` if given) with
/// export `filter`; returns the output's path in `work` (`out/document.<target_ext>`). Several
/// calls can share one work dir (and its profile): each copies its input in afresh.
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert(
    soffice: &Path,
    timeout: Duration,
    work: &WorkDir,
    input: &Path,
    in_ext: &str,
    infilter: Option<&str>,
    target_ext: &str,
    filter: &str,
) -> Result<PathBuf> {
    let in_dir = work.0.join("in");
    let out_dir = work.0.join("out");
    std::fs::create_dir_all(&in_dir)?;
    let source = in_dir.join(format!("document.{in_ext}"));
    std::fs::copy(input, &source)?;
    let mut args: Vec<OsString> = vec![
        "--headless".into(),
        "--invisible".into(),
        "--norestore".into(),
        "--nologo".into(),
        "--nolockcheck".into(),
        "--nodefault".into(),
        "--nofirststartwizard".into(),
        format!("-env:UserInstallation={}", file_url(&work.0.join("profile"))).into(),
    ];
    if let Some(f) = infilter {
        args.push(format!("--infilter={f}").into());
    }
    args.extend(["--convert-to".into(), filter.into(), "--outdir".into(), out_dir.clone().into(), source.into()]);
    let mut stderr = String::new();
    for attempt in 0..2 {
        match tools::run(TOOL, soffice, &args, Some(&work.0), timeout) {
            Err(EngineError::ToolFailed { status, .. }) if status == EXIT_RESTART && attempt == 0 => continue,
            Err(e) => return Err(e),
            Ok(err) => {
                stderr = err;
                break;
            }
        }
    }
    let output = out_dir.join(format!("document.{target_ext}"));
    if output.is_file() {
        return Ok(output);
    }
    let detail = tools::readable(&stderr);
    let detail = if detail == "no error output" {
        "it could not open the file (it may be damaged, password-protected or not really this type)".to_string()
    } else {
        detail
    };
    Err(EngineError::ToolFailed { tool: TOOL.into(), status: "no output".into(), stderr: detail })
}

/// PDF -> Word keeping the layout; returns the DOCX's bytes. LibreOffice's PDF import
/// (`writer_pdf_import`) places every text line, image and filled shape at its position on the
/// page. Two steps, PDF -> ODT -> DOCX: exporting the imported document to DOCX directly shifts
/// everything by the page margins. Then [`restack`] fixes the drawing order, which LibreOffice's
/// DOCX export loses for filled shapes (they would cover the text in front of them).
pub(crate) fn pdf_to_docx(soffice: &Path, timeout: Duration, input: &Path) -> Result<Vec<u8>> {
    if !has_pdf_import(soffice) {
        return Err(EngineError::ToolMissing(
            "LibreOffice's PDF import (this Office pack was installed without it: remove the Office pack in Settings and download it again)"
                .into(),
        ));
    }
    let work = WorkDir::create()?;
    let odt = convert(soffice, timeout, &work, input, "pdf", Some("writer_pdf_import"), "odt", "odt:writer8")?;
    let docx = convert(soffice, timeout, &work, &odt, "odt", None, "docx", TEXT[1].filter)?;
    restack_docx(&std::fs::read(docx)?)
}

/// Whether this LibreOffice can open PDFs: its PDF import helper sits next to the program
/// (`program/` on Linux and Windows, `Contents/MacOS` on macOS). Office packs installed before
/// the import was kept on Linux lack it. Symlinks are followed (`/usr/bin/soffice` in tests).
fn has_pdf_import(soffice: &Path) -> bool {
    let program = std::fs::canonicalize(soffice).unwrap_or_else(|_| soffice.to_path_buf());
    program.with_file_name(format!("xpdfimport{}", std::env::consts::EXE_SUFFIX)).is_file()
}

/// [`restack`] applied to `word/document.xml` of a DOCX; every other part is copied as is.
fn restack_docx(docx: &[u8]) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read, Write};
    let bad = |e: zip::result::ZipError| EngineError::ToolFailed { tool: TOOL.into(), status: "unreadable output".into(), stderr: e.to_string() };
    let mut zip = zip::ZipArchive::new(Cursor::new(docx)).map_err(bad)?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(bad)?;
        if entry.name().map_err(bad)? == "word/document.xml" {
            let mut xml = String::new();
            entry.read_to_string(&mut xml)?;
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            out.start_file("word/document.xml", options).map_err(bad)?;
            out.write_all(restack(&xml).as_bytes())?;
        } else {
            out.raw_copy_file(entry).map_err(bad)?;
        }
    }
    Ok(out.finish().map_err(bad)?.into_inner())
}

/// Puts the drawing objects of an imported PDF back in their stacking order. LibreOffice writes
/// text boxes and images as DrawingML anchors (`relativeHeight`), but filled shapes as legacy
/// VML (`<w:pict>`) with no `z-index`, so readers draw those last, over the text. The objects
/// are written front to back, so each gets a height from its position: the anchor's
/// `relativeHeight` and the VML shape's `z-index` (negative: behind text, like the anchors'
/// `behindDoc="1"`; the imported page has no flowing text, only these objects).
pub(crate) fn restack(xml: &str) -> String {
    const AC: (&str, &str) = ("<mc:AlternateContent>", "</mc:AlternateContent>");
    const PICT: (&str, &str) = ("<w:pict>", "</w:pict>");
    let mut spans = Vec::new();
    let mut at = 0;
    loop {
        let next = [AC, PICT].into_iter().filter_map(|(open, close)| Some((xml[at..].find(open)? + at, close))).min_by_key(|(i, _)| *i);
        let Some((start, close)) = next else { break };
        let Some(end) = xml[start..].find(close).map(|i| start + i + close.len()) else { break };
        spans.push((start, end));
        at = end;
    }
    let mut out = String::with_capacity(xml.len() + spans.len() * 16);
    let mut last = 0;
    for (i, &(start, end)) in spans.iter().enumerate() {
        let height = spans.len() - i;
        out.push_str(&xml[last..start]);
        out.push_str(&set_vml_z(&set_attr(&xml[start..end], "relativeHeight", &height.to_string()), -(height as i64)));
        last = end;
    }
    out.push_str(&xml[last..]);
    out
}

/// Every `name="…"` in `xml` set to `value`.
fn set_attr(xml: &str, name: &str, value: &str) -> String {
    let key = format!(" {name}=\"");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find(&key) {
        let from = i + key.len();
        let Some(len) = rest[from..].find('"') else { break };
        out.push_str(&rest[..from]);
        out.push_str(value);
        rest = &rest[from + len..];
    }
    out.push_str(rest);
    out
}

/// The first VML shape in `xml` (not a `v:shapetype`) with `z-index` in its style set to `z`.
fn set_vml_z(xml: &str, z: i64) -> String {
    let mut from = 0;
    while let Some(i) = xml[from..].find("<v:").map(|i| from + i) {
        from = i + 3;
        if xml[i..].starts_with("<v:shapetype") {
            continue;
        }
        let Some(tag_end) = xml[i..].find('>').map(|e| i + e) else { break };
        let Some(style) = xml[i..tag_end].find(" style=\"").map(|s| i + s + 8) else { break };
        let Some(style_end) = xml[style..tag_end].find('"').map(|e| style + e) else { break };
        let kept: Vec<&str> = xml[style..style_end].split(';').filter(|p| !p.trim_start().starts_with("z-index:") && !p.is_empty()).collect();
        return format!("{}{};z-index:{z}{}", &xml[..style], kept.join(";"), &xml[style_end..]);
    }
    xml.to_string()
}

/// A `file://` URL for a local path (LibreOffice's `-env:UserInstallation` wants one).
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

impl Converter for LibreOfficeConverter {
    fn name(&self) -> &'static str {
        "LibreOffice"
    }

    fn can_handle(&self, mime: &str) -> bool {
        family(mime).is_some()
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        let Some((targets, _)) = family(mime) else { return Vec::new() };
        let installed = self.soffice.is_some();
        // Until the pack is installed its operations go under More, so they never take a top
        // wedge from something that works now. "To PDF" is the exception: it is what most
        // people want from these files, so it stays put, offering the download.
        let waiting = |op: Operation, id: &str| if installed || id.ends_with(".to_pdf") { op } else { op.in_more() };
        let mut ops: Vec<Operation> = targets
            .iter()
            .filter(|t| t.mime != mime)
            .map(|t| waiting(catalog::op(t.id).available(installed).needs_pack(PACK), t.id))
            .collect();
        if targets.as_ptr() == SLIDES.as_ptr() {
            let op = catalog::op(TO_IMAGES);
            ops.push(match self.pdfium.is_some() {
                false => op.available(false), // no renderer: the pack wouldn't help
                true => waiting(op.available(installed).needs_pack(PACK), TO_IMAGES),
            });
        }
        ops
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        let id = op.id.as_str();
        if id != TO_IMAGES && ![TEXT, SHEETS, SLIDES].iter().any(|f| f.iter().any(|t| t.id == id)) {
            return Err(EngineError::UnknownOperation(op.id.clone()));
        }
        let Some(soffice) = self.soffice.as_deref() else { return Err(EngineError::PackMissing(PACK.into())) };
        let mut out = Vec::new();
        for input in inputs {
            match self.run_one(soffice, id, input) {
                Ok(files) => out.extend(files),
                Err(e) => {
                    for f in &out {
                        let _ = std::fs::remove_file(f);
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }
}

/// LibreOffice for tests: `RUPANTAR_SOFFICE`, else `soffice` / `libreoffice` on PATH.
#[cfg(test)]
pub(crate) fn for_tests() -> Option<PathBuf> {
    std::env::var_os("RUPANTAR_SOFFICE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| tools::find("soffice", &[]))
        .or_else(|| tools::find("libreoffice", &[]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn converter(soffice: Option<PathBuf>) -> LibreOfficeConverter {
        let pdfium = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/resources/pdfium");
        let config = EngineConfig {
            soffice_path: soffice,
            pdfium_path: crate::pdf::find_pdfium(&[pdfium]),
            tool_timeout: Duration::from_secs(180),
            ..EngineConfig::default()
        };
        LibreOfficeConverter::new(&config)
    }

    fn real() -> Option<LibreOfficeConverter> {
        match for_tests() {
            Some(p) => Some(converter(Some(p))),
            None => {
                eprintln!("skipping: LibreOffice not found (set RUPANTAR_SOFFICE or put soffice on PATH)");
                None
            }
        }
    }

    fn run(c: &LibreOfficeConverter, id: &str, inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        c.run(&Operation::new(id, id, "x"), inputs, &Options::new())
    }

    fn summary(ops: &[Operation]) -> Vec<(&str, bool, Option<&str>)> {
        ops.iter().map(|o| (o.id.as_str(), o.available, o.pack.as_deref())).collect()
    }

    const ODF_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
  xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
  xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
  xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
  xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
  xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" office:version="1.2""#;

    /// A minimal OpenDocument package: `mimetype` (stored, first), manifest, `content.xml`.
    fn odf(path: &Path, mime: &str, body: &str) {
        use zip::write::SimpleFileOptions;
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        z.start_file("mimetype", stored).unwrap();
        z.write_all(mime.as_bytes()).unwrap();
        z.start_file("META-INF/manifest.xml", stored).unwrap();
        write!(
            z,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="{mime}"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#
        )
        .unwrap();
        z.start_file("content.xml", stored).unwrap();
        write!(z, r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODF_NS}><office:body>{body}</office:body></office:document-content>"#).unwrap();
        z.finish().unwrap();
    }

    fn odt(path: &Path, text: &str) {
        odf(path, ODT, &format!("<office:text><text:p>{text}</text:p></office:text>"));
    }

    fn odp(path: &Path, slides: &[&str]) {
        let pages: String = slides
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    r#"<draw:page draw:name="p{i}"><draw:frame svg:x="2cm" svg:y="2cm" svg:width="20cm" svg:height="4cm"><draw:text-box><text:p>{s}</text:p></draw:text-box></draw:frame></draw:page>"#
                )
            })
            .collect();
        odf(path, ODP, &format!("<office:presentation>{pages}</office:presentation>"));
    }

    fn pdf_pages(path: &Path) -> usize {
        lopdf::Document::load(path).unwrap().get_pages().len()
    }

    fn zip_has(path: &Path, entry: &str) -> bool {
        zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap().by_name(entry).is_ok()
    }

    // ---- without LibreOffice --------------------------------------------------------------

    #[test]
    fn without_the_pack_ops_are_listed_as_needing_it() {
        let c = converter(None);
        let pack = Some(PACK);
        assert_eq!(summary(&c.operations(DOC)), [("doc.to_pdf", false, pack), ("doc.to_docx", false, pack), ("doc.to_odt", false, pack)]);
        assert_eq!(summary(&c.operations(ODT)), [("doc.to_pdf", false, pack), ("doc.to_docx", false, pack)]);
        assert_eq!(summary(&c.operations(RTF)), summary(&c.operations(DOC)));
        assert_eq!(summary(&c.operations(DOCX)), [("doc.to_pdf", false, pack), ("doc.to_odt", false, pack)], "never pandoc's");
        let more: Vec<_> = c.operations(DOCX).into_iter().map(|o| o.more).collect();
        assert_eq!(more, [false, true], "To PDF keeps its wedge");
        assert_eq!(summary(&c.operations(XLS)), [("sheet.to_pdf", false, pack), ("sheet.to_xlsx", false, pack), ("sheet.to_ods", false, pack)]);
        assert_eq!(summary(&c.operations(XLSX)), [("sheet.to_pdf", false, pack), ("sheet.to_ods", false, pack)]);
        let images = if c.pdfium.is_some() { pack } else { None };
        assert_eq!(
            summary(&c.operations(PPTX)),
            [("slides.to_pdf", false, pack), ("slides.to_odp", false, pack), (TO_IMAGES, false, images)]
        );
        assert!(c.operations(mime::PDF).is_empty() && !c.can_handle(mime::CSV));
    }

    #[test]
    fn with_the_pack_ops_are_available() {
        let exe = std::env::current_exe().unwrap(); // only presence is checked
        let c = converter(Some(exe));
        assert_eq!(summary(&c.operations(DOCX)), [("doc.to_pdf", true, Some(PACK)), ("doc.to_odt", true, Some(PACK))]);
        assert!(c.operations(DOCX).iter().all(|o| !o.more));
        assert_eq!(summary(&c.operations(PPT))[..3], [
            ("slides.to_pdf", true, Some(PACK)),
            ("slides.to_pptx", true, Some(PACK)),
            ("slides.to_odp", true, Some(PACK)),
        ]);
    }

    #[test]
    fn running_without_the_pack() {
        let d = tempfile::tempdir().unwrap();
        let c = converter(None);
        let doc = d.path().join("a.doc");
        std::fs::write(&doc, b"x").unwrap();
        assert!(matches!(run(&c, "doc.to_pdf", std::slice::from_ref(&doc)), Err(EngineError::PackMissing(p)) if p == PACK));
        let docx = d.path().join("a.docx");
        std::fs::write(&docx, b"x").unwrap();
        assert!(matches!(run(&c, "doc.to_pdf", &[docx]), Err(EngineError::PackMissing(_))), "no pandoc fallback");
        assert!(matches!(run(&c, "image.rotate.90", &[doc]), Err(EngineError::UnknownOperation(_))));
    }

    /// LibreOffice's DOCX of an imported PDF: anchors (front to back) with arbitrary heights,
    /// then filled shapes as VML without a z-index. Each gets its height from its position.
    #[test]
    fn restack_orders_objects_front_to_back() {
        let anchor = |h: u32, text: &str| {
            format!(
                r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor relativeHeight="{h}" behindDoc="1">{text}</wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shapetype id="t"/><v:shape id="s" style="position:absolute;z-index:7"/></w:pict></mc:Fallback></mc:AlternateContent>"#
            )
        };
        let fill = r##"<w:pict><v:shape id="f" fillcolor="#1c1d21" style="position:absolute;width:9pt"/></w:pict>"##;
        let xml = format!("<w:body><w:p><w:r>{}{}{fill}</w:r></w:p></w:body>", anchor(62, "Item"), anchor(9, "Logo"));
        let out = restack(&xml);
        assert_eq!(
            out,
            format!(
                "<w:body><w:p><w:r>{}{}{}</w:r></w:p></w:body>",
                anchor(3, "Item").replace("z-index:7", "z-index:-3"),
                anchor(2, "Logo").replace("z-index:7", "z-index:-2"),
                fill.replace("width:9pt", "width:9pt;z-index:-1"),
            )
        );
        assert_eq!(restack("<w:body/>"), "<w:body/>");
    }

    #[test]
    fn pdf_to_word_without_the_pdf_import_says_how_to_fix_it() {
        let d = tempfile::tempdir().unwrap();
        let soffice = d.path().join("soffice.bin");
        std::fs::write(&soffice, b"x").unwrap();
        let err = pdf_to_docx(&soffice, Duration::from_secs(1), &d.path().join("a.pdf")).unwrap_err();
        assert!(matches!(&err, EngineError::ToolMissing(m) if m.contains("download it again")), "{err}");
    }

    #[test]
    fn file_urls_are_escaped() {
        assert_eq!(file_url(Path::new("/tmp/a b/ü")), "file:///tmp/a%20b/%C3%BC");
        assert_eq!(file_url(Path::new(r"C:\Users\x")), "file:///C:/Users/x");
    }

    /// DOCX -> PDF / ODT go to LibreOffice even with pandoc and typst present. A stand-in
    /// `soffice` that always fails shows who ran, without needing LibreOffice.
    #[cfg(unix)]
    #[test]
    fn the_registry_sends_docx_to_libreoffice_once_installed() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("soffice.bin");
        std::fs::write(&fake, "#!/bin/sh\necho 'stand-in soffice' >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let docx = d.path().join("a.docx");
        std::fs::write(&docx, b"x").unwrap();
        let exe = std::env::current_exe().unwrap();
        let registry = crate::Registry::with_config(&EngineConfig {
            soffice_path: Some(fake),
            pandoc_path: Some(exe.clone()),
            typst_path: Some(exe),
            ..EngineConfig::default()
        });
        for id in ["doc.to_pdf", "doc.to_odt"] {
            let err = registry.run_traced(&[DOCX.to_string()], &Operation::new(id, "", ""), std::slice::from_ref(&docx), &Options::new()).unwrap_err();
            assert!(matches!(&err, EngineError::ToolFailed { tool, .. } if tool == TOOL), "{id}: {err}");
        }
    }

    // ---- with LibreOffice (skipped when it isn't installed) -------------------------------

    #[test]
    fn docx_to_pdf_and_odt_report_libreoffice_as_the_engine() {
        let Some(soffice) = for_tests() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("letter.odt");
        odt(&input, "Dear reader");
        let docx = run(&converter(Some(soffice.clone())), "doc.to_docx", &[input]).unwrap().remove(0);
        let config = EngineConfig {
            soffice_path: Some(soffice),
            pandoc_path: tools::for_tests("pandoc"),
            typst_path: tools::for_tests("typst"),
            tool_timeout: Duration::from_secs(180),
            ..EngineConfig::default()
        };
        let mimes = [DOCX.to_string()];
        let with_pack = crate::Registry::with_config(&config);
        for id in ["doc.to_pdf", "doc.to_odt"] {
            let run = with_pack.run_traced(&mimes, &Operation::new(id, "", ""), std::slice::from_ref(&docx), &Options::new()).unwrap();
            assert_eq!(run.engine, "LibreOffice", "{id}");
        }
        let without = crate::Registry::with_config(&EngineConfig { soffice_path: None, ..config });
        let err = without.run_traced(&mimes, &Operation::new("doc.to_pdf", "", ""), &[docx], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::PackMissing(_)), "never pandoc: {err}");
    }

    #[test]
    fn text_documents_convert() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("-my report ✓.odt");
        odt(&input, "Hello from Rupantar");

        let docx = run(&c, "doc.to_docx", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(docx, d.path().join("-my report ✓ (converted).docx"));
        assert!(zip_has(&docx, "word/document.xml"));

        let pdf = run(&c, "doc.to_pdf", std::slice::from_ref(&docx)).unwrap().remove(0);
        assert_eq!(pdf, d.path().join("-my report ✓ (converted) (converted).pdf"));
        assert_eq!(pdf_pages(&pdf), 1);
        let odt2 = run(&c, "doc.to_odt", &[docx]).unwrap().remove(0);
        assert!(zip_has(&odt2, "content.xml"));
        assert!(matches!(run(&c, "doc.to_odt", &[input]), Err(EngineError::InvalidOption(_))), "same type");
    }

    #[test]
    fn spreadsheets_convert() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("book.xlsx");
        let mut book = rust_xlsxwriter::Workbook::new();
        let ws = book.add_worksheet();
        ws.write(0, 0, "Item").unwrap();
        ws.write(1, 0, 42).unwrap();
        book.save(&input).unwrap();

        let ods = run(&c, "sheet.to_ods", std::slice::from_ref(&input)).unwrap().remove(0);
        assert!(zip_has(&ods, "content.xml"));
        let back = run(&c, "sheet.to_xlsx", &[ods]).unwrap().remove(0);
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&back).unwrap();
        use calamine::Reader;
        let range = wb.worksheet_range_at(0).unwrap().unwrap();
        assert_eq!(range.get_value((1, 0)), Some(&calamine::Data::Float(42.0)));
        let pdf = run(&c, "sheet.to_pdf", &[input]).unwrap().remove(0);
        assert_eq!(pdf_pages(&pdf), 1);
    }

    #[test]
    fn presentations_convert_and_render() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("deck.odp");
        odp(&input, &["First slide", "Second slide"]);

        let pptx = run(&c, "slides.to_pptx", std::slice::from_ref(&input)).unwrap().remove(0);
        assert!(zip_has(&pptx, "ppt/presentation.xml"));
        let pdf = run(&c, "slides.to_pdf", std::slice::from_ref(&pptx)).unwrap().remove(0);
        assert_eq!(pdf_pages(&pdf), 2);
        if c.pdfium.is_some() {
            let pngs = run(&c, TO_IMAGES, &[pptx]).unwrap();
            assert_eq!(pngs, [d.path().join("deck (converted) (slide 1).png"), d.path().join("deck (converted) (slide 2).png")]);
            let img = image::open(&pngs[0]).unwrap();
            assert!(img.width() > img.height() && img.width() > 500, "{}x{}", img.width(), img.height());
        }
    }

    #[test]
    fn unreadable_input_is_a_readable_error_and_leaves_nothing() {
        let Some(c) = real() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("broken.pptx");
        std::fs::write(&input, b"PK\x03\x04 this is not really a presentation").unwrap();
        let err = run(&c, "slides.to_pdf", std::slice::from_ref(&input)).unwrap_err();
        assert!(matches!(err, EngineError::ToolFailed { .. }), "{err}");
        assert!(err.to_string().starts_with("LibreOffice failed"), "{err}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no output left behind");
    }
}
