//! VieNeu-TTS v3 Nano: a 48M-parameter flow-matching model, 24 kHz.
//!
//! Port of `OnnxV3NanoEngine` (`src/vieneu/v3nano.py`): phonemes go through
//! a text encoder, a duration predictor sets the length, a vector field is
//! integrated with Euler steps (with classifier-free guidance) and a codec
//! decoder turns the latent into a waveform.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ort::session::Session;
use ort::value::DynValue;
use serde::Deserialize;

use super::{floats, npz, run, session, tensor, Phonemizer, Rng};

pub(crate) const SAMPLE_RATE: u32 = 24_000;
/// Graphs and data from the Hugging Face repository.
pub(crate) const REPO: &str = "pnnbao-ump/VieNeu-TTS-v3-Nano";
pub(crate) const REVISION: &str = "aba295eb96a6fa6003ebe417cc1f2802a7adc1dc";
pub(crate) const FILES: &[&str] = &[
    "text_encoder.onnx",
    "duration_predictor.onnx",
    "vector_estimator.onnx",
    "codec_decoder.onnx",
    "config.json",
    "constants.npz",
];
pub(crate) const VOICES: &str = "voices_v3_nano.json";

/// Euler steps (16 is VieNeu's default; 8 is twice as fast and rougher).
const STEPS: usize = 16;
/// Classifier-free guidance.
const CFG: f32 = 3.0;
/// Latent channels of the flow model (`rng.standard_normal((1, 144, T))`).
const LATENT: usize = 144;
/// Training clips were at most 15 s; longer targets drift.
const MAX_CHUNK_SECONDS: f32 = 15.0;
const MIN_FRAMES: usize = 2;
/// Nano was trained on short clips: keep chunks short.
const MAX_CHARS: usize = 140;
const MIN_CHARS: usize = 20;

#[derive(Deserialize)]
struct Config {
    vocab: HashMap<String, i64>,
    bos_id: i64,
    eos_id: i64,
    #[serde(default)]
    flow_fps: Option<f32>,
    #[serde(default)]
    emotion_tags: HashMap<String, String>,
}

#[derive(Deserialize)]
struct VoiceFile {
    presets: HashMap<String, Preset>,
}

#[derive(Deserialize)]
struct Preset {
    speaker_emb: Vec<f32>,
    style: Vec<Vec<f32>>,
}

pub(crate) struct Nano {
    text_encoder: Session,
    duration: Session,
    vector: Session,
    decoder: Session,
    phonemizer: Phonemizer,
    vocab: HashMap<char, i64>,
    emotion_tags: Vec<(String, String)>,
    bos: i64,
    eos: i64,
    fps: f32,
    /// The unconditional branch for guidance, built once.
    null_ctx: DynValue,
    null_spk: Vec<f32>,
    null_style: (Vec<usize>, Vec<f32>),
    /// The chosen voice: x-vector and `[50, 256]` style tokens.
    spk: Vec<f32>,
    style: (Vec<usize>, Vec<f32>),
    speed: f32,
    rng: Rng,
}

impl Nano {
    pub(crate) fn new(dir: &Path, voice: &str, speed: f32) -> Result<Nano> {
        let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)
            .context("reading the Nano config.json")?;
        let mut constants = npz::load(&dir.join("constants.npz"))?;
        let mut take = |name: &str| {
            constants
                .remove(name)
                .with_context(|| format!("constants.npz has no {name}"))
        };
        let null_spk = take("null_spk")?.into_f32();
        let null_style = take("null_style")?;
        let null_style = (
            [&[1][..], &null_style.shape[..]].concat(),
            null_style.into_f32(),
        );

        let voices: VoiceFile = serde_json::from_slice(&std::fs::read(dir.join(VOICES))?)
            .context("reading the Nano preset voices")?;
        let preset = voices
            .presets
            .get(voice)
            .with_context(|| format!("VieNeu Nano has no voice {voice:?}"))?;
        let rows = preset.style.len();
        let cols = preset.style.first().map_or(0, Vec::len);
        let style = (vec![1, rows, cols], preset.style.concat());

        let mut text_encoder = session(&dir.join("text_encoder.onnx"))?;
        let null_ids = vec![config.bos_id, config.eos_id];
        let null_ctx = run(
            &mut text_encoder,
            ort::inputs![
                "ids" => tensor(&[1, 2], null_ids)?,
                "style" => tensor(&null_style.0, null_style.1.clone())?,
            ],
        )?
        .into_iter()
        .next()
        .context("text encoder gave no output")?;

        let mut emotion_tags: Vec<(String, String)> = config.emotion_tags.into_iter().collect();
        emotion_tags.sort();
        Ok(Nano {
            text_encoder,
            duration: session(&dir.join("duration_predictor.onnx"))?,
            vector: session(&dir.join("vector_estimator.onnx"))?,
            decoder: session(&dir.join("codec_decoder.onnx"))?,
            phonemizer: Phonemizer::new(&dir.join(super::SEA_G2P_DICT))?,
            vocab: config
                .vocab
                .into_iter()
                .filter_map(|(k, v)| {
                    let mut chars = k.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => Some((c, v)),
                        _ => None,
                    }
                })
                .collect(),
            emotion_tags,
            bos: config.bos_id,
            eos: config.eos_id,
            fps: config.flow_fps.unwrap_or(24000.0 / 256.0 / 6.0),
            null_ctx,
            null_spk,
            null_style,
            spk: preset.speaker_emb.clone(),
            style,
            speed: speed.clamp(0.5, 2.0),
            rng: Rng::new(),
        })
    }

    /// Phoneme string to ids, dropping characters outside the vocabulary
    /// (`encode_phones`).
    fn encode(&self, phones: &str) -> Vec<i64> {
        let mut s = phones.to_string();
        for (tag, ch) in &self.emotion_tags {
            s = s.replace(tag, ch);
        }
        let mut ids = vec![self.bos];
        ids.extend(s.chars().filter_map(|c| self.vocab.get(&c).copied()));
        ids.push(self.eos);
        ids
    }

    /// One chunk of phonemes to a 24 kHz waveform (`OnnxV3NanoEngine.infer`).
    fn infer(&mut self, phones: &str) -> Result<Vec<f32>> {
        let ids = self.encode(phones);
        let len = ids.len();
        // pad_id never occurs in `ids`, so the mask is all true.
        let mask = || tensor(&[1, len], vec![true; len]);
        let spk = || tensor(&[1, self.spk.len()], self.spk.clone());
        let style = || tensor(&self.style.0, self.style.1.clone());

        let ctx = run(
            &mut self.text_encoder,
            ort::inputs!["ids" => tensor(&[1, len], ids)?, "style" => style()?],
        )?
        .into_iter()
        .next()
        .context("text encoder gave no output")?;
        let dur = run(
            &mut self.duration,
            ort::inputs!["ctx" => &ctx, "ctx_mask" => mask()?, "spk" => spk()?],
        )?;
        let log_s = *floats(&dur[0])?.1.first().context("no duration")?;
        let secs = (log_s.exp() / self.speed.max(1e-3)).min(MAX_CHUNK_SECONDS);
        let frames = ((secs * self.fps).round() as usize).max(MIN_FRAMES);

        let mut x: Vec<f32> = (0..LATENT * frames).map(|_| self.rng.normal()).collect();
        let shape = [1, LATENT, frames];
        let null_mask = tensor(&[1, 2], vec![true; 2])?;
        let null_spk = tensor(&[1, self.null_spk.len()], self.null_spk.clone())?;
        let null_style = tensor(&self.null_style.0, self.null_style.1.clone())?;
        let (spk_t, style_t, mask_t) = (spk()?, style()?, mask()?);
        // Uniform time grid (VieNeu's `sway` = 0).
        for i in 0..STEPS {
            let t0 = i as f32 / STEPS as f32;
            let dt = 1.0 / STEPS as f32;
            let out = run(
                &mut self.vector,
                ort::inputs![
                    "x" => tensor(&shape, x.clone())?,
                    "t" => tensor(&[1], vec![t0])?,
                    "ctx" => &ctx,
                    "ctx_mask" => &mask_t,
                    "spk" => &spk_t,
                    "style" => &style_t,
                ],
            )?;
            let mut v = floats(&out[0])?.1.to_vec();
            if CFG > 0.0 {
                let out = run(
                    &mut self.vector,
                    ort::inputs![
                        "x" => tensor(&shape, x.clone())?,
                        "t" => tensor(&[1], vec![t0])?,
                        "ctx" => &self.null_ctx,
                        "ctx_mask" => &null_mask,
                        "spk" => &null_spk,
                        "style" => &null_style,
                    ],
                )?;
                let vu = floats(&out[0])?.1;
                for (v, u) in v.iter_mut().zip(vu) {
                    *v = u + CFG * (*v - u);
                }
            }
            if v.len() != x.len() {
                bail!(
                    "vector estimator returned {} values for {}",
                    v.len(),
                    x.len()
                );
            }
            for (x, v) in x.iter_mut().zip(&v) {
                *x += dt * v;
            }
        }
        let out = run(&mut self.decoder, ort::inputs!["x" => tensor(&shape, x)?])?;
        let (dims, wav) = floats(&out[0])?;
        // [1, 1, samples]: the first channel.
        let n = dims.last().copied().unwrap_or(wav.len()).min(wav.len());
        let wav: Vec<f32> = wav[..n].iter().map(|s| s.clamp(-1.0, 1.0)).collect();
        Ok(super::trim_and_fade(&wav, SAMPLE_RATE))
    }

    pub(crate) fn synthesize(&mut self, text: &str) -> Result<Vec<f32>> {
        let normalized = self.phonemizer.normalize(text);
        let mut parts = Vec::new();
        for chunk in super::chunk_sentence(&normalized, MAX_CHARS, MIN_CHARS) {
            let phones = self.phonemizer.phonemize(&chunk);
            if phones.trim().is_empty() {
                continue;
            }
            let wav = self
                .infer(&phones)
                .map_err(|e| anyhow!("VieNeu Nano: {e:#}"))?;
            parts.push((wav, super::pause_after(&chunk)));
        }
        Ok(super::join(&parts, SAMPLE_RATE))
    }
}
