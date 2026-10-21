//! Install tests against a local HTTP server and a synthetic LibreOffice tarball (a few tiny
//! debs with the real layout), so nothing here touches the network.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use super::*;

// ---- fixtures -----------------------------------------------------------------------------

fn tar_file(builder: &mut tar::Builder<impl Write>, path: &str, mode: u32, data: &[u8]) {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(mode);
    h.set_entry_type(tar::EntryType::Regular);
    // Written raw so tests can also build the unsafe paths `tar::Builder` refuses.
    let name = &mut h.as_gnu_mut().unwrap().name;
    name[..path.len()].copy_from_slice(path.as_bytes());
    h.set_cksum();
    builder.append(&h, data).unwrap();
}

/// A .deb whose data.tar.xz holds `files` (path, mode, contents).
fn deb(files: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut data = tar::Builder::new(Vec::new());
    for (path, mode, bytes) in files {
        tar_file(&mut data, path, *mode, bytes);
    }
    let data = data.into_inner().unwrap();
    let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    xz.write_all(&data).unwrap();
    let data_xz = xz.finish().unwrap();

    let mut ar = ar::Builder::new(Vec::new());
    for (id, bytes) in [("debian-binary", &b"2.0\n"[..]), ("control.tar.xz", b""), ("data.tar.xz", &data_xz)] {
        ar.append(&ar::Header::new(id.as_bytes().to_vec(), bytes.len() as u64), bytes).unwrap();
    }
    ar.into_inner().unwrap()
}

/// The release tarball: `LibreOffice_X_Linux_x86-64_deb/DEBS/*.deb`, gzipped.
fn tarball(debs: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut t = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    tar_file(&mut t, "LibreOffice_X_deb/readmes/README_en-US", 0o644, b"readme");
    for (name, bytes) in debs {
        tar_file(&mut t, &format!("LibreOffice_X_deb/DEBS/{name}"), 0o644, bytes);
    }
    t.into_inner().unwrap().finish().unwrap()
}

fn good_tarball() -> Vec<u8> {
    tarball(&[
        ("libreoffice26.2_26.2.6.3-3_amd64.deb", deb(&[("./opt/libreoffice26.2/program/soffice.bin", 0o755, b"#!/bin/sh\n")])),
        (
            "libobasis26.2-core_26.2.6.3-3_amd64.deb",
            deb(&[
                ("./opt/libreoffice26.2/program/libmerged.so", 0o644, b"elf"),
                ("./opt/libreoffice26.2/share/registry/main.xcd", 0o644, b"<xcd/>"),
                ("./usr/share/applications/x.desktop", 0o644, b"outside the tree"),
            ]),
        ),
        ("libreoffice26.2-dict-en_26.2.6.3-3_amd64.deb", deb(&[("./opt/libreoffice26.2/share/extensions/dict-en/x", 0o644, b"skip")])),
    ])
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn leak_urls(urls: Vec<String>) -> &'static [&'static str] {
    Box::leak(urls.into_iter().map(leak).collect::<Vec<_>>().into_boxed_slice())
}

/// An Office-like build: one LibreOffice tarball served at `urls`.
fn build(urls: Vec<String>, body: &[u8]) -> Build {
    let c = Component {
        key: office::SOFFICE,
        urls: leak_urls(urls),
        file: "lo.tar.gz",
        sha256: leak(sha256(body)),
        size: body.len() as u64,
        archive: Archive::DebTarball,
        keep: &[],
        program: None,
        executable: false,
    };
    builds(vec![c])
}

fn builds(components: Vec<Component>) -> Build {
    Build {
        version: "26.2.6",
        os: "linux",
        arch: "x86_64",
        components: Box::leak(components.into_boxed_slice()),
        installed_size: 0,
        tested: true,
    }
}

// ---- a tiny HTTP server -------------------------------------------------------------------

struct Server {
    base: String,
    /// The Range header of each request for the file (None: no Range).
    ranges: Arc<Mutex<Vec<Option<String>>>>,
}

/// Serves `body` at `/file` (with Range support) and 404 elsewhere. With `cut`, the first
/// response stops after that many bytes and closes the connection.
fn serve(body: Vec<u8>, cut: Option<usize>) -> Server {
    serve_files(vec![("/file".into(), body)], cut)
}

/// Serves each `(path, body)`, like `serve`.
fn serve_files(files: Vec<(String, Vec<u8>)>, cut: Option<usize>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&ranges);
    std::thread::spawn(move || {
        let mut cut = cut;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut range = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("range:") {
                    range = Some(v.trim().to_string());
                }
                if request.is_empty() {
                    request = line;
                }
            }
            let path = request.split(' ').nth(1).unwrap_or("");
            let Some((_, body)) = files.iter().find(|(p, _)| p == path) else {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            };
            seen.lock().unwrap().push(range.clone());
            let start = range
                .as_deref()
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
                .unwrap_or(0);
            let rest = &body[start..];
            let head = if start > 0 {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n",
                    rest.len(),
                    body.len() - 1,
                    body.len()
                )
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
            };
            let _ = stream.write_all(head.as_bytes());
            let n = cut.take().map_or(rest.len(), |c| c.min(rest.len()));
            let _ = stream.write_all(&rest[..n]);
        }
    });
    Server { base, ranges }
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---- tests --------------------------------------------------------------------------------

#[test]
fn deb_components() {
    let c = extract::component_for_tests;
    assert_eq!(c("libobasis26.2-core_26.2.6.3-3_amd64.deb"), Some("core"));
    assert_eq!(c("libreoffice26.2_26.2.6.3-3_amd64.deb"), Some(""));
    assert_eq!(c("libreoffice26.2-en-us_26.2.6.3-3_amd64.deb"), Some("en-us"));
    assert_eq!(c("libreoffice26.2-debian-menus_26.2.6-3_all.deb"), Some("debian-menus"));
    assert_eq!(c("something-else_1_amd64.deb"), None);
    assert_eq!(c("README"), None);
}

#[test]
fn the_manifest_is_complete() {
    for b in office::BUILDS {
        let [c] = b.components else { panic!("one LibreOffice download per build") };
        assert_eq!(c.sha256.len(), 64, "{}", c.file);
        assert!(c.sha256.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(c.urls.len() >= 2 && c.urls.iter().all(|u| u.starts_with("https://")), "{}", c.file);
        assert!(c.urls[0].ends_with(c.file), "{}", c.file);
        assert!(c.size > 100_000_000 && b.installed_size > c.size);
        assert_eq!((b.version, c.key, c.program), (office::VERSION, office::SOFFICE, None));
    }
    for (os, arch) in [("linux", "x86_64"), ("linux", "aarch64"), ("windows", "x86_64"), ("windows", "aarch64"), ("macos", "x86_64"), ("macos", "aarch64")] {
        assert!(office::build_for(os, arch).is_some(), "{os}/{arch}");
    }
    assert!(office::build_for("freebsd", "x86_64").is_none());
    // Only the build that was actually installed on a real machine is marked tested.
    let tested: Vec<_> = office::BUILDS.iter().filter(|b| b.tested).map(|b| (b.os, b.arch)).collect();
    assert_eq!(tested, [("linux", "x86_64")]);
}

#[test]
fn the_ai_manifest_is_complete() {
    for b in ai::BUILDS {
        let keys: Vec<&str> = b.components.iter().map(|c| c.key).collect();
        for key in [ai::ONNXRUNTIME, ai::BIREFNET, ai::REALESRGAN] {
            assert_eq!(keys.iter().filter(|k| **k == key).count(), 1, "{}/{}: {key}", b.os, b.arch);
        }
        assert_eq!(keys.contains(&ai::DIRECTML), b.os == "windows", "DirectML ships with the Windows build");
        for c in b.components {
            assert_eq!(c.sha256.len(), 64, "{}", c.file);
            assert!(c.sha256.bytes().all(|c| c.is_ascii_hexdigit()));
            assert!(!c.urls.is_empty() && c.urls.iter().all(|u| u.starts_with("https://")), "{}", c.file);
            assert!(c.urls[0].ends_with(&format!("/{}", c.file)) && !c.file.contains('/'), "{}", c.file);
            let program = c.program.expect("every AI file is recorded");
            assert!(!c.keep.is_empty());
            assert!(c.keep.iter().any(|(pattern, folder)| program == format!("{folder}/{}", Path::new(pattern).file_name().map_or(c.file, |n| n.to_str().unwrap()))), "{}: {program} is unpacked", c.file);
            assert_eq!(c.executable, c.key == ai::REALESRGAN);
        }
        assert!(b.size() > 250_000_000 && b.installed_size > 250_000_000, "{}/{}", b.os, b.arch);
        assert_eq!(b.version, ai::VERSION);
    }
    for (os, arch) in [("linux", "x86_64"), ("windows", "x86_64"), ("macos", "x86_64"), ("macos", "aarch64")] {
        assert!(ai::build_for(os, arch).is_some(), "{os}/{arch}");
    }
    // Real-ESRGAN has no ARM Linux or Windows build, so neither does the pack.
    assert!(ai::build_for("linux", "aarch64").is_none() && ai::build_for("windows", "aarch64").is_none());
    let tested: Vec<_> = ai::BUILDS.iter().filter(|b| b.tested).map(|b| (b.os, b.arch)).collect();
    assert_eq!(tested, [("linux", "x86_64")]);
}

#[test]
fn keep_patterns() {
    let m = extract::matches_for_tests;
    assert!(m("*/lib/libonnxruntime.so.1.28.3", "onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so.1.28.3"));
    assert!(m("*/lib/libonnxruntime.so.1.28.3", "./onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so.1.28.3"));
    assert!(!m("*/lib/libonnxruntime.1.28.3.dylib", "x/lib/libonnxruntime.1.28.3.dylib.dSYM/Contents/Resources/DWARF/libonnxruntime.1.28.3.dylib"));
    assert!(m("models/realesrgan-x4plus.bin", "models/realesrgan-x4plus.bin"));
    assert!(!m("models/realesrgan-x4plus.bin", "models/realesrgan-x4plus-anime.bin"));
    assert!(m("a*b*c", "a-b-c") && m("a*b*c", "abc") && !m("a*b*c", "acb") && !m("ab*ba", "aba"));
}

#[test]
fn installs_only_the_needed_tree_and_removes_cleanly() {
    let body = good_tarball();
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    assert!(pack.installed().is_none() && pack.program().is_none());

    let mut phases = Vec::new();
    let files = pack
        .install(&b, &mut |p| {
            if phases.last() != Some(&p.phase) {
                phases.push(p.phase);
            }
            assert!(p.done <= p.total);
        }, &no_cancel())
        .unwrap();
    assert_eq!(phases, [Phase::Download, Phase::Verify, Phase::Extract]);
    let program = files[office::SOFFICE].clone();

    let tree = pack.dir().join("libreoffice-26.2.6");
    assert_eq!(program, tree.join("program/soffice.bin"));
    assert_eq!(pack.program(), Some(program.clone()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&program).unwrap().permissions().mode() & 0o111, 0o111, "still executable");
    }
    assert_eq!(fs::read(tree.join("share/registry/main.xcd")).unwrap(), b"<xcd/>");
    assert!(!tree.join("share/extensions").exists(), "dictionaries are left out");
    assert!(!tree.join("usr").exists() && !tree.join("opt").exists(), "only opt/libreoffice*/, flattened");
    assert!(!pack.dir().join("download").exists() && !pack.dir().join("staging").exists());
    let installed = pack.installed().unwrap();
    assert_eq!(installed.version, "26.2.6");
    assert!(installed.size > 0);

    pack.remove().unwrap();
    assert!(pack.installed().is_none() && !pack.dir().exists());
    pack.remove().unwrap(); // removing nothing is fine
}

#[test]
fn a_dropped_connection_resumes_with_a_range_request() {
    let body = good_tarball();
    let server = serve(body.clone(), Some(body.len() / 2));
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    Pack::office(d.path()).install(&b, &mut |_| {}, &no_cancel()).unwrap();
    let ranges = server.ranges.lock().unwrap().clone();
    assert_eq!(ranges, [None, Some(format!("bytes={}-", body.len() / 2))]);
}

#[test]
fn falls_back_to_the_next_url() {
    let body = good_tarball();
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/missing", server.base), format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    assert!(Pack::office(d.path()).install(&b, &mut |_| {}, &no_cancel()).is_ok());

    let b = build(vec![format!("{}/missing", server.base)], &body);
    let err = Pack::office(d.path()).install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(&err, PackError::Network(m) if m.contains("HTTP 404")), "{err}");
}

#[test]
fn a_checksum_mismatch_deletes_the_download() {
    let body = good_tarball();
    let server = serve(body.clone(), None);
    let mut c = build(vec![format!("{}/file", server.base)], &body).components[0].clone();
    c.sha256 = leak("0".repeat(64));
    let b = builds(vec![c]);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    let err = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(&err, PackError::Checksum { actual } if *actual == sha256(&body)), "{err}");
    assert_eq!(pack.downloaded(&b), 0);
    assert!(pack.installed().is_none() && !pack.dir().join("staging").exists());
}

#[test]
fn cancelling_keeps_the_partial_download_for_next_time() {
    let body = good_tarball();
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());

    let cancel = AtomicBool::new(false);
    let err = pack
        .install(&b, &mut |p| {
            if p.phase == Phase::Download && p.done > 0 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }, &cancel)
        .unwrap_err();
    assert!(matches!(err, PackError::Cancelled));
    assert!(pack.downloaded(&b) > 0 && pack.installed().is_none());

    // Cancelling while unpacking leaves no staging tree behind.
    let cancel = AtomicBool::new(false);
    let err = pack
        .install(&b, &mut |p| {
            if p.phase == Phase::Extract {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }, &cancel)
        .unwrap_err();
    assert!(matches!(err, PackError::Cancelled));
    assert!(!pack.dir().join("staging").exists() && pack.installed().is_none());

    pack.install(&b, &mut |_| {}, &no_cancel()).unwrap();
    assert!(pack.installed().is_some());
}

#[test]
fn unsafe_paths_are_rejected() {
    let body = tarball(&[(
        "libreoffice26.2_1_amd64.deb",
        deb(&[
            ("./opt/libreoffice26.2/program/soffice.bin", 0o755, b"x"),
            ("./opt/libreoffice26.2/../../../escaped", 0o644, b"evil"),
        ]),
    )]);
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(&d.path().join("packs"));
    let err = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(&err, PackError::Extract(m) if m.contains("unsafe path")), "{err}");
    assert!(!d.path().join("escaped").exists() && !d.path().join("packs/escaped").exists());
    assert!(pack.installed().is_none() && !pack.dir().join("staging").exists());
}

#[test]
fn a_tarball_without_libreoffice_is_an_error() {
    let body = tarball(&[("unrelated_1_amd64.deb", deb(&[("./opt/x", 0o644, b"x")]))]);
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    let err = Pack::office(d.path()).install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(err, PackError::Extract(_)), "{err}");
}

/// The real pinned Linux build, served locally from a copy already on disk:
/// `RUPANTAR_LO_TARBALL=/path/to/LibreOffice_26.2.6_Linux_x86-64_deb.tar.gz cargo test -p packs -- --ignored`.
#[test]
#[ignore = "needs the real 217 MB tarball (RUPANTAR_LO_TARBALL)"]
fn installs_the_real_pinned_build() {
    let Some(path) = std::env::var_os("RUPANTAR_LO_TARBALL") else { return };
    let real = office::build_for("linux", "x86_64").unwrap();
    let body = fs::read(path).unwrap();
    let server = serve(body, None);
    let c = Component { urls: leak_urls(vec![format!("{}/file", server.base)]), ..real.components[0].clone() };
    let b = Build { components: Box::leak(vec![c].into_boxed_slice()), ..real.clone() };
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    let started = std::time::Instant::now();
    let program = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap().remove(office::SOFFICE).unwrap();
    eprintln!("installed in {:?}, {} MB", started.elapsed(), pack.installed().unwrap().size / 1_000_000);
    let out = std::process::Command::new(&program).arg("--version").output().unwrap();
    let version = String::from_utf8_lossy(&out.stdout);
    assert!(version.starts_with("LibreOffice 26.2.6"), "{version}");
    assert!(program.with_file_name("xpdfimport").is_file(), "the PDF import (writer_pdf_import) is installed");
    if let Some(keep) = std::env::var_os("RUPANTAR_LO_KEEP") {
        let _ = fs::remove_dir_all(&keep);
        fs::rename(pack.dir(), &keep).unwrap();
    }
}

#[test]
fn office_markers_from_before_several_keys_still_read() {
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    fs::create_dir_all(pack.dir().join("libreoffice-26.2.6/program")).unwrap();
    fs::write(pack.dir().join("libreoffice-26.2.6/program/soffice.bin"), b"x").unwrap();
    fs::write(
        pack.dir().join("installed.json"),
        r#"{"version":"26.2.6","sha256":"fd0e","program":"libreoffice-26.2.6/program/soffice.bin"}"#,
    )
    .unwrap();
    let program = pack.dir().join("libreoffice-26.2.6/program/soffice.bin");
    assert_eq!(pack.program(), Some(program.clone()));
    let installed = pack.installed().unwrap();
    assert_eq!((installed.version.as_str(), installed.files[office::SOFFICE].clone()), ("26.2.6", program));
}

// ---- the AI pack ----------------------------------------------------------------------------

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn tgz_of(entries: &[(&str, &[u8])], symlink: Option<(&str, &str)>) -> Vec<u8> {
    let mut t = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    for (path, bytes) in entries {
        tar_file(&mut t, path, 0o644, bytes);
    }
    if let Some((path, target)) = symlink {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        t.append_link(&mut h, path, target).unwrap();
    }
    t.into_inner().unwrap().finish().unwrap()
}

/// A Linux-shaped AI build (ONNX Runtime tarball, a model file, a Real-ESRGAN zip) served from
/// `server`, whose files are named after each component.
fn ai_build(base: &str, ort: &[u8], model: &[u8], esrgan: &[u8]) -> Build {
    let real = ai::build_for("linux", "x86_64").unwrap();
    let with = |c: &Component, body: &[u8]| Component {
        urls: leak_urls(vec![format!("{base}/{}", c.file)]),
        sha256: leak(sha256(body)),
        size: body.len() as u64,
        ..c.clone()
    };
    Build { version: ai::VERSION, ..builds(vec![with(&real.components[0], ort), with(&real.components[1], model), with(&real.components[2], esrgan)]) }
}

fn ai_fixtures() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let ort = tgz_of(
        &[
            ("onnxruntime-linux-x64-1.28.3/LICENSE", b"MIT"),
            ("onnxruntime-linux-x64-1.28.3/ThirdPartyNotices.txt", b"notices"),
            ("onnxruntime-linux-x64-1.28.3/include/onnxruntime_c_api.h", b"skip"),
            ("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so.1.28.3", b"elf"),
            ("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime_providers_shared.so", b"elf2"),
        ],
        Some(("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so", "libonnxruntime.so.1.28.3")),
    );
    let esrgan = zip_of(&[
        ("input.jpg", b"demo"),
        ("models/realesrgan-x4plus.bin", b"weights"),
        ("models/realesrgan-x4plus.param", b"param"),
        ("models/realesrgan-x4plus-anime.bin", b"skip"),
        ("onepiece_demo.mp4", b"skip"),
        ("realesrgan-ncnn-vulkan", b"#!/bin/sh\n"),
    ]);
    (ort, b"onnx model bytes".to_vec(), esrgan)
}

#[test]
fn installs_the_ai_pack_from_several_downloads() {
    let (ort, model, esrgan) = ai_fixtures();
    let real = ai::build_for("linux", "x86_64").unwrap();
    let names: Vec<String> = real.components.iter().map(|c| format!("/{}", c.file)).collect();
    let server = serve_files(vec![(names[0].clone(), ort.clone()), (names[1].clone(), model.clone()), (names[2].clone(), esrgan.clone())], None);
    let b = ai_build(&server.base, &ort, &model, &esrgan);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::ai(d.path());

    let total = b.size();
    let mut last = Progress { phase: Phase::Download, done: 0, total: 0 };
    let files = pack
        .install(&b, &mut |p| {
            assert_eq!(p.total, total);
            assert!(p.done <= p.total);
            assert!(p.phase != last.phase || p.done >= last.done, "progress never goes back within a phase");
            last = p;
        }, &no_cancel())
        .unwrap();
    assert_eq!(last, Progress { phase: Phase::Extract, done: total, total });

    let tree = pack.dir().join("ai-1");
    assert_eq!(files[ai::ONNXRUNTIME], tree.join("onnxruntime/libonnxruntime.so.1.28.3"));
    assert_eq!(files[ai::BIREFNET], tree.join("birefnet/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"));
    assert_eq!(files[ai::REALESRGAN], tree.join("realesrgan/realesrgan-ncnn-vulkan"));
    assert_eq!(fs::read(&files[ai::BIREFNET]).unwrap(), model);
    assert!(tree.join("realesrgan/models/realesrgan-x4plus.param").is_file());
    assert!(tree.join("onnxruntime/libonnxruntime_providers_shared.so").is_file());
    assert_eq!(fs::read(tree.join("notices/onnxruntime/LICENSE")).unwrap(), b"MIT");
    for skipped in ["realesrgan/models/realesrgan-x4plus-anime.bin", "realesrgan/input.jpg", "onnxruntime/libonnxruntime.so", "onnxruntime/onnxruntime_c_api.h"] {
        assert!(!tree.join(skipped).exists(), "{skipped}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&files[ai::REALESRGAN]).unwrap().permissions().mode() & 0o111, 0o111);
    }
    assert_eq!(pack.file(ai::REALESRGAN), Some(files[ai::REALESRGAN].clone()));
    assert_eq!(pack.installed().unwrap().files, files);
    assert!(!pack.dir().join("download").exists() && !pack.dir().join("staging").exists());

    // A missing file means "not installed" (e.g. deleted by hand).
    fs::remove_file(&files[ai::BIREFNET]).unwrap();
    assert!(pack.installed().is_none() && pack.file(ai::BIREFNET).is_none());
    pack.remove().unwrap();
    assert!(!pack.dir().exists());
}

#[test]
fn ai_downloads_resume_and_count_together() {
    let (ort, model, esrgan) = ai_fixtures();
    let real = ai::build_for("linux", "x86_64").unwrap();
    let names: Vec<String> = real.components.iter().map(|c| format!("/{}", c.file)).collect();
    let server = serve_files(vec![(names[0].clone(), ort.clone()), (names[1].clone(), model.clone()), (names[2].clone(), esrgan.clone())], None);
    let b = ai_build(&server.base, &ort, &model, &esrgan);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::ai(d.path());

    // Cancel once the second download has started: the first is complete and kept.
    let cancel = AtomicBool::new(false);
    let err = pack
        .install(&b, &mut |p| {
            if p.phase == Phase::Download && p.done > ort.len() as u64 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }, &cancel)
        .unwrap_err();
    assert!(matches!(err, PackError::Cancelled));
    assert!(pack.downloaded(&b) > ort.len() as u64 && pack.installed().is_none());
    pack.install(&b, &mut |_| {}, &no_cancel()).unwrap();
    let ranges = server.ranges.lock().unwrap().clone();
    assert_eq!(ranges.iter().filter(|r| r.is_none()).count(), 3, "each file fetched from the start once: {ranges:?}");
}

#[test]
fn unsafe_zip_entries_are_rejected() {
    let (ort, model, _) = ai_fixtures();
    let esrgan = zip_of(&[("realesrgan-ncnn-vulkan", b"x"), ("../../escaped", b"evil")]);
    let real = ai::build_for("linux", "x86_64").unwrap();
    let names: Vec<String> = real.components.iter().map(|c| format!("/{}", c.file)).collect();
    let server = serve_files(vec![(names[0].clone(), ort.clone()), (names[1].clone(), model.clone()), (names[2].clone(), esrgan.clone())], None);
    let b = ai_build(&server.base, &ort, &model, &esrgan);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::ai(&d.path().join("packs"));
    let err = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(&err, PackError::Extract(m) if m.contains("unsafe path")), "{err}");
    assert!(!d.path().join("escaped").exists() && pack.installed().is_none() && !pack.dir().join("staging").exists());
}

#[test]
fn a_missing_program_is_an_error() {
    let (ort, model, _) = ai_fixtures();
    let esrgan = zip_of(&[("README_ubuntu.md", b"no binary here")]);
    let real = ai::build_for("linux", "x86_64").unwrap();
    let names: Vec<String> = real.components.iter().map(|c| format!("/{}", c.file)).collect();
    let server = serve_files(vec![(names[0].clone(), ort.clone()), (names[1].clone(), model.clone()), (names[2].clone(), esrgan.clone())], None);
    let b = ai_build(&server.base, &ort, &model, &esrgan);
    let d = tempfile::tempdir().unwrap();
    let err = Pack::ai(d.path()).install(&b, &mut |_| {}, &no_cancel()).unwrap_err();
    assert!(matches!(&err, PackError::Extract(m) if m.contains("realesrgan-ncnn-vulkan")), "{err}");
}

/// The real pinned Linux x64 AI pack, served locally from copies already on disk (each named
/// like its component's `file`):
/// `RUPANTAR_AI_DOWNLOADS=/dir cargo test -p packs -- --ignored`. With `RUPANTAR_AI_KEEP=/dir`
/// the installed pack is moved there (point the engine's tests at it with `RUPANTAR_AI_PACK`).
#[test]
#[ignore = "needs the real downloads (RUPANTAR_AI_DOWNLOADS)"]
fn installs_the_real_ai_pack() {
    let Some(dir) = std::env::var_os("RUPANTAR_AI_DOWNLOADS").map(std::path::PathBuf::from) else { return };
    let real = ai::build_for("linux", "x86_64").unwrap();
    let files: Vec<(String, Vec<u8>)> = real.components.iter().map(|c| (format!("/{}", c.file), fs::read(dir.join(c.file)).unwrap())).collect();
    let server = serve_files(files, None);
    let components: Vec<Component> =
        real.components.iter().map(|c| Component { urls: leak_urls(vec![format!("{}/{}", server.base, c.file)]), ..c.clone() }).collect();
    let b = Build { components: Box::leak(components.into_boxed_slice()), ..real.clone() };
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::ai(d.path());
    let started = std::time::Instant::now();
    let files = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap();
    let installed = pack.installed().unwrap();
    eprintln!("installed in {:?}, {} MB", started.elapsed(), installed.size / 1_000_000);
    assert!(installed.size < real.installed_size, "{} vs the {} advertised", installed.size, real.installed_size);
    let out = std::process::Command::new(&files[ai::REALESRGAN]).arg("-h").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("realesrgan-ncnn-vulkan"), "the program runs");
    if let Some(keep) = std::env::var_os("RUPANTAR_AI_KEEP") {
        let _ = fs::remove_dir_all(&keep);
        fs::rename(pack.dir(), &keep).unwrap();
    }
}

#[test]
fn sweep_finishes_a_pending_removal_and_only_that() {
    let dir = tempfile::tempdir().unwrap();
    let pack = Pack::ai(dir.path());
    fs::create_dir_all(pack.dir().join("ai-1/onnxruntime")).unwrap();
    fs::write(pack.dir().join("ai-1/onnxruntime/lib"), b"x").unwrap();

    pack.sweep(); // no marker: a partial download or an install must be left alone
    assert!(pack.dir().join("ai-1").exists());

    fs::write(pack.dir().join("remove-pending"), b"").unwrap();
    pack.sweep();
    assert!(!pack.dir().exists());
    pack.sweep(); // nothing left is fine
}
