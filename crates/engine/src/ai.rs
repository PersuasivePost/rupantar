//! Image operations from the optional AI pack (downloaded only after the user agrees, like the
//! Office pack; see the `packs` crate):
//!
//! - `image.remove_bg.transparent` / `image.remove_bg.white`: BiRefNet lite (general, 1024 x
//!   1024) through ONNX Runtime. The image is resized to 1024 x 1024, normalised with the
//!   ImageNet mean/std and run once; the logits go through a sigmoid, the mask is scaled back to
//!   the original size and becomes the alpha channel (combined with any alpha already there).
//!   Transparent → `name (no background).png`; white → composited onto white in the input's
//!   format, `name (white background).ext`. Execution providers are tried in order (CUDA on
//!   Linux/Windows, DirectML on Windows, CoreML on macOS), then the CPU; what ran goes into the
//!   log (`Job::note`). ONNX Runtime reports no progress of its own, so the bar eases toward the
//!   end over the expected time and the run can be terminated through `RunOptions`.
//! - `image.upscale.2` / `image.upscale.4`: Real-ESRGAN ncnn-vulkan with the general
//!   `realesrgan-x4plus` model (4x); 2x is that result scaled down by half (Lanczos), which
//!   looks better than a native 2x model. Progress is parsed from its `NN.NN%` output; cancel
//!   kills it. Input at most 4096 px on its longest side (the output is then up to 16384 px).
//!   It needs a Vulkan GPU: without one the error says so (`EngineError::NoGpu`). Output keeps
//!   the input's format: `name (upscaled 4x).png`.
//!
//! The operations always carry `pack: "ai"`; without the pack they are `available: false` (the
//! menu offers the download, Remove background keeping its wedge), and running one is
//! `PackMissing`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, ImageFormat, ImageReader, RgbImage};
use ort::ep::ExecutionProviderDispatch;
use ort::session::{RunOptions, Session};
use ort::value::Tensor;

use crate::documents::WorkDir;
use crate::{catalog, tools, write_siblings, Converter, EngineConfig, EngineError, Job, Operation, Options, Result};

/// The pack these operations need (`Operation::pack`).
pub const PACK: &str = "ai";
const REMOVE_BG: &str = "image.remove_bg";
const UPSCALE: &str = "image.upscale";
/// BiRefNet lite's input size.
const SIDE: u32 = 1024;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// Longest side Upscale accepts (4x of it is the largest output we write).
const MAX_UPSCALE_SIDE: u32 = 4096;
/// Generous: a big image on a slow GPU takes minutes, and the user can cancel.
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ESRGAN: &str = "Real-ESRGAN";
const ESRGAN_MODEL: &str = "realesrgan-x4plus";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Background {
    Transparent,
    White,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    RemoveBg(Background),
    Upscale(u32),
}

fn parse_action(id: &str, options: &Options) -> Result<Action> {
    let invalid = |key: &str, v: &str| EngineError::InvalidOption(format!("`{key}` is not valid: `{v}`"));
    if let Some(rest) = id.strip_prefix(REMOVE_BG) {
        let value = options.get("background").map(String::as_str).or_else(|| rest.strip_prefix('.')).unwrap_or("transparent");
        if !rest.is_empty() && !rest.starts_with('.') {
            return Err(EngineError::UnknownOperation(id.to_string()));
        }
        return match value {
            "transparent" => Ok(Action::RemoveBg(Background::Transparent)),
            "white" => Ok(Action::RemoveBg(Background::White)),
            v => Err(invalid("background", v)),
        };
    }
    if let Some(rest) = id.strip_prefix(UPSCALE) {
        if !rest.is_empty() && !rest.starts_with('.') {
            return Err(EngineError::UnknownOperation(id.to_string()));
        }
        let value = options.get("scale").map(String::as_str).or_else(|| rest.strip_prefix('.')).unwrap_or("4");
        return match value {
            "2" => Ok(Action::Upscale(2)),
            "4" => Ok(Action::Upscale(4)),
            v => Err(invalid("scale", v)),
        };
    }
    Err(EngineError::UnknownOperation(id.to_string()))
}

fn image_format(mime: &str) -> Option<ImageFormat> {
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

/// Decodes a PNG / JPEG / WebP, by content.
fn open(input: &Path) -> Result<(DynamicImage, ImageFormat)> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let reader = ImageReader::open(input)?.with_guessed_format()?;
    match reader.format() {
        Some(f @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) => Ok((reader.decode()?, f)),
        Some(other) => Err(EngineError::UnsupportedFormat(format!("{other:?}"))),
        None => Err(EngineError::UnsupportedFormat("unrecognised image data".into())),
    }
}

// ---- background removal: pre- and post-processing -------------------------------------------

/// The model's input: the image squeezed to 1024 x 1024 (bilinear, as in training), RGB scaled
/// to 0..1 and normalised with the ImageNet mean/std, laid out NCHW (`[1, 3, 1024, 1024]`).
/// Alpha is ignored here; it is kept in [`cut_out`].
pub(crate) fn to_tensor(img: &DynamicImage) -> Vec<f32> {
    let rgb = img.resize_exact(SIDE, SIDE, FilterType::Triangle).to_rgb8();
    let plane = (SIDE * SIDE) as usize;
    let mut data = vec![0f32; 3 * plane];
    for (i, p) in rgb.pixels().enumerate() {
        for c in 0..3 {
            data[c * plane + i] = (f32::from(p[c]) / 255.0 - MEAN[c]) / STD[c];
        }
    }
    data
}

/// The mask from the model's output logits (`[1, 1, 1024, 1024]`): sigmoid, as 0..255, scaled
/// back to `width` x `height` (bilinear).
pub(crate) fn mask_from_logits(logits: &[f32], width: u32, height: u32) -> Result<GrayImage> {
    if logits.len() != (SIDE * SIDE) as usize {
        return Err(EngineError::Inference(format!("the model returned {} values, expected {}", logits.len(), SIDE * SIDE)));
    }
    let pixels = logits.iter().map(|&x| (255.0 / (1.0 + (-x).exp())).round() as u8).collect();
    let mask = GrayImage::from_raw(SIDE, SIDE, pixels).ok_or_else(|| EngineError::Inference("bad mask size".into()))?;
    Ok(if (width, height) == (SIDE, SIDE) { mask } else { image::imageops::resize(&mask, width, height, FilterType::Triangle) })
}

/// Applies `mask` (same size as `img`) as alpha: transparent keeps the colours with
/// `alpha = min(alpha, mask)`; white composites that onto white (no alpha left).
pub(crate) fn cut_out(img: &DynamicImage, mask: &GrayImage, background: Background) -> DynamicImage {
    let mut rgba = img.to_rgba8();
    for (p, m) in rgba.pixels_mut().zip(mask.pixels()) {
        p[3] = p[3].min(m[0]);
    }
    match background {
        Background::Transparent => DynamicImage::ImageRgba8(rgba),
        Background::White => {
            let white = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let p = rgba.get_pixel(x, y);
                let a = u32::from(p[3]);
                let blend = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
                image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
            });
            DynamicImage::ImageRgb8(white)
        }
    }
}

/// Output format and name tag: transparent is always PNG; white keeps the input's format.
fn remove_bg_output(background: Background, source: ImageFormat) -> (ImageFormat, &'static str) {
    match background {
        Background::Transparent => (ImageFormat::Png, "no background"),
        Background::White => (source, "white background"),
    }
}

// ---- ONNX Runtime ------------------------------------------------------------------------

/// ONNX Runtime is loaded once per process; the first library path wins (a later install of the
/// pack lands at the same path, and a library can't be unloaded safely anyway).
static RUNTIME: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn load_runtime(library: &Path) -> Result<()> {
    RUNTIME
        .get_or_init(|| {
            #[cfg(windows)]
            preload_directml(library);
            match ort::init_from(library) {
                Ok(env) => {
                    env.with_name("rupantar").commit();
                    Ok(())
                }
                Err(e) => Err(format!("could not load ONNX Runtime from {}: {e}", library.display())),
            }
        })
        .clone()
        .map_err(EngineError::Inference)
}

/// The DirectML build of ONNX Runtime loads `DirectML.dll` by name, which Windows would look for
/// next to Rupantar and in System32 (an older copy), not next to `onnxruntime.dll`. Loading the
/// pack's copy first makes that lookup find it. Untested.
#[cfg(windows)]
fn preload_directml(library: &Path) {
    if let Some(dml) = library.parent().map(|d| d.join("DirectML.dll")).filter(|p| p.is_file()) {
        // SAFETY: loading a DLL runs its initialisers; DirectML's are plain library setup.
        if let Ok(lib) = unsafe { libloading::Library::new(&dml) } {
            std::mem::forget(lib); // keep it loaded for the process
        }
    }
}

/// GPU execution providers to try, in order, with a name for the log. ONNX Runtime refuses one
/// its build doesn't include (e.g. CUDA in the CPU build) or whose drivers are missing, and the
/// next is tried; the CPU always works.
fn gpu_providers() -> Vec<(&'static str, ExecutionProviderDispatch)> {
    vec![
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        ("CUDA", ort::ep::CUDA::default().build().error_on_failure()),
        #[cfg(target_os = "windows")]
        ("DirectML", ort::ep::DirectML::default().build().error_on_failure()),
        #[cfg(target_os = "macos")]
        ("CoreML", ort::ep::CoreML::default().build().error_on_failure()),
    ]
}

fn inference_error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Inference(e.to_string())
}

/// A session for the model on the first execution provider that accepts it, and that
/// provider's name ("CPU" if none did).
fn open_session(model: &Path) -> Result<(Session, &'static str)> {
    for (name, ep) in gpu_providers() {
        let builder = Session::builder().map_err(inference_error)?;
        // DirectML can't use memory patterns.
        let builder = if name == "DirectML" { builder.with_memory_pattern(false).map_err(inference_error)? } else { builder };
        if let Ok(mut builder) = builder.with_execution_providers([ep]) {
            if let Ok(session) = builder.commit_from_file(model) {
                return Ok((session, name));
            }
        }
    }
    let session = Session::builder().map_err(inference_error)?.commit_from_file(model).map_err(inference_error)?;
    Ok((session, "CPU"))
}

/// Runs the model on one input tensor and returns the output logits. Meanwhile the bar eases
/// from `from` toward `to` over about `expected` (no real progress is available), and
/// cancelling the job terminates the run.
fn infer(session: &mut Session, tensor: Vec<f32>, job: &Job, label: &str, (from, to): (f32, f32), expected: Duration) -> Result<Vec<f32>> {
    let input = Tensor::from_array(([1usize, 3, SIDE as usize, SIDE as usize], tensor)).map_err(inference_error)?;
    let options = RunOptions::new().map_err(inference_error)?;
    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            let started = Instant::now();
            while !finished.load(Ordering::SeqCst) {
                if job.is_cancelled() {
                    let _ = options.terminate();
                    return;
                }
                let t = started.elapsed().as_secs_f32() / expected.as_secs_f32().max(0.1);
                job.report(from + (to - from) * (1.0 - (-t).exp()), label);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let result = session
            .run_with_options(ort::inputs![input], &options)
            .and_then(|outputs| outputs[0].try_extract_tensor::<f32>().map(|(_, data)| data.to_vec()));
        finished.store(true, Ordering::SeqCst);
        result
    });
    job.check()?;
    result.map_err(inference_error)
}

// ---- Real-ESRGAN ----------------------------------------------------------------------------

/// `"25.00%"` → 0.25.
fn parse_percent(line: &str) -> Option<f32> {
    let value: f32 = line.trim().strip_suffix('%')?.trim().parse().ok()?;
    (0.0..=100.0).contains(&value).then_some(value / 100.0)
}

/// Whether Real-ESRGAN failed for want of a Vulkan device or loader (its own messages, the
/// dynamic loader's, or Windows' "DLL not found" exit status).
fn no_vulkan(status: &str, stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    ["vkcreateinstance failed", "invalid gpu device", "libvulkan", "vulkan-1.dll", "vkenumeratephysicaldevices failed", "no vulkan device"]
        .iter()
        .any(|m| s.contains(m))
        || status.contains("-1073741515") // 0xC0000135, STATUS_DLL_NOT_FOUND
}

pub struct AiConverter {
    onnxruntime: Option<PathBuf>,
    model: Option<PathBuf>,
    realesrgan: Option<PathBuf>,
}

impl AiConverter {
    pub fn new(config: &EngineConfig) -> Self {
        let file = |p: &Option<PathBuf>| p.clone().filter(|p| p.is_file());
        Self {
            onnxruntime: file(&config.onnxruntime_path),
            model: file(&config.birefnet_path),
            realesrgan: config.realesrgan_path.clone().filter(|p| tools::is_executable(p)),
        }
    }

    fn remove_bg(&self, inputs: &[PathBuf], background: Background, job: &Job) -> Result<Vec<PathBuf>> {
        let (Some(library), Some(model)) = (self.onnxruntime.as_deref(), self.model.as_deref()) else {
            return Err(EngineError::PackMissing(PACK.into()));
        };
        if let Some(missing) = inputs.iter().find(|i| !i.is_file()) {
            return Err(EngineError::InputNotFound(missing.clone()));
        }
        job.report(0.0, "Loading the AI model…");
        load_runtime(library)?;
        job.check()?;
        let (mut session, provider) = open_session(model)?;
        job.note(format!("ONNX Runtime on {provider}"));
        job.check()?;
        let (where_, expected) = match provider {
            "CPU" => ("on the CPU", Duration::from_secs(10)),
            _ => ("on the GPU", Duration::from_secs(2)),
        };
        let n = inputs.len() as f32;
        all_or_nothing(|outputs| {
            for (i, input) in inputs.iter().enumerate() {
                let label = if inputs.len() > 1 {
                    format!("Removing background {where_} ({} of {})…", i + 1, inputs.len())
                } else {
                    format!("Removing background {where_}…")
                };
                // Model loaded: 10%; each image takes an equal share of the rest.
                let share = |f: f32| 0.1 + 0.9 * (i as f32 + f) / n;
                let (img, source) = open(input)?;
                job.report(share(0.05), &label);
                let logits = infer(&mut session, to_tensor(&img), job, &label, (share(0.1), share(0.9)), expected)?;
                let mask = mask_from_logits(&logits, img.width(), img.height())?;
                let (format, tag) = remove_bg_output(background, source);
                let bytes = crate::images::encode(&cut_out(&img, &mask, background), format, None)?;
                outputs.extend(write_siblings(input, extension(format), vec![(tag.into(), bytes)])?);
                job.report(share(1.0), &label);
            }
            Ok(())
        })
    }

    fn upscale(&self, inputs: &[PathBuf], scale: u32, job: &Job) -> Result<Vec<PathBuf>> {
        let Some(program) = self.realesrgan.as_deref() else {
            return Err(EngineError::PackMissing(PACK.into()));
        };
        // Check every input before the first (slow) run.
        let mut jobs = Vec::new();
        for input in inputs {
            if !input.is_file() {
                return Err(EngineError::InputNotFound(input.clone()));
            }
            let reader = ImageReader::open(input)?.with_guessed_format()?;
            let format = match reader.format() {
                Some(f @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) => f,
                other => return Err(EngineError::UnsupportedFormat(other.map_or("unrecognised image data".into(), |f| format!("{f:?}")))),
            };
            let (w, h) = reader.into_dimensions()?;
            if w.max(h) > MAX_UPSCALE_SIDE {
                return Err(EngineError::InvalidOption(format!(
                    "images up to {MAX_UPSCALE_SIDE} px on their longest side can be upscaled; this one is {w} x {h}"
                )));
            }
            jobs.push((input, format, w, h));
        }
        job.note("Vulkan");
        all_or_nothing(|outputs| {
            for (i, (input, format, w, h)) in jobs.into_iter().enumerate() {
                let label = if inputs.len() > 1 { format!("Upscaling {scale}x ({} of {})…", i + 1, inputs.len()) } else { format!("Upscaling {scale}x…") };
                let share = |f: f32| (i as f32 + f) / inputs.len() as f32;
                job.report(share(0.0), &label);
                let img = self.esrgan(program, input, format, job, &mut |f| job.report(share(0.05 + 0.85 * f), &label))?;
                let img = if scale == 4 { img } else { img.resize_exact(w * scale, h * scale, FilterType::Lanczos3) };
                let bytes = crate::images::encode(&img, format, None)?;
                outputs.extend(write_siblings(input, extension(format), vec![(format!("upscaled {scale}x"), bytes)])?);
                job.report(share(1.0), &label);
            }
            Ok(())
        })
    }

    /// One 4x Real-ESRGAN run on a copy of `input` in a private work dir (odd file names can't
    /// trip up its argument parsing). `progress` gets 0..1.
    fn esrgan(&self, program: &Path, input: &Path, format: ImageFormat, job: &Job, progress: &mut dyn FnMut(f32)) -> Result<DynamicImage> {
        let work = WorkDir::create()?;
        let source = work.0.join(format!("input.{}", extension(format)));
        std::fs::copy(input, &source)?;
        let output = work.0.join("output.png");
        let models = program.parent().unwrap_or_else(|| Path::new(".")).join("models");
        let args = [
            "-i".as_ref(),
            source.as_os_str(),
            "-o".as_ref(),
            output.as_os_str(),
            "-n".as_ref(),
            ESRGAN_MODEL.as_ref(),
            "-s".as_ref(),
            "4".as_ref(),
            "-m".as_ref(),
            models.as_os_str(),
            "-f".as_ref(),
            "png".as_ref(),
        ];
        // Images with alpha are processed twice (colour, then alpha): keep the bar moving forward.
        let mut best = 0f32;
        let ran = tools::run_job(ESRGAN, program, args, Some(&work.0), TIMEOUT, job, &mut |line| {
            if let Some(f) = parse_percent(line) {
                best = best.max(f);
                progress(best);
            }
        });
        let stderr = match ran {
            Ok(stderr) => stderr,
            Err(EngineError::ToolFailed { status, stderr, .. }) if no_vulkan(&status, &stderr) => {
                return Err(EngineError::NoGpu(tools::readable(&stderr)));
            }
            Err(e) => return Err(e),
        };
        if !output.is_file() {
            if no_vulkan("", &stderr) {
                return Err(EngineError::NoGpu(tools::readable(&stderr)));
            }
            return Err(EngineError::ToolFailed { tool: ESRGAN.into(), status: "no output".into(), stderr: tools::readable(&stderr) });
        }
        Ok(image::open(&output)?)
    }
}

/// Runs `f`, which pushes each file it writes; if it fails (or is cancelled) part-way, the
/// files of the inputs already done are removed again.
fn all_or_nothing(f: impl FnOnce(&mut Vec<PathBuf>) -> Result<()>) -> Result<Vec<PathBuf>> {
    let mut outputs = Vec::new();
    match f(&mut outputs) {
        Ok(()) => Ok(outputs),
        Err(e) => {
            for p in &outputs {
                let _ = std::fs::remove_file(p);
            }
            Err(e)
        }
    }
}

/// Marks an operation and its sub-options as needing the pack (as the Office pack's are, installed
/// or not), and unavailable until it is installed.
fn needs_pack(op: Operation, ready: bool) -> Operation {
    let children = op.children.iter().cloned().map(|c| c.available(ready).needs_pack(PACK)).collect();
    op.with_children(children).available(ready).needs_pack(PACK)
}

impl Converter for AiConverter {
    fn name(&self) -> &'static str {
        "AI"
    }

    fn can_handle(&self, mime: &str) -> bool {
        image_format(mime).is_some()
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if !self.can_handle(mime) {
            return Vec::new();
        }
        let remove_bg = catalog::op(REMOVE_BG).with_children(vec![
            Operation::new("image.remove_bg.transparent", "Transparent PNG", "Grid2x2"),
            Operation::new("image.remove_bg.white", "White background", "Square"),
        ]);
        let upscale = catalog::op(UPSCALE)
            .with_children(vec![Operation::new("image.upscale.2", "2\u{d7}", "ZoomIn"), Operation::new("image.upscale.4", "4\u{d7}", "Scaling")]);
        vec![
            needs_pack(remove_bg, self.onnxruntime.is_some() && self.model.is_some()),
            needs_pack(upscale, self.realesrgan.is_some()),
        ]
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>> {
        self.run_job(op, inputs, options, &Job::default())
    }

    fn run_job(&self, op: &Operation, inputs: &[PathBuf], options: &Options, job: &Job) -> Result<Vec<PathBuf>> {
        match parse_action(&op.id, options)? {
            Action::RemoveBg(background) => self.remove_bg(inputs, background, job),
            Action::Upscale(scale) => self.upscale(inputs, scale, job),
        }
    }
}

#[cfg(test)]
mod tests;
