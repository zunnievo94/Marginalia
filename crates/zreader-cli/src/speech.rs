//! The `speak`/`say` pipeline: chunk text into sentences, synthesize on a
//! worker thread, and play (or record) while the next sentence is prepared.

use std::sync::mpsc;
use std::thread;

use anyhow::{bail, Context, Result};
use zreader_core::text::speech_chunks;
use zreader_tts::catalog::{self, InstalledVoice};
use zreader_tts::espeak::EspeakEngine;
use zreader_tts::piper::{self, PiperEngine, PiperOptions};
use zreader_tts::{Audio, Synthesizer};

use crate::{confirm, progress_bar, EngineKind, TtsArgs};

/// Longest chunk handed to the engine in one go.
const MAX_CHUNK_CHARS: usize = 400;
/// Pause inserted between chapters.
const CHAPTER_PAUSE_MS: u32 = 800;

pub struct Part {
    pub label: String,
    pub text: String,
}

enum Event {
    Part(String),
    Chunk(String, Audio),
}

pub fn speak(parts: Vec<Part>, lang: &str, args: &TtsArgs) -> Result<()> {
    let mut engine = build_engine(lang, args)?;

    let (tx, rx) = mpsc::sync_channel::<Result<Event>>(4);
    let worker = thread::spawn(move || {
        for part in parts {
            if tx.send(Ok(Event::Part(part.label))).is_err() {
                return;
            }
            let mut rate = None;
            for chunk in speech_chunks(&part.text, MAX_CHUNK_CHARS) {
                let result = engine.synthesize(&chunk).map(|audio| {
                    rate = Some(audio.sample_rate);
                    Event::Chunk(chunk, audio)
                });
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    return;
                }
            }
            if let Some(rate) = rate {
                let pause = Audio::silence(rate, CHAPTER_PAUSE_MS);
                if tx.send(Ok(Event::Chunk(String::new(), pause))).is_err() {
                    return;
                }
            }
        }
    });

    let mut output = Output::new(args)?;
    for event in rx {
        match event? {
            Event::Part(label) if !label.is_empty() => eprintln!("\n== {label} =="),
            Event::Part(_) => {}
            Event::Chunk(text, audio) => {
                if !text.is_empty() {
                    eprintln!("{text}");
                }
                output.push(&audio)?;
            }
        }
    }
    let _ = worker.join();
    output.finish()
}

enum Output {
    Wav {
        path: std::path::PathBuf,
        writer: Option<zreader_tts::audio::WavWriter>,
    },
    #[cfg(feature = "playback")]
    Speaker(zreader_tts::audio::Player),
}

impl Output {
    fn new(args: &TtsArgs) -> Result<Output> {
        if let Some(path) = &args.out {
            return Ok(Output::Wav {
                path: path.clone(),
                writer: None,
            });
        }
        #[cfg(feature = "playback")]
        return Ok(Output::Speaker(zreader_tts::audio::Player::new()?));
        #[cfg(not(feature = "playback"))]
        bail!("this build has no audio playback; use --out FILE.wav");
    }

    fn push(&mut self, audio: &Audio) -> Result<()> {
        match self {
            Output::Wav { path, writer } => {
                if writer.is_none() {
                    *writer = Some(zreader_tts::audio::WavWriter::create(
                        path,
                        audio.sample_rate,
                    )?);
                }
                writer.as_mut().expect("created above").append(audio)
            }
            #[cfg(feature = "playback")]
            Output::Speaker(player) => {
                // Keep only a few sentences queued so memory stays flat on
                // long books while playback remains gapless.
                while player.queued() > 3 {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
                player.enqueue(audio);
                Ok(())
            }
        }
    }

    fn finish(self) -> Result<()> {
        match self {
            Output::Wav { path, writer } => {
                if let Some(w) = writer {
                    w.finish()?;
                    eprintln!("\nSaved {}", path.display());
                }
                Ok(())
            }
            #[cfg(feature = "playback")]
            Output::Speaker(player) => {
                player.wait_until_done();
                Ok(())
            }
        }
    }
}

fn build_engine(lang: &str, args: &TtsArgs) -> Result<Box<dyn Synthesizer>> {
    match args.engine {
        EngineKind::Espeak => Ok(Box::new(espeak_engine(lang, args)?)),
        EngineKind::Piper => Ok(Box::new(piper_engine(lang, args)?)),
        EngineKind::Auto => match piper_engine(lang, args) {
            Ok(engine) => Ok(Box::new(engine)),
            Err(piper_err) => match espeak_engine(lang, args) {
                Ok(engine) => {
                    eprintln!("Piper unavailable ({piper_err:#}); using eSpeak NG.");
                    Ok(Box::new(engine))
                }
                Err(_) => Err(piper_err),
            },
        },
    }
}

fn espeak_engine(lang: &str, args: &TtsArgs) -> Result<EspeakEngine> {
    // A Piper key such as en_US-lessac-medium is not an eSpeak voice.
    let voice = match &args.voice {
        Some(v) if args.engine == EngineKind::Espeak => v.clone(),
        _ => lang.replace('_', "-").to_lowercase(),
    };
    EspeakEngine::new(&voice, args.speed)
}

fn piper_engine(lang: &str, args: &TtsArgs) -> Result<PiperEngine> {
    let voice = resolve_voice(lang, args)?;
    let executable = match piper::find_executable() {
        Some(p) => p,
        None => {
            if !confirm(
                &format!(
                    "The Piper speech engine ({}) is not installed. Download it?",
                    piper::RELEASE
                ),
                args.yes,
            )? {
                bail!("Piper is not installed");
            }
            let p = piper::install(&mut progress_bar("Piper runtime"))?;
            eprintln!();
            p
        }
    };
    let opts = PiperOptions {
        speed: args.speed,
        speaker: args.speaker,
    };
    PiperEngine::new(&executable, &voice, &opts)
}

/// Pick the Piper voice: explicit `--voice`, else an installed voice for the
/// book's language, else the catalog default (downloaded after asking).
fn resolve_voice(lang: &str, args: &TtsArgs) -> Result<InstalledVoice> {
    let key = match &args.voice {
        Some(key) => key.clone(),
        None => {
            if let Some(v) = installed_for_language(lang)? {
                return Ok(v);
            }
            let voices = catalog::load(false)?;
            catalog::default_for_language(&voices, lang)
                .with_context(|| format!("no Piper voice for language {lang:?}"))?
                .key
                .clone()
        }
    };
    if let Some(v) = InstalledVoice::find(&key)? {
        return Ok(v);
    }
    let voices = catalog::load(false)?;
    let voice = catalog::find(&voices, &key)?;
    let mb = voice.download_size() as f64 / 1e6;
    if !confirm(
        &format!("Voice {key} ({mb:.0} MB) is not downloaded. Download it now?"),
        args.yes,
    )? {
        bail!("voice {key} is not downloaded");
    }
    let installed = voice.download(&mut progress_bar(&key))?;
    eprintln!();
    Ok(installed)
}

fn installed_for_language(lang: &str) -> Result<Option<InstalledVoice>> {
    let lang = lang.replace('-', "_").to_lowercase();
    let family = lang.split('_').next().unwrap_or_default().to_string();
    let installed = InstalledVoice::list()?;
    let code_of = |v: &InstalledVoice| v.key.split('-').next().unwrap_or_default().to_lowercase();
    let exact = installed.iter().find(|v| code_of(v) == lang);
    let by_family = installed
        .iter()
        .find(|v| code_of(v).split('_').next() == Some(family.as_str()));
    Ok(exact.or(by_family).cloned())
}
