//! Image operations (PNG / JPEG / WebP) built on the `image` crate.
//!
//! Operation ids are `image.<action>[.<param>[.<value>]]`, e.g. `image.convert.jpg`,
//! `image.resize.pct.50`, `image.resize.max.1920`, `image.compress.70`, `image.rotate.90`,
//! `image.flip.h|v` (option `axis`), `image.grayscale`, `image.strip_metadata`.
//! A value can also come from `options` (which wins over the id), so the parent ids work too:
//! `image.resize.pct` + `{percent: "50"}`. `image.crop` takes `x`, `y`, `width`, `height`
//! options in original pixels; its operation has `prompt: "crop"`, so the UI asks for the
//! rectangle first (over [`preview`]).
//!
//! Strip metadata never decodes: see `metadata` (EXIF orientation is kept). Greyscale images
//! are written as single-channel JPEGs.
//!
//! Limitation: the `image` crate only writes *lossless* WebP, so WebP output ignores quality
//! and any request for lossy WebP (quality < 100) is rejected rather than silently ignored.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageFormat, ImageReader, RgbImage};

use crate::{catalog, create_unique_sibling, Converter, EngineError, Operation, Options, Result};

const JPEG_DEFAULT_QUALITY: u8 = 90;
const COMPRESS_DEFAULT_QUALITY: u8 = 70;
/// Guards against a runaway percent producing a multi-gigabyte buffer.
const MAX_OUTPUT_DIMENSION: u32 = 32_768;

pub struct ImageConverter;

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Convert(ImageFormat),
    ResizePercent(f64),
    ResizeMax(u32),
    Compress { quality: u8, format: ImageFormat },
    Rotate(u16),
    Flip { horizontal: bool },
    Crop { x: u32, y: u32, width: u32, height: u32 },
    Grayscale,
    /// Lossless: the file's bytes minus its metadata, never decoded (see `metadata`).
    StripMetadata,
}

fn format_for_mime(mime: &str) -> Option<ImageFormat> {
    match mime {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        _ => "webp",
    }
}

fn parse_format(s: &str) -> Result<ImageFormat> {
    match s.to_ascii_lowercase().as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "webp" => Ok(ImageFormat::WebP),
        other => Err(EngineError::UnsupportedFormat(other.to_string())),
    }
}

/// A parameter from `options[key]`, else the id segment, else a "missing" error.
fn param<T: FromStr>(options: &Options, key: &str, from_id: Option<&&str>) -> Result<T> {
    let raw = options
        .get(key)
        .map(String::as_str)
        .or(from_id.copied())
        .ok_or_else(|| EngineError::InvalidOption(format!("missing `{key}`")))?;
    raw.trim()
        .parse()
        .map_err(|_| EngineError::InvalidOption(format!("`{key}` is not valid: `{raw}`")))
}

fn parse_action(id: &str, options: &Options) -> Result<Action> {
    let parts: Vec<&str> = id.split('.').collect();
    match parts.as_slice() {
        ["image", "convert", rest @ ..] => {
            let name = options.get("format").map(String::as_str).or(rest.first().copied());
            let name = name.ok_or_else(|| EngineError::InvalidOption("missing `format`".into()))?;
            Ok(Action::Convert(parse_format(name)?))
        }
        ["image", "resize", "pct", rest @ ..] => {
            let p: f64 = param(options, "percent", rest.first())?;
            if !(p > 0.0 && p <= 1000.0) {
                return Err(EngineError::InvalidOption(format!("percent must be in (0, 1000], got {p}")));
            }
            Ok(Action::ResizePercent(p))
        }
        ["image", "resize", "max", rest @ ..] => {
            let m: u32 = param(options, "max", rest.first())?;
            if m == 0 || m > MAX_OUTPUT_DIMENSION {
                return Err(EngineError::InvalidOption(format!("max must be 1..={MAX_OUTPUT_DIMENSION}, got {m}")));
            }
            Ok(Action::ResizeMax(m))
        }
        ["image", "compress", rest @ ..] => {
            let quality = if options.contains_key("quality") || !rest.is_empty() {
                param(options, "quality", rest.first())?
            } else {
                COMPRESS_DEFAULT_QUALITY
            };
            if !(1..=100).contains(&quality) {
                return Err(EngineError::InvalidOption(format!("quality must be 1..=100, got {quality}")));
            }
            let format = match options.get("format") {
                Some(f) => parse_format(f)?,
                None => ImageFormat::Jpeg,
            };
            Ok(Action::Compress { quality, format })
        }
        ["image", "rotate", rest @ ..] => {
            let degrees: u16 = param(options, "degrees", rest.first())?;
            if ![90, 180, 270].contains(&degrees) {
                return Err(EngineError::InvalidOption(format!("degrees must be 90, 180 or 270, got {degrees}")));
            }
            Ok(Action::Rotate(degrees))
        }
        ["image", "flip", rest @ ..] => match param::<String>(options, "axis", rest.first())?.as_str() {
            "h" | "horizontal" => Ok(Action::Flip { horizontal: true }),
            "v" | "vertical" => Ok(Action::Flip { horizontal: false }),
            other => Err(EngineError::InvalidOption(format!("axis must be h or v, got `{other}`"))),
        },
        ["image", "crop"] => Ok(Action::Crop {
            x: param(options, "x", None)?,
            y: param(options, "y", None)?,
            width: param(options, "width", None)?,
            height: param(options, "height", None)?,
        }),
        ["image", "grayscale"] => Ok(Action::Grayscale),
        ["image", "strip_metadata"] => Ok(Action::StripMetadata),
        _ => Err(EngineError::UnknownOperation(id.to_string())),
    }
}

/// Flattens alpha onto white, since JPEG has no alpha channel.
fn flatten_on_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u32::from(p[3]);
        let blend = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])]));
    }
    out
}

pub(crate) fn encode(img: &DynamicImage, format: ImageFormat, quality: Option<u8>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match format {
        ImageFormat::Jpeg if !img.color().has_color() => {
            // Greyscale stays one channel (a third of the size), flattened onto white like RGB.
            let grey = DynamicImage::ImageRgb8(flatten_on_white(img)).to_luma8();
            let q = quality.unwrap_or(JPEG_DEFAULT_QUALITY);
            JpegEncoder::new_with_quality(&mut buf, q).encode(grey.as_raw(), grey.width(), grey.height(), ExtendedColorType::L8)?;
        }
        ImageFormat::Jpeg => {
            let rgb = flatten_on_white(img);
            let q = quality.unwrap_or(JPEG_DEFAULT_QUALITY);
            JpegEncoder::new_with_quality(&mut buf, q).encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )?;
        }
        ImageFormat::Png => img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)?,
        ImageFormat::WebP => {
            if quality.is_some_and(|q| q < 100) {
                return Err(EngineError::InvalidOption(
                    "lossy WebP is not supported (the image crate only writes lossless WebP)".into(),
                ));
            }
            let rgba = img.to_rgba8();
            WebPEncoder::new_lossless(&mut buf).encode(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )?;
        }
        other => return Err(EngineError::UnsupportedFormat(format!("{other:?}"))),
    }
    Ok(buf)
}

fn scaled(w: u32, h: u32, factor: f64) -> Result<(u32, u32)> {
    let dim = |v: u32| ((f64::from(v) * factor).round() as u32).max(1);
    let (nw, nh) = (dim(w), dim(h));
    if nw > MAX_OUTPUT_DIMENSION || nh > MAX_OUTPUT_DIMENSION {
        return Err(EngineError::InvalidOption(format!(
            "output would be {nw}x{nh}, over the {MAX_OUTPUT_DIMENSION}px limit"
        )));
    }
    Ok((nw, nh))
}

/// Returns the transformed image, the format to write, the quality to use, and the name tag.
fn apply(
    action: &Action,
    img: DynamicImage,
    src_format: ImageFormat,
) -> Result<(DynamicImage, ImageFormat, Option<u8>, &'static str)> {
    Ok(match *action {
        Action::Convert(f) => (img, f, None, "converted"),
        Action::ResizePercent(p) => {
            let (w, h) = scaled(img.width(), img.height(), p / 100.0)?;
            (img.resize_exact(w, h, FilterType::Lanczos3), src_format, None, "resized")
        }
        Action::ResizeMax(m) => {
            let longest = img.width().max(img.height());
            // Never upscale: an image already within the limit is written unchanged.
            if longest <= m {
                (img, src_format, None, "resized")
            } else {
                let (w, h) = scaled(img.width(), img.height(), f64::from(m) / f64::from(longest))?;
                (img.resize_exact(w, h, FilterType::Lanczos3), src_format, None, "resized")
            }
        }
        Action::Compress { quality, format } => (img, format, Some(quality), "compressed"),
        Action::Rotate(d) => {
            let rotated = match d {
                90 => img.rotate90(),
                180 => img.rotate180(),
                _ => img.rotate270(),
            };
            (rotated, src_format, None, "rotated")
        }
        Action::Flip { horizontal } => {
            let flipped = if horizontal { img.fliph() } else { img.flipv() };
            (flipped, src_format, None, "flipped")
        }
        // Luma (with alpha if there was any); JPEG output stays single-channel.
        Action::Grayscale => (img.grayscale(), src_format, None, "grayscale"),
        Action::StripMetadata => unreachable!("handled in `process` without decoding"),
        Action::Crop { x, y, width, height } => {
            let fits = width > 0
                && height > 0
                && x.checked_add(width).is_some_and(|r| r <= img.width())
                && y.checked_add(height).is_some_and(|b| b <= img.height());
            if !fits {
                return Err(EngineError::InvalidOption(format!(
                    "crop rect {width}x{height} at ({x}, {y}) is outside the {}x{} image",
                    img.width(),
                    img.height()
                )));
            }
            (img.crop_imm(x, y, width, height), src_format, None, "cropped")
        }
    })
}

fn process(input: &Path, action: &Action) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let reader = ImageReader::open(input)?.with_guessed_format()?;
    let src_format = match reader.format() {
        Some(f @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) => f,
        Some(other) => return Err(EngineError::UnsupportedFormat(format!("{other:?}"))),
        None => return Err(EngineError::UnsupportedFormat("unrecognised image data".into())),
    };
    let (bytes, format, tag) = if *action == Action::StripMetadata {
        (crate::metadata::strip(&std::fs::read(input)?, src_format)?, src_format, "no metadata")
    } else {
        let (out, format, quality, tag) = apply(action, reader.decode()?, src_format)?;
        (encode(&out, format, quality)?, format, tag)
    };

    let (mut file, path) = create_unique_sibling(input, tag, Some(extension(format)))?;
    if let Err(e) = file.write_all(&bytes) {
        let _ = std::fs::remove_file(&path); // don't leave a truncated output behind
        return Err(e.into());
    }
    Ok(path)
}

impl Converter for ImageConverter {
    fn name(&self) -> &'static str {
        "image"
    }

    fn can_handle(&self, mime: &str) -> bool {
        format_for_mime(mime).is_some()
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        let own = format_for_mime(mime);
        let convert = [("png", "PNG", "FileImage"), ("jpg", "JPG", "FileImage"), ("webp", "WebP", "Image")]
            .into_iter()
            .filter(|(ext, ..)| parse_format(ext).ok() != own)
            .map(|(ext, label, icon)| Operation::new(&format!("image.convert.{ext}"), label, icon))
            .collect();
        vec![
            catalog::op("image.convert").with_children(convert),
            catalog::op("image.resize").with_children(vec![
                Operation::new("image.resize.pct.50", "50%", "Percent"),
                Operation::new("image.resize.pct.25", "25%", "Percent"),
                Operation::new("image.resize.max.1920", "Max 1920", "Maximize2"),
                Operation::new("image.resize.max.1080", "Max 1080", "Maximize2"),
            ]),
            catalog::op("image.compress").with_children(vec![
                Operation::new("image.compress.85", "Light", "Feather"),
                Operation::new("image.compress.70", "Balanced", "Gauge"),
                Operation::new("image.compress.40", "Smallest", "Minimize2"),
            ]),
            catalog::op("image.rotate").with_children(vec![
                Operation::new("image.rotate.90", "90\u{b0} CW", "RotateCw"),
                Operation::new("image.rotate.270", "90\u{b0} CCW", "RotateCcw"),
                Operation::new("image.rotate.180", "180\u{b0}", "RotateCw"),
                Operation::new("image.flip.h", "Flip horizontal", "FlipHorizontal2"),
                Operation::new("image.flip.v", "Flip vertical", "FlipVertical2"),
            ]),
            catalog::op("image.crop").with_prompt("crop"),
            catalog::op("image.grayscale"),
            catalog::op("image.strip_metadata"),
        ]
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>> {
        let action = parse_action(&op.id, options)?; // validate before touching any file
        inputs.iter().map(|input| process(input, &action)).collect()
    }
}

/// A downscaled copy of an image for picking a crop rectangle: the encoded preview, its mime,
/// and the size of the *original* (crop options are in original pixels).
pub struct Preview {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Decodes `input` the same way the operations do (EXIF orientation is not applied there
/// either, so the preview matches what `image.crop` sees) and fits it in `max_side` px.
pub fn preview(input: &Path, max_side: u32) -> Result<Preview> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let img = ImageReader::open(input)?.with_guessed_format()?.decode()?;
    let (width, height) = (img.width(), img.height());
    let small = if width.max(height) > max_side { img.thumbnail(max_side, max_side) } else { img };
    // JPEG is much smaller to send to the UI; PNG only when there is transparency to show.
    let transparent = small.color().has_alpha() && small.to_rgba8().pixels().any(|p| p[3] < 255);
    let (format, mime) = if transparent { (ImageFormat::Png, "image/png") } else { (ImageFormat::Jpeg, "image/jpeg") };
    Ok(Preview { bytes: encode(&small, format, Some(85))?, mime, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// 8x4 image: left half red, right half blue, fully opaque.
    fn two_tone() -> RgbaImage {
        RgbaImage::from_fn(8, 4, |x, _| if x < 4 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 255]) })
    }

    fn write_fixture(dir: &Path, name: &str, img: &RgbaImage) -> PathBuf {
        let path = dir.join(name);
        let dynamic = DynamicImage::ImageRgba8(img.clone());
        let format = ImageFormat::from_path(&path).unwrap();
        std::fs::write(&path, encode(&dynamic, format, Some(100)).unwrap()).unwrap();
        path
    }

    fn run(id: &str, input: &Path, options: &[(&str, &str)]) -> Result<Vec<PathBuf>> {
        let opts: Options = options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ImageConverter.run(&Operation::new(id, id, "x"), &[input.to_path_buf()], &opts)
    }

    fn open(path: &Path) -> RgbaImage {
        image::open(path).unwrap().to_rgba8()
    }

    fn near(a: Rgba<u8>, b: [u8; 3], tol: i32) -> bool {
        (0..3).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= tol)
    }

    #[test]
    fn converts_between_all_formats() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());

        let jpg = run("image.convert.jpg", &png, &[]).unwrap().remove(0);
        assert_eq!(jpg, d.path().join("pic (converted).jpg"));
        assert_eq!(open(&jpg).dimensions(), (8, 4));
        assert!(near(*open(&jpg).get_pixel(1, 1), [255, 0, 0], 12)); // lossy, so approximate

        let webp = run("image.convert.webp", &png, &[]).unwrap().remove(0);
        assert_eq!(webp, d.path().join("pic (converted).webp"));
        assert_eq!(open(&webp), two_tone()); // lossless round trip is exact

        let back = run("image.convert.png", &webp, &[]).unwrap().remove(0);
        assert_eq!(back, d.path().join("pic (converted) (converted).png"));
        assert_eq!(open(&back), two_tone());
    }

    #[test]
    fn convert_reads_format_from_content_not_extension() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "real.png", &two_tone());
        let liar = d.path().join("liar.jpg");
        std::fs::copy(&png, &liar).unwrap();
        let out = run("image.convert.webp", &liar, &[]).unwrap().remove(0);
        assert_eq!(open(&out), two_tone());
    }

    #[test]
    fn output_never_overwrites_existing_files() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let first = run("image.convert.jpg", &png, &[]).unwrap().remove(0);
        let before = std::fs::read(&first).unwrap();
        let second = run("image.convert.jpg", &png, &[]).unwrap().remove(0);
        assert_eq!(first, d.path().join("pic (converted).jpg"));
        assert_eq!(second, d.path().join("pic (converted 2).jpg"));
        assert_eq!(std::fs::read(&first).unwrap(), before);
    }

    #[test]
    fn convert_requires_a_known_format() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        assert!(matches!(run("image.convert", &png, &[]), Err(EngineError::InvalidOption(_))));
        assert!(matches!(run("image.convert", &png, &[("format", "gif")]), Err(EngineError::UnsupportedFormat(_))));
        assert_eq!(open(&run("image.convert", &png, &[("format", "webp")]).unwrap()[0]).dimensions(), (8, 4));
    }

    #[test]
    fn jpeg_flattens_transparency_onto_white() {
        let d = tempfile::tempdir().unwrap();
        let clear = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
        let png = write_fixture(d.path(), "clear.png", &clear);
        let jpg = run("image.convert.jpg", &png, &[]).unwrap().remove(0);
        assert!(near(*open(&jpg).get_pixel(2, 2), [255, 255, 255], 6));
    }

    #[test]
    fn resize_by_percent() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let half = run("image.resize.pct.50", &png, &[]).unwrap().remove(0);
        assert_eq!(half, d.path().join("pic (resized).png"));
        assert_eq!(open(&half).dimensions(), (4, 2));
        let double = run("image.resize.pct", &png, &[("percent", "200")]).unwrap().remove(0);
        assert_eq!(open(&double).dimensions(), (16, 8));
        let tiny = run("image.resize.pct.1", &png, &[]).unwrap().remove(0);
        assert_eq!(open(&tiny).dimensions(), (1, 1)); // never collapses to zero
    }

    #[test]
    fn resize_by_max_dimension_keeps_aspect_and_never_upscales() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let small = run("image.resize.max.4", &png, &[]).unwrap().remove(0);
        assert_eq!(open(&small).dimensions(), (4, 2));
        let same = run("image.resize.max", &png, &[("max", "100")]).unwrap().remove(0);
        assert_eq!(open(&same).dimensions(), (8, 4));
        let tall = write_fixture(d.path(), "tall.png", &RgbaImage::from_pixel(3, 12, Rgba([9, 9, 9, 255])));
        assert_eq!(open(&run("image.resize.max.6", &tall, &[]).unwrap()[0]).dimensions(), (2, 6));
    }

    #[test]
    fn resize_rejects_bad_values() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        for bad in ["0", "-5", "abc", "1001"] {
            assert!(matches!(run("image.resize.pct", &png, &[("percent", bad)]), Err(EngineError::InvalidOption(_))), "{bad}");
        }
        assert!(matches!(run("image.resize.max.0", &png, &[]), Err(EngineError::InvalidOption(_))));
        // A valid percent whose output would exceed the dimension cap is refused, not attempted.
        let wide = write_fixture(d.path(), "wide.png", &RgbaImage::new(4000, 1));
        assert!(matches!(run("image.resize.pct.1000", &wide, &[]), Err(EngineError::InvalidOption(_))));
    }

    #[test]
    fn compress_jpeg_quality_changes_size() {
        let d = tempfile::tempdir().unwrap();
        // Deterministic noise so quality has something to bite on.
        let noisy = RgbaImage::from_fn(64, 64, |x, y| {
            let v = x.wrapping_mul(2_654_435_761).wrapping_add(y.wrapping_mul(40_503)) >> 8;
            Rgba([v as u8, (v >> 3) as u8, (v >> 5) as u8, 255])
        });
        let png = write_fixture(d.path(), "noise.png", &noisy);
        let high = run("image.compress.95", &png, &[]).unwrap().remove(0);
        let low = run("image.compress.15", &png, &[]).unwrap().remove(0);
        assert_eq!(high, d.path().join("noise (compressed).jpg"));
        assert!(std::fs::metadata(&low).unwrap().len() < std::fs::metadata(&high).unwrap().len());
        assert_eq!(open(&low).dimensions(), (64, 64));
        // The quality option overrides the id; no quality at all uses the default.
        let via_option = run("image.compress", &png, &[("quality", "15")]).unwrap().remove(0);
        assert_eq!(std::fs::metadata(&via_option).unwrap().len(), std::fs::metadata(&low).unwrap().len());
        assert!(run("image.compress", &png, &[]).is_ok());
    }

    #[test]
    fn compress_validates_quality_and_webp() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        for bad in ["0", "101", "x"] {
            assert!(matches!(run("image.compress", &png, &[("quality", bad)]), Err(EngineError::InvalidOption(_))), "{bad}");
        }
        // Lossy WebP is unavailable, so ask for it and get a clear error, not a silent lossless file.
        let err = run("image.compress.70", &png, &[("format", "webp")]).unwrap_err();
        assert!(matches!(err, EngineError::InvalidOption(m) if m.contains("lossy WebP")));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1); // nothing was written
        let lossless = run("image.compress.100", &png, &[("format", "webp")]).unwrap().remove(0);
        assert_eq!(open(&lossless), two_tone());
    }

    #[test]
    fn rotate_moves_pixels_correctly() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let cw = open(&run("image.rotate.90", &png, &[]).unwrap()[0]);
        assert_eq!(cw.dimensions(), (4, 8));
        // Red (left) goes to the top, blue (right) to the bottom.
        assert_eq!(*cw.get_pixel(0, 0), Rgba([255, 0, 0, 255]));
        assert_eq!(*cw.get_pixel(0, 7), Rgba([0, 0, 255, 255]));
        let ccw = open(&run("image.rotate", &png, &[("degrees", "270")]).unwrap()[0]);
        assert_eq!(*ccw.get_pixel(0, 0), Rgba([0, 0, 255, 255]));
        assert_eq!(*ccw.get_pixel(0, 7), Rgba([255, 0, 0, 255]));
        let half = open(&run("image.rotate.180", &png, &[]).unwrap()[0]);
        assert_eq!(half.dimensions(), (8, 4));
        assert_eq!(*half.get_pixel(0, 0), Rgba([0, 0, 255, 255]));
        assert!(matches!(run("image.rotate", &png, &[("degrees", "45")]), Err(EngineError::InvalidOption(_))));
    }

    #[test]
    fn crop_extracts_the_rect() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let rect = [("x", "2"), ("y", "1"), ("width", "4"), ("height", "2")];
        let out = run("image.crop", &png, &rect).unwrap().remove(0);
        assert_eq!(out, d.path().join("pic (cropped).png"));
        let img = open(&out);
        assert_eq!(img.dimensions(), (4, 2));
        assert_eq!(*img.get_pixel(0, 0), Rgba([255, 0, 0, 255])); // x=2 is still red
        assert_eq!(*img.get_pixel(3, 1), Rgba([0, 0, 255, 255])); // x=5 is blue
    }

    #[test]
    fn crop_rejects_bad_rects() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let cases: [[(&str, &str); 4]; 3] = [
            [("x", "6"), ("y", "0"), ("width", "4"), ("height", "4")], // spills past the right edge
            [("x", "0"), ("y", "0"), ("width", "0"), ("height", "2")], // empty
            [("x", "4294967295"), ("y", "0"), ("width", "2"), ("height", "2")], // overflow
        ];
        for rect in cases {
            assert!(matches!(run("image.crop", &png, &rect), Err(EngineError::InvalidOption(_))), "{rect:?}");
        }
        assert!(matches!(run("image.crop", &png, &[("x", "0")]), Err(EngineError::InvalidOption(_))));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn handles_multiple_inputs() {
        let d = tempfile::tempdir().unwrap();
        let a = write_fixture(d.path(), "a.png", &two_tone());
        let b = write_fixture(d.path(), "b.jpg", &two_tone());
        let op = Operation::new("image.rotate.90", "r", "x");
        let out = ImageConverter.run(&op, &[a, b], &Options::new()).unwrap();
        assert_eq!(out, vec![d.path().join("a (rotated).png"), d.path().join("b (rotated).jpg")]);
    }

    #[test]
    fn bad_inputs_are_errors_not_panics() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope.png");
        assert!(matches!(run("image.rotate.90", &missing, &[]), Err(EngineError::InputNotFound(_))));
        let garbage = d.path().join("garbage.png");
        std::fs::write(&garbage, b"definitely not an image").unwrap();
        // A `.png` name makes the reader try PNG and fail at decode time; no extension means no guess.
        assert!(matches!(run("image.rotate.90", &garbage, &[]), Err(EngineError::Image(_))));
        let unnamed = d.path().join("garbage");
        std::fs::write(&unnamed, b"definitely not an image").unwrap();
        assert!(matches!(run("image.rotate.90", &unnamed, &[]), Err(EngineError::UnsupportedFormat(_))));
        let truncated = d.path().join("cut.png");
        let full = std::fs::read(write_fixture(d.path(), "full.png", &two_tone())).unwrap();
        std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
        assert!(run("image.rotate.90", &truncated, &[]).is_err());
        assert!(matches!(run("image.bogus", &missing, &[]), Err(EngineError::UnknownOperation(_))));
    }

    #[test]
    fn operations_are_offered_per_format() {
        let ids = |mime: &str| -> Vec<String> {
            let convert = ImageConverter.operations(mime).remove(0);
            convert.children.into_iter().map(|c| c.id).collect()
        };
        assert_eq!(ids("image/png"), ["image.convert.jpg", "image.convert.webp"]);
        assert_eq!(ids("image/jpeg"), ["image.convert.png", "image.convert.webp"]);
        assert_eq!(ids("image/webp"), ["image.convert.png", "image.convert.jpg"]);
        assert_eq!(ImageConverter.operations("image/png").len(), 7);
        let crop = ImageConverter.operations("image/png").into_iter().find(|o| o.id == "image.crop").unwrap();
        assert_eq!(crop.prompt.as_deref(), Some("crop"));
        assert!(!ImageConverter.can_handle("application/pdf"));
    }

    #[test]
    fn every_listed_operation_runs() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let reg = crate::Registry::with_defaults();
        // Planned entries (listed unavailable, hidden by the menu) have nothing to run yet;
        // prompted ones (crop) need the options the UI asks for.
        for parent in reg.operations("image/png").into_iter().filter(|o| o.available && o.prompt.is_none()) {
            let leaves = if parent.children.is_empty() { vec![parent.clone()] } else { parent.children.clone() };
            for op in leaves {
                let out = reg.run("image/png", &op, std::slice::from_ref(&png), &Options::new());
                assert!(out.is_ok(), "{} failed: {:?}", op.id, out.err());
            }
        }
    }

    #[test]
    fn flips_mirror_the_image() {
        let d = tempfile::tempdir().unwrap();
        let png = write_fixture(d.path(), "pic.png", &two_tone());
        let h = run("image.flip.h", &png, &[]).unwrap().remove(0);
        assert_eq!(h, d.path().join("pic (flipped).png"));
        assert_eq!(*open(&h).get_pixel(0, 0), Rgba([0, 0, 255, 255]), "blue moved to the left");
        let top_red = RgbaImage::from_fn(2, 2, |_, y| if y == 0 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 255]) });
        let tall = write_fixture(d.path(), "tall.png", &top_red);
        let v = run("image.flip", &tall, &[("axis", "v")]).unwrap().remove(0);
        assert_eq!(*open(&v).get_pixel(0, 0), Rgba([0, 0, 255, 255]));
        assert!(matches!(run("image.flip.diagonal", &png, &[]), Err(EngineError::InvalidOption(_))));
    }

    #[test]
    fn grayscale_keeps_format_and_alpha() {
        let d = tempfile::tempdir().unwrap();
        let mut img = two_tone();
        img.put_pixel(0, 0, Rgba([255, 0, 0, 0])); // one transparent pixel
        let png = write_fixture(d.path(), "pic.png", &img);
        let out = run("image.grayscale", &png, &[]).unwrap().remove(0);
        assert_eq!(out, d.path().join("pic (grayscale).png"));
        let grey = image::open(&out).unwrap();
        assert_eq!(grey.color(), image::ColorType::La8);
        let p = grey.to_rgba8();
        assert_eq!(p.get_pixel(0, 0)[3], 0, "alpha kept");
        assert!(p.get_pixel(1, 1)[0] == p.get_pixel(1, 1)[1] && p.get_pixel(1, 1)[1] == p.get_pixel(1, 1)[2]);

        let jpg = write_fixture(d.path(), "photo.jpg", &two_tone());
        let out = run("image.grayscale", &jpg, &[]).unwrap().remove(0);
        assert_eq!(out, d.path().join("photo (grayscale).jpg"));
        assert_eq!(image::open(&out).unwrap().color(), image::ColorType::L8, "single-channel JPEG");
    }

    #[test]
    fn strip_metadata_writes_a_lossless_copy() {
        let d = tempfile::tempdir().unwrap();
        let jpg = write_fixture(d.path(), "photo.jpg", &two_tone());
        let out = run("image.strip_metadata", &jpg, &[]).unwrap().remove(0);
        assert_eq!(out, d.path().join("photo (no metadata).jpg"));
        assert_eq!(open(&out), open(&jpg), "same pixels, not re-encoded");
        let bad = d.path().join("bad.png");
        std::fs::write(&bad, b"\x89PNG\r\n\x1a\ntruncated").unwrap();
        assert!(run("image.strip_metadata", &bad, &[]).is_err());
        assert!(!d.path().join("bad (no metadata).png").exists());
    }

    #[test]
    fn preview_fits_the_box_and_reports_the_original_size() {
        let d = tempfile::tempdir().unwrap();
        let big = RgbaImage::from_pixel(300, 120, Rgba([10, 20, 30, 255]));
        let png = write_fixture(d.path(), "wide.png", &big);
        let p = preview(&png, 100).unwrap();
        assert_eq!((p.width, p.height, p.mime), (300, 120, "image/jpeg"));
        assert_eq!(image::load_from_memory(&p.bytes).unwrap().width(), 100);
        let mut clear = big.clone();
        clear.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        let p = preview(&write_fixture(d.path(), "clear.png", &clear), 1000).unwrap();
        assert_eq!((p.mime, image::load_from_memory(&p.bytes).unwrap().width()), ("image/png", 300), "never upscaled");
        assert!(matches!(preview(&d.path().join("gone.png"), 10), Err(EngineError::InputNotFound(_))));
    }
}
