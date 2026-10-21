//! Unpacking a verified download into the staging directory (the future installed tree).
//!
//! LibreOffice on Linux (`.tar.gz` of `.deb`s) is unpacked in pure Rust: only the debs needed for
//! headless conversion (see [`KEEP`]), and only their `opt/libreoffice*/` tree, which lands at
//! the top of the staging dir (`program/soffice.bin`, ...). Windows and macOS LibreOffice use
//! the system's own tools (untested so far).
//!
//! The AI pack's downloads are plain files, zips (NuGet packages are zips) and `.tar.gz`s: only
//! the entries named in `Component::keep` are written, by file name, into the folder given
//! there. Paths are checked so nothing can be written outside the staging dir.

use std::cell::Cell;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component as PathPart, Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::{check, Archive, Component, PackError, Phase, Progress, Result};

/// Debs kept, by component (`libobasis26.2-core_...` → `core`; the bare `libreoffice26.2_...`
/// is ""). `extension-pdf-import` (`xpdfimport`, 2.5 MB) is what lets LibreOffice open PDFs
/// (PDF -> Word, keep layout); without it a PDF is read as plain text. Left out: dictionaries,
/// desktop integration (GNOME/KDE), database, scripting providers, other extensions, online
/// update, help: nothing a headless conversion uses.
const KEEP: &[&str] = &[
    "",
    "ure",
    "core",
    "writer",
    "calc",
    "impress",
    "draw",
    "math",
    "graphicfilter",
    "images",
    "ooofonts",
    "xsltfilter",
    "en-us",
    "extension-pdf-import",
];

/// Unpacks `file` (the download of `c`) into `staging`, reporting `Extract` progress in bytes of
/// `file` read.
pub(crate) fn unpack(c: &Component, file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    match c.archive {
        Archive::DebTarball => deb_tarball(file, staging, progress, cancel),
        Archive::Msi => msi(file, staging, progress, cancel),
        Archive::Dmg => dmg(file, staging, progress, cancel),
        Archive::File => single(c, file, staging, progress),
        Archive::Zip => zip(c, file, staging, progress, cancel),
        Archive::TarGz => tar_gz(c, file, staging, progress, cancel),
    }
}

/// LibreOffice's program, relative to the staging dir.
pub(crate) fn find_program(archive: Archive, staging: &Path) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> = match archive {
        Archive::DebTarball => vec!["program/soffice.bin".into()],
        // An administrative install mirrors the install tree under TARGETDIR.
        Archive::Msi => vec!["program/soffice.bin".into(), "LibreOffice/program/soffice.bin".into(), "PFiles/LibreOffice/program/soffice.bin".into()],
        Archive::Dmg => vec!["LibreOffice.app/Contents/MacOS/soffice".into()],
        Archive::File | Archive::Zip | Archive::TarGz => vec![],
    };
    candidates
        .into_iter()
        .find(|c| staging.join(c).is_file())
        .ok_or_else(|| PackError::Extract("the LibreOffice program was not found in the download".into()))
}

/// `pattern` with `*` matching any run of characters (`/` included) against `path`.
pub(crate) fn matches(pattern: &str, path: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pattern == path;
    }
    if !path.starts_with(first) || !path[first.len()..].ends_with(last) || path.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &path[first.len()..path.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(i) => rest = &rest[i + middle.len()..],
            None => return false,
        }
    }
    true
}

/// Where an archive entry goes: `staging/<folder>/<file name>` for the first `keep` pattern it
/// matches, or `None` to skip it. Folders must be plain relative paths.
fn destination(c: &Component, entry: &str, staging: &Path) -> Result<Option<PathBuf>> {
    let entry = entry.trim_start_matches("./");
    let Some((_, folder)) = c.keep.iter().find(|(pattern, _)| matches(pattern, entry)) else { return Ok(None) };
    let Some(name) = Path::new(entry).file_name() else { return Ok(None) };
    if !Path::new(folder).components().all(|p| matches!(p, PathPart::Normal(_))) {
        return Err(PackError::Extract(format!("unsafe folder `{folder}`")));
    }
    Ok(Some(staging.join(folder).join(name)))
}

/// Writes one kept entry; two entries may not land on the same file.
fn write_entry(dest: &Path, mut data: impl Read) -> Result<()> {
    fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    let mut out = fs::OpenOptions::new().write(true).create_new(true).open(dest).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => PackError::Extract(format!("two entries unpack to {}", dest.display())),
        _ => e.into(),
    })?;
    io::copy(&mut data, &mut out)?;
    Ok(())
}

/// A file kept as it is, in the folder `keep[0].1` (linked if possible: the download is deleted
/// after the install).
fn single(c: &Component, file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress)) -> Result<()> {
    let folder = c.keep.first().map_or("", |k| k.1);
    let safe = |p: &Path| p.components().all(|p| matches!(p, PathPart::Normal(_)));
    if !safe(Path::new(folder)) || !safe(Path::new(c.file)) || Path::new(c.file).components().count() != 1 {
        return Err(PackError::Extract(format!("unsafe name `{folder}/{}`", c.file)));
    }
    let dest = staging.join(folder).join(c.file);
    fs::create_dir_all(dest.parent().unwrap_or(staging))?;
    if fs::hard_link(file, &dest).is_err() {
        fs::copy(file, &dest)?;
    }
    progress(Progress { phase: Phase::Extract, done: c.size, total: c.size });
    Ok(())
}

fn zip(c: &Component, file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(file)?).map_err(bad)?;
    let mut done = 0;
    for i in 0..archive.len() {
        check(cancel)?;
        let entry = archive.by_index(i).map_err(bad)?;
        done += entry.compressed_size();
        if entry.is_file() {
            // `enclosed_name` rejects absolute paths and `..`; only the file name is used anyway.
            let name = entry.enclosed_name().ok_or_else(|| PackError::Extract(format!("unsafe path in archive: {}", entry.name_raw().escape_ascii())))?;
            if let Some(dest) = destination(c, &name.to_string_lossy().replace('\\', "/"), staging)? {
                write_entry(&dest, entry)?;
            }
        }
        progress(Progress { phase: Phase::Extract, done, total: c.size });
    }
    Ok(())
}

fn tar_gz(c: &Component, file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let read = Rc::new(Cell::new(0));
    let counting = Counting { inner: io::BufReader::new(File::open(file)?), read: Rc::clone(&read) };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(counting));
    for entry in archive.entries().map_err(bad)? {
        check(cancel)?;
        let entry = entry.map_err(bad)?;
        if entry.header().entry_type() == tar::EntryType::Regular {
            let path = entry.path().map_err(bad)?.into_owned();
            if path.components().any(|p| matches!(p, PathPart::ParentDir | PathPart::RootDir | PathPart::Prefix(_))) {
                return Err(PackError::Extract(format!("unsafe path in archive: {}", path.display())));
            }
            if let Some(dest) = destination(c, &path.to_string_lossy(), staging)? {
                write_entry(&dest, entry)?;
            }
        }
        progress(Progress { phase: Phase::Extract, done: read.get(), total: c.size });
    }
    Ok(())
}

/// `libobasis26.2-core_26.2.6.3-3_amd64.deb` → `core`, `libreoffice26.2_26.2.6.3-3_amd64.deb` → "".
fn component(deb: &str) -> Option<&str> {
    let package = deb.strip_suffix(".deb")?.split('_').next()?;
    let rest = package.strip_prefix("libobasis").or_else(|| package.strip_prefix("libreoffice"))?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('-')
    }
}

/// Counts bytes read into a shared cell (the archive readers own it), for progress.
struct Counting<R> {
    inner: R,
    read: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

fn bad(e: impl std::fmt::Display) -> PackError {
    PackError::Extract(e.to_string())
}

fn deb_tarball(file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let total = fs::metadata(file)?.len();
    let read = Rc::new(Cell::new(0));
    let counting = Counting { inner: io::BufReader::new(File::open(file)?), read: Rc::clone(&read) };
    let mut outer = tar::Archive::new(flate2::read::GzDecoder::new(counting));
    let root = staging.canonicalize()?;
    let mut found = 0;
    for entry in outer.entries().map_err(bad)? {
        check(cancel)?;
        let entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !component(name).is_some_and(|c| KEEP.contains(&c)) {
            continue;
        }
        found += 1;
        let mut deb = ar::Archive::new(entry);
        while let Some(member) = deb.next_entry() {
            let member = member.map_err(bad)?;
            let id = String::from_utf8_lossy(member.header().identifier()).into_owned();
            if !id.starts_with("data.tar") {
                continue;
            }
            let data: Box<dyn Read> = match id.as_str() {
                "data.tar.xz" => Box::new(lzma_rust2::XzReader::new(member, true)),
                "data.tar.gz" => Box::new(flate2::read::GzDecoder::new(member)),
                "data.tar" => Box::new(member),
                other => return Err(PackError::Extract(format!("{name}: unsupported `{other}`"))),
            };
            unpack_data(data, &root, cancel)?;
            break;
        }
        progress(Progress { phase: Phase::Extract, done: read.get(), total });
    }
    if found == 0 {
        return Err(PackError::Extract("no LibreOffice packages in the download".into()));
    }
    progress(Progress { phase: Phase::Extract, done: total, total });
    Ok(())
}

/// A deb's `./opt/libreoffice<ver>/<rest>` entries, unpacked to `root/<rest>`.
fn unpack_data(data: impl Read, root: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut archive = tar::Archive::new(data);
    archive.set_preserve_mtime(false);
    for entry in archive.entries().map_err(bad)? {
        check(cancel)?;
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let mut parts = path.components().filter(|c| !matches!(c, PathPart::CurDir));
        let in_tree = matches!(parts.next(), Some(PathPart::Normal(o)) if o == "opt")
            && matches!(parts.next(), Some(PathPart::Normal(d)) if d.to_string_lossy().starts_with("libreoffice"));
        if !in_tree {
            continue;
        }
        let mut rel = PathBuf::new();
        for part in parts {
            match part {
                PathPart::Normal(p) => rel.push(p),
                _ => return Err(PackError::Extract(format!("unsafe path in archive: {}", path.display()))),
            }
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
            // A symlink unpacked earlier must not lead a later entry outside the tree.
            if !parent.canonicalize()?.starts_with(root) {
                return Err(PackError::Extract(format!("unsafe path in archive: {}", path.display())));
            }
        }
        if fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&dest)?;
        }
        entry.unpack(&dest).map_err(bad)?;
    }
    Ok(())
}

/// Runs a system tool, killing it if the user cancels.
fn run(cmd: &mut Command, what: &str, cancel: &AtomicBool) -> Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut err);
            }
            return Err(PackError::Extract(format!("{what} failed ({status}): {}", err.trim())));
        }
        if check(cancel).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PackError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Windows (untested): an administrative install unpacks the MSI's files into TARGETDIR
/// without installing anything system-wide or needing admin rights.
fn msi(file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let total = fs::metadata(file)?.len();
    progress(Progress { phase: Phase::Extract, done: 0, total });
    let mut target = std::ffi::OsString::from("TARGETDIR=");
    target.push(staging);
    run(Command::new("msiexec").arg("/a").arg(file).arg("/qn").arg(target), "msiexec", cancel)?;
    progress(Progress { phase: Phase::Extract, done: total, total });
    Ok(())
}

/// macOS (untested): mount the image read-only, copy `LibreOffice.app` out with `ditto`
/// (keeps signatures and extended attributes), always detach.
fn dmg(file: &Path, staging: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let total = fs::metadata(file)?.len();
    progress(Progress { phase: Phase::Extract, done: 0, total });
    let mount = staging.with_file_name("mount");
    let _ = fs::remove_dir_all(&mount);
    fs::create_dir_all(&mount)?;
    run(
        Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(file),
        "hdiutil attach",
        cancel,
    )?;
    let copied = run(Command::new("ditto").arg(mount.join("LibreOffice.app")).arg(staging.join("LibreOffice.app")), "ditto", cancel);
    let _ = Command::new("hdiutil").arg("detach").arg(&mount).arg("-force").status();
    let _ = fs::remove_dir(&mount);
    copied?;
    progress(Progress { phase: Phase::Extract, done: total, total });
    Ok(())
}

#[cfg(test)]
pub(crate) fn component_for_tests(deb: &str) -> Option<&str> {
    component(deb)
}

#[cfg(test)]
pub(crate) fn matches_for_tests(pattern: &str, path: &str) -> bool {
    matches(pattern, path)
}
