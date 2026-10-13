//! Catalog of open voice models and their installation.
//!
//! Most models are the sherpa-onnx packaged releases on GitHub
//! (<https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models>): each
//! is a `.tar.bz2` archive holding the ONNX files plus whatever the model
//! needs (tokens, espeak-ng data, lexicons). VieNeu voices are separate
//! files on Hugging Face and GitHub (see [`Pack::files`]). Either way they
//! end up in `<data dir>/models/<id>/`.

use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::download::{self, Progress};
use crate::paths;

const RELEASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models";
/// Written last when a model is fully extracted.
const COMPLETE_MARKER: &str = ".marginalia-complete";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Piper (VITS) voices: one language each, small and fast.
    Piper,
    /// Kokoro: very natural English and Mandarin voices.
    Kokoro,
    /// Supertonic 3: 31 languages including Vietnamese.
    Supertonic,
    /// VieNeu-TTS v3 Turbo: the most natural Vietnamese voices.
    VieneuTurbo,
    /// VieNeu-TTS v3 Nano: VieNeu for slower CPUs.
    VieneuNano,
}

impl Kind {
    /// VieNeu models, which run outside sherpa-onnx.
    pub fn is_vieneu(self) -> bool {
        matches!(self, Kind::VieneuTurbo | Kind::VieneuNano)
    }
}

/// A downloadable voice model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    /// Archive name without `.tar.bz2`; also the install directory name.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// Locales (`vi_VN`) or bare languages (`vi`) the model speaks.
    pub languages: Vec<String>,
    /// Download size in bytes.
    pub size: u64,
    /// Speaker names for multi-speaker models (empty for single-speaker).
    pub speakers: Vec<String>,
    /// A short description of each speaker (VieNeu: gender, region, style).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speaker_notes: Vec<String>,
    pub license: String,
    #[serde(default)]
    pub note: String,
    /// Lower is preferred among otherwise equal models.
    #[serde(default)]
    pub priority: u8,
}

/// All known models.
pub fn catalog() -> &'static [Pack] {
    static CATALOG: OnceLock<Vec<Pack>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        #[derive(Deserialize)]
        struct File {
            packs: Vec<Pack>,
        }
        serde_json::from_str::<File>(include_str!("models.json"))
            .expect("models.json is valid")
            .packs
    })
}

pub fn find(id: &str) -> Option<&'static Pack> {
    catalog().iter().find(|p| p.id == id)
}

/// Normalise `en-US`, `en_us`, `EN` to `en_US` / `en`.
pub fn normalize_lang(lang: &str) -> String {
    let lang = lang.trim().replace('-', "_");
    let mut parts = lang.splitn(2, '_');
    let family = parts.next().unwrap_or_default().to_lowercase();
    match parts.next() {
        Some(region) if !region.is_empty() => format!("{family}_{}", region.to_uppercase()),
        _ => family,
    }
}

fn family(lang: &str) -> &str {
    lang.split('_').next().unwrap_or(lang)
}

impl Pack {
    pub fn url(&self) -> String {
        format!("{RELEASE_URL}/{}.tar.bz2", self.id)
    }

    pub fn dir(&self) -> Result<PathBuf> {
        Ok(paths::models_dir()?.join(&self.id))
    }

    pub fn is_installed(&self) -> bool {
        self.dir().is_ok_and(|d| d.join(COMPLETE_MARKER).is_file())
    }

    /// How well this model matches `lang`: 2 = same locale, 1 = same
    /// language, 0 = not at all.
    pub fn language_match(&self, lang: &str) -> u8 {
        let lang = normalize_lang(lang);
        let mut best = 0;
        for l in &self.languages {
            if *l == lang {
                return 2;
            }
            if family(l) == family(&lang) {
                best = 1;
            }
        }
        best
    }

    pub fn speaker_count(&self) -> usize {
        self.speakers.len().max(1)
    }

    /// The speaker to use for `lang` when none was chosen.
    pub fn default_speaker(&self, lang: &str) -> i32 {
        let lang = normalize_lang(lang);
        let prefer: &[&str] = match (self.kind, lang.as_str()) {
            (Kind::Kokoro, "en_GB") => &["bf_emma"],
            (Kind::Kokoro, l) if l.starts_with("zh") => &["zf_xiaoxiao"],
            (Kind::Kokoro, _) => &["af_heart"],
            // VieNeu's own defaults.
            (Kind::VieneuTurbo, _) => &["Hải Đăng"],
            (Kind::VieneuNano, _) => &["Minh Quân"],
            _ => &[],
        };
        prefer
            .iter()
            .find_map(|name| self.speakers.iter().position(|s| s == name))
            .unwrap_or(0) as i32
    }

    /// `(url, file name)` of each file of a model that is not a sherpa-onnx
    /// archive.
    pub fn files(&self) -> Vec<(String, String)> {
        use crate::vieneu::{nano, turbo, HF, SEA_G2P_DICT, SEA_G2P_DICT_URL, VIENEU_RAW};
        // Pinned to the commits the port was checked against.
        let hf = |repo: &str, rev: &str, path: &str| format!("{HF}/{repo}/resolve/{rev}/{path}");
        let name = |f: &str| f.rsplit('/').next().unwrap_or(f).to_string();
        let mut files: Vec<(String, String)> = match self.kind {
            Kind::VieneuTurbo => turbo::FILES
                .iter()
                .map(|f| {
                    (
                        hf(
                            turbo::REPO,
                            turbo::REVISION,
                            &format!("{}/{f}", turbo::SUBFOLDER),
                        ),
                        name(f),
                    )
                })
                .chain(
                    turbo::CODEC_FILES
                        .iter()
                        .map(|f| (hf(turbo::CODEC_REPO, turbo::CODEC_REVISION, f), name(f))),
                )
                .chain([(
                    format!("{VIENEU_RAW}/src/vieneu/assets/{}", turbo::VOICES),
                    turbo::VOICES.into(),
                )])
                .collect(),
            Kind::VieneuNano => nano::FILES
                .iter()
                .map(|f| (hf(nano::REPO, nano::REVISION, f), name(f)))
                .chain([(
                    format!("{VIENEU_RAW}/src/vieneu/assets/{}", nano::VOICES),
                    nano::VOICES.into(),
                )])
                .collect(),
            _ => return Vec::new(),
        };
        files.push((SEA_G2P_DICT_URL.into(), SEA_G2P_DICT.into()));
        files
    }

    /// Download and extract the model. Safe to call again after a failure.
    pub fn download(&self, progress: Progress<'_>) -> Result<()> {
        if self.is_installed() {
            return Ok(());
        }
        let root = paths::models_dir()?;
        fs::create_dir_all(&root)?;
        let work = tempfile::tempdir_in(&root)?;
        if self.kind.is_vieneu() {
            return self.download_files(work.path(), progress);
        }
        let archive = work.path().join("model.tar.bz2");
        download::download_file(&self.url(), &archive, progress)?;

        let staging = work.path().join("x");
        let bz = bzip2::read::BzDecoder::new(File::open(&archive)?);
        tar::Archive::new(bz)
            .unpack(&staging)
            .with_context(|| format!("unpacking {}", self.id))?;
        // Archives contain a single top-level directory named like the model.
        let top = fs::read_dir(&staging)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .context("empty model archive")?;
        fs::write(top.join(COMPLETE_MARKER), b"")?;

        let dest = self.dir()?;
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&top, &dest)?;
        Ok(())
    }

    /// Download a model made of separate files into `work`, then move it
    /// into place. Progress counts bytes over the whole model.
    fn download_files(&self, work: &std::path::Path, progress: Progress<'_>) -> Result<()> {
        let staging = work.join(&self.id);
        fs::create_dir_all(&staging)?;
        let mut before = 0u64;
        for (url, name) in self.files() {
            let mut last = 0u64;
            download::download_file(&url, &staging.join(&name), &mut |done, _| {
                last = done;
                let total = self.size.max(before + done);
                progress(before + done, Some(total));
            })
            .with_context(|| format!("downloading {name} for {}", self.id))?;
            before += last;
        }
        fs::write(staging.join(COMPLETE_MARKER), b"")?;
        let dest = self.dir()?;
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&staging, &dest)?;
        Ok(())
    }

    pub fn remove(&self) -> Result<()> {
        let dir = self.dir()?;
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Path of a file in the installed model, by exact name.
    pub(crate) fn file(&self, name: &str) -> Result<PathBuf> {
        let p = self.dir()?.join(name);
        if !p.exists() {
            bail!("{} is missing from model {}", name, self.id);
        }
        Ok(p)
    }

    /// First file in the installed model matching a predicate.
    pub(crate) fn find_file(&self, pred: impl Fn(&str) -> bool) -> Result<PathBuf> {
        let dir = self.dir()?;
        let mut names: Vec<String> = fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| pred(n))
            .collect();
        names.sort();
        names
            .first()
            .map(|n| dir.join(n))
            .with_context(|| format!("model {} is incomplete", self.id))
    }
}

pub fn installed() -> Vec<&'static Pack> {
    catalog().iter().filter(|p| p.is_installed()).collect()
}

/// Kind preference for a language. For Vietnamese, VieNeu sounds the most
/// natural; among the rest Piper beats Supertonic, whose Vietnamese
/// sometimes repeated or dropped words in our tests.
fn kind_rank(kind: Kind, lang: &str) -> u8 {
    match (family(&normalize_lang(lang)), kind) {
        ("vi", Kind::VieneuTurbo) => 0,
        ("vi", Kind::VieneuNano) => 1,
        ("vi", Kind::Piper) => 2,
        ("vi", Kind::Supertonic) => 3,
        (_, Kind::Kokoro) => 0,
        (_, Kind::Piper) => 1,
        (_, Kind::Supertonic) => 2,
        (_, Kind::VieneuTurbo | Kind::VieneuNano) => 3,
    }
}

fn quality_rank(p: &Pack) -> u8 {
    // Piper names end with "(quality)".
    match p
        .name
        .rsplit('(')
        .next()
        .unwrap_or("")
        .trim_end_matches(')')
    {
        "medium" => 0,
        "high" => 1,
        "low" => 2,
        "x_low" => 3,
        _ => 0,
    }
}

fn ranked<'a>(packs: impl Iterator<Item = &'a Pack>, lang: &str) -> Vec<&'a Pack> {
    let mut v: Vec<&Pack> = packs.filter(|p| p.language_match(lang) > 0).collect();
    v.sort_by_key(|p| {
        (
            kind_rank(p.kind, lang),
            2 - p.language_match(lang),
            p.priority,
            quality_rank(p),
            p.size,
        )
    });
    v
}

/// The best installed model for a language, if any.
pub fn best_installed(lang: &str) -> Option<&'static Pack> {
    ranked(catalog().iter().filter(|p| p.is_installed()), lang)
        .into_iter()
        .next()
}

/// The model to suggest downloading for a language. For Vietnamese that is
/// still the small Piper voice: VieNeu is a much bigger download and needs
/// a faster CPU, so it is offered in the voice list rather than by default.
pub fn recommended(lang: &str) -> Option<&'static Pack> {
    if family(&normalize_lang(lang)) == "vi" {
        if let Some(p) = find("vits-piper-vi_VN-vais1000-medium") {
            return Some(p);
        }
    }
    ranked(catalog().iter(), lang).into_iter().next()
}

/// Models that speak a language, best first.
pub fn for_language(lang: &str) -> Vec<&'static Pack> {
    ranked(catalog().iter(), lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_loads() {
        let c = catalog();
        assert!(c.len() > 50);
        assert!(c.iter().any(|p| p.kind == Kind::Kokoro));
        assert!(c.iter().any(|p| p.kind == Kind::Supertonic));
        let ids: std::collections::HashSet<_> = c.iter().map(|p| &p.id).collect();
        assert_eq!(ids.len(), c.len(), "ids are unique");
    }

    #[test]
    fn recommendations() {
        assert_eq!(
            recommended("vi").unwrap().id,
            "vits-piper-vi_VN-vais1000-medium"
        );
        assert_eq!(recommended("en-US").unwrap().kind, Kind::Kokoro);
        assert_eq!(recommended("zh").unwrap().kind, Kind::Kokoro);
        assert_eq!(recommended("ko").unwrap().kind, Kind::Supertonic);
        assert_eq!(recommended("de").unwrap().kind, Kind::Piper);
        assert!(recommended("xx").is_none());
        let vi = for_language("vi");
        assert_eq!(vi[0].kind, Kind::VieneuTurbo);
        assert_eq!(vi[1].kind, Kind::VieneuNano);
        assert_eq!(vi[2].id, "vits-piper-vi_VN-vais1000-medium");
        assert_eq!(vi.last().unwrap().kind, Kind::Supertonic);
        assert!(vi.iter().any(|p| p.kind == Kind::Supertonic));
        assert!(vi.iter().all(|p| p.kind != Kind::Kokoro));
        assert!(for_language("en").iter().all(|p| !p.kind.is_vieneu()));
    }

    #[test]
    fn vieneu_packs() {
        let turbo = find("vieneu-tts-v3-turbo").unwrap();
        assert_eq!(
            turbo.speakers[turbo.default_speaker("vi") as usize],
            "Hải Đăng"
        );
        let files = turbo.files();
        assert!(files.iter().any(|(u, n)| n == "vieneu_prefill.onnx"
            && u == "https://huggingface.co/pnnbao-ump/VieNeu-TTS-v3-Turbo/resolve/61b85e3d937fbbacb387714180e8182823512523/onnx_update/vieneu_prefill.onnx"));
        assert!(files.iter().any(|(_, n)| n == "sea_g2p.bin"));
        assert!(files.iter().any(|(_, n)| n == "voices_v3_turbo.json"));
        let nano = find("vieneu-tts-v3-nano").unwrap();
        assert_eq!(
            nano.speakers[nano.default_speaker("vi") as usize],
            "Minh Quân"
        );
        assert_eq!(nano.files().len(), 8);
        assert!(find("vits-piper-vi_VN-vais1000-medium")
            .unwrap()
            .files()
            .is_empty());
    }

    #[test]
    fn languages() {
        assert_eq!(normalize_lang("en-us"), "en_US");
        assert_eq!(normalize_lang("VI"), "vi");
        assert_eq!(recommended("en").unwrap().id, "kokoro-multi-lang-v1_0");
        let kokoro = find("kokoro-int8-multi-lang-v1_0").unwrap();
        assert_eq!(kokoro.language_match("en_GB"), 2);
        assert_eq!(kokoro.language_match("en_AU"), 1);
        assert_eq!(
            kokoro.speakers[kokoro.default_speaker("en") as usize],
            "af_heart"
        );
        assert_eq!(
            kokoro.speakers[kokoro.default_speaker("zh-CN") as usize],
            "zf_xiaoxiao"
        );
    }
}
