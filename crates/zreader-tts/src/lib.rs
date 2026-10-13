//! Text-to-speech for ZReader.
//!
//! * [`catalog`] lists the open Piper neural voices and downloads them on
//!   demand into the ZReader data directory (with checksum verification).
//! * [`piper`] downloads the open-source Piper runtime and drives it.
//! * [`espeak`] is a zero-download fallback using a system eSpeak NG.
//! * [`audio`] writes WAV files and (with the `playback` feature) plays audio.

pub mod audio;
pub mod catalog;
pub mod download;
pub mod espeak;
pub mod paths;
pub mod piper;

pub use audio::Audio;

/// A speech engine that turns one chunk of text (a sentence) into audio.
pub trait Synthesizer: Send {
    fn synthesize(&mut self, text: &str) -> anyhow::Result<Audio>;
}
