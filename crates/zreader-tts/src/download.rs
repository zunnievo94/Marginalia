//! HTTP downloads with progress reporting and MD5 verification.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};

/// Progress callback: `(bytes_done, bytes_total_if_known)`.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

pub(crate) fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(concat!("zreader/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Fetch a small text resource (e.g. a JSON catalog).
pub fn get_string(url: &str) -> Result<String> {
    agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_to_string()
        .with_context(|| format!("reading {url}"))
}

/// Download `url` to `dest` atomically. If `md5_hex` is given the file is
/// verified before being moved into place.
pub fn download_file(
    url: &str,
    dest: &Path,
    md5_hex: Option<&str>,
    progress: Progress<'_>,
) -> Result<()> {
    let dir = dest.parent().context("destination has no parent")?;
    fs::create_dir_all(dir)?;

    let mut response = agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading {url}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tmp.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    tmp.flush()?;

    if let Some(expected) = md5_hex {
        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("checksum mismatch for {url}: expected {expected}, got {actual}");
        }
    }
    tmp.persist(dest)
        .with_context(|| format!("saving {}", dest.display()))?;
    Ok(())
}

/// MD5 of a file on disk, as lowercase hex.
pub fn md5_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Md5::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
