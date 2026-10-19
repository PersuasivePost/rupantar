//! PDF operations. Page manipulation is pure Rust (`lopdf`); rendering pages to images uses
//! pdfium (`pdfium-render`, bound at runtime to the library at `EngineConfig::pdfium_path`).
//!
//! Operation ids (`options` override the value in the id, like the image ops):
//! - `pdf.split`: one PDF per page, `name (page N).pdf`.
//! - `pdf.extract.<pages>`: `1`, `1-3`, `3-`, `-2`, `1,4-5`, `odd`, `even` (option `pages`).
//!   Ranges are clipped to the document; selecting no existing page is an error.
//! - `pdf.rotate.<90|180|270>`: clockwise, added to each page's current rotation (option `degrees`).
//! - `pdf.to_png.<dpi>` / `pdf.to_jpg.<dpi>` (listed under `pdf.to_image`): one image per page (option `dpi`, 1..=1200), white
//!   background; `name (page N).png`, or `name (converted).png` for a one-page PDF. Unavailable
//!   (listed, not rendered by the menu) when pdfium isn't found.
//! - Batch: `pdf.merge` (two or more PDFs, in selection order, next to the first) and
//!   `pdf.from_images.<a4|original>` (images, one per page, option `fit`). A4 picks portrait or
//!   landscape to match each image, keeps a 0.5" margin and only ever shrinks; original size
//!   maps pixels at 96 DPI. JPEGs are embedded as-is (no re-encode); other images losslessly.
//!
//! New documents keep the selected pages' content, resources and annotations; bookmarks
//! (outlines), form fields and document metadata are not carried over. Encrypted PDFs open
//! only if they need no password.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::{PdfColor, PdfRenderConfig, Pdfium};

use crate::mime::{self, PDF};
use crate::{
    catalog, create_unique, sibling_name, write_all_or_nothing, Converter, EngineConfig, EngineError, Operation, Options, Result,
};

const IMAGE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
/// A4 in points.
const A4: (f32, f32) = (595.28, 841.89);
const A4_MARGIN: f32 = 36.0;
/// "Original size": pixels at 96 DPI.
const PT_PER_PX: f32 = 72.0 / 96.0;
/// Rendered pages are capped at this many pixels per side (a poster at 300 DPI would need GBs).
const MAX_RENDER_PX: i32 = 16_384;
/// Page-tree attributes a page inherits from its ancestors (PDF 32000-1, 7.7.3.4).
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// The pdfium library file in the first of `dirs` that has it.
pub fn find_pdfium(dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = format!("{}pdfium{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    dirs.iter().map(|d| d.join(&name)).find(|p| p.is_file())
}

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Split,
    Extract(String),
    Rotate(i64),
    Render { format: ImageFormat, dpi: u16 },
    Merge,
    FromImages { a4: bool },
}

/// A value from `options[key]`, else the id's remaining segments (joined by '.'), else an error.
fn value<'a>(options: &'a Options, key: &str, rest: &'a [&'a str]) -> Result<std::borrow::Cow<'a, str>> {
    if let Some(v) = options.get(key) {
        return Ok(v.trim().into());
    }
    if rest.is_empty() {
        return Err(EngineError::InvalidOption(format!("missing `{key}`")));
    }
    Ok(rest.join(".").into())
}

fn parse_action(id: &str, options: &Options) -> Result<Action> {
    let parts: Vec<&str> = id.split('.').collect();
    let invalid = |key: &str, v: &str| EngineError::InvalidOption(format!("`{key}` is not valid: `{v}`"));
    match parts.as_slice() {
        ["pdf", "split"] => Ok(Action::Split),
        ["pdf", "extract", rest @ ..] => Ok(Action::Extract(value(options, "pages", rest)?.into_owned())),
        ["pdf", "rotate", rest @ ..] => {
            let v = value(options, "degrees", rest)?;
            match v.parse::<i64>() {
                Ok(d @ (90 | 180 | 270)) => Ok(Action::Rotate(d)),
                _ => Err(invalid("degrees", &v)),
            }
        }
        ["pdf", fmt @ ("to_png" | "to_jpg"), rest @ ..] => {
            let format = if *fmt == "to_png" { ImageFormat::Png } else { ImageFormat::Jpeg };
            let v = value(options, "dpi", rest)?;
            match v.parse::<u16>() {
                Ok(dpi @ 1..=1200) => Ok(Action::Render { format, dpi }),
                _ => Err(invalid("dpi", &v)),
            }
        }
        ["pdf", "merge"] => Ok(Action::Merge),
        ["pdf", "from_images", rest @ ..] => match value(options, "fit", rest)?.as_ref() {
            "a4" => Ok(Action::FromImages { a4: true }),
            "original" => Ok(Action::FromImages { a4: false }),
            other => Err(invalid("fit", other)),
        },
        _ => Err(EngineError::UnknownOperation(id.to_string())),
    }
}

/// Page numbers (1-based, sorted, unique) selected by `spec` in a document of `count` pages.
fn parse_pages(spec: &str, count: u32) -> Result<Vec<u32>> {
    let invalid = || EngineError::InvalidOption(format!("page range `{spec}` is not valid"));
    let spec = spec.trim().to_ascii_lowercase();
    let mut pages = BTreeSet::new();
    match spec.as_str() {
        "odd" => pages.extend((1..=count).step_by(2)),
        "even" => pages.extend((2..=count).step_by(2)),
        _ => {
            for item in spec.split(',').map(str::trim) {
                let num = |s: &str| s.trim().parse::<u32>().ok().filter(|n| *n >= 1);
                let (start, end) = match item.split_once('-') {
                    Some((a, b)) if a.trim().is_empty() && b.trim().is_empty() => return Err(invalid()),
                    Some((a, b)) => {
                        let start = if a.trim().is_empty() { 1 } else { num(a).ok_or_else(invalid)? };
                        let end = if b.trim().is_empty() { count } else { num(b).ok_or_else(invalid)? };
                        (start, end)
                    }
                    None => {
                        let n = num(item).ok_or_else(invalid)?;
                        (n, n)
                    }
                };
                if start > end {
                    return Err(invalid());
                }
                if start > count {
                    return Err(EngineError::InvalidOption(format!(
                        "page {start} doesn't exist (the PDF has {count} page{})",
                        if count == 1 { "" } else { "s" }
                    )));
                }
                pages.extend(start..=end.min(count));
            }
        }
    }
    if pages.is_empty() {
        return Err(EngineError::InvalidOption(format!("`{spec}` selects no pages")));
    }
    Ok(pages.into_iter().collect())
}

fn load(input: &Path) -> Result<Document> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let doc = Document::load(input)?;
    if doc.is_encrypted() {
        return Err(EngineError::UnsupportedFormat("password-protected PDF".into()));
    }
    Ok(doc)
}

/// The value of an inheritable attribute for `page`: its own, else the nearest ancestor's.
fn inherited(doc: &Document, page: ObjectId, key: &[u8]) -> Option<Object> {
    let mut dict = doc.get_dictionary(page).ok()?;
    for _ in 0..64 {
        // depth guard against a cyclic /Parent chain
        if let Ok(v) = dict.get(key) {
            return Some(v.clone());
        }
        let parent = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
        dict = doc.get_dictionary(parent).ok()?;
    }
    None
}

/// Copies inherited attributes onto each page, so pages keep their size, resources and
/// rotation when they are moved under a new page tree.
fn flatten_inherited(doc: &mut Document) -> Result<()> {
    for page in doc.get_pages().into_values() {
        let missing: Vec<(&[u8], Object)> = INHERITABLE
            .iter()
            .filter(|k| doc.get_dictionary(page).is_ok_and(|d| !d.has(k)))
            .filter_map(|k| inherited(doc, page, k).map(|v| (*k, v)))
            .collect();
        let dict = doc.get_object_mut(page)?.as_dict_mut()?;
        for (k, v) in missing {
            dict.set(k, v);
        }
    }
    Ok(())
}

/// Every object reachable from `roots`, not following a page's way back up (`/Parent`) or an
/// annotation's page link (`/P`), which would drag in the pages that weren't selected.
fn reachable(doc: &Document, roots: &[ObjectId]) -> BTreeSet<ObjectId> {
    fn refs<'a>(obj: &'a Object, out: &mut Vec<&'a Object>) {
        match obj {
            Object::Reference(_) => out.push(obj),
            Object::Array(items) => items.iter().for_each(|o| refs(o, out)),
            Object::Dictionary(d) => d.iter().filter(|(k, _)| !matches!(k.as_slice(), b"Parent" | b"P")).for_each(|(_, o)| refs(o, out)),
            Object::Stream(s) => s.dict.iter().filter(|(k, _)| k.as_slice() != b"Parent").for_each(|(_, o)| refs(o, out)),
            _ => {}
        }
    }
    let mut seen = BTreeSet::new();
    let mut stack: Vec<ObjectId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Ok(obj) = doc.get_object(id) {
            let mut found = Vec::new();
            refs(obj, &mut found);
            stack.extend(found.into_iter().filter_map(|o| o.as_reference().ok()));
        }
    }
    seen
}

/// Assembles a new document from pages of other documents.
struct Builder {
    out: Document,
    kids: Vec<ObjectId>,
}

impl Builder {
    fn new() -> Self {
        Self { out: Document::with_version("1.7"), kids: Vec::new() }
    }

    /// Appends pages `numbers` (1-based) of `doc`, in that order.
    fn add(&mut self, mut doc: Document, numbers: &[u32]) -> Result<()> {
        flatten_inherited(&mut doc)?;
        doc.renumber_objects_with(self.out.max_id + 1);
        let pages = doc.get_pages();
        let ids: Vec<ObjectId> = numbers.iter().filter_map(|n| pages.get(n).copied()).collect();
        for id in reachable(&doc, &ids) {
            if let Some(obj) = doc.objects.remove(&id) {
                self.out.objects.insert(id, obj);
            }
        }
        self.out.max_id = self.out.max_id.max(doc.max_id);
        self.kids.extend(ids);
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        let pages_id = self.out.new_object_id();
        for kid in &self.kids {
            self.out.get_object_mut(*kid)?.as_dict_mut()?.set("Parent", pages_id);
        }
        let kids: Vec<Object> = self.kids.iter().map(|k| Object::Reference(*k)).collect();
        let count = kids.len() as i64;
        self.out.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
        let catalog = self.out.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        self.out.trailer.set("Root", catalog);
        save(&mut self.out)
    }
}

fn save(doc: &mut Document) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    Ok(bytes)
}

fn stem(input: &Path) -> &str {
    input.file_stem().and_then(|s| s.to_str()).unwrap_or("document")
}

/// Reserves `name (tag).ext` next to `input` for each `(tag, bytes)` and writes them all, or none.
fn write_siblings(input: &Path, ext: &str, outputs: Vec<(String, Vec<u8>)>) -> Result<Vec<PathBuf>> {
    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = stem(input);
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

fn split(input: &Path) -> Result<Vec<PathBuf>> {
    let doc = load(input)?;
    let count = doc.get_pages().len() as u32;
    let mut outputs = Vec::new();
    for n in 1..=count {
        let mut b = Builder::new();
        b.add(doc.clone(), &[n])?;
        outputs.push((format!("page {n}"), b.finish()?));
    }
    write_siblings(input, "pdf", outputs)
}

fn extract(input: &Path, spec: &str) -> Result<PathBuf> {
    let doc = load(input)?;
    let pages = parse_pages(spec, doc.get_pages().len() as u32)?;
    let mut b = Builder::new();
    b.add(doc, &pages)?;
    Ok(write_siblings(input, "pdf", vec![("extracted".into(), b.finish()?)])?.remove(0))
}

fn rotate(input: &Path, degrees: i64) -> Result<PathBuf> {
    let mut doc = load(input)?;
    for page in doc.get_pages().into_values() {
        let current = inherited(&doc, page, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0);
        let next = (current + degrees).rem_euclid(360);
        doc.get_object_mut(page)?.as_dict_mut()?.set("Rotate", next);
    }
    Ok(write_siblings(input, "pdf", vec![("rotated".into(), save(&mut doc)?)])?.remove(0))
}

fn merge(inputs: &[PathBuf]) -> Result<PathBuf> {
    if inputs.len() < 2 {
        return Err(EngineError::InvalidOption("merging needs at least two PDFs".into()));
    }
    let mut b = Builder::new();
    for input in inputs {
        let doc = load(input)?;
        let all: Vec<u32> = doc.get_pages().into_keys().collect();
        b.add(doc, &all)?;
    }
    Ok(write_siblings(&inputs[0], "pdf", vec![("merged".into(), b.finish()?)])?.remove(0))
}

// ---- images -> PDF ----------------------------------------------------------------------

/// Colour components declared in a JPEG's SOF header (1 = grey, 3 = YCbCr/RGB, 4 = CMYK).
fn jpeg_components(b: &[u8]) -> Option<u8> {
    let mut i = 2; // past SOI
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return b.get(i + 9).copied(); // length(2) precision(1) height(2) width(2) -> count
        }
        i += 2 + usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
    }
    None
}

fn image_stream(width: u32, height: u32, color_space: &str, filter: Option<&str>, data: Vec<u8>) -> Stream {
    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => i64::from(width),
        "Height" => i64::from(height),
        "ColorSpace" => color_space,
        "BitsPerComponent" => 8,
    };
    if let Some(f) = filter {
        dict.set("Filter", f);
    }
    Stream::new(dict, data)
}

/// An image XObject for the file: JPEG passed through (DCTDecode), anything else decoded
/// and stored Flate-compressed, with its alpha channel as a soft mask.
fn image_xobject(doc: &mut Document, input: &Path) -> Result<(ObjectId, u32, u32)> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let bytes = std::fs::read(input)?;
    let reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
    if reader.format() == Some(ImageFormat::Jpeg) {
        let gray = match jpeg_components(&bytes) {
            Some(1) => Some(true),
            Some(3) => Some(false),
            _ => None, // CMYK etc.: decode below rather than guess at inverted Adobe CMYK
        };
        if let Some(gray) = gray {
            let (w, h) = reader.into_decoder()?.dimensions();
            let cs = if gray { "DeviceGray" } else { "DeviceRGB" };
            let id = doc.add_object(image_stream(w, h, cs, Some("DCTDecode"), bytes));
            return Ok((id, w, h));
        }
    }
    let img: DynamicImage = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?.decode()?;
    let (w, h) = img.dimensions();
    let mut stream = image_stream(w, h, "DeviceRGB", None, img.to_rgb8().into_raw());
    stream.compress()?;
    if img.color().has_alpha() {
        let alpha: Vec<u8> = img.to_rgba8().pixels().map(|p| p[3]).collect();
        let mut mask = image_stream(w, h, "DeviceGray", None, alpha);
        mask.compress()?;
        stream.dict.set("SMask", doc.add_object(mask));
    }
    Ok((doc.add_object(stream), w, h))
}

/// Page size and the image's placement `(page_w, page_h, x, y, draw_w, draw_h)` in points.
fn layout(px_w: u32, px_h: u32, a4: bool) -> (f32, f32, f32, f32, f32, f32) {
    let (w, h) = (px_w as f32 * PT_PER_PX, px_h as f32 * PT_PER_PX);
    if !a4 {
        return (w, h, 0.0, 0.0, w, h);
    }
    let (pw, ph) = if px_w > px_h { (A4.1, A4.0) } else { A4 };
    let scale = ((pw - 2.0 * A4_MARGIN) / w).min((ph - 2.0 * A4_MARGIN) / h).min(1.0);
    let (dw, dh) = (w * scale, h * scale);
    (pw, ph, (pw - dw) / 2.0, (ph - dh) / 2.0, dw, dh)
}

fn from_images(inputs: &[PathBuf], a4: bool) -> Result<PathBuf> {
    let first = inputs.first().ok_or_else(|| EngineError::InvalidOption("no images selected".into()))?;
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for input in inputs {
        let (image, w, h) = image_xobject(&mut doc, input)?;
        let (pw, ph, x, y, dw, dh) = layout(w, h, a4);
        let content = format!("q {dw:.3} 0 0 {dh:.3} {x:.3} {y:.3} cm /Im0 Do Q");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw), Object::Real(ph)],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page));
    }
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let tag = if inputs.len() == 1 { "converted" } else { "combined" };
    Ok(write_siblings(first, "pdf", vec![(tag.into(), save(&mut doc)?)])?.remove(0))
}

// ---- rendering (pdfium) -----------------------------------------------------------------

/// pdfium can be bound only once per process, so the first successful binding is shared.
fn pdfium(library: &Path) -> Result<&'static Pdfium> {
    static PDFIUM: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| Pdfium::bind_to_library(library).map(Pdfium::new).map_err(|e| format!("{e:?}")))
        .as_ref()
        .map_err(|e| EngineError::Pdfium(format!("could not load {}: {e}", library.display())))
}

fn render(library: &Path, input: &Path, format: ImageFormat, dpi: u16) -> Result<Vec<PathBuf>> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let pdfium = pdfium(library)?;
    let err = |e: pdfium_render::prelude::PdfiumError| EngineError::Pdfium(format!("{e:?}"));
    let doc = pdfium.load_pdf_from_file(input, None).map_err(err)?;
    let config = PdfRenderConfig::new()
        .scale_page_by_factor(f32::from(dpi) / 72.0)
        .set_maximum_width(MAX_RENDER_PX)
        .set_maximum_height(MAX_RENDER_PX)
        .set_clear_color(PdfColor::WHITE)
        .render_form_data(true);
    let pages = doc.pages();
    let count = pages.len();
    let ext = if format == ImageFormat::Png { "png" } else { "jpg" };
    let mut outputs = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let image = page.render_with_config(&config).map_err(err)?.as_image().map_err(err)?;
        let bytes = crate::images::encode(&image, format, Some(90))?;
        let tag = if count == 1 { "converted".to_string() } else { format!("page {}", i + 1) };
        outputs.push((tag, bytes));
    }
    write_siblings(input, ext, outputs)
}

// ---- converter ---------------------------------------------------------------------------

pub struct PdfConverter {
    pdfium: Option<PathBuf>,
}

impl PdfConverter {
    pub fn new(config: &EngineConfig) -> Self {
        Self { pdfium: config.pdfium_path.clone().filter(|p| p.is_file()) }
    }
}

fn op(id: &str, label: &str, icon: &str) -> Operation {
    Operation::new(id, label, icon)
}

/// PNG and JPG at each DPI: the sub-options of "To image".
fn render_targets() -> Vec<Operation> {
    let dpis = [(72, "Monitor"), (150, "Gauge"), (300, "Printer")];
    [("png", "PNG"), ("jpg", "JPG")]
        .into_iter()
        .flat_map(|(ext, name)| {
            dpis.into_iter().map(move |(dpi, icon)| op(&format!("pdf.to_{ext}.{dpi}"), &format!("{name} {dpi} DPI"), icon))
        })
        .collect()
}

fn check_inputs(inputs: &[PathBuf], allowed: &[&str]) -> Result<()> {
    for input in inputs {
        if !input.is_file() {
            return Err(EngineError::InputNotFound(input.clone()));
        }
        let m = mime::detect(input);
        if !allowed.contains(&m.as_str()) {
            return Err(EngineError::UnsupportedFormat(m));
        }
    }
    Ok(())
}

impl Converter for PdfConverter {
    fn can_handle(&self, mime: &str) -> bool {
        mime == PDF || IMAGE_MIMES.contains(&mime)
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if mime != PDF {
            return Vec::new(); // images only get the batch "Images -> PDF"
        }
        let render = self.pdfium.is_some();
        vec![
            catalog::op("pdf.split"),
            catalog::op("pdf.extract").with_children(vec![
                op("pdf.extract.1", "Page 1", "FileDigit"),
                op("pdf.extract.1-3", "Pages 1\u{2013}3", "Files"),
                op("pdf.extract.odd", "Odd", "ListOrdered"),
                op("pdf.extract.even", "Even", "ListOrdered"),
            ]),
            catalog::op("pdf.rotate").with_children(vec![
                op("pdf.rotate.90", "90\u{b0} CW", "RotateCw"),
                op("pdf.rotate.270", "90\u{b0} CCW", "RotateCcw"),
                op("pdf.rotate.180", "180\u{b0}", "RotateCw"),
            ]),
            catalog::op("pdf.to_image").with_children(render_targets()).available(render),
        ]
    }

    fn batch_operations(&self, mimes: &[String]) -> Vec<Operation> {
        let mut ops = Vec::new();
        if mimes.len() >= 2 && mimes.iter().all(|m| m == PDF) {
            ops.push(catalog::op("pdf.merge"));
        }
        if !mimes.is_empty() && mimes.iter().all(|m| IMAGE_MIMES.contains(&m.as_str())) {
            ops.push(catalog::op("pdf.from_images").with_children(vec![
                op("pdf.from_images.a4", "Fit A4", "RectangleVertical"),
                op("pdf.from_images.original", "Original size", "Scaling"),
            ]));
        }
        ops
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>> {
        let action = parse_action(&op.id, options)?; // validate before touching any file
        match &action {
            Action::FromImages { .. } => check_inputs(inputs, &IMAGE_MIMES)?,
            _ => check_inputs(inputs, &[PDF])?,
        }
        match action {
            Action::Split => {
                let mut out = Vec::new();
                for input in inputs {
                    out.extend(split(input)?);
                }
                Ok(out)
            }
            Action::Extract(spec) => inputs.iter().map(|i| extract(i, &spec)).collect(),
            Action::Rotate(degrees) => inputs.iter().map(|i| rotate(i, degrees)).collect(),
            Action::Merge => Ok(vec![merge(inputs)?]),
            Action::FromImages { a4 } => Ok(vec![from_images(inputs, a4)?]),
            Action::Render { format, dpi } => {
                let library = self.pdfium.as_deref().ok_or_else(|| EngineError::ToolMissing("pdfium".into()))?;
                let mut out = Vec::new();
                for input in inputs {
                    out.extend(render(library, input, format, dpi)?);
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn run_with(c: &PdfConverter, id: &str, inputs: &[PathBuf], options: &[(&str, &str)]) -> Result<Vec<PathBuf>> {
        let opts: Options = options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        c.run(&Operation::new(id, id, "x"), inputs, &opts)
    }

    fn run(id: &str, inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        run_with(&PdfConverter { pdfium: None }, id, inputs, &[])
    }

    /// A PDF whose pages say `{label}1`, `{label}2`, ... and draw a red square at the bottom
    /// left. Like many real files, the page tree is nested and MediaBox (300x200), the font
    /// resources and `rotate` are inherited from the root, not set on the pages.
    fn make_pdf(path: &Path, label: &str, pages: u32, rotate: i64) -> PathBuf {
        let mut doc = Document::with_version("1.5");
        let root_id = doc.new_object_id();
        let mid_id = doc.new_object_id();
        let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let mut kids = Vec::new();
        for i in 1..=pages {
            let content = format!("1 0 0 rg 0 0 20 20 re f BT /F1 12 Tf 40 100 Td ({label}{i}) Tj ET");
            let contents = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => mid_id, "Contents" => contents });
            kids.push(Object::Reference(page));
        }
        let count = i64::from(pages);
        doc.objects.insert(mid_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Parent" => root_id, "Kids" => kids, "Count" => count }));
        doc.objects.insert(root_id, Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(mid_id)],
            "Count" => count,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
            "Rotate" => rotate,
        }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root_id });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
        path.to_path_buf()
    }

    /// The label each page shows, in page order.
    fn labels(path: &Path) -> Vec<String> {
        let doc = Document::load(path).unwrap();
        doc.get_pages()
            .into_values()
            .map(|p| {
                let content = String::from_utf8_lossy(&doc.get_page_content(p)).into_owned();
                let start = content.find('(').unwrap() + 1;
                content[start..start + content[start..].find(')').unwrap()].to_string()
            })
            .collect()
    }

    /// `(width, height, rotate)` of each page, resolving inheritance.
    fn geometry(path: &Path) -> Vec<(f32, f32, i64)> {
        let doc = Document::load(path).unwrap();
        doc.get_pages()
            .into_values()
            .map(|p| {
                let mb = inherited(&doc, p, b"MediaBox").unwrap();
                let mb: Vec<f32> = mb.as_array().unwrap().iter().map(|o| o.as_float().unwrap()).collect();
                let rot = inherited(&doc, p, b"Rotate").map_or(0, |o| o.as_i64().unwrap());
                (mb[2] - mb[0], mb[3] - mb[1], rot)
            })
            .collect()
    }

    // ---- parsing ----------------------------------------------------------------------

    #[test]
    fn page_ranges() {
        assert_eq!(parse_pages("1", 5).unwrap(), [1]);
        assert_eq!(parse_pages("1-3", 5).unwrap(), [1, 2, 3]);
        assert_eq!(parse_pages("1-3", 2).unwrap(), [1, 2], "clipped to the document");
        assert_eq!(parse_pages("3-", 5).unwrap(), [3, 4, 5]);
        assert_eq!(parse_pages("-2", 5).unwrap(), [1, 2]);
        assert_eq!(parse_pages(" 4, 1-2 ,2 ", 5).unwrap(), [1, 2, 4], "sorted, deduplicated");
        assert_eq!(parse_pages("odd", 5).unwrap(), [1, 3, 5]);
        assert_eq!(parse_pages("EVEN", 5).unwrap(), [2, 4]);
        assert_eq!(parse_pages("9", 2).unwrap_err().to_string(), "invalid option: page 9 doesn't exist (the PDF has 2 pages)");
        assert_eq!(parse_pages("even", 1).unwrap_err().to_string(), "invalid option: `even` selects no pages");
        for bad in ["", "0", "3-1", "a", "1-x", "1,,2", "-"] {
            assert!(parse_pages(bad, 5).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn actions_from_ids_and_options() {
        let none = Options::new();
        let with = |k: &str, v: &str| Options::from([(k.to_string(), v.to_string())]);
        assert_eq!(parse_action("pdf.extract.1-3", &none).unwrap(), Action::Extract("1-3".into()));
        assert_eq!(parse_action("pdf.extract", &with("pages", "2,4")).unwrap(), Action::Extract("2,4".into()));
        assert_eq!(parse_action("pdf.rotate.270", &none).unwrap(), Action::Rotate(270));
        assert_eq!(parse_action("pdf.rotate.90", &with("degrees", "180")).unwrap(), Action::Rotate(180));
        assert_eq!(parse_action("pdf.to_jpg.150", &none).unwrap(), Action::Render { format: ImageFormat::Jpeg, dpi: 150 });
        assert_eq!(parse_action("pdf.from_images.original", &none).unwrap(), Action::FromImages { a4: false });
        for (id, opts) in [
            ("pdf.rotate.45", none.clone()),
            ("pdf.rotate", none.clone()),
            ("pdf.to_png.0", none.clone()),
            ("pdf.to_png", with("dpi", "5000")),
            ("pdf.from_images.letter", none.clone()),
        ] {
            assert!(matches!(parse_action(id, &opts), Err(EngineError::InvalidOption(_))), "{id}");
        }
        assert!(matches!(parse_action("image.rotate.90", &none), Err(EngineError::UnknownOperation(_))));
    }

    // ---- page operations --------------------------------------------------------------

    #[test]
    fn split_writes_one_pdf_per_page_keeping_inherited_attributes() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("doc.pdf"), "P", 3, 90);
        let out = run("pdf.split", &[input]).unwrap();
        let names: Vec<_> = (1..=3).map(|i| d.path().join(format!("doc (page {i}).pdf"))).collect();
        assert_eq!(out, names);
        for (i, path) in out.iter().enumerate() {
            assert_eq!(labels(path), [format!("P{}", i + 1)]);
            assert_eq!(geometry(path), [(300.0, 200.0, 90)]);
            let doc = Document::load(path).unwrap();
            let page = doc.get_pages()[&1];
            let fonts = doc.get_dictionary(page).unwrap().get(b"Resources").unwrap().as_dict().unwrap();
            assert!(fonts.get(b"Font").unwrap().as_dict().unwrap().has(b"F1"), "font resources carried over");
        }
    }

    #[test]
    fn extract_ranges_and_never_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("five.pdf"), "P", 5, 0);
        let one = |id: &str, opts: &[(&str, &str)]| {
            let out = run_with(&PdfConverter { pdfium: None }, id, std::slice::from_ref(&input), opts).unwrap();
            labels(&out[0])
        };
        assert_eq!(one("pdf.extract.1", &[]), ["P1"]);
        assert_eq!(one("pdf.extract.1-3", &[]), ["P1", "P2", "P3"]);
        assert_eq!(one("pdf.extract.odd", &[]), ["P1", "P3", "P5"]);
        assert_eq!(one("pdf.extract.even", &[]), ["P2", "P4"]);
        assert_eq!(one("pdf.extract", &[("pages", "5,2")]), ["P2", "P5"]);
        assert!(d.path().join("five (extracted 5).pdf").is_file(), "five runs, five distinct files");
        assert_eq!(labels(&d.path().join("five (extracted).pdf")), ["P1"], "the first output is untouched");

        let err = run("pdf.extract.9", &[input]).unwrap_err();
        assert!(matches!(err, EngineError::InvalidOption(_)), "{err}");
    }

    #[test]
    fn rotate_adds_to_the_current_rotation() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("r.pdf"), "P", 2, 90);
        let out = run("pdf.rotate.90", std::slice::from_ref(&input)).unwrap();
        assert_eq!(out, [d.path().join("r (rotated).pdf")]);
        assert_eq!(geometry(&out[0]), [(300.0, 200.0, 180), (300.0, 200.0, 180)]);
        let out = run("pdf.rotate.270", std::slice::from_ref(&input)).unwrap();
        assert_eq!(geometry(&out[0])[0].2, 0, "90 + 270 wraps to 0");
        assert_eq!(labels(&out[0]), ["P1", "P2"]);
        assert_eq!(geometry(&input)[0].2, 90, "the original is untouched");
    }

    #[test]
    fn merge_keeps_selection_order() {
        let d = tempfile::tempdir().unwrap();
        let a = make_pdf(&d.path().join("a.pdf"), "A", 2, 0);
        let b = make_pdf(&d.path().join("b.pdf"), "B", 1, 90);
        let out = run("pdf.merge", &[a.clone(), b.clone()]).unwrap();
        assert_eq!(out, [d.path().join("a (merged).pdf")]);
        assert_eq!(labels(&out[0]), ["A1", "A2", "B1"]);
        assert_eq!(geometry(&out[0]), [(300.0, 200.0, 0), (300.0, 200.0, 0), (300.0, 200.0, 90)]);

        let out = run("pdf.merge", &[b.clone(), a.clone(), b.clone()]).unwrap();
        assert_eq!(out, [d.path().join("b (merged).pdf")]);
        assert_eq!(labels(&out[0]), ["B1", "A1", "A2", "B1"]);

        let err = run("pdf.merge", &[a]).unwrap_err();
        assert!(matches!(err, EngineError::InvalidOption(_)));
    }

    // ---- images -> PDF ----------------------------------------------------------------

    fn xobject(doc: &Document, page: ObjectId) -> &Stream {
        let res = doc.get_dictionary(page).unwrap().get(b"Resources").unwrap().as_dict().unwrap();
        let id = res.get(b"XObject").unwrap().as_dict().unwrap().get(b"Im0").unwrap().as_reference().unwrap();
        doc.get_object(id).unwrap().as_stream().unwrap()
    }

    #[test]
    fn images_to_pdf_one_page_each() {
        let d = tempfile::tempdir().unwrap();
        let png = d.path().join("wide.png");
        RgbaImage::from_pixel(40, 20, Rgba([0, 128, 255, 100])).save(&png).unwrap();
        let jpg = d.path().join("tall.jpg");
        RgbImage::from_pixel(30, 60, Rgb([200, 10, 10])).save(&jpg).unwrap();
        let gray = d.path().join("gray.jpg");
        image::GrayImage::from_pixel(8, 8, image::Luma([90])).save(&gray).unwrap();

        let out = run("pdf.from_images.original", &[png.clone(), jpg.clone(), gray.clone()]).unwrap();
        assert_eq!(out, [d.path().join("wide (combined).pdf")]);
        assert_eq!(geometry(&out[0]), [(30.0, 15.0, 0), (22.5, 45.0, 0), (6.0, 6.0, 0)], "96 DPI");

        let doc = Document::load(&out[0]).unwrap();
        let pages = doc.get_pages();
        let png_x = xobject(&doc, pages[&1]);
        assert!(png_x.dict.has(b"SMask"), "alpha becomes a soft mask");
        let jpg_x = xobject(&doc, pages[&2]);
        assert_eq!(jpg_x.dict.get(b"Filter").unwrap().as_name().unwrap(), b"DCTDecode");
        assert_eq!(jpg_x.content, std::fs::read(&jpg).unwrap(), "JPEG embedded without re-encoding");
        assert_eq!(xobject(&doc, pages[&3]).dict.get(b"ColorSpace").unwrap().as_name().unwrap(), b"DeviceGray");

        let a4 = run("pdf.from_images.a4", &[png, jpg]).unwrap();
        assert_eq!(a4, [d.path().join("wide (combined 2).pdf")]);
        assert_eq!(geometry(&a4[0]), [(841.89, 595.28, 0), (595.28, 841.89, 0)], "orientation follows the image");

        let single = run("pdf.from_images.a4", &[gray]).unwrap();
        assert_eq!(single, [d.path().join("gray (converted).pdf")]);
    }

    #[test]
    fn a4_layout_shrinks_but_never_enlarges() {
        // Small: natural size, centred.
        let (pw, ph, x, y, w, h) = layout(40, 80, true);
        assert_eq!((pw, ph, w, h), (A4.0, A4.1, 30.0, 60.0));
        assert!((x - (A4.0 - 30.0) / 2.0).abs() < 0.01 && (y - (A4.1 - 60.0) / 2.0).abs() < 0.01);
        // Large landscape: fits inside the margins of a landscape page, aspect kept.
        let (pw, ph, x, y, w, h) = layout(4000, 2000, true);
        assert_eq!((pw, ph), (A4.1, A4.0));
        assert!((w - (pw - 2.0 * A4_MARGIN)).abs() < 0.01, "{w}");
        assert!((w / h - 2.0).abs() < 0.001 && x >= A4_MARGIN - 0.01 && y >= A4_MARGIN - 0.01);
    }

    #[test]
    fn jpeg_component_count() {
        let sof = |n: u8| [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC2, 0x00, 0x11, 8, 0, 16, 0, 16, n];
        assert_eq!(jpeg_components(&sof(4)), Some(4));
        assert_eq!(jpeg_components(&sof(1)), Some(1));
        assert_eq!(jpeg_components(&[0xFF, 0xD8, 0x00]), None);
    }

    // ---- errors and listing -----------------------------------------------------------

    #[test]
    fn bad_inputs_are_errors_not_panics() {
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("fake.pdf");
        std::fs::write(&fake, b"%PDF-1.7 but nothing else").unwrap();
        assert!(matches!(run("pdf.split", &[fake]), Err(EngineError::Pdf(_))));
        let png = d.path().join("p.png");
        RgbImage::new(2, 2).save(&png).unwrap();
        let pdf = make_pdf(&d.path().join("ok.pdf"), "P", 1, 0);
        assert!(matches!(run("pdf.rotate.90", &[png.clone()]), Err(EngineError::UnsupportedFormat(_))));
        assert!(matches!(run("pdf.from_images.a4", &[png, pdf.clone()]), Err(EngineError::UnsupportedFormat(_))));
        assert!(matches!(run("pdf.split", &[d.path().join("missing.pdf")]), Err(EngineError::InputNotFound(_))));
        assert!(matches!(run("pdf.to_png.72", &[pdf]), Err(EngineError::ToolMissing(t)) if t == "pdfium"));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 3, "no outputs written");
    }

    #[test]
    fn operations_and_batch_operations() {
        let without = PdfConverter { pdfium: None };
        let ids = |ops: Vec<Operation>| ops.into_iter().map(|o| (o.id, o.available)).collect::<Vec<_>>();
        assert_eq!(
            ids(without.operations(PDF)),
            [
                ("pdf.split".into(), true),
                ("pdf.extract".into(), true),
                ("pdf.rotate".into(), true),
                ("pdf.to_image".into(), false),
            ]
        );
        let exe = std::env::current_exe().unwrap();
        let with = PdfConverter::new(&EngineConfig { pdfium_path: Some(exe), ..EngineConfig::default() });
        assert!(with.operations(PDF).iter().all(|o| o.available));
        assert!(without.operations("image/png").is_empty());

        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let batch = |v: &[&str]| without.batch_operations(&s(v)).into_iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(batch(&[PDF, PDF]), ["pdf.merge"]);
        assert!(batch(&[PDF]).is_empty());
        assert_eq!(batch(&["image/png"]), ["pdf.from_images"]);
        assert_eq!(batch(&["image/png", "image/webp"]), ["pdf.from_images"]);
        let kids: Vec<_> = without.operations(PDF).pop().unwrap().children.into_iter().map(|o| o.id).collect();
        assert_eq!(kids, ["pdf.to_png.72", "pdf.to_png.150", "pdf.to_png.300", "pdf.to_jpg.72", "pdf.to_jpg.150", "pdf.to_jpg.300"]);
        assert!(batch(&[PDF, "image/png"]).is_empty());
    }

    // ---- rendering (skipped without pdfium) -------------------------------------------

    /// pdfium fetched by scripts/fetch-pdfium.sh, if present.
    fn renderer() -> Option<PdfConverter> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/resources/pdfium");
        let c = PdfConverter::new(&EngineConfig::locate(&[dir]));
        if c.pdfium.is_none() {
            eprintln!("skipping: pdfium not found (run scripts/fetch-pdfium.sh)");
            return None;
        }
        Some(c)
    }

    #[test]
    fn renders_pages_at_the_chosen_dpi() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("deck.pdf"), "P", 2, 0);
        let out = run_with(&c, "pdf.to_png.72", std::slice::from_ref(&input), &[]).unwrap();
        assert_eq!(out, [d.path().join("deck (page 1).png"), d.path().join("deck (page 2).png")]);
        let img = image::open(&out[0]).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (300, 200));
        assert_eq!(img.get_pixel(5, 195), &Rgb([255, 0, 0]), "the red square, bottom left");
        assert_eq!(img.get_pixel(150, 20), &Rgb([255, 255, 255]), "white background");

        let out = run_with(&c, "pdf.to_jpg.150", std::slice::from_ref(&input), &[]).unwrap();
        let (w, h) = image::image_dimensions(&out[1]).unwrap();
        assert_eq!(w, 625);
        assert!((416..=417).contains(&h), "{h}");
        assert_eq!(ImageFormat::from_path(&out[1]).unwrap(), ImageFormat::Jpeg);
    }

    #[test]
    fn renders_rotation_and_names_single_pages_converted() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("one.pdf"), "P", 1, 90);
        let out = run_with(&c, "pdf.to_png", &[input], &[("dpi", "36")]).unwrap();
        assert_eq!(out, [d.path().join("one (converted).png")]);
        assert_eq!(image::image_dimensions(&out[0]).unwrap(), (100, 150), "rotated 90: portrait, half size");
    }

    #[test]
    fn render_errors_are_readable() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("broken.pdf");
        std::fs::write(&fake, b"%PDF-1.7 nope").unwrap();
        let err = run_with(&c, "pdf.to_png.72", &[fake], &[]).unwrap_err();
        assert!(matches!(err, EngineError::Pdfium(_)), "{err}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
