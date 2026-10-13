//! Read-aloud sessions. A session runs on its own thread (the audio output
//! stream must stay on the thread that opened it), synthesizes sentence by
//! sentence, and emits `tts-position` exactly when each sentence starts
//! playing so the UI can highlight it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use zreader_core::text::{paragraph_sentences, speech_chunks};
use zreader_core::Book;
use zreader_tts::audio::{Player, PlayerHandle};
use zreader_tts::catalog::{self, InstalledVoice};
use zreader_tts::espeak::EspeakEngine;
use zreader_tts::piper::{self, PiperEngine, PiperOptions};
use zreader_tts::{Audio, Synthesizer};

use crate::store::{Position, Settings};

/// Audio chunks (plus markers) kept queued ahead of the speaker.
const QUEUE_AHEAD: usize = 8;
const CHAPTER_PAUSE_MS: u32 = 700;

#[derive(Clone, Serialize)]
pub struct PositionEvent {
    pub book_id: String,
    #[serde(flatten)]
    pub position: Position,
}

#[derive(Clone, Serialize)]
pub struct StateEvent {
    /// `loading`, `playing`, `paused` or `stopped`.
    pub state: &'static str,
    pub error: Option<String>,
    /// Engine/voice being used, for the status bar.
    pub voice: Option<String>,
}

struct Session {
    cancel: Arc<AtomicBool>,
    player: Arc<Mutex<Option<PlayerHandle>>>,
}

#[derive(Default)]
pub struct Speech {
    current: Mutex<Option<Session>>,
}

impl Speech {
    pub fn play(
        &self,
        app: AppHandle,
        book_id: String,
        book: Arc<Book>,
        start: Position,
        settings: Settings,
    ) {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        let player = Arc::new(Mutex::new(None));
        *self.current.lock().unwrap() = Some(Session {
            cancel: cancel.clone(),
            player: player.clone(),
        });
        thread::spawn(move || {
            let result = run(&app, &book_id, &book, start, &settings, &cancel, &player);
            // A newer session may already be running; only report for ours.
            if !cancel.load(Ordering::SeqCst) || result.is_err() {
                emit_state(
                    &app,
                    "stopped",
                    result.err().map(|e| format!("{e:#}")),
                    None,
                );
            }
        });
    }

    pub fn pause(&self, app: &AppHandle) {
        if let Some(h) = self.handle() {
            h.pause();
            emit_state(app, "paused", None, None);
        }
    }

    pub fn resume(&self, app: &AppHandle) {
        if let Some(h) = self.handle() {
            h.resume();
            emit_state(app, "playing", None, None);
        }
    }

    pub fn stop(&self) {
        if let Some(s) = self.current.lock().unwrap().take() {
            s.cancel.store(true, Ordering::SeqCst);
            if let Some(h) = s.player.lock().unwrap().as_ref() {
                h.stop();
            }
        }
    }

    fn handle(&self) -> Option<PlayerHandle> {
        let current = self.current.lock().unwrap();
        let session = current.as_ref()?;
        let handle = session.player.lock().unwrap().clone();
        handle
    }
}

fn emit_state(app: &AppHandle, state: &'static str, error: Option<String>, voice: Option<String>) {
    let _ = app.emit(
        "tts-state",
        StateEvent {
            state,
            error,
            voice,
        },
    );
}

fn run(
    app: &AppHandle,
    book_id: &str,
    book: &Book,
    start: Position,
    settings: &Settings,
    cancel: &AtomicBool,
    handle: &Mutex<Option<PlayerHandle>>,
) -> Result<()> {
    emit_state(app, "loading", None, None);
    let lang = book
        .metadata
        .language
        .clone()
        .unwrap_or_else(|| "en".into());
    let (mut engine, voice_name) = build_engine(&lang, settings)?;
    let player = Player::new()?;
    *handle.lock().unwrap() = Some(player.handle());
    let cancelled = || cancel.load(Ordering::SeqCst);
    let mut announced = false;

    for (ci, chapter) in book.chapters.iter().enumerate().skip(start.chapter) {
        let mut rate = None;
        for (pi, sentences) in paragraph_sentences(&chapter.text).iter().enumerate() {
            if ci == start.chapter && pi < start.paragraph {
                continue;
            }
            for (si, sentence) in sentences.iter().enumerate() {
                if ci == start.chapter && pi == start.paragraph && si < start.sentence {
                    continue;
                }
                let mut first = true;
                for chunk in speech_chunks(sentence, 400) {
                    if !chunk.chars().any(char::is_alphanumeric) {
                        continue;
                    }
                    if cancelled() {
                        return Ok(());
                    }
                    let audio = engine.synthesize(&chunk)?;
                    rate = Some(audio.sample_rate);
                    while player.queued() > QUEUE_AHEAD && !cancelled() {
                        thread::sleep(Duration::from_millis(30));
                    }
                    if cancelled() {
                        return Ok(());
                    }
                    if first {
                        first = false;
                        let app = app.clone();
                        let event = PositionEvent {
                            book_id: book_id.to_string(),
                            position: Position {
                                chapter: ci,
                                paragraph: pi,
                                sentence: si,
                            },
                        };
                        player.enqueue_marker(move || {
                            let _ = app.emit("tts-position", event.clone());
                        });
                    }
                    player.enqueue(&audio);
                    if !announced {
                        announced = true;
                        emit_state(app, "playing", None, Some(voice_name.clone()));
                    }
                }
            }
        }
        if let Some(rate) = rate {
            player.enqueue(&Audio::silence(rate, CHAPTER_PAUSE_MS));
        }
    }

    while player.queued() > 0 && !cancelled() {
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Piper with the chosen (or a language-matching) installed voice, else
/// eSpeak NG, else an error the UI turns into "install a voice".
fn build_engine(lang: &str, settings: &Settings) -> Result<(Box<dyn Synthesizer>, String)> {
    if let Some(exe) = piper::find_executable() {
        let chosen = match &settings.voice {
            Some(key) => InstalledVoice::find(key)?,
            None => None,
        };
        // Never fall back to a voice of another language: an English voice
        // reading Vietnamese is unintelligible. The UI offers a download.
        let voice = match chosen {
            Some(v) => Some(v),
            None => catalog::installed_for_language(lang)?,
        };
        if let Some(voice) = voice {
            let opts = PiperOptions {
                speed: settings.speed,
                speaker: None,
            };
            let engine = PiperEngine::new(&exe, &voice, &opts)?;
            return Ok((Box::new(engine), voice.key));
        }
    }
    if zreader_tts::espeak::find_executable().is_some() {
        let voice = zreader_tts::espeak::voice_for_language(lang);
        let engine = EspeakEngine::new(&voice, settings.speed)?;
        return Ok((Box::new(engine), format!("eSpeak NG ({voice})")));
    }
    bail!("NO_VOICE")
}
