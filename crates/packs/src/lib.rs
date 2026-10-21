//! Optional downloads ("packs"), each pinned per OS/architecture by version, size and SHA-256:
//!
//! - the Office pack: an official LibreOffice build (one download);
//! - the AI pack: ONNX Runtime, the BiRefNet lite model (background removal) and Real-ESRGAN
//!   ncnn-vulkan (upscaling), several downloads installed together.
//!
//! This is the only crate in Rupantar that touches the network, and only through
//! [`Pack::install`], which the shell calls solely after the user explicitly agreed (the
//! consent dialog, or the Download button in Settings, both showing the size first).
//! Conversions never download anything.
//!
//! Layout under a pack's directory (`<app data>/packs/<id>/`):
//!
//! ```text
//! download/<file>.part     each download, kept between attempts so it can resume
//! staging/                 being unpacked; removed on failure or cancel
//! <tree>-<version>/        the installed tree (`libreoffice-26.2.6`, `ai-1`)
//! installed.json           written last: its presence means "installed"
//! ```
//!
//! Install = download every component (resuming partial files, trying each mirror URL in turn)
//! → verify each SHA-256 (a mismatch deletes that file) → unpack each into `staging/` → rename
//! into place → write `installed.json` (the files the app needs, by key) → delete the downloads
//! and any older version. Cancelling stops at the next chunk; partial downloads are kept for
//! next time, a partial unpack is not.

use std::collections::BTreeMap;
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
    #[error("there is no build of this pack for this system ({0})")]
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

/// How a download is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Archive {
    /// Linux LibreOffice: `.tar.gz` of `.deb` files, unpacked here without dpkg (only the needed debs).
    DebTarball,
    /// Windows LibreOffice: `.msi`, unpacked with an administrative install (`msiexec /a`), no admin rights.
    Msi,
    /// macOS LibreOffice: `.dmg`, mounted with `hdiutil`, `LibreOffice.app` copied out.
    Dmg,
    /// A single file (a model), kept as it is in the folder named by `keep[0].1`.
    File,
    /// `.zip` (also NuGet `.nupkg`): only the entries matching `keep`.
    Zip,
    /// `.tar.gz`: only the regular files matching `keep`.
    TarGz,
}

/// One pinned download of a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// The name the installed file is recorded under (`Pack::file`): `soffice`, `onnxruntime`,
    /// `directml`, `birefnet`, `realesrgan`.
    pub key: &'static str,
    /// Tried in order (mirrors / permanent copies of the same bytes).
    pub urls: &'static [&'static str],
    /// Name of the downloaded file.
    pub file: &'static str,
    pub sha256: &'static str,
    /// Download size in bytes.
    pub size: u64,
    pub archive: Archive,
    /// `Zip` / `TarGz`: `(pattern, folder)`: entries whose path matches `pattern` (`*` matches
    /// anything, `/` included) are unpacked by file name into `folder` of the tree. Everything
    /// else is skipped. `File`: the folder is `keep[0].1`. Ignored for LibreOffice's archives.
    pub keep: &'static [(&'static str, &'static str)],
    /// The file recorded for `key`, relative to the tree; `None`: found by the archive's own
    /// rules (LibreOffice).
    pub program: Option<&'static str>,
    /// Make `program` executable after unpacking (zip entries carry no Unix mode).
    pub executable: bool,
}

/// One pinned build of a pack for one OS/architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: &'static str,
    /// `std::env::consts::OS` / `ARCH` values.
    pub os: &'static str,
    pub arch: &'static str,
    pub components: &'static [Component],
    /// Rough size once unpacked (what we keep of it), for the consent dialog.
    pub installed_size: u64,
    /// Whether this build's install has been exercised on a real machine.
    pub tested: bool,
}

impl Build {
    /// Total download size.
    pub fn size(&self) -> u64 {
        self.components.iter().map(|c| c.size).sum()
    }
}

/// The Office pack: LibreOffice 26.2.6 (the "still" branch), from documentfoundation.org.
/// Sizes and digests are TDF's (`<file>.mirrorlist`); the archive URLs have the same bytes.
pub mod office {
    use super::{Archive, Build, Component};

    pub const VERSION: &str = "26.2.6";
    /// `Pack::file` key of the program to run.
    pub const SOFFICE: &str = "soffice";
    const MB: u64 = 1_000_000;

    macro_rules! lo {
        ($dir:literal, $stable:literal, $archived:literal, $sha:literal, $size:literal, $archive:expr) => {
            &[Component {
                key: SOFFICE,
                urls: &[
                    concat!("https://download.documentfoundation.org/libreoffice/stable/26.2.6/", $dir, "/", $stable),
                    concat!("https://downloadarchive.documentfoundation.org/libreoffice/old/26.2.6.3/", $dir, "/", $archived),
                ],
                file: $stable,
                sha256: $sha,
                size: $size,
                archive: $archive,
                keep: &[],
                program: None,
                executable: false,
            }]
        };
    }

    pub const BUILDS: &[Build] = &[
        Build {
            version: VERSION,
            os: "linux",
            arch: "x86_64",
            components: lo!(
                "deb/x86_64",
                "LibreOffice_26.2.6_Linux_x86-64_deb.tar.gz",
                "LibreOffice_26.2.6.3_Linux_x86-64_deb.tar.gz",
                "fd0e8f8f2408dd2e5b90286e60f3f97cf566ba441cd48cfc5bcc68067303e0bc",
                217_285_735,
                Archive::DebTarball
            ),
            installed_size: 565 * MB,
            tested: true,
        },
        Build {
            version: VERSION,
            os: "linux",
            arch: "aarch64",
            components: lo!(
                "deb/aarch64",
                "LibreOffice_26.2.6_Linux_aarch64_deb.tar.gz",
                "LibreOffice_26.2.6.3_Linux_aarch64_deb.tar.gz",
                "f8e8b1d30abde0d530d727ce1b26909ccbedc3d76bcf75d93b3dd5fcd5b8d278",
                207_165_540,
                Archive::DebTarball
            ),
            installed_size: 540 * MB,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "windows",
            arch: "x86_64",
            components: lo!(
                "win/x86_64",
                "LibreOffice_26.2.6_Win_x86-64.msi",
                "LibreOffice_26.2.6.3_Win_x86-64.msi",
                "f9877032fd908beb9c0ddf06df4af5c2e85f419c42e14876c4cce5aae5fb2660",
                373_252_096,
                Archive::Msi
            ),
            installed_size: 900 * MB,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "windows",
            arch: "aarch64",
            components: lo!(
                "win/aarch64",
                "LibreOffice_26.2.6_Win_aarch64.msi",
                "LibreOffice_26.2.6.3_Win_aarch64.msi",
                "13be874da62665756b6d7c8a6afbc50e6465382a885ad90acf4dbe806d51db24",
                359_571_456,
                Archive::Msi
            ),
            installed_size: 880 * MB,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "macos",
            arch: "x86_64",
            components: lo!(
                "mac/x86_64",
                "LibreOffice_26.2.6_MacOS_x86-64.dmg",
                "LibreOffice_26.2.6.3_MacOS_x86-64.dmg",
                "135b8a95b8133396d54bf8e726dbc0066145efa0d785963fc3d4592acbfcfe5b",
                308_308_875,
                Archive::Dmg
            ),
            installed_size: 1_000 * MB,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "macos",
            arch: "aarch64",
            components: lo!(
                "mac/aarch64",
                "LibreOffice_26.2.6_MacOS_aarch64.dmg",
                "LibreOffice_26.2.6.3_MacOS_aarch64.dmg",
                "94bb3248df074c225490a8a6d1d9dc87c7d6783dbb7a8e9f0d0c3d94348552af",
                297_798_926,
                Archive::Dmg
            ),
            installed_size: 950 * MB,
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

/// The AI pack: background removal (BiRefNet lite, run by ONNX Runtime) and upscaling
/// (Real-ESRGAN ncnn-vulkan). Every file is an official release asset: the model from the
/// BiRefNet author's GitHub release, ONNX Runtime from Microsoft's GitHub releases (Windows:
/// the DirectML build and DirectML itself from nuget.org), Real-ESRGAN from its GitHub release.
/// Digests: GitHub's asset digests where published (ONNX Runtime), else computed from the
/// downloaded release asset (model, Real-ESRGAN, NuGet packages).
///
/// GPU: Windows gets DirectML (any DirectX 12 GPU), macOS CoreML (part of the standard build);
/// Linux gets the CPU build, because ONNX Runtime's CUDA build is 240–420 MB more and also needs
/// CUDA and cuDNN installed system-wide. Upscaling always runs on the GPU through Vulkan.
/// Not offered on ARM Linux / ARM Windows: Real-ESRGAN has no build for them.
pub mod ai {
    use super::{Archive, Build, Component};

    /// The pack's own version; bump it when any component changes.
    pub const VERSION: &str = "1";
    /// `Pack::file` keys.
    pub const ONNXRUNTIME: &str = "onnxruntime";
    pub const BIREFNET: &str = "birefnet";
    pub const REALESRGAN: &str = "realesrgan";
    pub const DIRECTML: &str = "directml";
    /// For the consent dialog and the licenses screen.
    pub const ONNXRUNTIME_VERSION: &str = "1.28.3";
    pub const REALESRGAN_VERSION: &str = "0.2.5.0";
    const MB: u64 = 1_000_000;

    const BIREFNET_MODEL: Component = Component {
        key: BIREFNET,
        urls: &["https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"],
        file: "BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx",
        sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
        size: 224_005_088,
        archive: Archive::File,
        keep: &[("", "birefnet")],
        program: Some("birefnet/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"),
        executable: false,
    };

    macro_rules! realesrgan {
        ($os:literal, $sha:literal, $size:literal, $keep:expr, $program:literal) => {
            Component {
                key: REALESRGAN,
                urls: &[concat!(
                    "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-ncnn-vulkan-20220424-",
                    $os,
                    ".zip"
                )],
                file: concat!("realesrgan-ncnn-vulkan-20220424-", $os, ".zip"),
                sha256: $sha,
                size: $size,
                archive: Archive::Zip,
                keep: $keep,
                program: Some($program),
                executable: true,
            }
        };
    }

    /// The program and the general-purpose 4x model; not the anime models or the demo files.
    const ESRGAN_UNIX: &[(&str, &str)] =
        &[("realesrgan-ncnn-vulkan", "realesrgan"), ("models/realesrgan-x4plus.bin", "realesrgan/models"), ("models/realesrgan-x4plus.param", "realesrgan/models")];
    const ESRGAN_WINDOWS: &[(&str, &str)] = &[
        ("realesrgan-ncnn-vulkan.exe", "realesrgan"),
        ("vcomp140.dll", "realesrgan"),
        ("models/realesrgan-x4plus.bin", "realesrgan/models"),
        ("models/realesrgan-x4plus.param", "realesrgan/models"),
    ];

    macro_rules! ort_tgz {
        ($tag:literal, $name:literal, $sha:literal, $size:literal, $lib:literal) => {
            Component {
                key: ONNXRUNTIME,
                urls: &[concat!("https://github.com/microsoft/onnxruntime/releases/download/", $tag, "/", $name, ".tgz")],
                file: concat!($name, ".tgz"),
                sha256: $sha,
                size: $size,
                archive: Archive::TarGz,
                keep: &[
                    (concat!("*/lib/", $lib), "onnxruntime"),
                    ("*/lib/libonnxruntime_providers_shared.so", "onnxruntime"),
                    ("*/LICENSE", "notices/onnxruntime"),
                    ("*/ThirdPartyNotices.txt", "notices/onnxruntime"),
                ],
                program: Some(concat!("onnxruntime/", $lib)),
                executable: false,
            }
        };
    }

    pub const BUILDS: &[Build] = &[
        Build {
            version: VERSION,
            os: "linux",
            arch: "x86_64",
            components: &[
                ort_tgz!(
                    "v1.28.3",
                    "onnxruntime-linux-x64-1.28.3",
                    "db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c",
                    9_130_098,
                    "libonnxruntime.so.1.28.3"
                ),
                BIREFNET_MODEL,
                realesrgan!(
                    "ubuntu",
                    "e5aa6eb131234b87c0c51f82b89390f5e3e642b7b70f2b9bbe95b6a285a40c96",
                    46_931_474,
                    ESRGAN_UNIX,
                    "realesrgan/realesrgan-ncnn-vulkan"
                ),
            ],
            installed_size: 295 * MB,
            tested: true,
        },
        Build {
            version: VERSION,
            os: "windows",
            arch: "x86_64",
            components: &[
                Component {
                    key: ONNXRUNTIME,
                    urls: &[
                        "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
                        "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime.DirectML/1.24.4",
                    ],
                    file: "microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
                    sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
                    size: 12_458_649,
                    archive: Archive::Zip,
                    keep: &[
                        ("runtimes/win-x64/native/onnxruntime.dll", "onnxruntime"),
                        ("runtimes/win-x64/native/onnxruntime_providers_shared.dll", "onnxruntime"),
                        ("LICENSE", "notices/onnxruntime"),
                        ("ThirdPartyNotices.txt", "notices/onnxruntime"),
                    ],
                    program: Some("onnxruntime/onnxruntime.dll"),
                    executable: false,
                },
                // The DirectML build needs DirectML 1.15.4 next to it (Windows' own copy is older).
                Component {
                    key: DIRECTML,
                    urls: &[
                        "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
                        "https://www.nuget.org/api/v2/package/Microsoft.AI.DirectML/1.15.4",
                    ],
                    file: "microsoft.ai.directml.1.15.4.nupkg",
                    sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9",
                    size: 202_292_617,
                    archive: Archive::Zip,
                    keep: &[
                        ("bin/x64-win/DirectML.dll", "onnxruntime"),
                        ("LICENSE.txt", "notices/directml"),
                        ("ThirdPartyNotices.txt", "notices/directml"),
                    ],
                    program: Some("onnxruntime/DirectML.dll"),
                    executable: false,
                },
                BIREFNET_MODEL,
                realesrgan!(
                    "windows",
                    "abc02804e17982a3be33675e4d471e91ea374e65b70167abc09e31acb412802d",
                    45_474_481,
                    ESRGAN_WINDOWS,
                    "realesrgan/realesrgan-ncnn-vulkan.exe"
                ),
            ],
            installed_size: 300 * MB,
            tested: false,
        },
        Build {
            version: VERSION,
            os: "macos",
            arch: "aarch64",
            components: &[
                ort_tgz!(
                    "v1.28.3",
                    "onnxruntime-osx-arm64-1.28.3",
                    "c436bd9f47dbce6f6311ccc21de97f829b3c862c09d24aa321958cb72f0a1d32",
                    33_057_743,
                    "libonnxruntime.1.28.3.dylib"
                ),
                BIREFNET_MODEL,
                realesrgan!(
                    "macos",
                    "e0ad05580abfeb25f8d8fb55aaf7bedf552c375b5b4d9bd3c8d59764d2cc333a",
                    51_817_124,
                    ESRGAN_UNIX,
                    "realesrgan/realesrgan-ncnn-vulkan"
                ),
            ],
            installed_size: 320 * MB,
            tested: false,
        },
        // ONNX Runtime stopped publishing Intel macOS builds after 1.23.
        Build {
            version: VERSION,
            os: "macos",
            arch: "x86_64",
            components: &[
                ort_tgz!(
                    "v1.23.2",
                    "onnxruntime-osx-x86_64-1.23.2",
                    "d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6",
                    11_676_322,
                    "libonnxruntime.1.23.2.dylib"
                ),
                BIREFNET_MODEL,
                realesrgan!(
                    "macos",
                    "e0ad05580abfeb25f8d8fb55aaf7bedf552c375b5b4d9bd3c8d59764d2cc333a",
                    51_817_124,
                    ESRGAN_UNIX,
                    "realesrgan/realesrgan-ncnn-vulkan"
                ),
            ],
            installed_size: 305 * MB,
            tested: false,
        },
    ];

    pub fn build_for(os: &str, arch: &str) -> Option<&'static Build> {
        BUILDS.iter().find(|b| b.os == os && b.arch == arch)
    }

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

/// `done` of `total` bytes in `phase`, across all of a build's components (Extract counts bytes
/// of the archives read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

/// The `installed.json` marker. Paths are relative to the pack directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Marker {
    version: String,
    /// The components' digests, comma-separated.
    #[serde(default)]
    sha256: String,
    /// Office packs installed before there were several keys: the LibreOffice program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<PathBuf>,
    #[serde(default)]
    files: BTreeMap<String, PathBuf>,
}

impl Marker {
    fn file(&self, key: &str) -> Option<&PathBuf> {
        self.files.get(key).or(if key == office::SOFFICE { self.program.as_ref() } else { None })
    }
}

/// What is installed, if anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Installed {
    pub version: String,
    /// Absolute paths by key (`Component::key`).
    pub files: BTreeMap<String, PathBuf>,
    /// Bytes on disk.
    pub size: u64,
}

/// An installed (or installable) pack in its own directory.
pub struct Pack {
    dir: PathBuf,
    /// Prefix of the installed tree's folder (`libreoffice` → `libreoffice-26.2.6`).
    tree: &'static str,
}

impl Pack {
    /// The Office pack, kept in `<packs_dir>/office`.
    pub fn office(packs_dir: &Path) -> Self {
        Self { dir: packs_dir.join("office"), tree: "libreoffice" }
    }

    /// The AI pack, kept in `<packs_dir>/ai`.
    pub fn ai(packs_dir: &Path) -> Self {
        Self { dir: packs_dir.join("ai"), tree: "ai" }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn marker(&self) -> Option<Marker> {
        let text = fs::read_to_string(self.dir.join("installed.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The installed version, if its marker is there and all of its files still exist.
    pub fn installed(&self) -> Option<Installed> {
        let m = self.marker()?;
        let keys: Vec<String> = m.files.keys().cloned().chain(m.program.iter().map(|_| office::SOFFICE.to_string())).collect();
        let mut files = BTreeMap::new();
        for key in keys {
            let path = self.dir.join(m.file(&key)?);
            if !path.is_file() {
                return None;
            }
            files.insert(key, path);
        }
        let tree = self.dir.join(m.file(files.keys().next()?)?.components().next()?);
        Some(Installed { size: dir_size(&tree), version: m.version, files })
    }

    /// An installed file by key (`Component::key`), if it exists.
    pub fn file(&self, key: &str) -> Option<PathBuf> {
        let m = self.marker()?;
        Some(self.dir.join(m.file(key)?)).filter(|p| p.is_file())
    }

    /// The Office pack's program (`soffice`), if installed.
    pub fn program(&self) -> Option<PathBuf> {
        self.file(office::SOFFICE)
    }

    /// Bytes of partial downloads of `build` already on disk (what a retry won't fetch again).
    pub fn downloaded(&self, build: &Build) -> u64 {
        build.components.iter().map(|c| fs::metadata(self.part(c)).map_or(0, |m| m.len().min(c.size))).sum()
    }

    fn part(&self, c: &Component) -> PathBuf {
        self.dir.join("download").join(format!("{}.part", c.file))
    }

    /// Downloads, verifies and unpacks every component of `build`. Only call this after the user
    /// agreed to the download. Returns the installed files by key. `cancel` is checked between
    /// chunks.
    pub fn install(&self, build: &Build, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<BTreeMap<String, PathBuf>> {
        let total = build.size();
        fs::create_dir_all(self.dir.join("download"))?;
        // Installing again supersedes a removal that was waiting for a restart.
        let _ = fs::remove_file(self.dir.join(REMOVE_PENDING));

        let mut offset = 0;
        for c in build.components {
            let mut report = |p: Progress| progress(Progress { done: offset + p.done, total, ..p });
            download::fetch(c.urls, &self.part(c), c.size, &mut report, cancel)?;
            offset += c.size;
        }
        let mut offset = 0;
        for c in build.components {
            let part = self.part(c);
            let actual = sha256_file(&part, &mut |p| progress(Progress { done: offset + p.done, total, ..p }), cancel)?;
            if !actual.eq_ignore_ascii_case(c.sha256) {
                let _ = fs::remove_file(&part);
                return Err(PackError::Checksum { actual });
            }
            offset += c.size;
        }

        let staging = self.dir.join("staging");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        let files = match self.unpack_all(build, &staging, total, progress, cancel) {
            Ok(files) => files,
            Err(e) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }
        };

        let name = format!("{}-{}", self.tree, build.version);
        let target = self.dir.join(&name);
        let _ = fs::remove_file(self.dir.join("installed.json")); // not "installed" mid-swap
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&staging, &target)?;
        let marker = Marker {
            version: build.version.into(),
            sha256: build.components.iter().map(|c| c.sha256).collect::<Vec<_>>().join(","),
            program: None,
            files: files.iter().map(|(k, p)| (k.clone(), Path::new(&name).join(p))).collect(),
        };
        let tmp = self.dir.join("installed.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&marker).map_err(io::Error::other)?)?;
        fs::rename(&tmp, self.dir.join("installed.json"))?;

        let _ = fs::remove_dir_all(self.dir.join("download"));
        let prefix = format!("{}-", self.tree);
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let n = entry.file_name();
            let n = n.to_string_lossy();
            if n.starts_with(&prefix) && n != name {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(files.into_iter().map(|(k, p)| (k, target.join(p))).collect())
    }

    /// Unpacks every component into `staging`; returns each recorded file relative to it.
    fn unpack_all(
        &self,
        build: &Build,
        staging: &Path,
        total: u64,
        progress: &mut dyn FnMut(Progress),
        cancel: &AtomicBool,
    ) -> Result<BTreeMap<String, PathBuf>> {
        let mut files = BTreeMap::new();
        let mut offset = 0;
        for c in build.components {
            let mut report = |p: Progress| progress(Progress { done: offset + p.done.min(c.size), total, ..p });
            extract::unpack(c, &self.part(c), staging, &mut report, cancel)?;
            report(Progress { phase: Phase::Extract, done: c.size, total: c.size }); // headers etc. aren't counted
            let program = match c.program {
                Some(p) => PathBuf::from(p),
                None => extract::find_program(c.archive, staging)?,
            };
            let path = staging.join(&program);
            if !path.is_file() {
                return Err(PackError::Extract(format!("{} was not found in {}", program.display(), c.file)));
            }
            #[cfg(unix)]
            if c.executable {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            files.insert(c.key.to_string(), program);
            offset += c.size;
        }
        Ok(files)
    }

    /// Removes everything: the installed tree, partial downloads, the marker (first, so a
    /// half-removed pack never counts as installed). A file that is still loaded by this process (ONNX Runtime's library,
    /// after a background removal; Windows won't delete a loaded DLL) cannot be deleted now: the
    /// pack then counts as removed anyway and what is left is deleted by `sweep` at the next start.
    pub fn remove(&self) -> Result<()> {
        match fs::remove_file(self.dir.join("installed.json")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                // Everything that could be deleted is gone; mark the rest. If even the marker
                // can't be written, report the original failure.
                fs::write(self.dir.join(REMOVE_PENDING), b"").map_err(|_| e)?;
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    /// Finishes a removal that `remove` could not (call at startup, before anything is loaded).
    /// Does nothing unless a removal was left pending; a failure just leaves it for next time.
    pub fn sweep(&self) {
        if self.dir.join(REMOVE_PENDING).exists() && fs::remove_dir_all(&self.dir).is_err() {
            // Keep the marker if it survived, so the next start tries again.
            let _ = fs::write(self.dir.join(REMOVE_PENDING), b"");
        }
    }
}

/// Written into a pack's directory when its removal could not finish (see `Pack::remove`).
const REMOVE_PENDING: &str = "remove-pending";

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(PackError::Cancelled)
    } else {
        Ok(())
    }
}

/// Hex SHA-256 of a file, reporting progress as `Verify` (bytes of this file).
fn sha256_file(path: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let total = file.metadata()?.len();
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
