//! Text-to-speech for Marginalia.
//!
//! * [`models`] is the catalog of open voice models (Piper, Kokoro,
//!   Supertonic) and downloads them on demand into the data directory.
//! * [`engine`] runs them in-process with sherpa-onnx (ONNX Runtime and
//!   espeak-ng linked into the app, no helper programs); VieNeu voices go
//!   through `vieneu`, a Rust port of VieNeu-TTS's ONNX pipeline.
//! * [`espeak`] is a zero-download fallback using a system eSpeak NG.
//! * [`audio`] writes WAV files and (with the `playback` feature) plays audio.

pub mod audio;
pub mod download;
pub mod engine;
pub mod espeak;
pub mod models;
pub mod paths;
mod vieneu;

pub use audio::Audio;

/// A speech engine that turns one chunk of text (a sentence) into audio.
pub trait Synthesizer: Send {
    fn synthesize(&mut self, text: &str) -> anyhow::Result<Audio>;
}
