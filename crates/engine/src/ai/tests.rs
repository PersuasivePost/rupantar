use super::*;
use image::{GenericImageView, Rgba, RgbaImage};
use std::sync::Mutex;

fn converter(onnxruntime: Option<PathBuf>, model: Option<PathBuf>, realesrgan: Option<PathBuf>) -> AiConverter {
    AiConverter::new(&EngineConfig { onnxruntime_path: onnxruntime, birefnet_path: model, realesrgan_path: realesrgan, ..EngineConfig::default() })
}

/// Any existing executable stands in for a pack file when only presence matters.
fn exe() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn op(id: &str) -> Operation {
    Operation::new(id, id, "x")
}

fn png(dir: &Path, name: &str, img: &RgbaImage) -> PathBuf {
    let path = dir.join(name);
    img.save(&path).unwrap();
    path
}

/// `(id, available, pack)`.
type Flags = (String, bool, Option<String>);

fn summary(ops: &[Operation]) -> Vec<(String, bool, Option<String>, Vec<Flags>)> {
    ops.iter()
        .map(|o| (o.id.clone(), o.available, o.pack.clone(), o.children.iter().map(|c| (c.id.clone(), c.available, c.pack.clone())).collect()))
        .collect()
}

// ---- listing and parsing ----------------------------------------------------------------

#[test]
fn without_the_pack_both_operations_need_it_with_their_sub_options() {
    let ops = converter(None, None, None).operations("image/png");
    let pack = Some(PACK.to_string());
    assert_eq!(
        summary(&ops),
        [
            (
                "image.remove_bg".into(),
                false,
                pack.clone(),
                vec![("image.remove_bg.transparent".into(), false, pack.clone()), ("image.remove_bg.white".into(), false, pack.clone())]
            ),
            ("image.upscale".into(), false, pack.clone(), vec![("image.upscale.2".into(), false, pack.clone()), ("image.upscale.4".into(), false, pack)]),
        ]
    );
    assert_eq!((ops[0].label.as_str(), ops[0].children[0].label.as_str()), ("Remove background", "Transparent PNG"));
    assert!(!ops[0].more, "Remove background keeps its top wedge while waiting for the pack");
}

#[test]
fn with_the_pack_everything_is_available() {
    let ops = converter(Some(exe()), Some(exe()), Some(exe())).operations("image/jpeg");
    let pack = Some(PACK.to_string());
    assert!(ops.iter().all(|o| o.available && o.pack == pack && o.children.iter().all(|c| c.available && c.pack == pack)));
    // Each half of the pack is checked on its own.
    let ops = converter(Some(exe()), None, Some(exe())).operations("image/webp");
    assert_eq!((ops[0].available, ops[1].available), (false, true));
    assert!(converter(None, None, None).operations("application/pdf").is_empty());
    assert!(!converter(None, None, None).can_handle("text/csv"));
}

#[test]
fn ids_and_options() {
    let none = Options::new();
    let opts = |k: &str, v: &str| Options::from([(k.to_string(), v.to_string())]);
    assert_eq!(parse_action("image.remove_bg.white", &none).unwrap(), Action::RemoveBg(Background::White));
    assert_eq!(parse_action("image.remove_bg", &none).unwrap(), Action::RemoveBg(Background::Transparent));
    assert_eq!(parse_action("image.remove_bg.transparent", &opts("background", "white")).unwrap(), Action::RemoveBg(Background::White), "options win");
    assert_eq!(parse_action("image.upscale.2", &none).unwrap(), Action::Upscale(2));
    assert_eq!(parse_action("image.upscale", &opts("scale", "4")).unwrap(), Action::Upscale(4));
    assert!(matches!(parse_action("image.upscale.3", &none), Err(EngineError::InvalidOption(_))));
    assert!(matches!(parse_action("image.remove_bg.green", &none), Err(EngineError::InvalidOption(_))));
    assert!(matches!(parse_action("image.upscaled", &none), Err(EngineError::UnknownOperation(_))));
    assert!(matches!(parse_action("image.resize.pct.50", &none), Err(EngineError::UnknownOperation(_))));
}

#[test]
fn running_without_the_pack_is_pack_missing() {
    let d = tempfile::tempdir().unwrap();
    let input = png(d.path(), "a.png", &RgbaImage::new(4, 4));
    let c = converter(None, None, None);
    for id in ["image.remove_bg.transparent", "image.upscale.4"] {
        let err = c.run(&op(id), std::slice::from_ref(&input), &Options::new()).unwrap_err();
        assert!(matches!(&err, EngineError::PackMissing(p) if p == PACK), "{id}: {err}");
    }
    assert!(matches!(c.run(&op("image.rotate.90"), &[input], &Options::new()), Err(EngineError::UnknownOperation(_))));
}

// ---- pre- and post-processing ---------------------------------------------------------------

#[test]
fn the_input_tensor_is_normalised_nchw() {
    // Left half one colour, right half another; resizing keeps the halves.
    let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(8, 4, |x, _| if x < 4 { Rgba([255, 0, 128, 255]) } else { Rgba([0, 255, 0, 0]) }));
    let t = to_tensor(&img);
    let plane = (SIDE * SIDE) as usize;
    assert_eq!(t.len(), 3 * plane);
    let at = |c: usize, x: u32, y: u32| t[c * plane + (y * SIDE + x) as usize];
    let norm = |v: f32, c: usize| (v / 255.0 - MEAN[c]) / STD[c];
    let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
    assert!(close(at(0, 10, 500), norm(255.0, 0)) && close(at(1, 10, 500), norm(0.0, 1)) && close(at(2, 10, 500), norm(128.0, 2)));
    assert!(close(at(0, 1000, 20), norm(0.0, 0)) && close(at(1, 1000, 20), norm(255.0, 1)), "alpha is ignored");
}

#[test]
fn the_mask_is_the_sigmoid_scaled_back_to_the_image() {
    // Logits: strongly foreground on the left half, background on the right, undecided on row 0.
    let logits: Vec<f32> = (0..SIDE * SIDE)
        .map(|i| {
            let (x, y) = (i % SIDE, i / SIDE);
            if y == 0 {
                0.0
            } else if x < SIDE / 2 {
                12.0
            } else {
                -12.0
            }
        })
        .collect();
    let full = mask_from_logits(&logits, SIDE, SIDE).unwrap();
    assert_eq!((full.get_pixel(0, 0)[0], full.get_pixel(3, 9)[0], full.get_pixel(900, 9)[0]), (128, 255, 0));
    let small = mask_from_logits(&logits, 40, 30).unwrap();
    assert_eq!(small.dimensions(), (40, 30));
    assert_eq!((small.get_pixel(5, 20)[0], small.get_pixel(35, 20)[0]), (255, 0));
    assert!(matches!(mask_from_logits(&[0.0; 10], 4, 4), Err(EngineError::Inference(_))));
}

#[test]
fn cut_out_keeps_existing_alpha_and_composites_on_white() {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(2, 1, |x, _| if x == 0 { Rgba([200, 20, 20, 255]) } else { Rgba([10, 10, 10, 100]) }));
    let mask = GrayImage::from_fn(2, 1, |x, _| image::Luma([if x == 0 { 64 } else { 255 }]));
    let clear = cut_out(&img, &mask, Background::Transparent).to_rgba8();
    assert_eq!(clear.get_pixel(0, 0), &Rgba([200, 20, 20, 64]), "the mask lowers alpha");
    assert_eq!(clear.get_pixel(1, 0), &Rgba([10, 10, 10, 100]), "existing transparency is kept");
    let white = cut_out(&img, &mask, Background::White);
    assert!(!white.color().has_alpha());
    let p = white.to_rgb8();
    assert_eq!(p.get_pixel(0, 0), &image::Rgb([241, 196, 196])); // 25% red over white
    assert_eq!(remove_bg_output(Background::Transparent, ImageFormat::Jpeg), (ImageFormat::Png, "no background"));
    assert_eq!(remove_bg_output(Background::White, ImageFormat::Jpeg), (ImageFormat::Jpeg, "white background"));
}

#[test]
fn progress_lines_and_vulkan_failures() {
    assert_eq!(parse_percent("25.00%"), Some(0.25));
    assert_eq!(parse_percent(" 100.00% "), Some(1.0));
    assert_eq!(parse_percent("[0 NVIDIA GeForce]  queueC=2[8]"), None);
    assert_eq!(parse_percent("120%"), None);
    assert!(no_vulkan("exit code 255", "vkCreateInstance failed -9\ninvalid gpu device"));
    assert!(no_vulkan("exit code 127", "error while loading shared libraries: libvulkan.so.1: cannot open shared object file"));
    assert!(no_vulkan("exit code -1073741515", ""));
    assert!(!no_vulkan("exit code 255", "decode image input.png failed"));
}

// ---- Real-ESRGAN, with a stand-in program (it copies its input, so "4x" keeps the size) -------

#[cfg(unix)]
mod esrgan {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// `realesrgan-ncnn-vulkan` stand-in in `dir` running `body`; `$2` is the input, `$4` the output.
    fn fake(dir: &Path, body: &str) -> PathBuf {
        let program = dir.join("realesrgan-ncnn-vulkan");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    const COPY: &str = r#"echo "[0 Fake GPU]" >&2; for p in 0.00 50.00 100.00; do echo "$p%" >&2; done; cp "$2" "$4""#;

    fn photo() -> RgbaImage {
        RgbaImage::from_fn(30, 20, |x, y| Rgba([(x * 8) as u8, (y * 12) as u8, 90, 255]))
    }

    #[test]
    fn runs_the_tool_reports_progress_and_halves_4x_for_2x() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let input = png(d.path(), "my photo.png", &photo());
        let c = converter(None, None, Some(fake(tools.path(), COPY)));
        let seen = Mutex::new(Vec::new());
        let job = Job::new(move |p| seen.lock().unwrap().push(p.fraction));
        let out = c.run_job(&op("image.upscale.2"), std::slice::from_ref(&input), &Options::new(), &job).unwrap();
        assert_eq!(out, [d.path().join("my photo (upscaled 2x).png")]);
        // The stand-in returns the input's size as its "4x", so 2x is that scaled by 2.
        assert_eq!(image::open(&out[0]).unwrap().dimensions(), (60, 40));
        let four = c.run(&op("image.upscale.4"), &[input], &Options::new()).unwrap();
        assert_eq!(image::open(&four[0]).unwrap().to_rgba8(), photo(), "4x is the tool's output, unchanged");
    }

    #[test]
    fn progress_follows_the_tool_output() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let input = png(d.path(), "a.png", &photo());
        let c = converter(None, None, Some(fake(tools.path(), COPY)));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let job = Job::new(move |p| log.lock().unwrap().push((p.fraction, p.label.clone())));
        c.run_job(&op("image.upscale.4"), &[input], &Options::new(), &job).unwrap();
        let seen = seen.lock().unwrap();
        let fractions: Vec<f32> = seen.iter().map(|p| p.0).collect();
        assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "{fractions:?}");
        assert!(fractions.iter().any(|f| (f - 0.475).abs() < 1e-5), "50% of the tool's run: {fractions:?}");
        assert_eq!(fractions.last(), Some(&1.0));
        assert!(seen.iter().all(|p| p.1 == "Upscaling 4x…"));
        assert_eq!(job.take_note().as_deref(), Some("Vulkan"));
    }

    #[test]
    fn without_vulkan_the_error_says_so() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let input = png(d.path(), "a.png", &photo());
        let program = fake(tools.path(), "echo 'vkCreateInstance failed -9' >&2; echo 'invalid gpu device' >&2; exit 255");
        let err = converter(None, None, Some(program)).run(&op("image.upscale.4"), &[input], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::NoGpu(_)), "{err}");
        assert!(err.to_string().starts_with("Upscale needs a graphics card with Vulkan support"), "{err}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "nothing written");
    }

    #[test]
    fn cancelling_stops_the_tool_and_keeps_nothing() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let input = png(d.path(), "a.png", &photo());
        let c = converter(None, None, Some(fake(tools.path(), "echo '0.00%' >&2; sleep 10")));
        let job = std::sync::Arc::new(Job::default());
        let canceller = std::sync::Arc::clone(&job);
        let started = Instant::now();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            canceller.cancel();
        });
        let err = c.run_job(&op("image.upscale.4"), &[input], &Options::new(), &job).unwrap_err();
        t.join().unwrap();
        assert!(matches!(err, EngineError::Cancelled), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_failure_on_a_later_image_removes_the_earlier_outputs() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let count = tools.path().join("count");
        // Succeeds once, then fails.
        let body = format!(r#"if [ -e "{0}" ]; then echo 'decode failed' >&2; exit 1; fi; touch "{0}"; cp "$2" "$4""#, count.display());
        let c = converter(None, None, Some(fake(tools.path(), &body)));
        let inputs = [png(d.path(), "a.png", &photo()), png(d.path(), "b.png", &photo())];
        let err = c.run(&op("image.upscale.4"), &inputs, &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::ToolFailed { .. }), "{err}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2, "a (upscaled 4x).png was removed again");
    }

    #[test]
    fn large_or_missing_inputs_are_rejected_before_running() {
        let tools = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let c = converter(None, None, Some(fake(tools.path(), "exit 99")));
        let wide = png(d.path(), "wide.png", &RgbaImage::new(MAX_UPSCALE_SIDE + 1, 1));
        assert!(matches!(c.run(&op("image.upscale.2"), &[wide], &Options::new()), Err(EngineError::InvalidOption(_))));
        let gone = d.path().join("gone.png");
        assert!(matches!(c.run(&op("image.upscale.2"), &[gone], &Options::new()), Err(EngineError::InputNotFound(_))));
    }
}

// ---- with the real AI pack (skipped when it isn't installed) ----------------------------------

/// The installed AI pack's files: `RUPANTAR_AI_PACK` (a pack directory with `installed.json`),
/// else the app's own pack directory. `None` (skip) when there is none.
fn real_pack() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or(home.map(|h| h.join(".local/share")))
    };
    let dir = std::env::var_os("RUPANTAR_AI_PACK").map(PathBuf::from).or(data.map(|d| d.join("app.rupantar/packs/ai")))?;
    let marker: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("installed.json")).ok()?).ok()?;
    let file = |key: &str| marker["files"][key].as_str().map(|p| dir.join(p)).filter(|p| p.is_file());
    let found = (file("onnxruntime")?, file("birefnet")?, file("realesrgan")?);
    Some(found)
}

fn real() -> Option<AiConverter> {
    match real_pack() {
        Some((ort, model, esrgan)) => Some(converter(Some(ort), Some(model), Some(esrgan))),
        None => {
            eprintln!("skipping: the AI pack is not installed (set RUPANTAR_AI_PACK to its directory)");
            None
        }
    }
}

/// A red disc on a plain light background: an easy subject.
fn subject() -> RgbaImage {
    RgbaImage::from_fn(480, 360, |x, y| {
        let (dx, dy) = (x as f32 - 240.0, y as f32 - 180.0);
        if dx * dx + dy * dy < 110.0 * 110.0 {
            Rgba([200, 30, 40, 255])
        } else {
            Rgba([235, 235, 228, 255])
        }
    })
}

#[test]
fn removes_the_background_of_a_simple_picture() {
    let Some(c) = real() else { return };
    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("disc.jpg");
    DynamicImage::ImageRgba8(subject()).to_rgb8().save(&input).unwrap();
    let job = Job::default();
    let started = Instant::now();
    let out = c.run_job(&op("image.remove_bg.transparent"), std::slice::from_ref(&input), &Options::new(), &job).unwrap();
    eprintln!("remove background: {:?} via {:?}", started.elapsed(), job.take_note());
    assert_eq!(out, [d.path().join("disc (no background).png")]);
    let cut = image::open(&out[0]).unwrap().to_rgba8();
    assert_eq!(cut.dimensions(), (480, 360));
    let alpha = |x, y| cut.get_pixel(x, y)[3];
    assert!(alpha(240, 180) > 200, "the disc stays: {}", alpha(240, 180));
    for (x, y) in [(5, 5), (474, 5), (5, 354), (474, 354)] {
        assert!(alpha(x, y) < 40, "corner ({x}, {y}) is background: {}", alpha(x, y));
    }

    let white = c.run(&op("image.remove_bg.white"), &[input], &Options::new()).unwrap();
    assert_eq!(white, [d.path().join("disc (white background).jpg")]);
    let w = image::open(&white[0]).unwrap().to_rgb8();
    assert!(w.get_pixel(5, 5).0.iter().all(|c| *c > 245), "{:?}", w.get_pixel(5, 5));
}

#[test]
fn background_removal_can_be_cancelled() {
    let Some(c) = real() else { return };
    let d = tempfile::tempdir().unwrap();
    let input = png(d.path(), "disc.png", &subject());
    let job = std::sync::Arc::new(Job::default());
    let canceller = std::sync::Arc::clone(&job);
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });
    let err = c.run_job(&op("image.remove_bg.transparent"), &[input], &Options::new(), &job).unwrap_err();
    t.join().unwrap();
    assert!(matches!(err, EngineError::Cancelled), "{err}");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "nothing written");
}

#[test]
fn upscales_on_the_gpu() {
    let Some(c) = real() else { return };
    let d = tempfile::tempdir().unwrap();
    let input = png(d.path(), "small.png", &RgbaImage::from_fn(64, 48, |x, y| Rgba([(x * 4) as u8, (y * 5) as u8, 128, 255])));
    let job = Job::default();
    let out = match c.run_job(&op("image.upscale.4"), std::slice::from_ref(&input), &Options::new(), &job) {
        Err(EngineError::NoGpu(why)) => return eprintln!("skipping: no Vulkan device ({why})"),
        other => other.unwrap(),
    };
    assert_eq!(image::open(&out[0]).unwrap().dimensions(), (256, 192));
    let two = c.run(&op("image.upscale.2"), &[input], &Options::new()).unwrap();
    assert_eq!(two, [d.path().join("small (upscaled 2x).png")]);
    assert_eq!(image::open(&two[0]).unwrap().dimensions(), (128, 96));
}
