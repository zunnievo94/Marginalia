//! Piper (<https://github.com/rhasspy/piper>, MIT) neural TTS runtime.
//!
//! The Piper executable is downloaded once from its GitHub release and kept
//! running as a child process: each sentence is written to its stdin and it
//! answers with the path of a WAV file, so the model is loaded only once.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::audio::Audio;
use crate::catalog::InstalledVoice;
use crate::download::{self, Progress};
use crate::{paths, Synthesizer};

pub const RELEASE: &str = "2023.11.14-2";
const RELEASE_URL: &str = "https://github.com/rhasspy/piper/releases/download";

/// Release asset for the current platform, if Piper ships one.
pub fn release_asset() -> Option<&'static str> {
    use std::env::consts::{ARCH, OS};
    Some(match (OS, ARCH) {
        ("linux", "x86_64") => "piper_linux_x86_64.tar.gz",
        ("linux", "aarch64") => "piper_linux_aarch64.tar.gz",
        ("linux", "arm") => "piper_linux_armv7l.tar.gz",
        ("macos", "x86_64") => "piper_macos_x64.tar.gz",
        ("macos", "aarch64") => "piper_macos_aarch64.tar.gz",
        ("windows", "x86_64") => "piper_windows_amd64.zip",
        _ => return None,
    })
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "piper.exe"
    } else {
        "piper"
    }
}

/// Locate the Piper executable: `MARGINALIA_PIPER`, then the copy installed by
/// [`install`], then `piper` on `PATH`.
pub fn find_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MARGINALIA_PIPER") {
        return Some(PathBuf::from(p));
    }
    if let Ok(dir) = paths::engine_dir() {
        let p = dir.join("piper").join(exe_name());
        if p.is_file() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(exe_name()))
        .find(|p| p.is_file())
}

/// Download and unpack the Piper runtime into the engine directory.
pub fn install(progress: Progress<'_>) -> Result<PathBuf> {
    let asset = release_asset().context(
        "Piper has no prebuilt binary for this platform; build it from source and set MARGINALIA_PIPER",
    )?;
    let url = format!("{RELEASE_URL}/{RELEASE}/{asset}");
    let dir = paths::engine_dir()?;
    fs::create_dir_all(&dir)?;

    let tmp = tempfile::tempdir_in(&dir)?;
    let archive = tmp.path().join(asset);
    download::download_file(&url, &archive, None, progress)?;

    let staging = tmp.path().join("unpacked");
    if asset.ends_with(".zip") {
        zip::ZipArchive::new(File::open(&archive)?)?.extract(&staging)?;
    } else {
        let gz = flate2::read::GzDecoder::new(File::open(&archive)?);
        tar::Archive::new(gz).unpack(&staging)?;
    }
    let unpacked = staging.join("piper");
    if !unpacked.join(exe_name()).is_file() {
        bail!("unexpected archive layout in {asset}");
    }
    let target = dir.join("piper");
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&unpacked, &target)?;
    Ok(target.join(exe_name()))
}

#[derive(Debug, Clone)]
pub struct PiperOptions {
    /// 1.0 is normal; 1.5 is 50% faster.
    pub speed: f32,
    /// Speaker id for multi-speaker voices.
    pub speaker: Option<u32>,
}

impl Default for PiperOptions {
    fn default() -> Self {
        PiperOptions {
            speed: 1.0,
            speaker: None,
        }
    }
}

pub struct PiperEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    work_dir: tempfile::TempDir,
}

impl PiperEngine {
    pub fn new(executable: &Path, voice: &InstalledVoice, opts: &PiperOptions) -> Result<Self> {
        let work_dir = tempfile::tempdir()?;
        let log = File::create(work_dir.path().join("piper.log"))?;
        let out_dir = work_dir.path().join("out");
        fs::create_dir(&out_dir)?;

        let mut cmd = Command::new(executable);
        cmd.arg("--model")
            .arg(&voice.model)
            .arg("--config")
            .arg(&voice.config)
            .arg("--output_dir")
            .arg(&out_dir)
            .arg("--length_scale")
            .arg(format!("{:.3}", 1.0 / opts.speed.clamp(0.25, 4.0)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        if let Some(s) = opts.speaker {
            cmd.arg("--speaker").arg(s.to_string());
        }
        // The release bundles espeak-ng-data next to the executable.
        if let Some(data) = executable.parent().map(|d| d.join("espeak-ng-data")) {
            if data.is_dir() {
                cmd.arg("--espeak_data").arg(data);
            }
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", executable.display()))?;
        let stdin = child.stdin.take().context("piper stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("piper stdout")?);
        Ok(PiperEngine {
            child,
            stdin,
            stdout,
            work_dir,
        })
    }

    fn log_tail(&self) -> String {
        let log = fs::read_to_string(self.work_dir.path().join("piper.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(10)..].join("\n")
    }
}

impl Synthesizer for PiperEngine {
    fn synthesize(&mut self, text: &str) -> Result<Audio> {
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            return Ok(Audio::silence(22050, 0));
        }
        let sent = writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush());
        let mut path = String::new();
        let read = sent.and_then(|_| self.stdout.read_line(&mut path));
        if matches!(read, Ok(0) | Err(_)) {
            bail!("piper exited unexpectedly:\n{}", self.log_tail());
        }
        let path = PathBuf::from(path.trim());
        let audio = Audio::read_wav(&path)?;
        let _ = fs::remove_file(&path);
        Ok(audio)
    }
}

impl Drop for PiperEngine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
