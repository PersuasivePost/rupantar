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
//! - `pdf.to_pptx` (pages rendered at 150 DPI, one per slide, deck built by pandoc; slides take
//!   the first page's aspect ratio) and `pdf.to_docx` ("To Word (text)": text structure rebuilt
//!   from the layout, then pandoc). Both need pdfium and pandoc.
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
    catalog, write_siblings, Converter, EngineConfig, EngineError, Operation, Options, Result,
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
    Compress(Preset),
    PageNumbers(Corner),
    Watermark(String),
    Text { markdown: bool },
    /// pdfium + pandoc.
    ToPptx,
    ToDocx,
}

/// Image recompression for `pdf.compress`: images larger than `max_side` px are downsampled,
/// JPEGs re-encoded at `quality`; with `flate_to_jpeg`, lossless colour/grey images become JPEG.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Preset {
    pub(crate) max_side: u32,
    pub(crate) quality: u8,
    pub(crate) flate_to_jpeg: bool,
}

/// `(id suffix, label, icon, preset)`; DOCX compress (`office`) uses the same levels.
pub(crate) const PRESETS: [(&str, &str, &str, Preset); 3] = [
    ("light", "Light", "Feather", Preset { max_side: 2400, quality: 85, flate_to_jpeg: false }),
    ("balanced", "Balanced", "Gauge", Preset { max_side: 1600, quality: 70, flate_to_jpeg: true }),
    ("strong", "Strong", "Minimize2", Preset { max_side: 1100, quality: 50, flate_to_jpeg: true }),
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Corner {
    BottomCenter,
    BottomRight,
    TopRight,
}

const CORNERS: [(&str, &str, &str, Corner); 3] = [
    ("bottom_center", "Bottom center", "ArrowDown", Corner::BottomCenter),
    ("bottom_right", "Bottom right", "ArrowDownRight", Corner::BottomRight),
    ("top_right", "Top right", "ArrowUpRight", Corner::TopRight),
];

/// `(id suffix, label, text)` of the watermark presets; option `text` sets any other.
const WATERMARKS: [(&str, &str, &str); 3] =
    [("confidential", "Confidential", "CONFIDENTIAL"), ("draft", "Draft", "DRAFT"), ("copy", "Copy", "COPY")];

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
        ["pdf", "compress", rest @ ..] => {
            let v = value(options, "level", rest)?;
            PRESETS.iter().find(|p| p.0 == v).map(|p| Action::Compress(p.3)).ok_or_else(|| invalid("level", &v))
        }
        ["pdf", "page_numbers", rest @ ..] => {
            let v = value(options, "position", rest)?;
            CORNERS.iter().find(|c| c.0 == v).map(|c| Action::PageNumbers(c.3)).ok_or_else(|| invalid("position", &v))
        }
        ["pdf", "watermark", rest @ ..] => {
            let text = match options.get("text") {
                Some(t) => t.trim().to_string(),
                None => {
                    let v = value(options, "text", rest)?;
                    WATERMARKS.iter().find(|w| w.0 == v).map(|w| w.2.to_string()).ok_or_else(|| invalid("text", &v))?
                }
            };
            if text.is_empty() || text.chars().count() > 60 || win_ansi(&text).is_none() {
                return Err(EngineError::InvalidOption(
                    "watermark text must be 1-60 Latin characters (the built-in PDF fonts have no others)".into(),
                ));
            }
            Ok(Action::Watermark(text))
        }
        ["pdf", "to_pptx"] => Ok(Action::ToPptx),
        ["pdf", "to_docx"] => Ok(Action::ToDocx),
        ["pdf", "to_text", rest @ ..] => match value(options, "format", rest)?.as_ref() {
            "txt" => Ok(Action::Text { markdown: false }),
            "md" => Ok(Action::Text { markdown: true }),
            other => Err(invalid("format", other)),
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
pub(crate) fn jpeg_components(b: &[u8]) -> Option<u8> {
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

// ---- compress --------------------------------------------------------------------------

/// Upper bound for decoding one stream while compressing (guards against decompression bombs).
const MAX_STREAM_BYTES: usize = 256 << 20;
/// Images above this many pixels are left alone rather than decoded.
const MAX_IMAGE_PIXELS: u64 = 60_000_000;

/// Filter names of a stream, in order (`[]` for none); `None` if `/Filter` is malformed.
fn filters(stream: &Stream) -> Option<Vec<Vec<u8>>> {
    match stream.dict.get(b"Filter") {
        Err(_) => Some(Vec::new()),
        Ok(Object::Name(n)) => Some(vec![n.clone()]),
        Ok(Object::Array(a)) => a.iter().map(|o| o.as_name().ok().map(<[u8]>::to_vec)).collect(),
        Ok(_) => None,
    }
}

/// Colour components of an image colour space we can re-encode (grey or RGB-like), else `None`.
fn components(doc: &Document, cs: &Object) -> Option<u8> {
    match cs {
        Object::Name(n) if n == b"DeviceGray" => Some(1),
        Object::Name(n) if n == b"DeviceRGB" => Some(3),
        Object::Reference(id) => match doc.get_object(*id).ok()? {
            Object::Reference(_) => None, // no chains
            other => components(doc, other),
        },
        Object::Array(a) => match a.first()?.as_name().ok()? {
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            b"ICCBased" => {
                let n = doc.get_object(a.get(1)?.as_reference().ok()?).ok()?.as_stream().ok()?.dict.get(b"N").ok()?.as_i64().ok()?;
                matches!(n, 1 | 3).then_some(n as u8)
            }
            _ => None,
        },
        _ => None,
    }
}

/// A smaller replacement for an image XObject, if the preset can make one: downsampled past
/// `max_side`, JPEGs re-encoded, lossless images turned into JPEG when the preset allows.
/// Anything unusual (masks, 16-bit, Decode arrays, CMYK, palettes) is left alone.
fn smaller_image(doc: &Document, stream: &Stream, preset: Preset) -> Option<Stream> {
    let d = &stream.dict;
    if d.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false) || d.has(b"Decode") {
        return None;
    }
    let (w, h) = (u32::try_from(d.get(b"Width").ok()?.as_i64().ok()?).ok()?, u32::try_from(d.get(b"Height").ok()?.as_i64().ok()?).ok()?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS || d.get(b"BitsPerComponent").ok()?.as_i64().ok()? != 8 {
        return None;
    }
    let comps = components(doc, d.get(b"ColorSpace").ok()?)?;
    let filters = filters(stream)?;
    let is_jpeg = filters == [b"DCTDecode".to_vec()];
    let img: DynamicImage = if is_jpeg {
        if d.has(b"DecodeParms") || jpeg_components(&stream.content) != Some(comps) {
            return None;
        }
        image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg).ok()?
    } else if filters.is_empty() || filters == [b"FlateDecode".to_vec()] {
        let expected = w as usize * h as usize * usize::from(comps);
        let mut raw = stream.decompressed_content_with_limit(expected + 1024).ok()?;
        raw.truncate(expected);
        if comps == 1 {
            DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, raw)?)
        } else {
            DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, raw)?)
        }
    } else {
        return None;
    };

    let resize = w.max(h) > preset.max_side;
    let img = if resize { img.resize(preset.max_side, preset.max_side, image::imageops::FilterType::Lanczos3) } else { img };
    let to_jpeg = is_jpeg || preset.flate_to_jpeg;
    if !resize && !to_jpeg {
        return None; // a lossless image at its size: only the stream recompression applies
    }
    let mut out = stream.clone();
    out.dict.set("Width", i64::from(img.width()));
    out.dict.set("Height", i64::from(img.height()));
    if to_jpeg {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, preset.quality);
        if comps == 1 {
            let g = img.to_luma8();
            encoder.encode(g.as_raw(), g.width(), g.height(), image::ExtendedColorType::L8).ok()?;
        } else {
            let c = img.to_rgb8();
            encoder.encode(c.as_raw(), c.width(), c.height(), image::ExtendedColorType::Rgb8).ok()?;
        }
        out.set_plain_content(bytes);
        out.dict.set("Filter", "DCTDecode");
    } else {
        let raw = if comps == 1 { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() };
        out.set_plain_content(raw);
        out.compress().ok()?;
    }
    (out.content.len() < stream.content.len()).then_some(out)
}

/// A Flate stream recompressed at the best level, if that is smaller. Streams with other
/// filters or predictors are left as they are.
fn smaller_flate(stream: &Stream) -> Option<Stream> {
    if filters(stream)? != [b"FlateDecode".to_vec()] || stream.dict.has(b"DecodeParms") || !stream.allows_compression {
        return None;
    }
    let mut out = stream.clone();
    out.set_plain_content(stream.decompressed_content_with_limit(MAX_STREAM_BYTES).ok()?);
    out.compress().ok()?;
    (out.content.len() < stream.content.len()).then_some(out)
}

/// Recompresses streams and images, drops unreachable objects and writes with object streams.
/// The result is never larger than the input: if nothing could be saved, the copy is identical.
fn compress(input: &Path, preset: Preset) -> Result<PathBuf> {
    let mut doc = load(input)?;
    let original = std::fs::read(input)?;
    // Soft masks and stencil masks stay lossless.
    let masks: BTreeSet<ObjectId> = doc
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .flat_map(|s| [b"SMask".as_slice(), b"Mask"].map(|k| s.dict.get(k).and_then(Object::as_reference).ok()))
        .flatten()
        .collect();
    let mut replacements = Vec::new();
    for (id, obj) in &doc.objects {
        let Object::Stream(stream) = obj else { continue };
        let is_image = stream.dict.get(b"Subtype").and_then(Object::as_name).is_ok_and(|n| n == b"Image");
        let smaller = if is_image && !masks.contains(id) { smaller_image(&doc, stream, preset) } else { None };
        if let Some(s) = smaller.or_else(|| smaller_flate(stream)) {
            replacements.push((*id, s));
        }
    }
    for (id, s) in replacements {
        doc.objects.insert(id, Object::Stream(s));
    }
    doc.compress(); // streams stored without any filter
    doc.prune_objects();
    if doc.version.as_str() < "1.5" {
        doc.version = "1.5".into(); // object streams
    }
    let mut bytes = Vec::new();
    doc.save_with_options(&mut bytes, lopdf::SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build())?;
    let bytes = if bytes.len() < original.len() { bytes } else { original };
    Ok(write_siblings(input, "pdf", vec![("compressed".into(), bytes)])?.remove(0))
}

// ---- stamping: page numbers, watermark --------------------------------------------------

/// Helvetica-Bold advance widths (1/1000 em) for ASCII 32..=126; Latin-1 letters use 611.
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556,
    556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833,
    722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611,
    556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389,
    280, 389, 584,
];

/// WinAnsi bytes for text in the printable Latin-1 range (what the standard fonts can show).
fn win_ansi(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| {
            let u = u32::from(c);
            ((0x20..=0x7E).contains(&u) || (0xA0..=0xFF).contains(&u)).then_some(u as u8)
        })
        .collect()
}

/// Width of WinAnsi `bytes` in Helvetica-Bold at size 1. Digits are 0.556 in every Helvetica.
fn text_width(bytes: &[u8]) -> f32 {
    bytes.iter().map(|&b| f32::from(HELVETICA_BOLD.get(usize::from(b).wrapping_sub(32)).copied().unwrap_or(611)) / 1000.0).sum()
}

/// A PDF literal string `(…)` for raw bytes.
fn literal(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![b'('];
    for &b in bytes {
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

/// Adds `value` to the page's `category` resources (e.g. `Font`) and returns its name. Reuses
/// the name if the same value is already there (pages often share one resource dictionary).
fn add_resource(doc: &mut Document, page: ObjectId, category: &[u8], prefix: &str, value: Object) -> Result<String> {
    let res_ref = match doc.get_dictionary(page)?.get(b"Resources") {
        Ok(Object::Reference(id)) => Some(*id),
        Ok(Object::Dictionary(_)) => None,
        _ => {
            doc.get_object_mut(page)?.as_dict_mut()?.set("Resources", lopdf::Dictionary::new());
            None
        }
    };
    let resources = match res_ref {
        Some(id) => doc.get_dictionary(id)?,
        None => doc.get_dictionary(page)?.get(b"Resources")?.as_dict()?,
    };
    let cat_ref = resources.get(category).and_then(Object::as_reference).ok();
    let existing: Vec<(Vec<u8>, Object)> = match cat_ref {
        Some(id) => doc.get_dictionary(id)?.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        None => resources.get(category).and_then(Object::as_dict).map(|d| d.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
    };
    if let Some((k, _)) = existing.iter().find(|(_, v)| *v == value) {
        return Ok(String::from_utf8_lossy(k).into_owned());
    }
    let name = (1..).map(|n| format!("{prefix}{n}")).find(|n| !existing.iter().any(|(k, _)| k == n.as_bytes())).unwrap_or_default();
    let target = match (cat_ref, res_ref) {
        (Some(id), _) => doc.get_object_mut(id)?.as_dict_mut()?,
        (None, res) => {
            let resources = match res {
                Some(id) => doc.get_object_mut(id)?.as_dict_mut()?,
                None => doc.get_object_mut(page)?.as_dict_mut()?.get_mut(b"Resources")?.as_dict_mut()?,
            };
            if !resources.get(category).is_ok_and(|o| o.as_dict().is_ok()) {
                resources.set(category, lopdf::Dictionary::new());
            }
            resources.get_mut(category)?.as_dict_mut()?
        }
    };
    target.set(name.as_bytes(), value);
    Ok(name)
}

/// The page as shown: its visible box, and the matrix from "display" coordinates (origin at the
/// bottom left of the page as a viewer shows it, after `/Rotate`) to the page's own space.
fn display_space(doc: &Document, page: ObjectId) -> ([f32; 2], [f32; 6]) {
    let rect = ["CropBox", "MediaBox"]
        .iter()
        .filter_map(|k| inherited(doc, page, k.as_bytes()))
        .find_map(|o| {
            let v: Vec<f32> = o.as_array().ok()?.iter().filter_map(|n| n.as_float().ok()).collect();
            (v.len() == 4).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
        })
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let [llx, lly, urx, ury] = rect;
    let (w, h) = (urx - llx, ury - lly);
    match inherited(doc, page, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0).rem_euclid(360) {
        90 => ([h, w], [0.0, 1.0, -1.0, 0.0, urx, lly]),
        180 => ([w, h], [-1.0, 0.0, 0.0, -1.0, urx, ury]),
        270 => ([h, w], [0.0, -1.0, 1.0, 0.0, llx, ury]),
        _ => ([w, h], [1.0, 0.0, 0.0, 1.0, llx, lly]),
    }
}

/// Draws over every page: `draw(page number, page count, display size, resource names)` returns
/// content in display coordinates. The original content is wrapped in q/Q so its graphics state
/// can't leak into ours. `resources` are `(category, name prefix, object)`.
fn stamp(
    input: &Path,
    tag: &str,
    resources: impl Fn(&mut Document) -> Vec<(&'static [u8], &'static str, Object)>,
    draw: impl Fn(u32, u32, [f32; 2], &[String]) -> Vec<u8>,
) -> Result<PathBuf> {
    let mut doc = load(input)?;
    flatten_inherited(&mut doc)?;
    let wanted = resources(&mut doc);
    let save_state = doc.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
    let pages = doc.get_pages();
    let count = pages.len() as u32;
    for (number, page) in pages {
        let names = wanted
            .iter()
            .map(|(cat, prefix, obj)| add_resource(&mut doc, page, cat, prefix, obj.clone()))
            .collect::<Result<Vec<_>>>()?;
        let (size, m) = display_space(&doc, page);
        let mut ops = format!("\nQ q {} {} {} {} {} {} cm\n", m[0], m[1], m[2], m[3], m[4], m[5]).into_bytes();
        ops.extend(draw(number, count, size, &names));
        ops.extend(b"\nQ\n");
        let ours = doc.add_object(Stream::new(dictionary! {}, ops));
        let dict = doc.get_object_mut(page)?.as_dict_mut()?;
        let mut contents = vec![Object::Reference(save_state)];
        match dict.get(b"Contents") {
            Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
            Ok(other @ Object::Reference(_)) => contents.push(other.clone()),
            _ => {}
        }
        contents.push(Object::Reference(ours));
        dict.set("Contents", contents);
    }
    doc.compress();
    Ok(write_siblings(input, "pdf", vec![(tag.into(), save(&mut doc)?)])?.remove(0))
}

fn standard_font(doc: &mut Document, base: &str) -> Object {
    Object::Reference(doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => base, "Encoding" => "WinAnsiEncoding",
    }))
}

fn page_numbers(input: &Path, corner: Corner) -> Result<PathBuf> {
    stamp(
        input,
        "numbered",
        |doc| vec![(b"Font".as_slice(), "RpNum", standard_font(doc, "Helvetica"))],
        |n, _, [w, h], names| {
            let text = n.to_string();
            let size = (w.min(h) * 0.017).clamp(7.0, 11.0);
            let tw = text_width(text.as_bytes()) * size;
            let margin = size * 2.4;
            let (x, y) = match corner {
                Corner::BottomCenter => ((w - tw) / 2.0, margin),
                Corner::BottomRight => (w - margin - tw, margin),
                Corner::TopRight => (w - margin - tw, h - margin - size * 0.7),
            };
            format!("0.25 g BT /{} {size:.2} Tf {x:.2} {y:.2} Td ({text}) Tj ET", names[0]).into_bytes()
        },
    )
}

fn watermark(input: &Path, text: &str) -> Result<PathBuf> {
    let bytes = win_ansi(text).ok_or_else(|| EngineError::InvalidOption("watermark text must be Latin characters".into()))?;
    let width = text_width(&bytes);
    stamp(
        input,
        "watermarked",
        |doc| {
            let alpha = doc.add_object(dictionary! { "Type" => "ExtGState", "ca" => Object::Real(0.18), "CA" => Object::Real(0.18) });
            vec![
                (b"Font".as_slice(), "RpMark", standard_font(doc, "Helvetica-Bold")),
                (b"ExtGState".as_slice(), "RpAlpha", Object::Reference(alpha)),
            ]
        },
        |_, _, [w, h], names| {
            // Along the diagonal, as large as fits comfortably.
            let angle = h.atan2(w);
            let size = (0.6 * w.hypot(h) / width).min(w.min(h) * 0.3);
            let (sin, cos) = angle.sin_cos();
            let mut ops = format!(
                "/{gs} gs 0.5 g 1 0 0 1 {cx:.2} {cy:.2} cm {cos:.4} {sin:.4} {nsin:.4} {cos:.4} 0 0 cm BT /{font} {size:.2} Tf {x:.2} {y:.2} Td ",
                gs = names[1], font = names[0], cx = w / 2.0, cy = h / 2.0, nsin = -sin, x = -width * size / 2.0, y = -size * 0.35,
            )
            .into_bytes();
            ops.extend(literal(&bytes));
            ops.extend(b" Tj ET");
            ops
        },
    )
}

// ---- text (pdfium) ------------------------------------------------------------------------

/// A line that Markdown would read as syntax (heading, quote, list, rule, table, fence) gets a
/// backslash; leading spaces are dropped so text doesn't turn into a code block.
fn markdown_line(line: &str) -> String {
    let line = line.trim_start();
    let numbered = line.find(|c: char| !c.is_ascii_digit()).is_some_and(|i| i > 0 && line[i..].starts_with(['.', ')']));
    if numbered {
        let i = line.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
        return format!("{}\\{}", &line[..i], &line[i..]);
    }
    if line.starts_with(['#', '>', '-', '+', '*', '=', '|', '`', '~', '_']) {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

/// PDF -> `.txt` (pages separated by a form feed, like pdftotext) or `.md` (pages separated by
/// rules, lines escaped). A PDF without a text layer (scans) is an `Empty` error.
fn to_text(library: &Path, input: &Path, markdown: bool) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let pdfium = pdfium(library)?;
    let err = |e: pdfium_render::prelude::PdfiumError| EngineError::Pdfium(format!("{e:?}"));
    let doc = pdfium.load_pdf_from_file(input, None).map_err(err)?;
    let mut pages = Vec::new();
    for page in doc.pages().iter() {
        let text = clean_text(&page.text().map_err(err)?.all().replace("\r\n", "\n").replace('\r', "\n"));
        let lines: Vec<String> =
            text.lines().map(|l| if markdown { markdown_line(l.trim_end()) } else { l.trim_end().to_string() }).collect();
        pages.push(lines.join("\n").trim_matches('\n').to_string());
    }
    if pages.iter().all(|p| p.trim().is_empty()) {
        return Err(EngineError::Empty("the PDF has no text layer (scanned pages would need OCR)".into()));
    }
    let (text, ext) = if markdown { (pages.join("\n\n---\n\n"), "md") } else { (pages.join("\n\x0c\n"), "txt") };
    Ok(write_siblings(input, ext, vec![("converted".into(), (text + "\n").into_bytes())])?.remove(0))
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

fn pdfium_error(e: pdfium_render::prelude::PdfiumError) -> EngineError {
    EngineError::Pdfium(format!("{e:?}"))
}

/// Renders each page at `dpi` on white and hands it to `each(index, page count, image)`, one at
/// a time (a long document never has every page in memory).
pub(crate) fn render_pages(library: &Path, input: &Path, dpi: u16, mut each: impl FnMut(usize, usize, DynamicImage) -> Result<()>) -> Result<()> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let doc = pdfium(library)?.load_pdf_from_file(input, None).map_err(pdfium_error)?;
    let config = PdfRenderConfig::new()
        .scale_page_by_factor(f32::from(dpi) / 72.0)
        .set_maximum_width(MAX_RENDER_PX)
        .set_maximum_height(MAX_RENDER_PX)
        .set_clear_color(PdfColor::WHITE)
        .render_form_data(true);
    let pages = doc.pages();
    let count = pages.len() as usize;
    for (i, page) in pages.iter().enumerate() {
        each(i, count, page.render_with_config(&config).map_err(pdfium_error)?.as_image().map_err(pdfium_error)?)?;
    }
    Ok(())
}

fn render(library: &Path, input: &Path, format: ImageFormat, dpi: u16) -> Result<Vec<PathBuf>> {
    let ext = if format == ImageFormat::Png { "png" } else { "jpg" };
    let mut outputs = Vec::new();
    render_pages(library, input, dpi, |i, count, image| {
        let tag = if count == 1 { "converted".to_string() } else { format!("page {}", i + 1) };
        outputs.push((tag, crate::images::encode(&image, format, Some(90))?));
        Ok(())
    })?;
    write_siblings(input, ext, outputs)
}

// ---- PDF -> PowerPoint / Word (pdfium + pandoc) ------------------------------------------

/// Slides are 10 in on their long side (EMU: 914 400 per inch); PowerPoint allows 1-56 in.
const SLIDE_LONG_EMU: f64 = 9_144_000.0;
const SLIDE_DPI: u16 = 150;

/// Reserves `name (converted).ext` for a tool to write, runs `write(output)`, and removes the
/// reservation if that fails.
fn tool_output(input: &Path, ext: &str, write: impl FnOnce(&Path) -> Result<()>) -> Result<PathBuf> {
    let (file, output) = crate::create_unique_sibling(input, "converted", Some(ext))?;
    drop(file);
    match write(&output) {
        Ok(()) => Ok(output),
        Err(e) => {
            let _ = std::fs::remove_file(&output);
            Err(e)
        }
    }
}

/// `(cx, cy)` in EMU of a slide with the aspect ratio of a `w` x `h` page.
fn slide_size(w: u32, h: u32) -> (i64, i64) {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let (cx, cy) = if w >= h { (SLIDE_LONG_EMU, SLIDE_LONG_EMU * h / w) } else { (SLIDE_LONG_EMU * w / h, SLIDE_LONG_EMU) };
    (cx.max(914_400.0).round() as i64, cy.max(914_400.0).round() as i64)
}

/// Replaces the first `<tag … />` after `from` in `xml` with `with`; `None` if there is none.
fn replace_tag(xml: &str, from: usize, tag: &str, with: &str) -> Option<(String, usize)> {
    let start = from + xml[from..].find(&format!("<{tag} "))?;
    let end = start + xml[start..].find("/>")? + 2;
    Some((format!("{}{with}{}", &xml[..start], &xml[end..]), start + with.len()))
}

/// pandoc lays every page out at a modest size inside a 16:9 content area. This sizes the
/// slides to the first page's aspect ratio and makes each page's picture fill its slide
/// (letterboxed if a page has another shape). Left as pandoc wrote it if the XML is unexpected.
fn fill_slides(pptx: &[u8], pages: &[(u32, u32)]) -> Result<Vec<u8>> {
    use std::io::{Read, Write};
    let bad = |e: zip::result::ZipError| EngineError::Pdfium(format!("pandoc wrote an unreadable PowerPoint file: {e}"));
    let (cx, cy) = slide_size(pages.first().map_or(16, |p| p.0), pages.first().map_or(9, |p| p.1));
    let mut archive = zip::ZipArchive::new(Cursor::new(pptx)).map_err(bad)?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let name = archive.by_index_raw(i).map_err(bad)?.name().map_err(bad)?.into_owned();
        let slide = name.strip_prefix("ppt/slides/slide").and_then(|n| n.strip_suffix(".xml")).and_then(|n| n.parse::<usize>().ok());
        let edited = if name == "ppt/presentation.xml" || slide.is_some() {
            let mut xml = String::new();
            archive.by_index(i).map_err(bad)?.read_to_string(&mut xml)?;
            if name == "ppt/presentation.xml" {
                replace_tag(&xml, 0, "p:sldSz", &format!(r#"<p:sldSz cx="{cx}" cy="{cy}" />"#)).map(|r| r.0)
            } else {
                let (w, h) = pages.get(slide.unwrap_or(1).saturating_sub(1)).map_or((cx, cy), |p| (i64::from(p.0), i64::from(p.1)));
                let scale = (cx as f64 / w as f64).min(cy as f64 / h as f64);
                let (ew, eh) = ((w as f64 * scale).round() as i64, (h as f64 * scale).round() as i64);
                xml.find("<p:pic>")
                    .and_then(|pic| replace_tag(&xml, pic, "a:off", &format!(r#"<a:off x="{}" y="{}" />"#, (cx - ew) / 2, (cy - eh) / 2)))
                    .and_then(|(x, at)| replace_tag(&x, at, "a:ext", &format!(r#"<a:ext cx="{ew}" cy="{eh}" />"#)))
                    .map(|r| r.0)
            }
        } else {
            None
        };
        match edited {
            Some(xml) => {
                out.start_file(&name, zip::write::SimpleFileOptions::default()).map_err(bad)?;
                out.write_all(xml.as_bytes())?;
            }
            None => out.raw_copy_file(archive.by_index_raw(i).map_err(bad)?).map_err(bad)?,
        }
    }
    Ok(out.finish().map_err(bad)?.into_inner())
}

/// PDF -> PowerPoint: one slide per page, each page rendered at 150 DPI and filling its slide
/// (slides take the first page's aspect ratio). pandoc builds the deck.
fn to_pptx(library: &Path, pandoc: &Path, timeout: std::time::Duration, input: &Path) -> Result<PathBuf> {
    let input = std::path::absolute(input)?;
    let work = crate::documents::WorkDir::create()?;
    let mut sizes = Vec::new();
    let mut slides = Vec::new();
    render_pages(library, &input, SLIDE_DPI, |i, _, image| {
        let name = format!("page-{:04}.png", i + 1);
        image.save_with_format(work.0.join(&name), ImageFormat::Png)?;
        sizes.push((image.width(), image.height()));
        slides.push(format!("![]({name})"));
        Ok(())
    })?;
    std::fs::write(work.0.join("slides.md"), slides.join("\n\n---\n\n"))?;
    tool_output(&input, "pptx", |output| {
        let args: [&std::ffi::OsStr; 7] = [
            "--from".as_ref(), "markdown-implicit_figures".as_ref(), "--to".as_ref(), "pptx".as_ref(),
            "--output".as_ref(), output.as_os_str(), "slides.md".as_ref(),
        ];
        crate::tools::run("pandoc", pandoc, args, Some(&work.0), timeout)?;
        let filled = fill_slides(&std::fs::read(output)?, &sizes)?;
        Ok(std::fs::write(output, filled)?)
    })
}

/// A line of text as pdfium lays it out (PDF points, y up).
#[derive(Debug, Clone, PartialEq)]
struct Line {
    text: String,
    size: f32,
    bold: bool,
    top: f32,
    bottom: f32,
}

/// pdfium's text with its markers made readable: U+0002 is a hyphen that ended a line (pdfium
/// reports it apart from a typed hyphen); other control characters except tab/newline go.
fn clean_text(s: &str) -> String {
    s.chars().filter_map(|c| match c {
        '\u{2}' => Some('-'),
        '\t' | '\n' => Some(c),
        c if c.is_control() => None,
        c => Some(c),
    }).collect()
}

/// The lines of a page, top to bottom: pdfium's text segments joined when they share a line.
fn page_lines(page: &pdfium_render::prelude::PdfPage) -> Result<Vec<Line>> {
    use pdfium_render::prelude::PdfFontWeight as W;
    let text = page.text().map_err(pdfium_error)?;
    let mut runs = Vec::new();
    for segment in text.segments().iter() {
        let s = clean_text(&segment.text());
        if s.trim().is_empty() {
            continue;
        }
        let b = segment.bounds();
        let chars = segment.chars().map_err(pdfium_error)?;
        let first = chars.iter().find(|c| c.unicode_char().is_some_and(|c| !c.is_whitespace()));
        let size = first.as_ref().map_or(b.height().value, |c| c.scaled_font_size().value);
        let bold = first.as_ref().is_some_and(|c| {
            matches!(c.font_weight(), Some(W::Weight600 | W::Weight700Bold | W::Weight800 | W::Weight900))
                || matches!(c.font_weight(), Some(W::Custom(n)) if n >= 600)
                || c.font_name().to_ascii_lowercase().contains("bold")
        });
        runs.push((b.left().value, Line { text: s, size, bold, top: b.top().value, bottom: b.bottom().value }));
    }
    runs.sort_by(|a, b| b.1.top.total_cmp(&a.1.top).then(a.0.total_cmp(&b.0)));
    let mut lines: Vec<(f32, Line)> = Vec::new();
    for (left, run) in runs {
        let mid = (run.top + run.bottom) / 2.0;
        match lines.last_mut() {
            Some((_, line)) if mid <= line.top && mid >= line.bottom => {
                line.text.push(' ');
                line.text.push_str(run.text.trim());
                line.size = line.size.max(run.size);
                line.bold &= run.bold;
                line.bottom = line.bottom.min(run.bottom);
            }
            _ => lines.push((left, Line { text: run.text.trim().to_string(), ..run })),
        }
    }
    Ok(lines.into_iter().map(|(_, l)| l).collect())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Block {
    Heading(u8),
    Bullet,
    Numbered,
    Text,
}

/// `(numbered, rest)` if a line starts with a list marker (`•`, `-`, `1.`, `2)`, ...).
fn list_marker(text: &str) -> Option<(bool, &str)> {
    let t = text.trim_start();
    for b in ['•', '◦', '▪', '‣', '●', '-', '–', '*'] {
        if let Some(rest) = t.strip_prefix(b) {
            return rest.starts_with(char::is_whitespace).then(|| (false, rest.trim_start()));
        }
    }
    let digits = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    let rest = t.get(digits..)?;
    (1..=3).contains(&digits)
        .then(|| rest.strip_prefix(['.', ')']))
        .flatten()
        .filter(|r| r.starts_with(char::is_whitespace))
        .map(|r| (true, r.trim_start()))
}

/// A running page number: only digits (or "Page 3", "3 of 9", "3 / 9") near the top/bottom edge.
fn is_page_number(line: &Line, page_height: f32) -> bool {
    let near_edge = line.bottom < page_height * 0.08 || line.top > page_height * 0.92;
    let t = line.text.trim().to_ascii_lowercase();
    let t = t.strip_prefix("page").unwrap_or(&t).trim();
    let numeric = |s: &str| !s.is_empty() && s.trim().chars().all(|c| c.is_ascii_digit());
    let whole = numeric(t)
        || t.split_once(" of ").is_some_and(|(a, b)| numeric(a) && numeric(b))
        || t.split_once('/').is_some_and(|(a, b)| numeric(a) && numeric(b));
    near_edge && whole
}

/// Backslash-escapes every ASCII punctuation character, so text never reads as Markdown syntax.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Rebuilds document structure from laid-out lines (`(page height, lines)` per page):
/// - body size = the font size of most of the text; lines ≥ 1.6× / 1.3× / 1.12× of it are
///   headings 1/2/3, as is a lone short bold line;
/// - lines starting with a bullet or `1.` are list items (wrapped lines continue them);
/// - lines join into a paragraph while the gap between them stays under ~0.6 em and the size
///   doesn't change; a page break ends a paragraph; a word hyphenated across lines is rejoined;
/// - running page numbers at the page edges are dropped.
fn lines_to_markdown(pages: &[(f32, Vec<Line>)]) -> String {
    let mut sizes: Vec<(f32, usize)> = pages.iter().flat_map(|(_, ls)| ls.iter().map(|l| (l.size, l.text.chars().count()))).collect();
    sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: usize = sizes.iter().map(|s| s.1).sum();
    let mut seen = 0;
    let body = sizes.iter().find(|s| {
        seen += s.1;
        seen * 2 >= total
    });
    let body = body.map_or(12.0, |s| s.0).max(1.0);

    // (kind, text, line count, all bold)
    let mut blocks: Vec<(Block, String, usize, bool)> = Vec::new();
    for (height, lines) in pages {
        let mut prev: Option<&Line> = None;
        for line in lines.iter().filter(|l| !is_page_number(l, *height)) {
            let ratio = line.size / body;
            let marker = list_marker(&line.text);
            let kind = match (ratio, marker) {
                (r, _) if r >= 1.6 => Block::Heading(1),
                (r, _) if r >= 1.3 => Block::Heading(2),
                (r, _) if r >= 1.12 => Block::Heading(3),
                (_, Some((true, _))) => Block::Numbered,
                (_, Some((false, _))) => Block::Bullet,
                _ => Block::Text,
            };
            let text = marker.filter(|_| matches!(kind, Block::Bullet | Block::Numbered)).map_or(line.text.trim(), |m| m.1);
            let joins = prev.zip(blocks.last()).is_some_and(|(p, last)| {
                let close = p.bottom - line.top < 0.6 * p.size.max(line.size);
                let same_size = (p.size - line.size).abs() <= 0.1 * p.size.max(line.size);
                close && match (kind, last.0) {
                    (Block::Text, Block::Text | Block::Bullet | Block::Numbered) => same_size,
                    (Block::Heading(a), Block::Heading(b)) => a == b,
                    _ => false,
                }
            });
            match blocks.last_mut() {
                Some(last) if joins => {
                    let hyphenated = last.1.ends_with('-') && text.starts_with(char::is_lowercase)
                        && last.1.chars().rev().nth(1).is_some_and(char::is_alphabetic);
                    if hyphenated {
                        last.1.pop();
                    } else {
                        last.1.push(' ');
                    }
                    last.1.push_str(text);
                    last.2 += 1;
                    last.3 &= line.bold;
                }
                _ => blocks.push((kind, text.to_string(), 1, line.bold)),
            }
            prev = Some(line);
        }
    }

    let mut out = String::new();
    let mut last: Option<Block> = None;
    for (kind, text, lines, bold) in blocks {
        let lead_in = kind == Block::Text && lines == 1 && bold && text.chars().count() < 80 && !text.ends_with(['.', ',', ':', ';']);
        let kind = if lead_in { Block::Heading(3) } else { kind };
        let tight = matches!((last, kind), (Some(Block::Bullet), Block::Bullet) | (Some(Block::Numbered), Block::Numbered));
        if !out.is_empty() {
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        match kind {
            Block::Heading(n) => out.push_str(&format!("{} {}", "#".repeat(usize::from(n)), escape_markdown(&text))),
            Block::Bullet => out.push_str(&format!("- {}", escape_markdown(&text))),
            Block::Numbered => out.push_str(&format!("1. {}", escape_markdown(&text))),
            Block::Text => out.push_str(&escape_markdown(&text)),
        }
        last = Some(kind);
    }
    out + "\n"
}

/// PDF -> Word as editable text: structure rebuilt from the text layout (see
/// [`lines_to_markdown`]), then pandoc writes the DOCX. Images and tables are not carried over.
fn to_docx_text(library: &Path, pandoc: &Path, timeout: std::time::Duration, input: &Path) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let input = std::path::absolute(input)?;
    let doc = pdfium(library)?.load_pdf_from_file(&input, None).map_err(pdfium_error)?;
    let mut pages = Vec::new();
    for page in doc.pages().iter() {
        pages.push((page.height().value, page_lines(&page)?));
    }
    if pages.iter().all(|(_, lines)| lines.is_empty()) {
        return Err(EngineError::Empty("the PDF has no text layer (scanned pages would need OCR)".into()));
    }
    let work = crate::documents::WorkDir::create()?;
    std::fs::write(work.0.join("text.md"), lines_to_markdown(&pages))?;
    tool_output(&input, "docx", |output| {
        let args: [&std::ffi::OsStr; 7] = [
            "--from".as_ref(), "markdown".as_ref(), "--to".as_ref(), "docx".as_ref(),
            "--output".as_ref(), output.as_os_str(), "text.md".as_ref(),
        ];
        crate::tools::run("pandoc", pandoc, args, Some(&work.0), timeout).map(drop)
    })
}

// ---- converter ---------------------------------------------------------------------------

pub struct PdfConverter {
    pdfium: Option<PathBuf>,
    pandoc: Option<PathBuf>,
    timeout: std::time::Duration,
}

impl PdfConverter {
    pub fn new(config: &EngineConfig) -> Self {
        Self {
            pdfium: config.pdfium_path.clone().filter(|p| p.is_file()),
            pandoc: config.pandoc_path.clone().filter(|p| crate::tools::is_executable(p)),
            timeout: config.tool_timeout,
        }
    }

    /// pdfium and pandoc, for the conversions that render or read pages and let pandoc write.
    fn office_tools(&self) -> Result<(&Path, &Path)> {
        let pdfium = self.pdfium.as_deref().ok_or_else(|| EngineError::ToolMissing("pdfium".into()))?;
        let pandoc = self.pandoc.as_deref().ok_or_else(|| EngineError::ToolMissing("pandoc".into()))?;
        Ok((pdfium, pandoc))
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
    fn name(&self) -> &'static str {
        "PDF"
    }

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
            catalog::op("pdf.compress").with_children(PRESETS.iter().map(|(id, label, icon, _)| op(&format!("pdf.compress.{id}"), label, icon)).collect()),
            catalog::op("pdf.page_numbers").with_children(
                CORNERS.iter().map(|(id, label, icon, _)| op(&format!("pdf.page_numbers.{id}"), label, icon)).collect(),
            ),
            catalog::op("pdf.watermark")
                .with_children(WATERMARKS.iter().map(|(id, label, _)| op(&format!("pdf.watermark.{id}"), label, "Stamp")).collect()),
            catalog::op("pdf.to_text")
                .with_children(vec![op("pdf.to_text.txt", "Plain text", "Text"), op("pdf.to_text.md", "Markdown", "FileCode")])
                .available(render),
            catalog::op("pdf.to_pptx").available(render && self.pandoc.is_some()),
            catalog::op("pdf.to_docx").available(render && self.pandoc.is_some()),
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
            Action::Text { markdown } => {
                let library = self.pdfium.as_deref().ok_or_else(|| EngineError::ToolMissing("pdfium".into()))?;
                inputs.iter().map(|i| to_text(library, i, markdown)).collect()
            }
            Action::ToPptx => {
                let (pdfium, pandoc) = self.office_tools()?;
                inputs.iter().map(|i| to_pptx(pdfium, pandoc, self.timeout, i)).collect()
            }
            Action::ToDocx => {
                let (pdfium, pandoc) = self.office_tools()?;
                inputs.iter().map(|i| to_docx_text(pdfium, pandoc, self.timeout, i)).collect()
            }
            Action::Compress(preset) => inputs.iter().map(|i| compress(i, preset)).collect(),
            Action::PageNumbers(corner) => inputs.iter().map(|i| page_numbers(i, corner)).collect(),
            Action::Watermark(text) => inputs.iter().map(|i| watermark(i, &text)).collect(),
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
        run_with(&PdfConverter::new(&EngineConfig::default()), id, inputs, &[])
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
        assert_eq!(parse_action("pdf.compress.strong", &none).unwrap(), Action::Compress(PRESETS[2].3));
        assert_eq!(parse_action("pdf.page_numbers", &with("position", "top_right")).unwrap(), Action::PageNumbers(Corner::TopRight));
        assert_eq!(parse_action("pdf.watermark.draft", &none).unwrap(), Action::Watermark("DRAFT".into()));
        assert_eq!(parse_action("pdf.watermark", &with("text", " Entwürfe ")).unwrap(), Action::Watermark("Entwürfe".into()));
        assert_eq!(parse_action("pdf.to_text.md", &none).unwrap(), Action::Text { markdown: true });
        for (id, opts) in [
            ("pdf.compress.max", none.clone()),
            ("pdf.page_numbers.left", none.clone()),
            ("pdf.watermark", with("text", "機密")),
            ("pdf.watermark", with("text", "  ")),
            ("pdf.watermark.secret", none.clone()),
            ("pdf.to_text.docx", none.clone()),
        ] {
            assert!(matches!(parse_action(id, &opts), Err(EngineError::InvalidOption(_))), "{id} {opts:?}");
        }
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
            let out = run_with(&PdfConverter::new(&EngineConfig::default()), id, std::slice::from_ref(&input), opts).unwrap();
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
        assert!(matches!(run("pdf.rotate.90", std::slice::from_ref(&png)), Err(EngineError::UnsupportedFormat(_))));
        assert!(matches!(run("pdf.from_images.a4", &[png, pdf.clone()]), Err(EngineError::UnsupportedFormat(_))));
        assert!(matches!(run("pdf.split", &[d.path().join("missing.pdf")]), Err(EngineError::InputNotFound(_))));
        assert!(matches!(run("pdf.to_png.72", &[pdf]), Err(EngineError::ToolMissing(t)) if t == "pdfium"));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 3, "no outputs written");
    }

    #[test]
    fn operations_and_batch_operations() {
        let without = PdfConverter::new(&EngineConfig::default());
        let ids = |ops: Vec<Operation>| ops.into_iter().map(|o| (o.id, o.available)).collect::<Vec<_>>();
        assert_eq!(
            ids(without.operations(PDF)),
            [
                ("pdf.split".into(), true),
                ("pdf.extract".into(), true),
                ("pdf.rotate".into(), true),
                ("pdf.to_image".into(), false),
                ("pdf.compress".into(), true),
                ("pdf.page_numbers".into(), true),
                ("pdf.watermark".into(), true),
                ("pdf.to_text".into(), false),
                ("pdf.to_pptx".into(), false),
                ("pdf.to_docx".into(), false),
            ]
        );
        let exe = std::env::current_exe().unwrap();
        let with = PdfConverter::new(&EngineConfig { pdfium_path: Some(exe.clone()), pandoc_path: Some(exe), ..EngineConfig::default() });
        assert!(with.operations(PDF).iter().all(|o| o.available));
        assert!(without.operations("image/png").is_empty());

        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let batch = |v: &[&str]| without.batch_operations(&s(v)).into_iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(batch(&[PDF, PDF]), ["pdf.merge"]);
        assert!(batch(&[PDF]).is_empty());
        assert_eq!(batch(&["image/png"]), ["pdf.from_images"]);
        assert_eq!(batch(&["image/png", "image/webp"]), ["pdf.from_images"]);
        let to_image = without.operations(PDF).into_iter().find(|o| o.id == "pdf.to_image").unwrap();
        let kids: Vec<_> = to_image.children.into_iter().map(|o| o.id).collect();
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

    // ---- compress ---------------------------------------------------------------------

    /// Incompressible pixels, so JPEG quality and size actually matter.
    fn noise(w: u32, h: u32) -> RgbImage {
        let mut state = 0x2545_f491_u32;
        RgbImage::from_fn(w, h, |_, _| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            };
            Rgb([next(), next(), next()])
        })
    }

    /// One page showing a q95 JPEG (`jpeg_size`) and a lossless RGB image (`flate_size`) with
    /// a soft mask; the content stream is stored uncompressed.
    fn image_pdf(path: &Path, jpeg_size: (u32, u32), flate_size: (u32, u32)) -> PathBuf {
        let mut doc = Document::with_version("1.4");
        let mut jpeg = Vec::new();
        let img = noise(jpeg_size.0, jpeg_size.1);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
            .unwrap();
        let photo = doc.add_object(image_stream(jpeg_size.0, jpeg_size.1, "DeviceRGB", Some("DCTDecode"), jpeg));
        let (fw, fh) = flate_size;
        let gradient = RgbImage::from_fn(fw, fh, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8]));
        let mut mask = image_stream(fw, fh, "DeviceGray", None, vec![200; (fw * fh) as usize]);
        mask.compress().unwrap();
        let mask = doc.add_object(mask);
        let mut art = image_stream(fw, fh, "DeviceRGB", None, gradient.into_raw());
        art.compress().unwrap();
        art.dict.set("SMask", mask);
        let art = doc.add_object(art);
        let pages_id = doc.new_object_id();
        let content = doc.add_object(Stream::new(dictionary! {}, b"q 300 0 0 150 0 0 cm /Im0 Do Q q 300 0 0 150 0 150 cm /Im1 Do Q".to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => photo, "Im1" => art } },
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
        path.to_path_buf()
    }

    /// `(name, width, height, filter, has SMask)` of each image XObject on page 1.
    fn images_of(path: &Path) -> Vec<(String, i64, i64, String, bool)> {
        let doc = Document::load(path).unwrap();
        let page = *doc.get_pages().get(&1).unwrap();
        let resources = doc.get_dictionary(page).unwrap().get(b"Resources").unwrap().as_dict().unwrap();
        let mut out: Vec<_> = resources
            .get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap()
            .iter()
            .map(|(name, r)| {
                let s = doc.get_object(r.as_reference().unwrap()).unwrap().as_stream().unwrap();
                let num = |k: &[u8]| s.dict.get(k).unwrap().as_i64().unwrap();
                let filter = s.dict.get(b"Filter").map(|f| String::from_utf8_lossy(f.as_name().unwrap()).into_owned()).unwrap_or_default();
                (String::from_utf8_lossy(name).into_owned(), num(b"Width"), num(b"Height"), filter, s.dict.has(b"SMask"))
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn compress_downsamples_and_recompresses_images() {
        let d = tempfile::tempdir().unwrap();
        let input = image_pdf(&d.path().join("scan.pdf"), (1800, 900), (1700, 850));
        let size = |p: &Path| std::fs::metadata(p).unwrap().len();

        let balanced = run("pdf.compress.balanced", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(balanced, d.path().join("scan (compressed).pdf"));
        assert!(size(&balanced) * 2 < size(&input), "{} vs {}", size(&balanced), size(&input));
        assert_eq!(
            images_of(&balanced),
            [("Im0".into(), 1600, 800, "DCTDecode".into(), false), ("Im1".into(), 1600, 800, "DCTDecode".into(), true)],
        );
        let doc = Document::load(&balanced).unwrap();
        let mask = doc.objects.values().filter_map(|o| o.as_stream().ok()).find(|s| s.dict.get(b"ColorSpace").and_then(Object::as_name).is_ok_and(|n| n == b"DeviceGray")).unwrap();
        assert_eq!(mask.dict.get(b"Width").unwrap().as_i64().unwrap(), 1700, "soft masks are left lossless");

        let light = run("pdf.compress.light", std::slice::from_ref(&input)).unwrap().remove(0);
        let imgs = images_of(&light);
        assert_eq!((imgs[0].1, imgs[0].3.as_str()), (1800, "DCTDecode"), "under 2400 px: re-encoded, not resized");
        assert_eq!((imgs[1].1, imgs[1].3.as_str()), (1700, "FlateDecode"), "light keeps lossless images lossless");
        assert!(size(&light) < size(&input));
    }

    #[test]
    fn compress_never_makes_a_file_bigger() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("tiny.pdf"), "P", 2, 0);
        let out = run("pdf.compress.strong", std::slice::from_ref(&input)).unwrap().remove(0);
        assert!(std::fs::metadata(&out).unwrap().len() <= std::fs::metadata(&input).unwrap().len());
        assert_eq!(labels(&out), ["P1", "P2"]);
    }

    // ---- page numbers, watermark ------------------------------------------------------

    fn page_text(path: &Path, n: u32) -> String {
        let doc = Document::load(path).unwrap();
        String::from_utf8_lossy(&doc.get_page_content(*doc.get_pages().get(&n).unwrap())).into_owned()
    }

    #[test]
    fn page_numbers_are_added_over_each_page() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("doc.pdf"), "P", 3, 0);
        let out = run("pdf.page_numbers.bottom_center", &[input]).unwrap().remove(0);
        assert_eq!(out, d.path().join("doc (numbered).pdf"));
        assert_eq!(labels(&out), ["P1", "P2", "P3"], "original content kept, first");
        for n in 1..=3 {
            let text = page_text(&out, n);
            assert!(text.starts_with("q\n") && text.contains(&format!("({n}) Tj")), "{text}");
        }
        // Pages shared one inherited resource dictionary: the font is added to it once.
        let doc = Document::load(&out).unwrap();
        let fonts = doc.objects.values().filter(|o| o.as_dict().is_ok_and(|d| d.get(b"BaseFont").is_ok())).count();
        assert_eq!(fonts, 2, "F1 + one Helvetica for the numbers");
    }

    #[test]
    fn watermark_text_is_escaped_and_translucent() {
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("doc.pdf"), "P", 1, 0);
        let out = run("pdf.watermark.confidential", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(out, d.path().join("doc (watermarked).pdf"));
        let text = page_text(&out, 1);
        assert!(text.contains("(CONFIDENTIAL) Tj") && text.contains("/RpAlpha1 gs"), "{text}");
        let custom = run_with(&PdfConverter::new(&EngineConfig::default()), "pdf.watermark", &[input], &[("text", "Q3 (draft)")]).unwrap().remove(0);
        assert!(page_text(&custom, 1).contains(r"(Q3 \(draft\)) Tj"));
    }

    /// Darkest pixel in a region of a rendered page (255 = all white).
    fn darkest(img: &image::RgbImage, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> u8 {
        ys.flat_map(|y| xs.clone().map(move |x| (x, y))).map(|(x, y)| img.get_pixel(x, y).0.into_iter().min().unwrap()).min().unwrap()
    }

    #[test]
    fn stamps_follow_page_rotation_when_rendered() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        // Rotated 90: shown as a 200x300 portrait; the number must be at its bottom center.
        let input = make_pdf(&d.path().join("turned.pdf"), "", 1, 90);
        let numbered = run_with(&c, "pdf.page_numbers.bottom_center", std::slice::from_ref(&input), &[]).unwrap().remove(0);
        let png = run_with(&c, "pdf.to_png.72", &[numbered], &[]).unwrap().remove(0);
        let img = image::open(&png).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (200, 300));
        assert!(darkest(&img, 90..110, 270..295) < 160, "number at the bottom center");
        assert!(darkest(&img, 90..110, 5..30) > 240, "nothing at the top");

        let marked = run_with(&c, "pdf.watermark.draft", &[input], &[]).unwrap().remove(0);
        let png = run_with(&c, "pdf.to_png.72", &[marked], &[]).unwrap().remove(0);
        let img = image::open(&png).unwrap().to_rgb8();
        let center = darkest(&img, 80..120, 130..170);
        assert!((150..250).contains(&center), "light grey text across the middle, got {center}");
    }

    // ---- text ---------------------------------------------------------------------------

    #[test]
    fn markdown_lines_are_escaped() {
        assert_eq!(markdown_line("# not a heading"), r"\# not a heading");
        assert_eq!(markdown_line("   - item"), r"\- item");
        assert_eq!(markdown_line("12. point"), r"12\. point");
        assert_eq!(markdown_line("2026 was a year"), "2026 was a year");
        assert_eq!(markdown_line("plain"), "plain");
    }

    #[test]
    fn extracts_text_per_page() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("notes.pdf"), "Hello", 2, 0);
        let txt = run_with(&c, "pdf.to_text.txt", std::slice::from_ref(&input), &[]).unwrap().remove(0);
        assert_eq!(txt, d.path().join("notes (converted).txt"));
        assert_eq!(std::fs::read_to_string(&txt).unwrap(), "Hello1\n\x0c\nHello2\n");
        let md = run_with(&c, "pdf.to_text", &[input], &[("format", "md")]).unwrap().remove(0);
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "Hello1\n\n---\n\nHello2\n");

        let png = d.path().join("scan.png");
        RgbImage::from_pixel(20, 20, Rgb([9, 9, 9])).save(&png).unwrap();
        let scan = run("pdf.from_images.original", &[png]).unwrap().remove(0);
        assert!(matches!(run_with(&c, "pdf.to_text.txt", &[scan], &[]), Err(EngineError::Empty(_))));
    }

    // ---- PDF -> PowerPoint / Word -------------------------------------------------------

    fn line(text: &str, size: f32, bold: bool, top: f32) -> Line {
        Line { text: text.into(), size, bold, top, bottom: top - size }
    }

    #[test]
    fn structure_is_rebuilt_from_layout() {
        let page = vec![
            line("Annual Report", 24.0, true, 760.0),
            line("Overview", 16.0, false, 720.0),
            line("The first paragraph starts here and is con-", 12.0, false, 690.0),
            line("tinued on the second line.", 12.0, false, 676.0),
            line("A second paragraph with *stars* and #hash.", 12.0, false, 640.0),
            line("Key points", 12.0, true, 610.0),
            line("• Fast", 12.0, false, 590.0),
            line("wrapping onto a second line", 12.0, false, 576.0),
            line("• Local", 12.0, false, 562.0),
            line("1. First", 12.0, false, 530.0),
            line("2) Second", 12.0, false, 516.0),
            line("3", 10.0, false, 30.0), // running page number
        ];
        let next = vec![line("Page 2 of 9", 10.0, false, 785.0), line("Last words.", 12.0, false, 700.0)];
        assert_eq!(
            lines_to_markdown(&[(792.0, page), (792.0, next)]),
            "# Annual Report\n\n## Overview\n\n\
             The first paragraph starts here and is continued on the second line\\.\n\n\
             A second paragraph with \\*stars\\* and \\#hash\\.\n\n\
             ### Key points\n\n\
             - Fast wrapping onto a second line\n- Local\n\n\
             1\\. First\n\n\
             1. Second\n\n\
             Last words\\.\n"
                .replace("1\\. First\n\n1. Second", "1. First\n1. Second")
        );
    }

    #[test]
    fn list_markers_and_page_numbers() {
        assert_eq!(list_marker("• item"), Some((false, "item")));
        assert_eq!(list_marker("12) item"), Some((true, "item")));
        assert_eq!(list_marker("-5 degrees"), None);
        assert_eq!(list_marker("2026. A year"), None, "four digits is a year, not a list");
        assert!(is_page_number(&line("12", 10.0, false, 30.0), 792.0));
        assert!(is_page_number(&line("Page 3 / 9", 10.0, false, 785.0), 792.0));
        assert!(!is_page_number(&line("12", 10.0, false, 400.0), 792.0), "not at an edge");
        assert!(!is_page_number(&line("Chapter 12", 10.0, false, 30.0), 792.0));
    }

    #[test]
    fn slides_take_the_page_aspect() {
        assert_eq!(slide_size(300, 200), (9_144_000, 6_096_000));
        assert_eq!(slide_size(200, 300), (6_096_000, 9_144_000));
        assert_eq!(slide_size(1000, 10), (9_144_000, 914_400), "at least an inch");
    }

    /// pdfium and pandoc (PATH or fetched sidecar), or `None` to skip.
    fn office() -> Option<PdfConverter> {
        let mut c = renderer()?;
        c.pandoc = crate::tools::for_tests("pandoc");
        if c.pandoc.is_none() {
            eprintln!("skipping: pandoc not found");
        }
        c.pandoc.is_some().then_some(c)
    }

    fn zip_entry(path: &Path, name: &str) -> String {
        use std::io::Read;
        let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn pdf_to_pptx_fills_one_slide_per_page() {
        let Some(c) = office() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = make_pdf(&d.path().join("deck.pdf"), "S", 2, 0);
        let out = run_with(&c, "pdf.to_pptx", &[input], &[]).unwrap().remove(0);
        assert_eq!(out, d.path().join("deck (converted).pptx"));
        // 300 x 200 pt at 150 DPI renders as 625 x 417 px.
        let (cx, cy) = slide_size(625, 417);
        assert!(zip_entry(&out, "ppt/presentation.xml").contains(&format!(r#"<p:sldSz cx="{cx}" cy="{cy}" />"#)));
        for n in 1..=2 {
            let slide = zip_entry(&out, &format!("ppt/slides/slide{n}.xml"));
            assert!(slide.contains(&format!(r#"<a:off x="0" y="0" /><a:ext cx="{cx}" cy="{cy}" />"#)), "{slide}");
        }
        let media = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap().file_names().filter(|n| n.as_ref().is_ok_and(|n| n.starts_with("ppt/media/"))).count();
        assert_eq!(media, 2);
    }

    /// A Letter page: a 24 pt bold title, a paragraph hyphenated across two lines, a bold
    /// lead-in, two bullets and a page number at the bottom.
    fn report_pdf(path: &Path) -> PathBuf {
        let mut doc = Document::with_version("1.5");
        let regular = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let bold = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold" });
        let content = "BT /F2 24 Tf 72 700 Td (Annual Report) Tj ET \
            BT /F1 12 Tf 72 660 Td (The first paragraph starts here and is con-) Tj 0 -14 Td (tinued on the second line.) Tj ET \
            BT /F2 12 Tf 72 610 Td (Key points) Tj ET \
            BT /F1 12 Tf 72 585 Td (- Fast) Tj 0 -14 Td (- Local) Tj ET \
            BT /F1 10 Tf 300 30 Td (1) Tj ET";
        let pages_id = doc.new_object_id();
        let contents = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => contents,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => regular, "F2" => bold } },
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
        path.to_path_buf()
    }

    #[test]
    fn pdf_layout_is_read_back_as_structure() {
        let Some(c) = renderer() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = report_pdf(&d.path().join("report.pdf"));
        let doc = pdfium(c.pdfium.as_deref().unwrap()).unwrap().load_pdf_from_file(&input, None).unwrap();
        let page = doc.pages().get(0).unwrap();
        let md = lines_to_markdown(&[(page.height().value, page_lines(&page).unwrap())]);
        assert_eq!(
            md,
            "# Annual Report\n\nThe first paragraph starts here and is continued on the second line\\.\n\n### Key points\n\n- Fast\n- Local\n"
        );
    }

    #[test]
    fn pdf_to_word_as_editable_text() {
        let Some(c) = office() else { return };
        let d = tempfile::tempdir().unwrap();
        let input = report_pdf(&d.path().join("report.pdf"));
        let out = run_with(&c, "pdf.to_docx", &[input], &[]).unwrap().remove(0);
        assert_eq!(out, d.path().join("report (converted).docx"));
        let body = zip_entry(&out, "word/document.xml");
        assert!(body.contains(r#"<w:pStyle w:val="Heading1" />"#) && body.contains("Annual Report"), "{body}");
        assert!(body.contains("is continued on the second line."));
        assert!(body.contains("<w:numPr>"), "bullets are a real list");
        assert!(!body.contains(r#"<w:t xml:space="preserve">1</w:t>"#) && !body.contains("<w:t>1</w:t>"), "page number dropped");

        let png = d.path().join("scan.png");
        RgbImage::from_pixel(20, 20, Rgb([9, 9, 9])).save(&png).unwrap();
        let scan = run("pdf.from_images.original", &[png]).unwrap().remove(0);
        assert!(matches!(run_with(&c, "pdf.to_docx", &[scan], &[]), Err(EngineError::Empty(_))));
        assert!(matches!(run("pdf.to_pptx", &[d.path().join("report.pdf")]), Err(EngineError::ToolMissing(_))));
    }
}
