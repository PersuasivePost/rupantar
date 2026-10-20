//! Optional downloads ("packs"). Today there is one: the Office pack, an official LibreOffice
//! build pinned per OS/architecture by version, size and SHA-256.
//!
//! This is the only crate in Rupantar that touches the network, and only through
//! [`Pack::install`], which the shell calls solely after the user explicitly agreed (the
//! consent dialog, or the Download button in Settings, both showing the size first).
//! Conversions never download anything.
//!
//! Layout under the pack's directory (`<app data>/packs/office/`):
//!
//! ```text
//! download/<file>.part     the download, kept between attempts so it can resume
//! staging/                 being unpacked; removed on failure or cancel
//! libreoffice-<version>/   the installed tree
//! installed.json           written last: its presence means "installed"
//! ```
//!
//! Install = download (resumes a partial file, tries each mirror URL in turn) → verify the
//! SHA-256 (a mismatch deletes the file) → unpack into `staging/` → rename into place → write
//! `installed.json` → delete the download and any older version. Cancelling stops at the next
//! chunk; a partial download is kept for next time, a partial unpack is not.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod download;
mod extract;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("there is no Office pack build for this system ({0})")]
    Unsupported(String),
    #[error("download failed: {0}")]
    Network(String),
    #[error("the download did not match its pinned SHA-256 (got {actual}), so it was deleted; try again")]
    Checksum { actual: String },
    #[error("cancelled")]
    Cancelled,
    #[error("could not unpack: {0}")]
    Extract(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, PackError>;

/// How a build is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Archive {
    /// Linux: `.tar.gz` of `.deb` files, unpacked here without dpkg (only the needed debs).
    DebTarball,
    /// Windows: `.msi`, unpacked with an administrative install (`msiexec /a`), no admin rights.
    Msi,
    /// macOS: `.dmg`, mounted with `hdiutil`, `LibreOffice.app` copied out.
    Dmg,
}

/// One pinned download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: &'static str,
    /// `std::env::consts::OS` / `ARCH` values.
    pub os: &'static str,
    pub arch: &'static str,
    /// Tried in order: the redirector picks a nearby mirror; the archive is the permanent copy.
    pub urls: &'static [&'static str],
    /// Name of the downloaded file.
    pub file: &'static str,
    pub sha256: &'static str,
    /// Download size in bytes.
    pub size: u64,
    /// Rough size once unpacked (what we keep of it), for the consent dialog.
    pub installed_size: u64,
    pub archive: Archive,
    /// Whether this build's install has been exercised on a real machine.
    pub tested: bool,
}

/// The Office pack: LibreOffice 26.2.6 (the "still" branch), from documentfoundation.org.
/// Sizes and digests are TDF's (`<file>.mirrorlist`); the archive URLs have the same bytes.
pub mod office {
    use super::{Archive, Build};

    pub const VERSION: &str = "26.2.6";
    const MB: u64 = 1_000_000;

    macro_rules! urls {
        ($dir:literal, $stable:literal, $archived:literal) => {
            &[
                concat!("https://download.documentfoundation.org/libreoffice/stable/26.2.6/", $dir, "/", $stable),
                concat!("https://downloadarchive.documentfoundation.org/libreoffice/old/26.2.6.3/", $dir, "/", $archived),
            ]
        };
    }

    pub const BUILDS: &[Build] = &[
        Build {
            version: VERSION,
            os: "linux",
            arch: "x86_64",
            urls: urls!("deb/x86_64", "LibreOffice_26.2.6_Linux_x86-64_deb.tar.gz", "LibreOffice_26.2.6.3_Linux_x86-64_deb.tar.gz"),
            file: "LibreOffice_26.2.6_Linux_x86-64_deb.tar.gz",
            sha256: "fd0e8f8f2408dd2e5b90286e60f3f97cf566ba441cd48cfc5bcc68067303e0bc",
            size: 217_285_735,
            installed_size: 560 * MB,
            archive: Archive::DebTarball,
            tested: true,
        },
        Build {
            version: VERSION,
            os: "linux",
            arch: "aarch64",
            urls: urls!("deb/aarch64", "LibreOffice_26.2.6_Linux_aarch64_deb.tar.gz", "LibreOffice_26.2.6.3_Linux_aarch64_deb.tar.gz"),
            file: "LibreOffice_26.2.6_Linux_aarch64_deb.tar.gz",
            sha256: "f8e8b1d30abde0d530d727ce1b26909ccbedc3d76bcf75d93b3dd5fcd5b8d278",
            size: 207_165_540,
            installed_size: 540 * MB,
            archive: Archive::DebTarball,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "windows",
            arch: "x86_64",
            urls: urls!("win/x86_64", "LibreOffice_26.2.6_Win_x86-64.msi", "LibreOffice_26.2.6.3_Win_x86-64.msi"),
            file: "LibreOffice_26.2.6_Win_x86-64.msi",
            sha256: "f9877032fd908beb9c0ddf06df4af5c2e85f419c42e14876c4cce5aae5fb2660",
            size: 373_252_096,
            installed_size: 900 * MB,
            archive: Archive::Msi,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "windows",
            arch: "aarch64",
            urls: urls!("win/aarch64", "LibreOffice_26.2.6_Win_aarch64.msi", "LibreOffice_26.2.6.3_Win_aarch64.msi"),
            file: "LibreOffice_26.2.6_Win_aarch64.msi",
            sha256: "13be874da62665756b6d7c8a6afbc50e6465382a885ad90acf4dbe806d51db24",
            size: 359_571_456,
            installed_size: 880 * MB,
            archive: Archive::Msi,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "macos",
            arch: "x86_64",
            urls: urls!("mac/x86_64", "LibreOffice_26.2.6_MacOS_x86-64.dmg", "LibreOffice_26.2.6.3_MacOS_x86-64.dmg"),
            file: "LibreOffice_26.2.6_MacOS_x86-64.dmg",
            sha256: "135b8a95b8133396d54bf8e726dbc0066145efa0d785963fc3d4592acbfcfe5b",
            size: 308_308_875,
            installed_size: 1_000 * MB,
            archive: Archive::Dmg,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "macos",
            arch: "aarch64",
            urls: urls!("mac/aarch64", "LibreOffice_26.2.6_MacOS_aarch64.dmg", "LibreOffice_26.2.6.3_MacOS_aarch64.dmg"),
            file: "LibreOffice_26.2.6_MacOS_aarch64.dmg",
            sha256: "94bb3248df074c225490a8a6d1d9dc87c7d6783dbb7a8e9f0d0c3d94348552af",
            size: 297_798_926,
            installed_size: 950 * MB,
            archive: Archive::Dmg,
            tested: false,
        },
    ];

    pub fn build_for(os: &str, arch: &str) -> Option<&'static Build> {
        BUILDS.iter().find(|b| b.os == os && b.arch == arch)
    }

    /// The build for the machine we are running on.
    pub fn current() -> Option<&'static Build> {
        build_for(std::env::consts::OS, std::env::consts::ARCH)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Download,
    Verify,
    Extract,
}

/// `done` of `total` bytes in `phase` (Extract counts bytes of the archive read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

/// The `installed.json` marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    version: String,
    sha256: String,
    /// The program to run, relative to the pack directory.
    program: PathBuf,
}

/// What is installed, if anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Installed {
    pub version: String,
    pub program: PathBuf,
    /// Bytes on disk.
    pub size: u64,
}

/// An installed (or installable) pack in its own directory.
pub struct Pack {
    dir: PathBuf,
}

impl Pack {
    /// The Office pack, kept in `<packs_dir>/office`.
    pub fn office(packs_dir: &Path) -> Self {
        Self { dir: packs_dir.join("office") }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn marker(&self) -> Option<Marker> {
        let text = fs::read_to_string(self.dir.join("installed.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The installed version, if its marker is there and its program still exists.
    pub fn installed(&self) -> Option<Installed> {
        let m = self.marker()?;
        let program = self.dir.join(&m.program);
        let tree = self.dir.join(m.program.components().next()?);
        program.is_file().then(|| Installed { size: dir_size(&tree), version: m.version, program })
    }

    /// The program to run (`soffice`), if installed.
    pub fn program(&self) -> Option<PathBuf> {
        let m = self.marker()?;
        Some(self.dir.join(m.program)).filter(|p| p.is_file())
    }

    /// Bytes of a partial download of `build` already on disk (what a retry won't fetch again).
    pub fn downloaded(&self, build: &Build) -> u64 {
        fs::metadata(self.part(build)).map_or(0, |m| m.len().min(build.size))
    }

    fn part(&self, build: &Build) -> PathBuf {
        self.dir.join("download").join(format!("{}.part", build.file))
    }

    /// Downloads, verifies and unpacks `build`. Only call this after the user agreed to the
    /// download. Returns the program path. `cancel` is checked between chunks.
    pub fn install(&self, build: &Build, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<PathBuf> {
        let part = self.part(build);
        fs::create_dir_all(part.parent().unwrap_or(&self.dir))?;
        download::fetch(build.urls, &part, build.size, progress, cancel)?;

        let actual = sha256_file(&part, build.size, progress, cancel)?;
        if !actual.eq_ignore_ascii_case(build.sha256) {
            let _ = fs::remove_file(&part);
            return Err(PackError::Checksum { actual });
        }

        let staging = self.dir.join("staging");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        let unpacked = extract::unpack(build.archive, &part, &staging, progress, cancel)
            .and_then(|()| extract::find_program(build.archive, &staging));
        let program = match unpacked {
            Ok(p) => p,
            Err(e) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }
        };

        let name = format!("libreoffice-{}", build.version);
        let target = self.dir.join(&name);
        let _ = fs::remove_file(self.dir.join("installed.json")); // not "installed" mid-swap
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&staging, &target)?;
        let marker = Marker { version: build.version.into(), sha256: build.sha256.into(), program: Path::new(&name).join(&program) };
        let tmp = self.dir.join("installed.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&marker).map_err(io::Error::other)?)?;
        fs::rename(&tmp, self.dir.join("installed.json"))?;

        let _ = fs::remove_dir_all(self.dir.join("download"));
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let n = entry.file_name();
            let n = n.to_string_lossy();
            if n.starts_with("libreoffice-") && n != name {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(target.join(program))
    }

    /// Removes everything: the installed tree, a partial download, the marker (first, so a
    /// half-removed pack never counts as installed).
    pub fn remove(&self) -> Result<()> {
        match fs::remove_file(self.dir.join("installed.json")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(PackError::Cancelled)
    } else {
        Ok(())
    }
}

/// Hex SHA-256 of a file, reporting progress as `Verify`.
fn sha256_file(path: &Path, total: u64, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        check(cancel)?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(Progress { phase: Phase::Verify, done, total });
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else { return 0 };
    if meta.is_dir() {
        fs::read_dir(path).map_or(0, |entries| entries.flatten().map(|e| dir_size(&e.path())).sum())
    } else {
        meta.len()
    }
}

#[cfg(test)]
mod tests;
