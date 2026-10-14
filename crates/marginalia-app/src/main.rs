//! Marginalia desktop app (Tauri). The UI lives in `../ui` as plain
//! HTML/CSS/JS; this file exposes the reader and TTS to it as commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bookmarks;
mod notes;
mod replacements;
mod speech;
mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use marginalia_core::text::sentences_or_whole;
use marginalia_core::{Block, Book};
use marginalia_tts::models;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use store::{Entry, Library, Position, Settings};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Default)]
struct AppState {
    library: Mutex<Library>,
    settings: Mutex<Settings>,
    /// The open book, parsed once and shared with the speech thread.
    open: Mutex<Option<(String, Arc<Book>)>>,
    speech: speech::Speech,
}

impl AppState {
    fn book(&self, id: &str) -> CmdResult<Arc<Book>> {
        let mut open = self.open.lock().unwrap();
        if let Some((open_id, book)) = open.as_ref() {
            if open_id == id {
                return Ok(book.clone());
            }
        }
        let path = self
            .library
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.path.clone())
            .ok_or("This book is no longer in the library.")?;
        let mut book = marginalia_core::open(&path).map_err(err)?;
        replacements::replacer(id).apply_to_book(&mut book);
        let book = Arc::new(book);
        *open = Some((id.to_string(), book.clone()));
        Ok(book)
    }
}

// ---------------------------------------------------------------- library

#[derive(Serialize)]
struct AddResult {
    added: Vec<Entry>,
    errors: Vec<String>,
}

#[tauri::command]
fn library_list(state: State<AppState>) -> Vec<Entry> {
    state.library.lock().unwrap().sorted()
}

#[tauri::command]
fn library_add(state: State<AppState>, paths: Vec<PathBuf>) -> CmdResult<AddResult> {
    let mut added = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let id = Library::id_for(&path);
        if let Some(existing) = state.library.lock().unwrap().get(&id) {
            added.push(existing.clone());
            continue;
        }
        match marginalia_core::open(&path) {
            Ok(book) => {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let title = if book.metadata.title.trim().is_empty() {
                    name
                } else {
                    book.metadata.title.clone()
                };
                let entry = Entry {
                    id,
                    path: path.clone(),
                    title,
                    authors: book.metadata.authors.clone(),
                    language: book.metadata.language.clone(),
                    format: format!("{:?}", book.format).to_uppercase(),
                    chapters: book.chapters.len(),
                    added: store::now(),
                    last_opened: None,
                    position: Position::default(),
                    progress: 0.0,
                    has_cover: false,
                };
                let mut entry = entry;
                entry.has_cover = save_cover(&entry.id, &book);
                state.library.lock().unwrap().books.push(entry.clone());
                added.push(entry);
            }
            Err(e) => errors.push(format!(
                "{}: {e}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    state.library.lock().unwrap().save().map_err(err)?;
    Ok(AddResult { added, errors })
}

#[tauri::command]
async fn library_pick(app: AppHandle, state: State<'_, AppState>) -> CmdResult<AddResult> {
    let picked = app
        .dialog()
        .file()
        .set_title("Open books")
        .add_filter("Ebooks", &["epub", "mobi", "prc", "azw", "azw3", "pdb"])
        .blocking_pick_files()
        .unwrap_or_default();
    let paths = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    library_add(state, paths)
}

#[tauri::command]
fn library_remove(state: State<AppState>, id: String) -> CmdResult<()> {
    let mut lib = state.library.lock().unwrap();
    lib.books.retain(|b| b.id != id);
    if let Ok(dir) = covers_dir() {
        let _ = std::fs::remove_file(dir.join(&id));
    }
    lib.save().map_err(err)
}

// ------------------------------------------------------------------ books

#[derive(Serialize)]
struct TocItem {
    title: String,
    words: usize,
    /// Text paragraphs, as counted by reading positions.
    paragraphs: usize,
    images: bool,
}

#[derive(Serialize)]
struct BookView {
    entry: Entry,
    toc: Vec<TocItem>,
}

#[tauri::command]
fn book_open(state: State<AppState>, id: String) -> CmdResult<BookView> {
    let book = state.book(&id)?;
    let mut lib = state.library.lock().unwrap();
    let entry = lib.get_mut(&id).ok_or("unknown book")?;
    entry.last_opened = Some(store::now());
    entry.chapters = book.chapters.len();
    entry.has_cover = save_cover(&id, &book);
    let entry = entry.clone();
    lib.save().map_err(err)?;
    let toc = book
        .chapters
        .iter()
        .map(|c| TocItem {
            title: c.title.clone(),
            words: marginalia_core::text::word_count(&c.text),
            paragraphs: c
                .blocks
                .iter()
                .filter(|b| matches!(b, Block::Text(_)))
                .count(),
            images: c.has_images(),
        })
        .collect();
    Ok(BookView { entry, toc })
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum BlockView {
    /// A paragraph split into sentences, the units read aloud.
    Text { sentences: Vec<String> },
    /// An image, served at `mgimg://localhost/<book id>/<image>`.
    Image { image: usize, alt: String },
}

#[tauri::command]
fn chapter_get(state: State<AppState>, id: String, index: usize) -> CmdResult<Vec<BlockView>> {
    let book = state.book(&id)?;
    let chapter = book.chapters.get(index).ok_or("chapter out of range")?;
    Ok(chapter
        .blocks
        .iter()
        .map(|b| match b {
            Block::Text(t) => BlockView::Text {
                sentences: sentences_or_whole(t),
            },
            Block::Image { image, alt } => BlockView::Image {
                image: *image,
                alt: alt.clone(),
            },
        })
        .collect())
}

fn covers_dir() -> anyhow::Result<PathBuf> {
    Ok(marginalia_tts::paths::data_dir()?.join("covers"))
}

/// Keep a copy of the cover so the library can show it without parsing
/// every book.
fn save_cover(id: &str, book: &Book) -> bool {
    let Some(cover) = book.cover_image() else {
        return false;
    };
    let Ok(dir) = covers_dir() else { return false };
    let path = dir.join(id);
    path.exists()
        || (std::fs::create_dir_all(&dir).is_ok() && std::fs::write(path, &cover.data).is_ok())
}

/// Serves book images and library covers to the webview:
/// `mgimg://localhost/<book id>/<image index>` and `mgimg://localhost/cover/<book id>`.
fn serve_image(app: &AppHandle, path: &str) -> Option<(String, Vec<u8>)> {
    let mut parts = path.trim_start_matches('/').split('/');
    let (first, second) = (parts.next()?, parts.next()?);
    if first == "cover" {
        let data = std::fs::read(covers_dir().ok()?.join(second)).ok()?;
        let mime = marginalia_core::sniff_image(&data).unwrap_or("application/octet-stream");
        return Some((mime.to_string(), data));
    }
    let state = app.state::<AppState>();
    let book = state.book(first).ok()?;
    let image = book.images.get(second.parse::<usize>().ok()?)?;
    Some((image.media_type.clone(), image.data.clone()))
}

#[tauri::command]
fn position_save(
    state: State<AppState>,
    id: String,
    position: Position,
    progress: f32,
) -> CmdResult<()> {
    let mut lib = state.library.lock().unwrap();
    if let Some(e) = lib.get_mut(&id) {
        if e.position != position {
            e.position = position;
            e.progress = progress.clamp(0.0, 1.0);
            lib.save().map_err(err)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ notes

#[tauri::command]
fn notes_list(id: String) -> Vec<notes::Note> {
    notes::load(&id)
}

#[tauri::command]
fn note_save(id: String, note: notes::Note) -> CmdResult<notes::Note> {
    notes::save(&id, note).map_err(err)
}

#[tauri::command]
fn note_delete(id: String, note_id: String) -> CmdResult<()> {
    notes::delete(&id, &note_id).map_err(err)
}

/// Save the book's notes as a Markdown file chosen by the user.
#[tauri::command]
async fn notes_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Option<String>> {
    let book = state.book(&id)?;
    let titles: Vec<String> = book.chapters.iter().map(|c| c.title.clone()).collect();
    let title = state
        .library
        .lock()
        .unwrap()
        .get(&id)
        .map(|e| e.title.clone())
        .unwrap_or_else(|| book.metadata.title.clone());
    let md = notes::to_markdown(&title, &book.metadata.authors, &titles, &notes::load(&id));
    let file_name: String = format!("{title} - notes.md")
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export notes")
        .set_file_name(&file_name)
        .add_filter("Markdown", &["md"])
        .blocking_save_file()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    std::fs::write(&path, md).map_err(err)?;
    Ok(Some(path.display().to_string()))
}

// --------------------------------------------------------- search, bookmarks

#[derive(Serialize)]
struct SearchHit {
    chapter: usize,
    paragraph: usize,
    start: usize,
    end: usize,
    before: String,
    matched: String,
    after: String,
}

#[derive(Serialize)]
struct SearchResult {
    hits: Vec<SearchHit>,
    /// More matches exist than were returned.
    truncated: bool,
}

/// Case- and accent-insensitive search through the whole book.
#[tauri::command]
async fn book_search(
    state: State<'_, AppState>,
    id: String,
    query: String,
) -> CmdResult<SearchResult> {
    const LIMIT: usize = 500;
    let book = state.book(&id)?;
    let hits = tauri::async_runtime::spawn_blocking(move || {
        marginalia_core::search::search(&book, &query, LIMIT + 1)
    })
    .await
    .map_err(err)?;
    let truncated = hits.len() > LIMIT;
    Ok(SearchResult {
        hits: hits
            .into_iter()
            .take(LIMIT)
            .map(|h| SearchHit {
                chapter: h.chapter,
                paragraph: h.paragraph,
                start: h.start,
                end: h.end,
                before: h.before,
                matched: h.matched,
                after: h.after,
            })
            .collect(),
        truncated,
    })
}

#[tauri::command]
fn bookmarks_list(id: String) -> Vec<bookmarks::Bookmark> {
    bookmarks::load(&id)
}

#[tauri::command]
fn bookmark_add(id: String, position: Position, label: String) -> CmdResult<bookmarks::Bookmark> {
    bookmarks::add(&id, position, label).map_err(err)
}

#[tauri::command]
fn replacements_get(id: String) -> replacements::Rules {
    replacements::load(&id)
}

/// Save the rules and reload the book with them on its next use.
#[tauri::command]
fn replacements_set(
    state: State<AppState>,
    id: String,
    rules: replacements::Rules,
) -> CmdResult<()> {
    replacements::save(&id, &rules).map_err(err)?;
    *state.open.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
fn bookmark_remove(id: String, bookmark_id: String) -> CmdResult<()> {
    bookmarks::remove(&id, &bookmark_id).map_err(err)
}

// --------------------------------------------------------------- settings

#[tauri::command]
fn settings_get(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn settings_set(state: State<AppState>, settings: Settings) -> CmdResult<()> {
    settings.save().map_err(err)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

// ------------------------------------------------------------------- TTS

#[tauri::command]
fn tts_play(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    position: Position,
) -> CmdResult<()> {
    let book = state.book(&id)?;
    let settings = state.settings.lock().unwrap().clone();
    state.speech.play(app, id, book, position, settings);
    Ok(())
}

#[tauri::command]
fn tts_pause(app: AppHandle, state: State<AppState>) {
    state.speech.pause(&app);
}

#[tauri::command]
fn tts_resume(app: AppHandle, state: State<AppState>) {
    state.speech.resume(&app);
}

#[tauri::command]
fn tts_stop(app: AppHandle, state: State<AppState>) {
    state.speech.stop();
    let _ = app.emit(
        "tts-state",
        speech::StateEvent {
            state: "stopped",
            error: None,
            voice: None,
        },
    );
}

#[tauri::command]
fn tts_preview(
    app: AppHandle,
    state: State<AppState>,
    text: String,
    lang: String,
    model: String,
    speaker: Option<i32>,
) {
    let speed = state.settings.lock().unwrap().speed;
    state.speech.preview(app, text, lang, model, speaker, speed);
}

#[derive(Serialize)]
struct EngineStatus {
    espeak: Option<String>,
    installed: Vec<String>,
    data_dir: String,
}

#[tauri::command]
fn engine_status() -> CmdResult<EngineStatus> {
    Ok(EngineStatus {
        espeak: marginalia_tts::espeak::find_executable().map(|p| p.display().to_string()),
        installed: models::installed()
            .into_iter()
            .map(|p| p.id.clone())
            .collect(),
        data_dir: marginalia_tts::paths::data_dir()
            .map_err(err)?
            .display()
            .to_string(),
    })
}

/// The voice that would read a book in `lang` right now, if any.
#[derive(Serialize)]
struct CurrentVoice {
    model: String,
    speaker: i32,
}

#[tauri::command]
fn voice_current(state: State<AppState>, lang: String) -> Option<CurrentVoice> {
    let settings = state.settings.lock().unwrap();
    speech::voice_for(&lang, &settings).map(|(pack, speaker)| CurrentVoice {
        model: pack.id.clone(),
        speaker: speaker.unwrap_or_else(|| pack.default_speaker(&lang)),
    })
}

#[derive(Clone, Serialize)]
struct Progress {
    key: String,
    done: u64,
    total: Option<u64>,
}

fn progress_emitter(app: &AppHandle, key: &str) -> impl FnMut(u64, Option<u64>) {
    let app = app.clone();
    let key = key.to_string();
    let mut last = 0u64;
    move |done, total| {
        // Throttle to ~every 512 KB to keep the IPC channel quiet.
        if done.saturating_sub(last) >= 512 * 1024 || Some(done) == total {
            last = done;
            let _ = app.emit(
                "download-progress",
                Progress {
                    key: key.clone(),
                    done,
                    total,
                },
            );
        }
    }
}

#[derive(Serialize)]
struct ModelInfo {
    #[serde(flatten)]
    pack: &'static models::Pack,
    installed: bool,
    /// Best model for each language comes first in `models_for`.
    recommended: bool,
}

/// Voice models, all or those speaking `lang` (best first).
#[tauri::command]
fn models_list(lang: Option<String>) -> Vec<ModelInfo> {
    let recommended = lang.as_deref().and_then(models::recommended).map(|p| &p.id);
    let packs: Vec<&'static models::Pack> = match &lang {
        Some(l) => models::for_language(l),
        None => models::catalog().iter().collect(),
    };
    packs
        .into_iter()
        .map(|pack| ModelInfo {
            installed: pack.is_installed(),
            recommended: Some(&pack.id) == recommended,
            pack,
        })
        .collect()
}

#[tauri::command]
async fn model_download(app: AppHandle, id: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        let pack = models::find(&id).ok_or_else(|| anyhow::anyhow!("unknown voice model {id}"))?;
        pack.download(&mut progress_emitter(&app, &id))
    })
    .await
    .map_err(err)?
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn model_remove(state: State<AppState>, id: String) -> CmdResult<()> {
    state.speech.stop();
    match models::find(&id) {
        Some(p) => p.remove().map_err(err),
        None => Ok(()),
    }
}

fn main() {
    let state = AppState {
        library: Mutex::new(Library::load()),
        settings: Mutex::new(Settings::load()),
        ..AppState::default()
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("mgimg", |ctx, request| {
            let builder = tauri::http::Response::builder();
            match serve_image(ctx.app_handle(), request.uri().path()) {
                Some((mime, data)) => builder
                    .header("Content-Type", mime)
                    .header("Cache-Control", "max-age=3600")
                    .body(data),
                None => builder.status(404).body(Vec::new()),
            }
            .expect("valid response")
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            library_list,
            library_add,
            library_pick,
            library_remove,
            book_open,
            chapter_get,
            position_save,
            book_search,
            bookmarks_list,
            bookmark_add,
            bookmark_remove,
            replacements_get,
            replacements_set,
            notes_list,
            note_save,
            note_delete,
            notes_export,
            settings_get,
            settings_set,
            tts_play,
            tts_pause,
            tts_resume,
            tts_stop,
            tts_preview,
            engine_status,
            voice_current,
            models_list,
            model_download,
            model_remove,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Marginalia");
}
