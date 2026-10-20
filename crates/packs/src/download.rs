//! Resumable HTTPS download of a pinned file.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::{check, PackError, Phase, Progress, Result};

/// Attempts per URL; each one resumes where the last stopped.
const ATTEMPTS: usize = 2;
const CHUNK: usize = 256 * 1024;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("Rupantar/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

/// Fetches `size` bytes into `dest`, resuming a partial file, trying each URL in turn. The
/// bytes are not checked here (the caller verifies the digest).
pub(crate) fn fetch(urls: &[&str], dest: &Path, size: u64, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let agent = agent();
    let mut last = PackError::Network("no download URL".into());
    for url in urls {
        for _ in 0..ATTEMPTS {
            match fetch_from(&agent, url, dest, size, progress, cancel) {
                Ok(()) => return Ok(()),
                Err(e @ (PackError::Cancelled | PackError::Io(_))) => return Err(e),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
}

fn fetch_from(agent: &ureq::Agent, url: &str, dest: &Path, size: u64, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    check(cancel)?;
    let mut have = fs::metadata(dest).map_or(0, |m| m.len());
    if have > size {
        fs::remove_file(dest)?;
        have = 0;
    }
    if have == size {
        return Ok(());
    }
    let net = |e: &dyn std::fmt::Display| PackError::Network(format!("{}: {e}", host(url)));
    let mut request = agent.get(url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut response = request.call().map_err(|e| net(&e))?;
    let resumed = match response.status().as_u16() {
        206 => {
            let range = response.headers().get("content-range").and_then(|v| v.to_str().ok()).unwrap_or("");
            if !range.starts_with(&format!("bytes {have}-")) {
                return Err(net(&format!("unexpected Content-Range `{range}`")));
            }
            true
        }
        200 => false,
        status => return Err(net(&format!("HTTP {status}"))),
    };
    let mut file = if resumed {
        OpenOptions::new().append(true).open(dest)?
    } else {
        have = 0;
        fs::File::create(dest)?
    };

    let mut body = response.body_mut().with_config().limit(size - have + 1).reader();
    let mut buf = vec![0u8; CHUNK];
    let mut done = have;
    progress(Progress { phase: Phase::Download, done, total: size });
    loop {
        check(cancel)?;
        let n = body.read(&mut buf).map_err(|e| net(&e))?;
        if n == 0 {
            break;
        }
        if done + n as u64 > size {
            drop(file);
            fs::remove_file(dest)?;
            return Err(net(&"the server sent more data than expected"));
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(Progress { phase: Phase::Download, done, total: size });
    }
    file.flush()?;
    if done < size {
        return Err(net(&format!("the connection closed after {done} of {size} bytes")));
    }
    Ok(())
}

/// The host part of a URL, for error messages.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}
