//! External tools (pandoc, typst): finding them, and running them with a timeout, captured
//! stderr and no network.
//!
//! "No network" is enforced by pointing every proxy variable at a closed local port, so any
//! HTTP(S) request a tool makes (pandoc fetching a remote image, typst downloading a package)
//! fails at once instead of leaving the machine. Converters also avoid triggering fetches in
//! the first place (see `documents`).

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::{EngineError, Result};

/// Nothing listens on the discard port, so connections are refused immediately.
const DEAD_PROXY: &str = "http://127.0.0.1:9";
const PROXY_VARS: [&str; 6] = ["http_proxy", "https_proxy", "all_proxy", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"];
/// Keep error messages short enough for a toast.
const MAX_ERROR_CHARS: usize = 600;

/// `name` (plus `.exe` on Windows) in the first of `dirs` that has it, else on PATH.
pub fn find(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.iter().chain(&path_dirs).map(|d| d.join(&file)).find(|p| is_executable(p))
}

/// For tests: `name` on PATH, else a sidecar fetched into `src-tauri/binaries/` by
/// scripts/fetch-sidecars.sh (`name-<target triple>`) that runs on this machine.
#[cfg(test)]
pub(crate) fn for_tests(name: &str) -> Option<PathBuf> {
    find(name, &[]).or_else(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/binaries");
        let prefix = format!("{name}-");
        std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))
                && is_executable(p)
                && std::process::Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success())
        })
    })
}

/// Whether `path` is a file we could run (on Unix: has an execute bit).
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Runs `program` with `args` in `cwd`, offline, killing it after `timeout`. Returns its stderr
/// (tools print warnings there even on success). A non-zero exit becomes `ToolFailed` with the
/// readable part of stderr.
pub fn run<I, S>(tool: &str, program: &Path, args: I, cwd: Option<&Path>, timeout: Duration) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for var in PROXY_VARS {
        cmd.env(var, DEAD_PROXY);
    }
    cmd.env_remove("no_proxy").env_remove("NO_PROXY");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000; // no console window flashing up from a GUI app
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => EngineError::ToolMissing(tool.to_string()),
        _ => e.into(),
    })?;
    // Drain stderr on a thread so a chatty tool can't block on a full pipe.
    let reader = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::ToolTimeout { tool: tool.to_string(), timeout });
        }
        thread::sleep(Duration::from_millis(15));
    };
    let stderr = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    if status.success() {
        Ok(stderr)
    } else {
        Err(EngineError::ToolFailed { tool: tool.to_string(), status: describe(status), stderr: readable(&stderr) })
    }
}

fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "no exit code".to_string(),
    }
}

/// The part of a tool's stderr worth showing: warnings dropped when there is an actual error,
/// whitespace collapsed, and the tail kept if it is long (the error is usually last).
fn readable(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let errors: Vec<&str> = lines.iter().copied().filter(|l| !l.starts_with("[WARNING]")).collect();
    let text = if errors.is_empty() { lines.join(" ") } else { errors.join(" ") };
    if text.is_empty() {
        return "no error output".to_string();
    }
    let count = text.chars().count();
    if count <= MAX_ERROR_CHARS {
        text
    } else {
        let tail: String = text.chars().skip(count - MAX_ERROR_CHARS).collect();
        format!("…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_prefers_errors_and_truncates() {
        assert_eq!(readable(""), "no error output");
        assert_eq!(readable("[WARNING] a\n  boom  \n\n"), "boom");
        assert_eq!(readable("[WARNING] only a warning\n"), "[WARNING] only a warning");
        let long = "x".repeat(2000);
        let r = readable(&long);
        assert_eq!(r.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(r.starts_with('…'));
    }

    #[test]
    fn missing_program_is_tool_missing() {
        let err = run("nope", Path::new("/definitely/not/a/tool"), ["x"], None, Duration::from_secs(5)).unwrap_err();
        assert!(matches!(err, EngineError::ToolMissing(t) if t == "nope"));
    }

    #[test]
    fn find_checks_dirs_before_path() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(find("rupantar-no-such-tool", &[d.path().to_path_buf()]), None);
        let tool = d.path().join(format!("fake-tool{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&tool, b"").unwrap();
        #[cfg(unix)]
        {
            assert_eq!(find("fake-tool", &[d.path().to_path_buf()]), None, "not executable");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find("fake-tool", &[d.path().to_path_buf()]), Some(tool));
    }

    #[cfg(unix)]
    mod unix {
        use super::*;

        fn sh(script: &str, timeout: Duration) -> Result<String> {
            run("sh", Path::new("/bin/sh"), ["-c", script], None, timeout)
        }

        #[test]
        fn success_returns_stderr() {
            assert_eq!(sh("echo note >&2", Duration::from_secs(5)).unwrap(), "note\n");
        }

        #[test]
        fn failure_carries_status_and_stderr() {
            let err = sh("echo '[WARNING] meh' >&2; echo 'bad input' >&2; exit 3", Duration::from_secs(5)).unwrap_err();
            assert_eq!(err.to_string(), "sh failed (exit code 3): bad input");
        }

        #[test]
        fn slow_tools_are_killed() {
            let started = Instant::now();
            let err = sh("sleep 5", Duration::from_millis(200)).unwrap_err();
            assert!(matches!(err, EngineError::ToolTimeout { .. }), "{err}");
            assert!(started.elapsed() < Duration::from_secs(3));
        }

        #[test]
        fn network_is_routed_to_a_dead_proxy() {
            let out = sh("echo \"$https_proxy $HTTP_PROXY ${no_proxy-unset}\" >&2", Duration::from_secs(5)).unwrap();
            assert_eq!(out.trim(), format!("{DEAD_PROXY} {DEAD_PROXY} unset"));
        }

        #[test]
        fn chatty_stderr_does_not_deadlock() {
            // Far more than a pipe buffer (64 KiB on Linux).
            let out = sh("head -c 400000 /dev/zero | tr '\\0' x >&2", Duration::from_secs(10)).unwrap();
            assert_eq!(out.len(), 400_000);
        }
    }
}
