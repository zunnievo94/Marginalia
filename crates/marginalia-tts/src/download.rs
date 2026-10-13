//! HTTP downloads with progress reporting.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// Progress callback: `(bytes_done, bytes_total_if_known)`.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

pub(crate) fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(concat!("marginalia/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Download `url` to `dest` atomically (via a temporary file next to it).
pub fn download_file(url: &str, dest: &Path, progress: Progress<'_>) -> Result<()> {
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
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading {url}"))?;
        if n == 0 {
            break;
        }
        tmp.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    tmp.flush()?;
    if let Some(total) = total {
        anyhow::ensure!(done == total, "download of {url} was cut short");
    }
    tmp.persist(dest)
        .with_context(|| format!("saving {}", dest.display()))?;
    Ok(())
}
