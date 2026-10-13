//! The Piper voice catalog (<https://huggingface.co/rhasspy/piper-voices>).
//!
//! All voices are openly licensed (see each voice's `MODEL_CARD`). The
//! catalog is fetched once and cached; models are downloaded on demand.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::download::{self, Progress};
use crate::paths;

/// Pinned revision of the voice repository so checksums stay stable.
pub const VOICES_BASE_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0";

#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    pub key: String,
    pub name: String,
    pub language: Language,
    pub quality: String,
    pub num_speakers: u32,
    /// Repository-relative path -> size/checksum.
    pub files: BTreeMap<String, FileInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Language {
    /// e.g. `en_US`
    pub code: String,
    /// e.g. `en`
    pub family: String,
    pub name_english: String,
    #[serde(default)]
    pub country_english: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub size_bytes: u64,
    pub md5_digest: String,
}

impl Voice {
    /// Total download size of the model and its config.
    pub fn download_size(&self) -> u64 {
        self.model_files().map(|(_, f)| f.size_bytes).sum()
    }

    fn model_files(&self) -> impl Iterator<Item = (&String, &FileInfo)> {
        self.files
            .iter()
            .filter(|(p, _)| p.ends_with(".onnx") || p.ends_with(".onnx.json"))
    }

    pub fn is_installed(&self) -> bool {
        InstalledVoice::find(&self.key).is_ok_and(|v| v.is_some())
    }

    /// Download the model and config into the voices directory, verifying
    /// MD5 checksums. Already-installed files are skipped.
    pub fn download(&self, progress: Progress<'_>) -> Result<InstalledVoice> {
        let dir = paths::voices_dir()?;
        let total = self.download_size();
        let mut base = 0u64;
        for (path, info) in self.model_files() {
            let file_name = path.rsplit('/').next().unwrap_or(path);
            let dest = dir.join(file_name);
            let already = dest.exists()
                && download::md5_file(&dest)
                    .is_ok_and(|m| m.eq_ignore_ascii_case(&info.md5_digest));
            if !already {
                let url = format!("{VOICES_BASE_URL}/{path}");
                download::download_file(&url, &dest, Some(&info.md5_digest), &mut |done, _| {
                    progress(base + done, Some(total))
                })?;
            }
            base += info.size_bytes;
            progress(base, Some(total));
        }
        InstalledVoice::find(&self.key)?
            .with_context(|| format!("voice {} missing after download", self.key))
    }
}

/// Load the catalog, from cache if present, otherwise from the network.
pub fn load(refresh: bool) -> Result<Vec<Voice>> {
    let cache = paths::data_dir()?.join("voices.json");
    let json = match fs::read_to_string(&cache) {
        Ok(s) if !refresh => s,
        _ => {
            let s = download::get_string(&format!("{VOICES_BASE_URL}/voices.json"))
                .context("downloading the Piper voice catalog")?;
            if let Some(dir) = cache.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&cache, &s)?;
            s
        }
    };
    parse(&json)
}

pub fn parse(json: &str) -> Result<Vec<Voice>> {
    let map: BTreeMap<String, Voice> =
        serde_json::from_str(json).context("parsing the voice catalog")?;
    Ok(map.into_values().collect())
}

/// Find a voice by its exact key, e.g. `en_US-lessac-medium`.
pub fn find<'a>(voices: &'a [Voice], key: &str) -> Result<&'a Voice> {
    match voices.iter().find(|v| v.key == key) {
        Some(v) => Ok(v),
        None => bail!("unknown voice {key:?}; run `zreader voices list` to see available voices"),
    }
}

/// Pick a reasonable default voice for a book language such as `en`,
/// `en-GB` or `fr_FR`: exact locale match first, then the language family,
/// preferring `medium` quality.
pub fn default_for_language<'a>(voices: &'a [Voice], lang: &str) -> Option<&'a Voice> {
    let lang = lang.replace('-', "_").to_lowercase();
    let family = lang.split('_').next().unwrap_or("").to_string();
    let quality_rank = |q: &str| match q {
        "medium" => 0,
        "high" => 1,
        "low" => 2,
        _ => 3,
    };
    voices
        .iter()
        .filter(|v| v.language.family.eq_ignore_ascii_case(&family))
        .min_by_key(|v| {
            (
                !v.language.code.eq_ignore_ascii_case(&lang),
                quality_rank(&v.quality),
                v.num_speakers > 1,
                v.key.clone(),
            )
        })
}

/// A voice model present on disk.
#[derive(Debug, Clone)]
pub struct InstalledVoice {
    pub key: String,
    pub model: PathBuf,
    pub config: PathBuf,
}

impl InstalledVoice {
    pub fn find(key: &str) -> Result<Option<InstalledVoice>> {
        let dir = paths::voices_dir()?;
        let model = dir.join(format!("{key}.onnx"));
        let config = dir.join(format!("{key}.onnx.json"));
        Ok(
            (model.is_file() && config.is_file()).then(|| InstalledVoice {
                key: key.to_string(),
                model,
                config,
            }),
        )
    }

    pub fn list() -> Result<Vec<InstalledVoice>> {
        let dir = paths::voices_dir()?;
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(key) = name.strip_suffix(".onnx") {
                if let Some(v) = InstalledVoice::find(key)? {
                    out.push(v);
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    pub fn remove(&self) -> Result<()> {
        fs::remove_file(&self.model)?;
        fs::remove_file(&self.config)?;
        Ok(())
    }

    /// Output sample rate from the voice config.
    pub fn sample_rate(&self) -> Result<u32> {
        #[derive(Deserialize)]
        struct Cfg {
            audio: AudioCfg,
        }
        #[derive(Deserialize)]
        struct AudioCfg {
            sample_rate: u32,
        }
        let cfg: Cfg = serde_json::from_str(&fs::read_to_string(&self.config)?)
            .with_context(|| format!("parsing {}", self.config.display()))?;
        Ok(cfg.audio.sample_rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "en_US-lessac-medium": {
        "key": "en_US-lessac-medium", "name": "lessac",
        "language": {"code": "en_US", "family": "en", "region": "US",
          "name_native": "English", "name_english": "English", "country_english": "United States"},
        "quality": "medium", "num_speakers": 1, "speaker_id_map": {},
        "files": {
          "en/en_US/lessac/medium/en_US-lessac-medium.onnx": {"size_bytes": 100, "md5_digest": "aa"},
          "en/en_US/lessac/medium/en_US-lessac-medium.onnx.json": {"size_bytes": 5, "md5_digest": "bb"},
          "en/en_US/lessac/medium/MODEL_CARD": {"size_bytes": 1, "md5_digest": "cc"}
        },
        "aliases": []
      },
      "en_GB-alan-low": {
        "key": "en_GB-alan-low", "name": "alan",
        "language": {"code": "en_GB", "family": "en", "region": "GB",
          "name_native": "English", "name_english": "English", "country_english": "Great Britain"},
        "quality": "low", "num_speakers": 1, "speaker_id_map": {}, "files": {}, "aliases": []
      }
    }"#;

    #[test]
    fn parses_and_picks_defaults() {
        let voices = parse(SAMPLE).unwrap();
        assert_eq!(voices.len(), 2);
        let lessac = find(&voices, "en_US-lessac-medium").unwrap();
        assert_eq!(lessac.download_size(), 105);
        assert_eq!(
            default_for_language(&voices, "en-GB").unwrap().key,
            "en_GB-alan-low"
        );
        assert_eq!(
            default_for_language(&voices, "en").unwrap().key,
            "en_US-lessac-medium"
        );
        assert!(default_for_language(&voices, "de").is_none());
    }
}
