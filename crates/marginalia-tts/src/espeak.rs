//! eSpeak NG (<https://github.com/espeak-ng/espeak-ng>, GPL-3.0) fallback.
//!
//! Robotic but tiny, supports 100+ languages, and needs no model download
//! when it is already installed (`apt install espeak-ng`, `brew install
//! espeak-ng`, ...). It is run as an external program.

use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::audio::Audio;
use crate::Synthesizer;

pub fn find_executable() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: &[&str] = if cfg!(windows) {
        &["espeak-ng.exe", "espeak.exe"]
    } else {
        &["espeak-ng", "espeak"]
    };
    std::env::split_paths(&path).find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
}

/// eSpeak NG voice name for a book language such as `en-US`, `vi`,
/// `zh-CN` or `zh_TW` (eSpeak calls Mandarin `cmn` and Cantonese `yue`).
pub fn voice_for_language(lang: &str) -> String {
    let lang = lang.replace('_', "-").to_lowercase();
    match lang.as_str() {
        "zh-hk" | "zh-mo" | "yue" => "yue".into(),
        l if l == "zh" || l.starts_with("zh-") || l == "cmn" => "cmn".into(),
        l => l.into(),
    }
}

pub struct EspeakEngine {
    executable: std::path::PathBuf,
    voice: String,
    words_per_minute: u32,
}

impl EspeakEngine {
    /// `voice` is an eSpeak voice/language such as `en`, `en-us`, `fr`.
    pub fn new(voice: &str, speed: f32) -> Result<Self> {
        let executable = find_executable()
            .context("eSpeak NG is not installed (try `apt install espeak-ng`)")?;
        Ok(EspeakEngine {
            executable,
            voice: voice.to_string(),
            words_per_minute: (175.0 * speed.clamp(0.25, 4.0)) as u32,
        })
    }
}

impl Synthesizer for EspeakEngine {
    fn synthesize(&mut self, text: &str) -> Result<Audio> {
        let wav = tempfile::Builder::new().suffix(".wav").tempfile()?;
        let status = Command::new(&self.executable)
            .arg("-v")
            .arg(&self.voice)
            .arg("-s")
            .arg(self.words_per_minute.to_string())
            .arg("-w")
            .arg(wav.path())
            .arg("--")
            .arg(text)
            .status()
            .with_context(|| format!("running {}", self.executable.display()))?;
        if !status.success() {
            bail!("espeak-ng failed with {status}");
        }
        Audio::read_wav(wav.path())
    }
}

#[cfg(test)]
mod tests {
    use super::voice_for_language;

    #[test]
    fn maps_languages() {
        assert_eq!(voice_for_language("zh-CN"), "cmn");
        assert_eq!(voice_for_language("zh_TW"), "cmn");
        assert_eq!(voice_for_language("zh-HK"), "yue");
        assert_eq!(voice_for_language("vi"), "vi");
        assert_eq!(voice_for_language("en_US"), "en-us");
    }
}
