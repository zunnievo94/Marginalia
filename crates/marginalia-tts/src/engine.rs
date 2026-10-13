//! In-process speech synthesis with sherpa-onnx.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig, OfflineTtsSupertonicModelConfig, OfflineTtsVitsModelConfig,
};

use crate::audio::Audio;
use crate::models::{normalize_lang, Kind, Pack};
use crate::Synthesizer;

#[derive(Debug, Clone)]
pub struct Options {
    /// Speaker index; `None` picks the model's default for `lang`.
    pub speaker: Option<i32>,
    /// 1.0 is normal; 1.5 is 50% faster.
    pub speed: f32,
    /// Language of the text, e.g. `vi` or `en_US`.
    pub lang: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            speaker: None,
            speed: 1.0,
            lang: "en".into(),
        }
    }
}

/// Denoising steps for Supertonic: more is smoother but slower (16 runs
/// at ~0.6x real time on 4 CPU threads).
const SUPERTONIC_STEPS: i32 = 16;

pub struct Engine {
    tts: OfflineTts,
    generation: GenerationConfig,
    /// Display name, e.g. "Kokoro v1.0 · af_heart".
    pub label: String,
}

fn s(p: std::path::PathBuf) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

fn threads() -> i32 {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 4) as i32
}

impl Engine {
    /// Load an installed model.
    pub fn new(pack: &Pack, opts: &Options) -> Result<Engine> {
        if !pack.is_installed() {
            bail!("voice model {} is not downloaded", pack.id);
        }
        let lang = normalize_lang(&opts.lang);
        let mut model = OfflineTtsModelConfig {
            num_threads: threads(),
            ..Default::default()
        };
        let mut rule_fsts = None;
        let mut extra = None;
        match pack.kind {
            Kind::Piper => {
                model.vits = OfflineTtsVitsModelConfig {
                    model: s(pack.find_file(|n| n.ends_with(".onnx"))?),
                    tokens: s(pack.file("tokens.txt")?),
                    data_dir: s(pack.file("espeak-ng-data")?),
                    noise_scale: 0.667,
                    noise_scale_w: 0.8,
                    length_scale: 1.0,
                    ..Default::default()
                };
            }
            Kind::Kokoro => {
                let en_lexicon = if lang == "en_GB" {
                    "lexicon-gb-en.txt"
                } else {
                    "lexicon-us-en.txt"
                };
                let lexicons = [en_lexicon, "lexicon-zh.txt"]
                    .iter()
                    .map(|f| pack.file(f).map(|p| p.to_string_lossy().into_owned()))
                    .collect::<Result<Vec<_>>>()?
                    .join(",");
                model.kokoro = OfflineTtsKokoroModelConfig {
                    model: s(pack.find_file(|n| n.ends_with(".onnx"))?),
                    voices: s(pack.file("voices.bin")?),
                    tokens: s(pack.file("tokens.txt")?),
                    data_dir: s(pack.file("espeak-ng-data")?),
                    dict_dir: s(pack.file("dict")?),
                    lexicon: Some(lexicons),
                    length_scale: 1.0,
                    ..Default::default()
                };
                // Read dates, phone numbers and numbers in Chinese text.
                rule_fsts = Some(
                    ["date-zh.fst", "phone-zh.fst", "number-zh.fst"]
                        .iter()
                        .filter_map(|f| pack.file(f).ok())
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            Kind::Supertonic => {
                let f = |prefix: &str| {
                    pack.find_file(|n| n.starts_with(prefix) && n.ends_with(".onnx"))
                };
                model.supertonic = OfflineTtsSupertonicModelConfig {
                    duration_predictor: s(f("duration_predictor")?),
                    text_encoder: s(f("text_encoder")?),
                    vector_estimator: s(f("vector_estimator")?),
                    vocoder: s(f("vocoder")?),
                    tts_json: s(pack.file("tts.json")?),
                    unicode_indexer: s(pack.file("unicode_indexer.bin")?),
                    voice_style: s(pack.file("voice.bin")?),
                };
                let family = lang.split('_').next().unwrap_or("en").to_string();
                extra = Some(HashMap::from([(
                    "lang".to_string(),
                    serde_json::Value::String(family),
                )]));
            }
        }
        let config = OfflineTtsConfig {
            model,
            rule_fsts,
            ..Default::default()
        };
        let tts = OfflineTts::create(&config)
            .with_context(|| format!("could not load voice model {}", pack.id))?;

        let speakers = tts.num_speakers().max(1);
        let sid = opts
            .speaker
            .unwrap_or_else(|| pack.default_speaker(&lang))
            .clamp(0, speakers - 1);
        let speaker_name = pack.speakers.get(sid as usize).cloned();
        let label = match speaker_name {
            Some(n) if pack.speakers.len() > 1 => format!("{} · {n}", pack.name),
            _ => pack.name.clone(),
        };
        let generation = GenerationConfig {
            sid,
            speed: opts.speed.clamp(0.25, 4.0),
            num_steps: if pack.kind == Kind::Supertonic {
                SUPERTONIC_STEPS
            } else {
                GenerationConfig::default().num_steps
            },
            extra,
            ..Default::default()
        };
        Ok(Engine {
            tts,
            generation,
            label,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.tts.sample_rate().max(1) as u32
    }
}

impl Synthesizer for Engine {
    fn synthesize(&mut self, text: &str) -> Result<Audio> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return Ok(Audio::silence(self.sample_rate(), 0));
        }
        let audio = self
            .tts
            .generate_with_config(&text, &self.generation, None::<fn(&[f32], f32) -> bool>)
            .context("speech synthesis failed")?;
        Ok(Audio {
            sample_rate: audio.sample_rate().max(1) as u32,
            samples: audio
                .samples()
                .iter()
                .map(|&x| (x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect(),
        })
    }
}
