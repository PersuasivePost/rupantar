//! A small rotating log file (`<app data>/logs/rupantar.log`, ~1 MB, one `.1` backup) so the
//! "Copy debug info" button has something to show. Local only: nothing is ever sent anywhere.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate when the file would grow past this.
const MAX_BYTES: u64 = 1_000_000;

struct Log {
    path: PathBuf,
    file: File,
    size: u64,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

fn open(path: &PathBuf) -> Option<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, size))
}

/// Starts logging to `dir/rupantar.log`. Without it (or if the file can't be opened) `log!` still
/// writes to stderr.
pub fn init(dir: PathBuf) {
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("rupantar.log");
    if let Some((file, size)) = open(&path) {
        let _ = LOG.set(Mutex::new(Log { path, file, size }));
    }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
fn timestamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Writes one line (the `log!` macro calls this).
pub fn write(message: &str) {
    eprintln!("rupantar: {message}");
    let Some(log) = LOG.get() else { return };
    let Ok(mut log) = log.lock() else { return };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // Keep each entry on one line so "last 100 lines" means 100 entries.
    let line = format!("{} {}\n", timestamp(now), message.replace('\n', " | "));
    if log.size + line.len() as u64 > MAX_BYTES {
        let backup = log.path.with_extension("log.1");
        let _ = fs::rename(&log.path, &backup);
        if let Some((file, size)) = open(&log.path) {
            log.file = file;
            log.size = size;
        }
    }
    if log.file.write_all(line.as_bytes()).is_ok() {
        log.size += line.len() as u64;
    }
}

/// The last `n` log lines (older backup included if the current file is short).
pub fn tail(n: usize) -> Vec<String> {
    let Some(log) = LOG.get() else { return Vec::new() };
    let path = log.lock().map(|l| l.path.clone()).unwrap_or_default();
    let read = |p: &PathBuf| fs::read_to_string(p).unwrap_or_default();
    let mut text = read(&path);
    if text.lines().count() < n {
        text = read(&path.with_extension("log.1")) + &text;
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect()
}

/// Like `eprintln!("rupantar: …")`, and also appended to the log file.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::applog::write(&format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(timestamp(1_763_164_800 + 3_661), "2025-11-15 01:01:01");
        assert_eq!(timestamp(1_709_251_199), "2024-02-29 23:59:59");
    }
}
