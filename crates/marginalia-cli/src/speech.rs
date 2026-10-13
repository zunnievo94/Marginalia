//! The `speak`/`say` pipeline: chunk text into sentences, synthesize on a
//! worker thread, and play (or record) while the next sentence is prepared.

use std::sync::mpsc;
use std::thread;

use anyhow::{bail, Context, Result};
use marginalia_core::text::speech_chunks;
use marginalia_tts::engine::{Engine, Options};
use marginalia_tts::espeak::EspeakEngine;
use marginalia_tts::models;
use marginalia_tts::{Audio, Synthesizer};

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
        writer: Option<marginalia_tts::audio::WavWriter>,
    },
    #[cfg(feature = "playback")]
    Speaker(marginalia_tts::audio::Player),
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
        return Ok(Output::Speaker(marginalia_tts::audio::Player::new()?));
        #[cfg(not(feature = "playback"))]
        bail!("this build has no audio playback; use --out FILE.wav");
    }

    fn push(&mut self, audio: &Audio) -> Result<()> {
        match self {
            Output::Wav { path, writer } => {
                if writer.is_none() {
                    *writer = Some(marginalia_tts::audio::WavWriter::create(
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
        EngineKind::Neural => Ok(Box::new(neural_engine(lang, args)?)),
        EngineKind::Auto => match neural_engine(lang, args) {
            Ok(engine) => Ok(Box::new(engine)),
            Err(neural_err) => match espeak_engine(lang, args) {
                Ok(engine) => {
                    eprintln!("No neural voice ({neural_err:#}); using eSpeak NG.");
                    Ok(Box::new(engine))
                }
                Err(_) => Err(neural_err),
            },
        },
    }
}

fn espeak_engine(lang: &str, args: &TtsArgs) -> Result<EspeakEngine> {
    // A model id is not an eSpeak voice.
    let voice = match &args.voice {
        Some(v) if args.engine == EngineKind::Espeak => v.clone(),
        _ => marginalia_tts::espeak::voice_for_language(lang),
    };
    EspeakEngine::new(&voice, args.speed)
}

fn neural_engine(lang: &str, args: &TtsArgs) -> Result<Engine> {
    let pack = resolve_model(lang, args)?;
    let speaker = match &args.speaker {
        None => None,
        Some(s) => Some(match s.parse::<i32>() {
            Ok(i) => i,
            Err(_) => pack
                .speakers
                .iter()
                .position(|n| n.eq_ignore_ascii_case(s))
                .with_context(|| format!("{} has no speaker {s:?}", pack.id))?
                as i32,
        }),
    };
    let opts = Options {
        speaker,
        speed: args.speed,
        lang: lang.to_string(),
    };
    let engine = Engine::new(pack, &opts)?;
    eprintln!("Voice: {}", engine.label);
    Ok(engine)
}

/// The model to use: `--voice`, else the best installed one for the
/// language, else the recommended one (downloaded after asking).
fn resolve_model(lang: &str, args: &TtsArgs) -> Result<&'static models::Pack> {
    let pack = match &args.voice {
        Some(id) => models::find(id)
            .with_context(|| format!("unknown voice model {id:?}; see `marginalia voices list`"))?,
        None => {
            if let Some(p) = models::best_installed(lang) {
                return Ok(p);
            }
            models::recommended(lang).with_context(|| format!("no voice model speaks {lang:?}"))?
        }
    };
    if !pack.is_installed() {
        let mb = pack.size as f64 / 1e6;
        if !confirm(
            &format!(
                "Voice model {} ({mb:.0} MB) is not downloaded. Download it now?",
                pack.id
            ),
            args.yes,
        )? {
            bail!("voice model {} is not downloaded", pack.id);
        }
        pack.download(&mut progress_bar(&pack.id))?;
        eprintln!();
    }
    Ok(pack)
}
