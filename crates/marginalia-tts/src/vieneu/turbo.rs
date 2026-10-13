//! VieNeu-TTS v3 Turbo: an autoregressive speech model over MOSS audio codec
//! tokens, 48 kHz. VieNeu's default and most natural voice.
//!
//! Port of `OnnxV3LiteEngine`
//! (`src/vieneu/_v3_turbo_engine/onnx_runtime_lite.py`): the transformer
//! forwards and the codec run in ONNX Runtime; embeddings, the speaker
//! anchor, the output heads and sampling are plain Rust here, as they are
//! NumPy there. Each audio frame is 16 codebook tokens; a small "acoustic"
//! transformer emits them one by one, and the text head says when to stop.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ort::session::Session;
use ort::value::DynValue;
use serde::Deserialize;

use super::{floats, npz, run, session, tensor, Phonemizer, Rng};

pub(crate) const SAMPLE_RATE: u32 = 48_000;
pub(crate) const REPO: &str = "pnnbao-ump/VieNeu-TTS-v3-Turbo";
pub(crate) const REVISION: &str = "61b85e3d937fbbacb387714180e8182823512523";
/// The fp32 graphs (VieNeu's CPU default; the int8 ones garble audio on
/// CPUs without VNNI).
pub(crate) const SUBFOLDER: &str = "onnx_update";
pub(crate) const FILES: &[&str] = &[
    "vieneu_prefill.onnx",
    "vieneu_decode_step.onnx",
    "vieneu_acoustic_cached.onnx",
    "vieneu_backbone_shared.data",
    "vieneu_v3_heads.npz",
    "config.json",
    "tokenizer.json",
];
pub(crate) const CODEC_REPO: &str = "OpenMOSS-Team/MOSS-Audio-Tokenizer-Nano-ONNX";
pub(crate) const CODEC_REVISION: &str = "ceff0d0749bfb3fa2d61149794ec6feef0d1e1ae";
pub(crate) const CODEC_FILES: &[&str] = &[
    "moss_audio_tokenizer_decode_full.onnx",
    "moss_audio_tokenizer_decode_shared.data",
];
pub(crate) const VOICES: &str = "voices_v3_turbo.json";

// Sampling defaults of `V3TurboVieNeuTTS.infer`.
const TEMPERATURE: f32 = 0.8;
const TOP_K: usize = 25;
const TOP_P: f32 = 0.95;
const REPETITION_PENALTY: f32 = 1.2;
const REPETITION_WINDOW: usize = 64;
const MAX_NEW_FRAMES: usize = 300;
const MAX_CHARS: usize = 256;
const MIN_CHARS: usize = 20;
/// What the reference encoder emits for a padded last frame (an audible
/// click, VieNeu issue #198); dropped from preset codes.
const ENCODER_PAD_CODE: i64 = 455;
const BABBLE_MAX_RETRIES: usize = 2;

#[derive(Deserialize)]
struct Config {
    n_vq: usize,
    hidden_size: usize,
    num_hidden_layers: usize,
    #[serde(default = "one")]
    local_num_hidden_layers: usize,
    #[serde(default = "eight")]
    local_num_attention_heads: usize,
    audio_pad_token_id: i64,
    text_prompt_start_token_id: i64,
    text_prompt_end_token_id: i64,
    speech_generation_start_token_id: i64,
    speech_generation_end_token_id: i64,
    audio_ref_slot_token_id: i64,
    #[serde(default = "sixteen")]
    default_style_token_id: i64,
    #[serde(default)]
    use_speaker_embedding: bool,
}

fn one() -> usize {
    1
}
fn eight() -> usize {
    8
}
fn sixteen() -> i64 {
    16
}

#[derive(Deserialize)]
struct VoiceFile {
    presets: HashMap<String, Preset>,
}

#[derive(Deserialize)]
struct Preset {
    speaker_emb: Vec<f32>,
    #[serde(default)]
    codes: Option<Vec<Vec<i64>>>,
}

/// A row-major matrix of embeddings / output-head weights.
struct Matrix {
    rows: usize,
    cols: usize,
    data: Vec<f32>,
}

impl Matrix {
    fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.cols..(i + 1) * self.cols]
    }

    /// `self @ v`: one logit per row.
    fn logits(&self, v: &[f32]) -> Vec<f32> {
        (0..self.rows).map(|r| dot(self.row(r), v)).collect()
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Eight partial sums so the compiler can vectorize.
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

pub(crate) struct Turbo {
    prefill: Session,
    decode: Session,
    acoustic: Session,
    codec: Session,
    tokenizer: tokenizers::Tokenizer,
    phonemizer: Phonemizer,
    cfg: Config,
    /// Text embeddings, tied with the text head.
    text_emb: Matrix,
    /// One embedding table per codebook, tied with its head.
    audio_emb: Vec<Matrix>,
    local_head_dim: usize,
    /// The voice: speaker anchor (projected x-vector) and reference codes.
    anchor: Option<Vec<f32>>,
    ref_codes: Vec<Vec<i64>>,
    rng: Rng,
}

impl Turbo {
    pub(crate) fn new(dir: &Path, voice: &str) -> Result<Turbo> {
        let cfg: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)
            .context("reading the Turbo config.json")?;
        let h = cfg.hidden_size;

        let mut heads = npz::load(&dir.join("vieneu_v3_heads.npz"))?;
        let mut take = |name: &str| {
            heads
                .remove(name)
                .with_context(|| format!("vieneu_v3_heads.npz has no {name}"))
        };
        let text = take("text_emb")?;
        if text.shape.len() != 2 || text.shape[1] != h {
            bail!("unexpected text_emb shape {:?}", text.shape);
        }
        let text_emb = Matrix {
            rows: text.shape[0],
            cols: h,
            data: text.into_f32(),
        };
        let audio = take("audio_emb")?;
        if audio.shape.len() != 3 || audio.shape[0] != cfg.n_vq || audio.shape[2] != h {
            bail!("unexpected audio_emb shape {:?}", audio.shape);
        }
        let per = audio.shape[1] * h;
        let audio_rows = audio.shape[1];
        let all = audio.into_f32();
        let audio_emb = all
            .chunks_exact(per)
            .map(|c| Matrix {
                rows: audio_rows,
                cols: h,
                data: c.to_vec(),
            })
            .collect();

        let voices: VoiceFile =
            serde_json::from_slice(&std::fs::read(dir.join(super::turbo::VOICES))?)
                .context("reading the Turbo preset voices")?;
        let preset = voices
            .presets
            .get(voice)
            .with_context(|| format!("VieNeu Turbo has no voice {voice:?}"))?;
        let anchor = if cfg.use_speaker_embedding {
            let mut xvec = |n: &str| {
                heads
                    .remove(n)
                    .with_context(|| format!("vieneu_v3_heads.npz has no {n}"))
            };
            let w = xvec("xvec_w")?;
            let w = Matrix {
                rows: w.shape[0],
                cols: w.shape.get(1).copied().unwrap_or(0),
                data: w.into_f32(),
            };
            let b = xvec("xvec_b")?.into_f32();
            let ln_w = xvec("xvec_ln_w")?.into_f32();
            let ln_b = xvec("xvec_ln_b")?.into_f32();
            let eps = xvec("xvec_ln_eps")?.scalar() as f32;
            Some(speaker_anchor(
                &preset.speaker_emb,
                &w,
                &b,
                &ln_w,
                &ln_b,
                eps,
            )?)
        } else {
            None
        };
        let mut ref_codes = preset.codes.clone().unwrap_or_default();
        if ref_codes.len() > 1
            && ref_codes.last().and_then(|f| f.first()) == Some(&ENCODER_PAD_CODE)
        {
            ref_codes.pop();
        }
        if ref_codes.iter().any(|f| f.len() != cfg.n_vq) {
            bail!("voice {voice:?} has malformed reference codes");
        }

        let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("reading tokenizer.json: {e}"))?;
        Ok(Turbo {
            prefill: session(&dir.join("vieneu_prefill.onnx"))?,
            decode: session(&dir.join("vieneu_decode_step.onnx"))?,
            acoustic: session(&dir.join("vieneu_acoustic_cached.onnx"))?,
            codec: session(&dir.join("moss_audio_tokenizer_decode_full.onnx"))?,
            tokenizer,
            phonemizer: Phonemizer::new(&dir.join(super::SEA_G2P_DICT))?,
            local_head_dim: h / cfg.local_num_attention_heads,
            cfg,
            text_emb,
            audio_emb,
            anchor,
            ref_codes,
            rng: Rng::new(),
        })
    }

    /// Prompt rows `[text id, 16 audio ids]` (`_build_rows`): style token,
    /// text start, phoneme tokens, text end, then the reference frames.
    fn build_rows(&self, phonemes: &str) -> Result<Vec<Vec<i64>>> {
        let enc = self
            .tokenizer
            .encode(phonemes, false)
            .map_err(|e| anyhow!("tokenizing phonemes: {e}"))?;
        let c = &self.cfg;
        let mut text = vec![c.default_style_token_id, c.text_prompt_start_token_id];
        text.extend(enc.get_ids().iter().map(|&i| i as i64));
        text.push(c.text_prompt_end_token_id);
        let mut rows: Vec<Vec<i64>> = text
            .into_iter()
            .map(|t| {
                let mut r = vec![c.audio_pad_token_id; c.n_vq + 1];
                r[0] = t;
                r
            })
            .collect();
        for frame in &self.ref_codes {
            let mut r = vec![c.audio_ref_slot_token_id];
            r.extend(frame);
            rows.push(r);
        }
        Ok(rows)
    }

    /// Sum of the text and audio embeddings of each row, plus the speaker
    /// anchor (`_embed_rows`). Returns `(len, flat [len * H])`.
    fn embed_rows(&self, rows: &[Vec<i64>]) -> Result<Vec<f32>> {
        let h = self.cfg.hidden_size;
        let mut out = Vec::with_capacity(rows.len() * h);
        for row in rows {
            let t = usize::try_from(row[0])
                .ok()
                .filter(|&t| t < self.text_emb.rows)
                .context("text token out of range")?;
            let mut e = self.text_emb.row(t).to_vec();
            for (ch, &id) in row[1..].iter().enumerate() {
                if id == self.cfg.audio_pad_token_id {
                    continue;
                }
                let table = &self.audio_emb[ch];
                let id = usize::try_from(id)
                    .ok()
                    .filter(|&i| i < table.rows)
                    .context("audio token out of range")?;
                for (x, y) in e.iter_mut().zip(table.row(id)) {
                    *x += y;
                }
            }
            if let Some(a) = &self.anchor {
                for (x, y) in e.iter_mut().zip(a) {
                    *x += y;
                }
            }
            out.extend(e);
        }
        Ok(out)
    }

    fn sample(&mut self, mut logits: Vec<f32>, prev: Option<&Window>) -> usize {
        if let Some(prev) = prev {
            for &i in prev.counts.keys() {
                if let Some(l) = logits.get_mut(i) {
                    *l = if *l < 0.0 {
                        *l * REPETITION_PENALTY
                    } else {
                        *l / REPETITION_PENALTY
                    };
                }
            }
        }
        sample_top_k_p(&mut logits, TEMPERATURE, TOP_K, TOP_P, &mut self.rng)
    }

    /// One audio frame from the backbone's hidden state (`_acoustic_frame`):
    /// 16 codebook tokens and whether the text head says "end of speech".
    fn acoustic_frame(&mut self, h: &[f32], hist: &mut [Window]) -> Result<(Vec<i64>, bool)> {
        let hid = self.cfg.hidden_size;
        let layers = self.cfg.local_num_hidden_layers;
        let heads = self.cfg.local_num_attention_heads;
        let hd = self.local_head_dim;
        let mut tok = h.to_vec();
        tok.extend_from_slice(
            self.text_emb
                .row(self.cfg.speech_generation_start_token_id as usize),
        );

        let mut inputs = ort::inputs![
            "token_emb" => tensor(&[1, 2, hid], tok)?,
            "position_ids" => tensor(&[1, 2], vec![0i64, 1])?,
        ];
        for i in 0..layers {
            inputs.push((
                format!("past_k_{i}").into(),
                tensor::<f32>(&[1, heads, 0, hd], vec![])?.into(),
            ));
            inputs.push((
                format!("past_v_{i}").into(),
                tensor::<f32>(&[1, heads, 0, hd], vec![])?.into(),
            ));
        }
        let mut out = run(&mut self.acoustic, inputs)?;
        let (slot0, last) = {
            let (_, hidden) = floats(&out[0])?;
            (hidden[..hid].to_vec(), hidden[hid..2 * hid].to_vec())
        };
        let logits = self.audio_emb[0].logits(&last);
        let mut codes = vec![self.sample(logits, Some(&hist[0])) as i64];
        hist[0].add(codes[0] as usize);

        for ch in 1..self.cfg.n_vq {
            let emb = self.audio_emb[ch - 1].row(codes[ch - 1] as usize).to_vec();
            let past: Vec<DynValue> = out.drain(1..1 + 2 * layers).collect();
            let mut inputs = ort::inputs![
                "token_emb" => tensor(&[1, 1, hid], emb)?,
                "position_ids" => tensor(&[1, 1], vec![ch as i64 + 1])?,
            ];
            let mut past = past.into_iter();
            let (ks, vs): (Vec<_>, Vec<_>) = (past.by_ref().take(layers).collect(), past.collect());
            for (i, (k, v)) in ks.into_iter().zip(vs).enumerate() {
                inputs.push((format!("past_k_{i}").into(), k.into()));
                inputs.push((format!("past_v_{i}").into(), v.into()));
            }
            out = run(&mut self.acoustic, inputs)?;
            let (_, hidden) = floats(&out[0])?;
            let logits = self.audio_emb[ch].logits(&hidden[..hid]);
            let code = self.sample(logits, Some(&hist[ch]));
            hist[ch].add(code);
            codes.push(code as i64);
        }
        let text_logits = self.text_emb.logits(&slot0);
        let eos = argmax(&text_logits) as i64 == self.cfg.speech_generation_end_token_id;
        Ok((codes, eos))
    }

    /// Generate the codec frames of one chunk (`_gen_once`).
    fn generate(
        &mut self,
        prompt: &[f32],
        prompt_len: usize,
        max_frames: usize,
    ) -> Result<Vec<Vec<i64>>> {
        let hid = self.cfg.hidden_size;
        let layers = self.cfg.num_hidden_layers;
        let mut out = run(
            &mut self.prefill,
            ort::inputs!["inputs_embeds" => tensor(&[1, prompt_len, hid], prompt.to_vec())?],
        )?;
        let mut h = {
            let (_, hidden) = floats(&out[0])?;
            hidden[(prompt_len - 1) * hid..prompt_len * hid].to_vec()
        };
        let mut hist: Vec<Window> = (0..self.cfg.n_vq).map(|_| Window::default()).collect();
        let mut frames = Vec::new();
        for t in 0..max_frames {
            let (codes, eos) = self.acoustic_frame(&h, &mut hist)?;
            frames.push(codes.clone());
            if eos {
                break;
            }
            let mut slot = vec![self.cfg.speech_generation_start_token_id];
            slot.extend(&codes);
            let emb = self.embed_rows(&[slot])?;
            let past: Vec<DynValue> = out.drain(1..1 + 2 * layers).collect();
            let mut inputs = ort::inputs![
                "inputs_embeds" => tensor(&[1, 1, hid], emb)?,
                "position_ids" => tensor(&[1, 1], vec![(prompt_len + t) as i64])?,
            ];
            let mut past = past.into_iter();
            let (ks, vs): (Vec<_>, Vec<_>) = (past.by_ref().take(layers).collect(), past.collect());
            for (i, (k, v)) in ks.into_iter().zip(vs).enumerate() {
                inputs.push((format!("past_k_{i}").into(), k.into()));
                inputs.push((format!("past_v_{i}").into(), v.into()));
            }
            out = run(&mut self.decode, inputs)?;
            h = floats(&out[0])?.1[..hid].to_vec();
        }
        Ok(frames)
    }

    /// Codec frames to a mono waveform (`_decode_codes`).
    fn decode_codes(&mut self, frames: &[Vec<i64>]) -> Result<Vec<f32>> {
        let n = frames.len();
        let codes: Vec<i32> = frames.iter().flatten().map(|&c| c as i32).collect();
        let out = run(
            &mut self.codec,
            ort::inputs![
                "audio_codes" => tensor(&[1, n, self.cfg.n_vq], codes)?,
                "audio_code_lengths" => tensor(&[1], vec![n as i32])?,
            ],
        )?;
        let (dims, data) = floats(&out[0])?;
        // [1, channels, samples] -> mean over channels.
        let (channels, samples) = match dims.as_slice() {
            [_, c, s] => (*c, *s),
            [_, s] => (1, *s),
            _ => bail!("unexpected codec output shape {dims:?}"),
        };
        Ok((0..samples)
            .map(|i| (0..channels).map(|c| data[c * samples + i]).sum::<f32>() / channels as f32)
            .collect())
    }

    /// One chunk of phonemes to a 48 kHz waveform (`OnnxV3LiteEngine.infer`),
    /// with VieNeu's guard against short chunks that babble on.
    fn infer(&mut self, phonemes: &str) -> Result<Vec<f32>> {
        let max_frames = MAX_NEW_FRAMES.min(max_expected_frames(phonemes));
        let rows = self.build_rows(phonemes)?;
        let prompt = self.embed_rows(&rows)?;
        let frames = self.generate(&prompt, rows.len(), max_frames)?;
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let mut wav = self.decode_codes(&frames)?;
        let mut best = babble_suspect(&wav, phonemes, max_frames, frames.len());
        let mut tries = 0;
        while best.suspect && tries < BABBLE_MAX_RETRIES {
            tries += 1;
            let again = self.generate(&prompt, rows.len(), max_frames)?;
            if again.is_empty() {
                continue;
            }
            let w = self.decode_codes(&again)?;
            let cand = babble_suspect(&w, phonemes, max_frames, again.len());
            if cand.better_than(&best) {
                wav = w;
                best = cand;
            }
        }
        Ok(wav.into_iter().map(|s| s.clamp(-1.0, 1.0)).collect())
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
                .map_err(|e| anyhow!("VieNeu Turbo: {e:#}"))?;
            parts.push((wav, super::pause_after(&chunk)));
        }
        Ok(super::join(&parts, SAMPLE_RATE))
    }
}

/// The 192-d x-vector projected into the model (`_speaker_anchor`: Linear
/// then LayerNorm).
fn speaker_anchor(
    x: &[f32],
    w: &Matrix,
    b: &[f32],
    ln_w: &[f32],
    ln_b: &[f32],
    eps: f32,
) -> Result<Vec<f32>> {
    if w.cols != x.len() || b.len() != w.rows || ln_w.len() != w.rows || ln_b.len() != w.rows {
        bail!(
            "speaker projection does not match the voice ({} values)",
            x.len()
        );
    }
    if x.iter().all(|&v| v == 0.0) {
        bail!("the voice has an empty speaker embedding");
    }
    let v: Vec<f32> = w.logits(x).iter().zip(b).map(|(a, b)| a + b).collect();
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    let sd = (var + eps).sqrt();
    Ok(v.iter()
        .zip(ln_w)
        .zip(ln_b)
        .map(|((x, w), b)| (x - mean) / sd * w + b)
        .collect())
}

fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .fold((0, f32::NEG_INFINITY), |best, (i, &x)| {
            if x > best.1 {
                (i, x)
            } else {
                best
            }
        })
        .0
}

/// Temperature, top-k then nucleus sampling, as `OnnxV3LiteEngine._sample`.
fn sample_top_k_p(
    logits: &mut [f32],
    temperature: f32,
    top_k: usize,
    top_p: f32,
    rng: &mut Rng,
) -> usize {
    if temperature <= 0.0 {
        return argmax(logits);
    }
    for l in logits.iter_mut() {
        *l /= temperature;
    }
    let mut cand: Vec<usize> = (0..logits.len()).collect();
    cand.sort_unstable_by(|&a, &b| logits[b].total_cmp(&logits[a]));
    if top_k > 0 && top_k < cand.len() {
        cand.truncate(top_k);
    }
    let max = logits[cand[0]];
    let mut p: Vec<f64> = cand
        .iter()
        .map(|&i| ((logits[i] - max) as f64).exp())
        .collect();
    let sum: f64 = p.iter().sum();
    p.iter_mut().for_each(|x| *x /= sum);
    if top_p < 1.0 {
        let mut cum = 0.0;
        for x in p.iter_mut() {
            let keep = cum < top_p as f64;
            cum += *x;
            if !keep {
                *x = 0.0;
            }
        }
        let sum: f64 = p.iter().sum();
        p.iter_mut().for_each(|x| *x /= sum);
    }
    let mut r = rng.uniform();
    for (i, &x) in p.iter().enumerate() {
        if r < x {
            return cand[i];
        }
        r -= x;
    }
    cand[p.iter().rposition(|&x| x > 0.0).unwrap_or(0)]
}

/// The codes one codebook produced in the last `REPETITION_WINDOW` frames:
/// the repetition penalty breaks local loops without punishing a vowel or
/// silence for having occurred long ago (`rep_history.py`).
#[derive(Default)]
struct Window {
    counts: HashMap<usize, usize>,
    order: VecDeque<usize>,
}

impl Window {
    fn add(&mut self, code: usize) {
        *self.counts.entry(code).or_default() += 1;
        self.order.push_back(code);
        if self.order.len() > REPETITION_WINDOW {
            let old = self.order.pop_front().unwrap();
            match self.counts.get_mut(&old) {
                Some(n) if *n > 1 => *n -= 1,
                _ => {
                    self.counts.remove(&old);
                }
            }
        }
    }
}

// ── Frame cap and babble guard (`vieneu_utils/core_utils.py`) ──────────────

const MAX_FRAMES_PER_PHONE: f32 = 2.0;
const FRAME_CAP_SLACK: usize = 24;
const SINGLE_WORD_MAX_FRAMES: usize = 13;
const SYLLABLE_CAP_PER_EXTRA: usize = 5;
const SYLLABLE_CAP_MAX_SYL: usize = 4;
const SINGLE_WORD_MAX_PHONES: usize = 24;
const BABBLE_MAX_SYLLABLES: usize = 3;

fn strip_markup(phonemes: &str) -> String {
    let mut s = phonemes.replace("<en>", "").replace("</en>", "");
    while let Some(a) = s.find("<|emotion_") {
        match s[a..].find("|>") {
            Some(b) => s.replace_range(a..a + b + 2, ""),
            None => break,
        }
    }
    s
}

/// The most frames a chunk with these phonemes plausibly needs; past it
/// the model is making things up (`max_expected_frames`).
fn max_expected_frames(phonemes: &str) -> usize {
    let stripped = strip_markup(phonemes);
    let len = stripped.chars().count();
    let mut cap = FRAME_CAP_SLACK + (MAX_FRAMES_PER_PHONE * len as f32).ceil() as usize;
    if !phonemes.contains("<|emotion_") {
        let syl = syllable_count(phonemes).max(1);
        if syl <= SYLLABLE_CAP_MAX_SYL && len <= SINGLE_WORD_MAX_PHONES * syl {
            cap = cap.min(SINGLE_WORD_MAX_FRAMES + SYLLABLE_CAP_PER_EXTRA * (syl - 1));
        }
    }
    cap
}

/// Syllables in a sea-g2p phoneme string (`syllable_count`): a Vietnamese
/// word is one syllable, an English one is counted by vowel groups.
fn syllable_count(phonemes: &str) -> usize {
    const VOWELS: &str = "aeiouyæɐɑɒɔəɘɛɜɤɯɵøœʉʊʌɪɨɚɝᵻᵿ";
    let mut total = 0;
    for tok in strip_markup(phonemes).split_whitespace() {
        let (mut groups, mut in_v, mut consonant_seen) = (0, false, true);
        for ch in tok.chars() {
            if VOWELS.contains(ch) {
                if !in_v && consonant_seen {
                    groups += 1;
                }
                in_v = true;
                consonant_seen = false;
            } else if "ːˈˌ".contains(ch) || ch.is_ascii_digit() {
                if "ˈˌ".contains(ch) && groups > 0 {
                    in_v = false;
                    consonant_seen = true;
                } else {
                    in_v = false;
                }
            } else {
                in_v = false;
                consonant_seen = true;
            }
        }
        if tok.chars().any(char::is_alphabetic) {
            total += groups.max(1);
        }
    }
    total
}

/// Bursts of energy (about one per syllable) in a waveform
/// (`count_speech_bursts`).
fn count_speech_bursts(wav: &[f32], sr: u32) -> usize {
    let hop = ((sr as f32 * 0.010) as usize).max(1);
    let n = wav.len() / hop;
    if n == 0 {
        return 0;
    }
    let env: Vec<f32> = (0..n)
        .map(|i| {
            (wav[i * hop..(i + 1) * hop]
                .iter()
                .map(|x| x * x)
                .sum::<f32>()
                / hop as f32)
                .sqrt()
        })
        .collect();
    let peak = env.iter().copied().fold(0.0, f32::max);
    if peak <= 1e-6 {
        return 0;
    }
    let thresh = peak * 10f32.powf(-18.0 / 20.0);
    let (min_gap, min_len) = (6, 3);
    let mut bursts = Vec::new();
    let (mut start, mut last_on): (Option<usize>, usize) = (None, 0);
    for (i, &e) in env.iter().enumerate() {
        if e > thresh {
            match start {
                None => start = Some(i),
                Some(s) if i - last_on > min_gap => {
                    bursts.push((s, last_on));
                    start = Some(i);
                }
                _ => {}
            }
            last_on = i;
        }
    }
    if let Some(s) = start {
        bursts.push((s, last_on));
    }
    bursts.iter().filter(|(a, b)| b - a + 1 >= min_len).count()
}

#[derive(Debug, Clone, Copy)]
struct Babble {
    suspect: bool,
    bursts: usize,
    frames: usize,
}

impl Babble {
    /// `babble_prefer`: a clean take beats a suspect one, then fewer bursts,
    /// then fewer frames.
    fn better_than(&self, old: &Babble) -> bool {
        if self.suspect != old.suspect {
            return !self.suspect;
        }
        self.bursts < old.bursts || (self.bursts == old.bursts && self.frames < old.frames)
    }
}

/// A chunk of one to three syllables that came out with more bursts than
/// syllables, or ran into the frame cap, probably gained invented words
/// (`babble_suspect`).
fn babble_suspect(wav: &[f32], phonemes: &str, cap: usize, frames: usize) -> Babble {
    let syl = syllable_count(phonemes);
    if syl == 0 || syl > BABBLE_MAX_SYLLABLES || phonemes.contains("<|emotion_") {
        return Babble {
            suspect: false,
            bursts: 0,
            frames: 0,
        };
    }
    let bursts = count_speech_bursts(wav, SAMPLE_RATE);
    let hit_cap = syl <= 2 && frames + 1 >= cap;
    Babble {
        suspect: bursts > syl || hit_cap,
        bursts,
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints the token ids of each line of `$VIENEU_TEST_PHONES` with the
    /// tokenizer at `$VIENEU_TEST_TOKENIZER`, for comparing with Python's
    /// `tokenizers`.
    #[test]
    #[ignore]
    fn tokenize_lines() {
        let tok = tokenizers::Tokenizer::from_file(std::env::var("VIENEU_TEST_TOKENIZER").unwrap())
            .unwrap();
        let phones = std::fs::read_to_string(std::env::var("VIENEU_TEST_PHONES").unwrap()).unwrap();
        for line in phones.lines() {
            let ids = tok.encode(line, false).unwrap();
            let ids: Vec<String> = ids.get_ids().iter().map(u32::to_string).collect();
            println!("IDS\t{}", ids.join(" "));
        }
    }

    #[test]
    fn counts_syllables_like_vieneu() {
        // Examples from the comments in core_utils.py.
        assert_eq!(syllable_count("kwˈaːɜ"), 1);
        assert_eq!(syllable_count("bˈaːɜw kwˈaːɜ."), 2);
        assert_eq!(syllable_count("kɹiːˈeɪt"), 2);
        assert_eq!(syllable_count("kəmpjˈuːɾɚ"), 3);
        assert_eq!(syllable_count("ɹˈeɪdɪˌoʊ"), 3);
        assert_eq!(syllable_count("<|emotion_1|> ."), 0);
    }

    #[test]
    fn caps_frames() {
        // One syllable: about a second.
        assert_eq!(max_expected_frames("tʃˈaːw2."), 13);
        assert_eq!(max_expected_frames("tʃˈaːw2 bˈaːn5."), 18);
        let long = "tʃˈaːw2 ".repeat(20);
        assert_eq!(max_expected_frames(&long), 24 + 2 * long.chars().count());
        assert_eq!(strip_markup("a <|emotion_2|>. <en>b</en>"), "a . b");
    }

    #[test]
    fn samples_within_top_k() {
        let mut rng = Rng(7);
        let logits: Vec<f32> = (0..100).map(|i| i as f32 / 10.0).collect();
        for _ in 0..200 {
            let i = sample_top_k_p(&mut logits.clone(), 0.8, 25, 0.95, &mut rng);
            assert!(i >= 75, "{i}");
        }
        assert_eq!(
            sample_top_k_p(&mut logits.clone(), 0.0, 25, 0.95, &mut rng),
            99
        );
    }

    #[test]
    fn repetition_window_forgets() {
        let mut w = Window::default();
        for i in 0..(REPETITION_WINDOW + 10) {
            w.add(i);
        }
        assert_eq!(w.order.len(), REPETITION_WINDOW);
        assert!(!w.counts.contains_key(&0));
        assert_eq!(w.counts.values().sum::<usize>(), REPETITION_WINDOW);
    }

    #[test]
    fn bursts_and_anchor() {
        let sr = SAMPLE_RATE as usize;
        let mut wav = vec![0.0; sr / 10];
        for _ in 0..2 {
            wav.extend((0..sr / 5).map(|i| (i as f32 * 0.05).sin() * 0.5));
            wav.extend(vec![0.0; sr / 5]);
        }
        assert_eq!(count_speech_bursts(&wav, SAMPLE_RATE), 2);
        let b = babble_suspect(&wav, "tʃˈaːw2.", 13, 6);
        assert!(b.suspect);
        assert!(!babble_suspect(&wav, "tʃˈaːw2 bˈaːn5.", 18, 8).suspect);

        let w = Matrix {
            rows: 2,
            cols: 2,
            data: vec![1.0, 0.0, 0.0, 1.0],
        };
        let a =
            speaker_anchor(&[1.0, 3.0], &w, &[0.0, 0.0], &[1.0, 1.0], &[0.0, 0.0], 0.0).unwrap();
        assert!((a[0] + 1.0).abs() < 1e-5 && (a[1] - 1.0).abs() < 1e-5);
        assert!(speaker_anchor(&[0.0, 0.0], &w, &[0.0; 2], &[1.0; 2], &[0.0; 2], 0.0).is_err());
    }
}
