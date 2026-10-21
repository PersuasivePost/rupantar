//! Layout-fidelity tests: each page-layout conversion is checked against its source, so a
//! regression in routing or rendering (a document going through pandoc instead of LibreOffice,
//! a shape covering text) fails a test instead of shipping.
//!
//! The corpus is in `tests/fixtures/` (built by `make_fixtures.py`): a letterhead DOCX (header
//! image and band, floating photo, a tinted box behind text, a table, signature and stamp), its
//! PDF, a three-slide PPTX and a styled XLSX. Source and output are rendered to greyscale at
//! 50 DPI (documents through LibreOffice to PDF, PDFs with pdfium); page counts must match and,
//! per page, the mean absolute difference (0-255) and that of the worst half-inch tile must stay
//! under the case's thresholds. The tile catches local damage the page mean hides: on the
//! letterhead, PDF -> Word with filled shapes covering text scores 3.4 on the mean but 97 on
//! the worst tile (fixed: 1.8 / 19); the pandoc route LibreOffice replaced scores 44 / 226.
//!
//! On failure the pages are saved side by side (source | output | difference) under
//! `RUPANTAR_FIDELITY_OUT`, else `target/fidelity/<case>/`; `RUPANTAR_FIDELITY_SAVE=1` saves
//! them for passing cases too. Skipped without LibreOffice (`RUPANTAR_SOFFICE`, else PATH) or
//! pdfium (`src-tauri/resources/pdfium/`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use image::{imageops, GrayImage, Luma, RgbImage};

use crate::documents::WorkDir;
use crate::{libreoffice, EngineConfig, Operation, Options, Registry};

const DPI: u16 = 50;
/// Half an inch at `DPI`.
const TILE: u32 = 25;

/// How far apart two pages are: mean absolute difference (0-255) of the page and of its worst tile.
#[derive(Debug, Clone, Copy)]
struct Score {
    mean: f64,
    tile: f64,
}

/// Highest allowed [`Score`].
const fn limit(mean: f64, tile: f64) -> Score {
    Score { mean, tile }
}

struct Env {
    registry: Registry,
    soffice: PathBuf,
    pdfium: PathBuf,
    timeout: Duration,
}

fn env() -> Option<Env> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(pdfium) = crate::pdf::find_pdfium(&[root.join("../../src-tauri/resources/pdfium")]) else {
        eprintln!("skipping: pdfium not found");
        return None;
    };
    let Some(soffice) = libreoffice::for_tests() else {
        eprintln!("skipping: LibreOffice not found (set RUPANTAR_SOFFICE or put soffice on PATH)");
        return None;
    };
    let timeout = Duration::from_secs(180);
    let config = EngineConfig {
        soffice_path: Some(soffice.clone()),
        pdfium_path: Some(pdfium.clone()),
        pandoc_path: crate::tools::for_tests("pandoc"),
        typst_path: crate::tools::for_tests("typst"),
        tool_timeout: timeout,
        ..EngineConfig::default()
    };
    Some(Env { registry: Registry::with_config(&config), soffice, pdfium, timeout })
}

/// A copy of a fixture in `dir`, so outputs land there.
fn fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name), &path).unwrap();
    path
}

/// Every page of `path` in grey: PDFs with pdfium, images as they are, anything else through
/// LibreOffice to PDF first.
fn pages(env: &Env, path: &Path) -> Vec<GrayImage> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "png" {
        return vec![image::open(path).unwrap().to_luma8()];
    }
    let work = WorkDir::create().unwrap();
    let pdf = if ext == "pdf" {
        path.to_path_buf()
    } else {
        libreoffice::convert(&env.soffice, env.timeout, &work, path, &ext, None, "pdf", "pdf").unwrap()
    };
    let mut out = Vec::new();
    crate::pdf::render_pages(&env.pdfium, &pdf, DPI, |_, _, page| {
        out.push(page.to_luma8());
        Ok(())
    })
    .unwrap();
    out
}

/// How far `b` (scaled to `a`'s size) is from `a`, and the per-pixel difference.
fn difference(a: &GrayImage, b: &GrayImage) -> (Score, GrayImage) {
    let b = imageops::resize(b, a.width(), a.height(), imageops::FilterType::Triangle);
    let diff = GrayImage::from_fn(a.width(), a.height(), |x, y| Luma([a.get_pixel(x, y)[0].abs_diff(b.get_pixel(x, y)[0])]));
    let mean = |x0: u32, y0: u32, w: u32, h: u32| {
        let mut total = 0u64;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                total += u64::from(diff.get_pixel(x, y)[0]);
            }
        }
        total as f64 / f64::from((w * h).max(1))
    };
    let mut tile: f64 = 0.0;
    for y in (0..a.height().saturating_sub(TILE - 1)).step_by(TILE as usize) {
        for x in (0..a.width().saturating_sub(TILE - 1)).step_by(TILE as usize) {
            tile = tile.max(mean(x, y, TILE, TILE));
        }
    }
    (Score { mean: mean(0, 0, a.width(), a.height()), tile }, diff)
}

fn out_dir(case: &str) -> PathBuf {
    let base = std::env::var_os("RUPANTAR_FIDELITY_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fidelity"));
    base.join(case)
}

/// Source | output | difference (dark = equal), one PNG per page.
fn save_side_by_side(case: &str, page: usize, source: &GrayImage, output: &GrayImage) -> PathBuf {
    let (_, diff) = difference(source, output);
    let scaled = imageops::resize(output, source.width(), source.height(), imageops::FilterType::Triangle);
    let (w, h) = source.dimensions();
    let mut sheet = RgbImage::from_pixel(w * 3 + 16, h, image::Rgb([220, 40, 40]));
    for (i, img) in [source, &scaled, &diff].into_iter().enumerate() {
        imageops::replace(&mut sheet, &image::DynamicImage::ImageLuma8(img.clone()).to_rgb8(), (i as u32 * (w + 8)).into(), 0);
    }
    let dir = out_dir(case);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("page-{}.png", page + 1));
    sheet.save(&path).unwrap();
    path
}

/// Runs `op` on the fixture `name` and compares every output with the source; `engine` is the
/// converter that must have run it.
fn check(case: &str, name: &str, op: &str, engine: &str, threshold: Score) {
    let Some(env) = env() else { return };
    let d = tempfile::tempdir().unwrap();
    let input = fixture(d.path(), name);
    let mime = crate::mime::detect(&input);
    let run = env.registry.run_traced(&[mime], &Operation::new(op, op, ""), std::slice::from_ref(&input), &Options::new()).unwrap();
    assert_eq!(run.engine, engine, "{case}: ran through the wrong engine");

    let source = pages(&env, &input);
    let output: Vec<GrayImage> = run.outputs.iter().flat_map(|o| pages(&env, o)).collect();
    let save_all = std::env::var_os("RUPANTAR_FIDELITY_SAVE").is_some();
    let scores: Vec<Score> = source.iter().zip(&output).map(|(a, b)| difference(a, b).0).collect();
    let over = |s: &Score| s.mean > threshold.mean || s.tile > threshold.tile;
    eprintln!("{case}: {} page(s), difference per page {scores:.2?} (limit {threshold:?})", source.len());
    let failed = source.len() != output.len() || scores.iter().any(over);
    if failed || save_all {
        let saved: Vec<PathBuf> = source.iter().zip(&output).enumerate().map(|(i, (a, b))| save_side_by_side(case, i, a, b)).collect();
        if let Some(first) = saved.first() {
            eprintln!("{case}: side-by-side pages in {}", first.parent().unwrap().display());
        }
    }
    assert_eq!(output.len(), source.len(), "{case}: page count differs (see {})", out_dir(case).display());
    for (i, s) in scores.iter().enumerate() {
        assert!(!over(s), "{case}: page {} differs by {s:.2?} > {threshold:?} (see {})", i + 1, out_dir(case).display());
    }
}

#[test]
fn letterhead_docx_to_pdf() {
    check("docx-to-pdf", "letterhead.docx", "doc.to_pdf", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn letterhead_docx_to_odt() {
    check("docx-to-odt", "letterhead.docx", "doc.to_odt", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn deck_pptx_to_pdf() {
    check("pptx-to-pdf", "deck.pptx", "slides.to_pdf", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn deck_pptx_to_odp() {
    check("pptx-to-odp", "deck.pptx", "slides.to_odp", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn deck_pptx_to_images() {
    check("pptx-to-images", "deck.pptx", "slides.to_images", "LibreOffice", limit(3.0, 40.0));
}

#[test]
fn styled_xlsx_to_pdf() {
    check("xlsx-to-pdf", "styled.xlsx", "sheet.to_pdf", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn styled_xlsx_to_ods() {
    check("xlsx-to-ods", "styled.xlsx", "sheet.to_ods", "LibreOffice", limit(2.0, 20.0));
}

#[test]
fn letterhead_pdf_to_word_keeping_layout() {
    check("pdf-to-word-layout", "letterhead.pdf", "pdf.to_docx.layout", "PDF (LibreOffice)", limit(3.0, 40.0));
}
