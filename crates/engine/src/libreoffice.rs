//! Office formats through LibreOffice (the optional "Office pack"), run headless.
//!
//! Operations (top level, no sub-options):
//! - Word (DOCX / DOC / ODT): `doc.to_pdf`, `doc.to_docx`, `doc.to_odt`
//! - Excel (XLSX / XLS / ODS): `sheet.to_pdf`, `sheet.to_xlsx`, `sheet.to_ods`
//! - PowerPoint (PPTX / PPT / ODP): `slides.to_pdf`, `slides.to_pptx`, `slides.to_odp`, and
//!   `slides.to_images` (PDF via LibreOffice, then every slide rendered to PNG at 150 DPI with
//!   pdfium: `name (slide N).png`, `name (converted).png` for one slide)
//!
//! A conversion to the file's own type is never offered. Outputs are `name (converted).ext`.
//!
//! Without the pack these are listed with `pack: "office"` and `available: false` (the menu
//! offers to download it), and running one is `PackMissing`. Exception: DOCX -> PDF / ODT
//! also exist through pandoc, so for DOCX this converter offers them only once LibreOffice is
//! installed (it then takes over: better layout fidelity) and otherwise leaves them to
//! `documents`.
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
use crate::mime::{self, DOC, DOCX, ODP, ODS, ODT, PPT, PPTX, XLS, XLSX};
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
        XLSX => (SHEETS, "xlsx"),
        XLS => (SHEETS, "xls"),
        ODS => (SHEETS, "ods"),
        PPTX => (SLIDES, "pptx"),
        PPT => (SLIDES, "ppt"),
        ODP => (SLIDES, "odp"),
        _ => return None,
    })
}

/// Operations another converter also implements for this type (pandoc, for DOCX).
fn has_fallback(mime: &str, id: &str) -> bool {
    mime == DOCX && matches!(id, "doc.to_pdf" | "doc.to_odt")
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
        let in_dir = work.0.join("in");
        let out_dir = work.0.join("out");
        std::fs::create_dir_all(&in_dir)?;
        let source = in_dir.join(format!("document.{in_ext}"));
        std::fs::copy(input, &source)?;
        let args: Vec<OsString> = vec![
            "--headless".into(),
            "--invisible".into(),
            "--norestore".into(),
            "--nologo".into(),
            "--nolockcheck".into(),
            "--nodefault".into(),
            "--nofirststartwizard".into(),
            format!("-env:UserInstallation={}", file_url(&work.0.join("profile"))).into(),
            "--convert-to".into(),
            filter.into(),
            "--outdir".into(),
            out_dir.clone().into(),
            source.into(),
        ];
        let mut stderr = String::new();
        for attempt in 0..2 {
            match tools::run(TOOL, soffice, &args, Some(&work.0), self.timeout) {
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
    fn can_handle(&self, mime: &str) -> bool {
        family(mime).is_some()
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        let Some((targets, _)) = family(mime) else { return Vec::new() };
        let installed = self.soffice.is_some();
        let mut ops: Vec<Operation> = targets
            .iter()
            .filter(|t| t.mime != mime)
            .filter(|t| installed || !has_fallback(mime, t.id))
            .map(|t| {
                let op = catalog::op(t.id).available(installed);
                if has_fallback(mime, t.id) {
                    op
                } else {
                    op.needs_pack(PACK)
                }
            })
            .collect();
        if targets.as_ptr() == SLIDES.as_ptr() {
            let op = catalog::op(TO_IMAGES);
            ops.push(match (installed, self.pdfium.is_some()) {
                (_, false) => op.available(false), // no renderer: the pack wouldn't help
                (installed, true) => op.available(installed).needs_pack(PACK),
            });
        }
        ops
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        let id = op.id.as_str();
        if id != TO_IMAGES && ![TEXT, SHEETS, SLIDES].iter().any(|f| f.iter().any(|t| t.id == id)) {
            return Err(EngineError::UnknownOperation(op.id.clone()));
        }
        let Some(soffice) = self.soffice.as_deref() else {
            // Let pandoc have DOCX -> PDF / ODT when every input is a DOCX.
            if inputs.iter().all(|i| has_fallback(&mime::detect(i), id)) && !inputs.is_empty() {
                return Err(EngineError::UnknownOperation(op.id.clone()));
            }
            return Err(EngineError::PackMissing(PACK.into()));
        };
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
        assert!(c.operations(DOCX).is_empty(), "DOCX -> PDF / ODT stay with pandoc");
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
    fn with_the_pack_ops_are_available_and_docx_switches_over() {
        let exe = std::env::current_exe().unwrap(); // only presence is checked
        let c = converter(Some(exe));
        assert_eq!(summary(&c.operations(DOCX)), [("doc.to_pdf", true, None), ("doc.to_odt", true, None)]);
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
        assert!(matches!(run(&c, "doc.to_pdf", &[docx]), Err(EngineError::UnknownOperation(_))), "pandoc's turn");
        assert!(matches!(run(&c, "image.rotate.90", &[doc]), Err(EngineError::UnknownOperation(_))));
    }

    #[test]
    fn file_urls_are_escaped() {
        assert_eq!(file_url(Path::new("/tmp/a b/ü")), "file:///tmp/a%20b/%C3%BC");
        assert_eq!(file_url(Path::new(r"C:\Users\x")), "file:///C:/Users/x");
    }

    // ---- with LibreOffice (skipped when it isn't installed) -------------------------------

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
