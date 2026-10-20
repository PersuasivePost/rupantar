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

fn build(urls: Vec<String>, body: &[u8]) -> Build {
    Build {
        version: "26.2.6",
        os: "linux",
        arch: "x86_64",
        urls: Box::leak(urls.into_iter().map(leak).collect::<Vec<_>>().into_boxed_slice()),
        file: "lo.tar.gz",
        sha256: leak(sha256(body)),
        size: body.len() as u64,
        installed_size: 0,
        archive: Archive::DebTarball,
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
            if !request.starts_with("GET /file ") {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            }
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
        assert_eq!(b.sha256.len(), 64, "{}", b.file);
        assert!(b.sha256.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(b.urls.len() >= 2 && b.urls.iter().all(|u| u.starts_with("https://")), "{}", b.file);
        assert!(b.urls[0].ends_with(b.file), "{}", b.file);
        assert!(b.size > 100_000_000 && b.installed_size > b.size);
        assert_eq!(b.version, office::VERSION);
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
fn installs_only_the_needed_tree_and_removes_cleanly() {
    let body = good_tarball();
    let server = serve(body.clone(), None);
    let b = build(vec![format!("{}/file", server.base)], &body);
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    assert!(pack.installed().is_none() && pack.program().is_none());

    let mut phases = Vec::new();
    let program = pack
        .install(&b, &mut |p| {
            if phases.last() != Some(&p.phase) {
                phases.push(p.phase);
            }
            assert!(p.done <= p.total);
        }, &no_cancel())
        .unwrap();
    assert_eq!(phases, [Phase::Download, Phase::Verify, Phase::Extract]);

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
    let mut b = build(vec![format!("{}/file", server.base)], &body);
    b.sha256 = leak("0".repeat(64));
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
    let b = Build { urls: build(vec![format!("{}/file", server.base)], b"").urls, ..real.clone() };
    let d = tempfile::tempdir().unwrap();
    let pack = Pack::office(d.path());
    let started = std::time::Instant::now();
    let program = pack.install(&b, &mut |_| {}, &no_cancel()).unwrap();
    eprintln!("installed in {:?}, {} MB", started.elapsed(), pack.installed().unwrap().size / 1_000_000);
    let out = std::process::Command::new(&program).arg("--version").output().unwrap();
    let version = String::from_utf8_lossy(&out.stdout);
    assert!(version.starts_with("LibreOffice 26.2.6"), "{version}");
    if let Some(keep) = std::env::var_os("RUPANTAR_LO_KEEP") {
        let _ = fs::remove_dir_all(&keep);
        fs::rename(pack.dir(), &keep).unwrap();
    }
}
