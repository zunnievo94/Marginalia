//! VieNeu-TTS voices (<https://github.com/pnnbao97/VieNeu-TTS>), run without
//! Python.
//!
//! VieNeu is a Vietnamese text-to-speech family by Phạm Nguyễn Ngọc Bảo
//! (Apache-2.0). Its CPU path is already torch-free: ONNX graphs plus some
//! NumPy glue. This module is a Rust port of that glue:
//!
//! * text is normalized and turned into phonemes by [sea-g2p], whose core is
//!   a Rust crate;
//! * the ONNX graphs run in the ONNX Runtime that sherpa-onnx links into the
//!   app (see [`env`]);
//! * [`turbo`] is v3 Turbo (an autoregressive speech model plus the MOSS
//!   audio codec, 48 kHz), [`nano`] is v3 Nano (a small flow-matching model,
//!   24 kHz).
//!
//! The ports follow `src/vieneu/_v3_turbo_engine/onnx_runtime_lite.py` and
//! `src/vieneu/v3nano.py` of VieNeu-TTS; comments name the Python function a
//! piece mirrors.
//!
//! [sea-g2p]: https://github.com/pnnbao97/sea-g2p

pub(crate) mod nano;
mod npz;
pub(crate) mod turbo;

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use ort::environment::Environment;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;

/// The Hugging Face repositories the graphs come from.
pub(crate) const HF: &str = "https://huggingface.co";
/// VieNeu-TTS source at the commit the ports follow; preset voices come
/// from its `assets/`.
pub(crate) const VIENEU_RAW: &str =
    "https://raw.githubusercontent.com/pnnbao97/VieNeu-TTS/bd0f88eb8de7d560753a6172ba9c868f309a101c";
/// The sea-g2p dictionary, at the commit of the `sea-g2p-rs` dependency.
pub(crate) const SEA_G2P_DICT_URL: &str =
    "https://raw.githubusercontent.com/pnnbao97/sea-g2p/e825173f235d08ea19315b2b279fb11153b44cea/python/sea_g2p/sea_g2p.bin";
pub(crate) const SEA_G2P_DICT: &str = "sea_g2p.bin";

extern "C" {
    /// ONNX Runtime's entry point, from the static library sherpa-onnx links.
    fn OrtGetApiBase() -> *const ort::sys::OrtApiBase;
}

/// The process-wide ONNX Runtime environment for VieNeu sessions.
///
/// `ort` is built with `alternative-backend`, so it links nothing itself
/// and is pointed here at the ONNX Runtime that sherpa-onnx already
/// carries: one copy of the runtime in the binary, no extra library to ship.
pub(crate) fn env() -> Result<&'static Environment> {
    static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        unsafe {
            let base = OrtGetApiBase();
            if base.is_null() {
                return Err("ONNX Runtime is not available".into());
            }
            let api = ((*base).GetApi)(ort::sys::ORT_API_VERSION);
            if api.is_null() {
                let version = std::ffi::CStr::from_ptr(((*base).GetVersionString)());
                return Err(format!(
                    "ONNX Runtime {} is too old for VieNeu voices",
                    version.to_string_lossy()
                ));
            }
            // A copy of the runtime's own (static, never freed) function table.
            ort::set_api(std::ptr::read(api));
        }
        ort::init()
            .with_name("marginalia")
            .with_telemetry(false)
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| anyhow!("{e}"))
}

fn threads() -> usize {
    // VieNeu: about half the logical cores, at most 8; the sentence being
    // played needs a little CPU too.
    std::thread::available_parallelism()
        .map_or(4, |n| n.get() / 2)
        .clamp(1, 8)
}

/// Load an ONNX graph with VieNeu's session options (`_so` in the Python).
pub(crate) fn session(path: &Path) -> Result<Session> {
    let load = || -> ort::Result<Session> {
        Session::builder(env().map_err(|e| ort::Error::new(e.to_string()))?)?
            .with_optimization_level(GraphOptimizationLevel::All)?
            .with_inter_threads(1)?
            .with_intra_threads(threads())?
            .with_intra_op_spinning(false)?
            .commit_from_file(path)
    };
    load().map_err(|e| anyhow!("{e}")).with_context(|| {
        format!(
            "could not load {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })
}

/// Run a session and return its outputs in graph order.
pub(crate) fn run(
    session: &mut Session,
    inputs: Vec<(
        std::borrow::Cow<'_, str>,
        ort::session::SessionInputValue<'_>,
    )>,
) -> Result<Vec<ort::value::DynValue>> {
    let outputs = session.run(inputs).map_err(|e| anyhow!("{e}"))?;
    Ok(outputs.into_iter().map(|(_, v)| v).collect())
}

pub(crate) fn tensor<T>(shape: &[usize], data: Vec<T>) -> Result<ort::value::Tensor<T>>
where
    T: ort::value::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static,
{
    ort::value::Tensor::from_array((shape.to_vec(), data)).map_err(|e| anyhow!("{e}"))
}

/// The data of an `f32` output.
pub(crate) fn floats(v: &ort::value::DynValue) -> Result<(Vec<usize>, &[f32])> {
    let (shape, data) = v.try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
    Ok((shape.iter().map(|&d| d as usize).collect(), data))
}

/// Text to phonemes with sea-g2p, the way VieNeu calls it.
pub(crate) struct Phonemizer {
    normalizer: sea_g2p_rs::lang::vi::Normalizer,
    g2p: sea_g2p_rs::g2p::G2PEngine,
}

impl Phonemizer {
    pub(crate) fn new(dict: &Path) -> Result<Phonemizer> {
        let path = dict.to_str().context("dictionary path is not UTF-8")?;
        Ok(Phonemizer {
            normalizer: sea_g2p_rs::lang::vi::Normalizer::new("vi", Some(path)),
            g2p: sea_g2p_rs::g2p::G2PEngine::new(path)
                .with_context(|| format!("could not open {}", dict.display()))?,
        })
    }

    /// Normalize without the trailing-punctuation rule: the length a
    /// chunker should measure ("3,5 triệu" is longer once spelled out).
    pub(crate) fn normalize(&self, text: &str) -> String {
        self.normalizer.normalize(text, false)
    }

    /// Phonemes for one chunk: `SEAPipeline("vi").run(text, punc_norm=True)`,
    /// i.e. `phonemize_text` in `vieneu_utils/phonemize_text.py`.
    pub(crate) fn phonemize(&self, text: &str) -> String {
        let normalized = self.normalizer.normalize(text, true);
        if normalized.trim().is_empty() {
            return String::new();
        }
        self.g2p.phonemize(&normalized)
    }
}

/// Split one (normalized) sentence into chunks of at most `max_chars`
/// characters, at clause punctuation when possible, else between words.
///
/// VieNeu packs whole sentences into chunks; Marginalia already speaks one
/// sentence at a time, so only an unusually long sentence needs cutting.
/// Chunks shorter than `min_chars` are merged into a neighbour: on its own a
/// one- or two-word chunk makes the speech model invent words
/// (`_merge_short_chunks`).
pub(crate) fn chunk_sentence(text: &str, max_chars: usize, min_chars: usize) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chunks = Vec::new();
    let mut rest = text.as_str();
    while rest.chars().count() > max_chars {
        let limit = rest
            .char_indices()
            .nth(max_chars)
            .map_or(rest.len(), |(i, _)| i);
        let head = &rest[..limit];
        let cut = head
            .rfind([',', ';', ':'])
            .map(|i| i + 1)
            .filter(|&i| head[..i].chars().count() >= min_chars)
            .or_else(|| head.rfind(' '))
            .unwrap_or(limit);
        chunks.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.trim().is_empty() {
        chunks.push(rest.trim().to_string());
    }
    chunks.retain(|c| !c.is_empty());
    // Merge short chunks, preferring the shorter neighbour.
    while chunks.len() > 1 {
        let Some(i) = (0..chunks.len())
            .filter(|&i| chunks[i].chars().count() < min_chars)
            .min_by_key(|&i| chunks[i].chars().count())
        else {
            break;
        };
        let right = chunks.get(i + 1).map(|c| c.chars().count());
        let left = i.checked_sub(1).map(|j| chunks[j].chars().count());
        let into_right = match (left, right) {
            (Some(l), Some(r)) => r <= l,
            (None, _) => true,
            (_, None) => false,
        };
        if into_right {
            let next = chunks.remove(i + 1);
            chunks[i] = format!("{} {next}", chunks[i]);
        } else {
            let this = chunks.remove(i);
            chunks[i - 1] = format!("{} {this}", chunks[i - 1]);
        }
    }
    chunks
}

/// The pause after a chunk, in seconds: VieNeu's `V3_GAP_SILENCE` for a
/// sentence end or a clause break.
pub(crate) fn pause_after(chunk: &str) -> f32 {
    match chunk.trim_end().chars().last() {
        Some('.' | '!' | '?' | '…') => 0.50,
        _ => 0.30,
    }
}

/// `(lead, tail)`: samples of silence at each end (`edge_silence`: 10 ms
/// windows of mean |x| below -45 dB).
pub(crate) fn edge_silence(wav: &[f32], sr: u32) -> (usize, usize) {
    let win = ((sr as f32 * 0.01) as usize).max(1);
    let n_win = wav.len() / win;
    if n_win == 0 {
        return (wav.len(), 0);
    }
    let thresh = 10f32.powf(-45.0 / 20.0);
    let loud = |w: usize| {
        let s = &wav[w * win..(w + 1) * win];
        s.iter().map(|x| x.abs()).sum::<f32>() / win as f32 > thresh
    };
    let Some(first) = (0..n_win).find(|&w| loud(w)) else {
        return (wav.len(), 0);
    };
    let last = (0..n_win).rev().find(|&w| loud(w)).unwrap_or(first);
    (first * win, wav.len() - (last + 1) * win)
}

/// Zeros to put between two chunks so that the silence the model left at
/// their edges plus the zeros adds up to `pause` (`pause_pad_samples`).
pub(crate) fn pause_pad(prev: &[f32], next: &[f32], sr: u32, pause: f32) -> usize {
    let (lead_prev, mut tail) = edge_silence(prev, sr);
    if lead_prev == prev.len() {
        tail = prev.len();
    }
    let (lead, _) = if next.is_empty() {
        (0, 0)
    } else {
        edge_silence(next, sr)
    };
    ((pause * sr as f32) as usize).saturating_sub(tail + lead)
}

/// Cut the silence a model left at both ends down to 40 ms and fade the
/// edges over 15 ms (`trim_and_fade`).
pub(crate) fn trim_and_fade(wav: &[f32], sr: u32) -> Vec<f32> {
    if wav.is_empty() {
        return Vec::new();
    }
    let (lead, tail) = edge_silence(wav, sr);
    let keep = (0.04 * sr as f32) as usize;
    let a = lead.saturating_sub(keep);
    let b = wav.len() - tail.saturating_sub(keep);
    let mut out = wav[a..b.max(a)].to_vec();
    let n = ((0.015 * sr as f32) as usize).min(out.len() / 2);
    if n > 1 {
        let len = out.len();
        for i in 0..n {
            let ramp = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
            out[i] *= ramp;
            out[len - 1 - i] *= ramp;
        }
    }
    out
}

/// Join chunk waveforms with VieNeu's pauses (`join_audio_chunks` with
/// `silence_ps`), and end with enough silence for a sentence break before
/// whatever is spoken next.
pub(crate) fn join(chunks: &[(Vec<f32>, f32)], sr: u32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for (i, (wav, _)) in chunks.iter().enumerate() {
        if i > 0 {
            let (prev, pause) = (&chunks[i - 1].0, chunks[i - 1].1);
            out.resize(out.len() + pause_pad(prev, wav, sr, pause), 0.0);
        }
        out.extend_from_slice(wav);
    }
    if let Some((last, pause)) = chunks.last() {
        // The next sentence is synthesized separately; leave about half of
        // the pause here (its own leading silence supplies the rest).
        out.resize(out.len() + pause_pad(last, &[], sr, pause * 0.6), 0.0);
    }
    out
}

/// A small seeded random number generator (SplitMix64): sampling needs
/// uniform and normal draws, not cryptographic quality.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new() -> Rng {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub(crate) fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub(crate) fn normal(&mut self) -> f32 {
        let u1 = 1.0 - self.uniform();
        let u2 = self.uniform();
        ((-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_long_sentences() {
        let s = "một hai ba bốn năm sáu bảy tám chín mười, một hai ba bốn năm sáu bảy tám chín mười, một hai ba";
        let c = chunk_sentence(s, 50, 20);
        assert!(c.len() >= 2, "{c:?}");
        assert!(c.iter().all(|c| c.chars().count() <= 50 + 21), "{c:?}");
        assert_eq!(c.join(" "), s);
        assert_eq!(chunk_sentence("Xin chào.", 140, 20), vec!["Xin chào."]);
        assert!(chunk_sentence("  ", 140, 20).is_empty());
    }

    #[test]
    fn pauses_and_edges() {
        let sr = 1000;
        let mut wav = vec![0.0; 100];
        wav.extend(vec![0.5; 100]);
        wav.extend(vec![0.0; 200]);
        assert_eq!(edge_silence(&wav, sr), (100, 200));
        let t = trim_and_fade(&wav, sr);
        assert_eq!(t.len(), 40 + 100 + 40);
        assert_eq!(pause_pad(&wav, &wav, sr, 0.5), 200);
        assert_eq!(pause_after("xin chào."), 0.5);
        assert_eq!(pause_after("xin chào,"), 0.3);
    }

    /// Prints the phonemes of each line of `$VIENEU_TEST_TEXTS`, with the
    /// dictionary at `$VIENEU_TEST_DICT`, for comparing against the Python
    /// `sea_g2p` package (`SEAPipeline("vi").run(line, punc_norm=True)`).
    #[test]
    #[ignore]
    fn phonemize_lines() {
        let dict = std::env::var("VIENEU_TEST_DICT").expect("VIENEU_TEST_DICT");
        let texts = std::env::var("VIENEU_TEST_TEXTS").expect("VIENEU_TEST_TEXTS");
        let p = Phonemizer::new(Path::new(&dict)).unwrap();
        for line in std::fs::read_to_string(texts).unwrap().lines() {
            println!("PHONES\t{}", p.phonemize(line));
        }
    }

    #[test]
    fn rng_is_sane() {
        let mut r = Rng(1);
        let n = 20000;
        let mean = (0..n).map(|_| r.normal() as f64).sum::<f64>() / n as f64;
        assert!(mean.abs() < 0.05);
        assert!((0..1000).all(|_| (0.0..1.0).contains(&r.uniform())));
    }
}
