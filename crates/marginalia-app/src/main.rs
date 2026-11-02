//! Marginalia desktop app (Tauri). The UI lives in `../ui` as plain
//! HTML/CSS/JS; this file exposes the reader and TTS to it as commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod bookmarks;
#[cfg(target_os = "linux")]
mod desktop;
mod notes;
mod replacements;
mod speech;
mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use marginalia_core::text::sentences_or_whole;
use marginalia_core::{Block, Book, ParaStyle};
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
    /// Books the system asked us to open (double-click, "Open with"),
    /// waiting for the window to take them.
    pending: Mutex<Vec<PathBuf>>,
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
        if ai::load_config().use_ai_text {
            ai::apply(id, &mut book);
        }
        replacements::replacer(id).apply_to_book(&mut book);
        let book = Arc::new(book);
        *open = Some((id.to_string(), book.clone()));
        Ok(book)
    }

    /// Forget the parsed book, so the next read shows changed text.
    fn reload(&self) {
        *self.open.lock().unwrap() = None;
    }

    fn book_path(&self, id: &str) -> CmdResult<PathBuf> {
        self.library
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.path.clone())
            .ok_or_else(|| "This book is no longer in the library.".into())
    }
}

// ---------------------------------------------------------------- desktop

/// Whether Marginalia is in the Linux application menu, and whether it
/// manages that entry itself (as an AppImage). `null` elsewhere.
#[tauri::command]
fn desktop_status() -> Option<serde_json::Value> {
    #[cfg(target_os = "linux")]
    return serde_json::to_value(desktop::status()).ok();
    #[cfg(not(target_os = "linux"))]
    None
}

/// Add Marginalia to the application menu, or take it out (and keep it
/// out: the AppImage no longer adds itself at start).
#[tauri::command]
fn desktop_set_menu(state: State<AppState>, on: bool) -> CmdResult<Option<serde_json::Value>> {
    #[cfg(target_os = "linux")]
    {
        desktop::set_launcher(on).map_err(err)?;
        let mut settings = state.settings.lock().unwrap();
        settings.app_menu = on;
        settings.save().map_err(err)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (state, on);
    Ok(desktop_status())
}

// --------------------------------------------------------------------- AI

#[derive(Serialize)]
struct AiSettings {
    #[serde(flatten)]
    config: ai::Config,
    /// The last characters of the saved key; the key itself stays here.
    key_hint: String,
    price: Option<(f64, f64)>,
    spending: ai::Spending,
}

#[tauri::command]
fn ai_config_get() -> AiSettings {
    let mut config = ai::load_config();
    let key = std::mem::take(&mut config.api_key);
    let n = key.chars().count();
    let key_hint = if n == 0 {
        String::new()
    } else {
        key.chars().skip(n.saturating_sub(4)).collect()
    };
    AiSettings {
        price: ai::price(&config.provider, &config.model),
        config,
        key_hint,
        spending: ai::spending(),
    }
}

/// Save the AI settings; the key changes only when `api_key` is given.
#[tauri::command]
fn ai_config_set(
    state: State<AppState>,
    mut config: ai::Config,
    api_key: Option<String>,
) -> CmdResult<AiSettings> {
    config.api_key = api_key.unwrap_or_else(|| ai::load_config().api_key);
    ai::save_config(&config).map_err(err)?;
    state.reload();
    Ok(ai_config_get())
}

/// For each chapter: whether its translation and its edit are saved.
#[tauri::command]
async fn ai_status(state: State<'_, AppState>, id: String) -> CmdResult<Vec<[bool; 2]>> {
    let path = state.book_path(&id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let book = marginalia_core::open(&path).map_err(err)?;
        Ok(ai::status(&id, &book))
    })
    .await
    .map_err(err)?
}

#[derive(Serialize)]
struct AiResult {
    usage: ai::Usage,
    new_terms: usize,
    kept: usize,
}

#[derive(Clone, Serialize)]
struct AiProgress {
    id: String,
    chapter: usize,
    step: ai::Step,
    done: usize,
    total: usize,
}

/// Translate or edit a chapter with the reader's AI service.
#[tauri::command]
async fn ai_run(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    chapter: usize,
    step: ai::Step,
) -> CmdResult<AiResult> {
    let config = ai::load_config();
    if config.api_key.trim().is_empty() {
        return Err("Add an API key in Settings → AI first.".into());
    }
    if config.model.trim().is_empty() {
        return Err("Choose a model in Settings → AI first.".into());
    }
    let path = state.book_path(&id)?;
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<AiResult> {
        let book = marginalia_core::open(&path)?;
        let mut job = ai::Job::new(&config, step, ai::load_glossary(&id));
        let replacer = replacements::replacer(&id);
        let mut progress = |done, total| {
            let _ = app2.emit(
                "ai-progress",
                AiProgress {
                    id: id.clone(),
                    chapter,
                    step,
                    done,
                    total,
                },
            );
        };
        ai::run(&id, &book, chapter, &mut job, &replacer, &mut progress)?;
        Ok(AiResult {
            usage: job.usage,
            new_terms: job.new_terms,
            kept: job.kept,
        })
    })
    .await
    .map_err(err)?
    .map_err(|e| format!("{e:#}"))?;
    app.state::<AppState>().reload();
    Ok(result)
}

#[tauri::command]
fn ai_clear(state: State<AppState>, id: String, chapter: usize, step: ai::Step) -> CmdResult<()> {
    ai::clear(&id, chapter, step).map_err(err)?;
    state.reload();
    Ok(())
}

#[tauri::command]
fn ai_glossary_get(id: String) -> Vec<ai::Term> {
    ai::load_glossary(&id)
}

#[tauri::command]
fn ai_glossary_set(id: String, terms: Vec<ai::Term>) -> CmdResult<()> {
    ai::save_glossary(&id, &terms).map_err(err)
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
                    format: match book.format {
                        marginalia_core::Format::Markdown => "MD".into(),
                        f => format!("{f:?}").to_uppercase(),
                    },
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
        .add_filter(
            "Books",
            &[
                "epub", "mobi", "prc", "azw", "azw3", "pdb", "fb2", "fbz", "zip", "txt", "md",
                "markdown", "pdf",
            ],
        )
        .blocking_pick_files()
        .unwrap_or_default();
    let paths = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    library_add(state, paths)
}

/// Files given on a command line, as the system passes the books to open.
fn file_args<I: IntoIterator<Item = String>>(args: I, cwd: &std::path::Path) -> Vec<PathBuf> {
    args.into_iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            let p = PathBuf::from(a);
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        })
        .filter(|p| p.is_file())
        .collect()
}

/// Queue books to open and tell the window, bringing it to the front.
fn open_files(app: &AppHandle, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    app.state::<AppState>()
        .pending
        .lock()
        .unwrap()
        .extend(paths);
    let _ = app.emit("open-files", ());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Add the books waiting to be opened; the window opens the first one.
#[tauri::command]
fn library_pending(state: State<AppState>) -> CmdResult<AddResult> {
    let paths = std::mem::take(&mut *state.pending.lock().unwrap());
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
    Text {
        sentences: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<StyleView>,
    },
    /// An image, served at `mgimg://localhost/<book id>/<image>`.
    Image { image: usize, alt: String },
    /// A table, shown but not read aloud.
    Table {
        rows: Vec<Vec<String>>,
        header: bool,
    },
}

/// Paragraph styling, with spans in character offsets into the text as
/// displayed (the sentences joined).
#[derive(Serialize)]
struct StyleView {
    heading: u8,
    quote: bool,
    /// (style, start, end): `b`, `i`, `u`, `s`, `sup`, `sub` or `code`.
    spans: Vec<(&'static str, usize, usize)>,
}

fn style_view(paragraph: &str, sentences: &[String], style: &ParaStyle) -> StyleView {
    let spans = if style.spans.is_empty() {
        Vec::new()
    } else {
        let display = marginalia_core::text::display_paragraph(sentences);
        let map = marginalia_core::text::display_offsets(paragraph, &display);
        let at = |i: usize| map[i.min(map.len() - 1)];
        style
            .spans
            .iter()
            .map(|s| (s.kind.name(), at(s.start), at(s.end)))
            .filter(|(_, a, b)| a < b)
            .collect()
    };
    StyleView {
        heading: style.heading,
        quote: style.quote,
        spans,
    }
}

#[tauri::command]
fn chapter_get(state: State<AppState>, id: String, index: usize) -> CmdResult<Vec<BlockView>> {
    let book = state.book(&id)?;
    let chapter = book.chapters.get(index).ok_or("chapter out of range")?;
    let mut paragraph = 0;
    Ok(chapter
        .blocks
        .iter()
        .map(|b| match b {
            Block::Text(t) => {
                let sentences = sentences_or_whole(t);
                let style = chapter
                    .style(paragraph)
                    .map(|st| style_view(t, &sentences, st));
                paragraph += 1;
                BlockView::Text { sentences, style }
            }
            Block::Image { image, alt } => BlockView::Image {
                image: *image,
                alt: alt.clone(),
            },
            Block::Table(t) => BlockView::Table {
                rows: t.rows.clone(),
                header: t.header,
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

#[derive(Serialize)]
struct Imported {
    rules: replacements::Rules,
    imported: usize,
}

/// Add rules from a text file to this book's (`scope` = "book") or every
/// book's ("all") list.
#[tauri::command]
async fn replacements_import(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    scope: String,
) -> CmdResult<Option<Imported>> {
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Import replacements")
        .add_filter("Rule list", &["txt", "csv", "tsv"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    let text = marginalia_core::decode_text(&std::fs::read(&path).map_err(err)?);
    let new = replacements::parse_list(&text);
    if new.is_empty() {
        return Err("No rules found. Write one per line, as find=replace.".into());
    }
    let mut rules = replacements::load(&id);
    let list = if scope == "all" {
        &mut rules.all
    } else {
        &mut rules.book
    };
    let imported = replacements::merge(list, new);
    replacements::save(&id, &rules).map_err(err)?;
    *state.open.lock().unwrap() = None;
    Ok(Some(Imported { rules, imported }))
}

#[tauri::command]
async fn replacements_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    scope: String,
) -> CmdResult<Option<String>> {
    let rules = replacements::load(&id);
    let (list, name) = if scope == "all" {
        (&rules.all, "All books".to_string())
    } else {
        let title = state
            .library
            .lock()
            .unwrap()
            .get(&id)
            .map(|e| e.title.clone())
            .unwrap_or_default();
        (&rules.book, title)
    };
    let file_name: String = format!("{name} - replacements.txt")
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export replacements")
        .set_file_name(&file_name)
        .add_filter("Rule list", &["txt"])
        .blocking_save_file()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    std::fs::write(&path, replacements::to_list(&name, list)).map_err(err)?;
    Ok(Some(path.display().to_string()))
}

#[derive(Serialize)]
struct NameGroupView {
    key: String,
    total: usize,
    names: Vec<(String, usize)>,
}

/// Proper names used in the book (after replacements), grouped by their
/// first word so different translations of one name sit together.
#[tauri::command]
async fn book_names(state: State<'_, AppState>, id: String) -> CmdResult<Vec<NameGroupView>> {
    let book = state.book(&id)?;
    let groups =
        tauri::async_runtime::spawn_blocking(move || marginalia_core::names::names(&book, 2, 400))
            .await
            .map_err(err)?;
    Ok(groups
        .into_iter()
        .map(|g| NameGroupView {
            key: g.key,
            total: g.total,
            names: g.names.into_iter().map(|n| (n.name, n.count)).collect(),
        })
        .collect())
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

#[derive(Serialize)]
struct UpdateInfo {
    /// Whether this build can update itself (release builds made with a
    /// signing key).
    enabled: bool,
    current: String,
    /// A newer version, if there is one.
    version: Option<String>,
    notes: Option<String>,
}

fn updates_enabled(app: &AppHandle) -> bool {
    app.config().plugins.0.contains_key("updater")
}

#[tauri::command]
async fn update_check(app: AppHandle) -> CmdResult<UpdateInfo> {
    use tauri_plugin_updater::UpdaterExt;
    let mut info = UpdateInfo {
        enabled: updates_enabled(&app),
        current: app.package_info().version.to_string(),
        version: None,
        notes: None,
    };
    if !info.enabled {
        return Ok(info);
    }
    if let Some(update) = app.updater().map_err(err)?.check().await.map_err(err)? {
        info.version = Some(update.version);
        info.notes = update.body;
    }
    Ok(info)
}

#[derive(Clone, Serialize)]
struct UpdateProgress {
    done: u64,
    total: Option<u64>,
}

/// Download and install the newest version, then restart into it.
#[tauri::command]
async fn update_install(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_updater::UpdaterExt;
    if !updates_enabled(&app) {
        return Err("this build cannot update itself".into());
    }
    let update = app
        .updater()
        .map_err(err)?
        .check()
        .await
        .map_err(err)?
        .ok_or("Marginalia is up to date")?;
    let mut done = 0u64;
    update
        .download_and_install(
            |chunk, total| {
                done += chunk as u64;
                let _ = app.emit("update-progress", UpdateProgress { done, total });
            },
            || {},
        )
        .await
        .map_err(err)?;
    app.restart()
}

fn main() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let settings = Settings::load();
    #[cfg(target_os = "linux")]
    if settings.app_menu {
        desktop::integrate();
    }
    let state = AppState {
        library: Mutex::new(Library::load()),
        settings: Mutex::new(settings),
        pending: Mutex::new(file_args(std::env::args(), &cwd)),
        ..AppState::default()
    };
    let app = tauri::Builder::default()
        // A second launch (opening a book while Marginalia runs) hands its
        // files to this window and quits.
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            open_files(app, file_args(args, std::path::Path::new(&cwd)));
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Only builds made with an update signing key carry the updater
            // settings; the plugin refuses to start without them.
            if updates_enabled(app.handle()) {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            Ok(())
        })
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
            library_pending,
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
            replacements_import,
            replacements_export,
            book_names,
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
            desktop_status,
            desktop_set_menu,
            ai_config_get,
            ai_config_set,
            ai_status,
            ai_run,
            ai_clear,
            ai_glossary_get,
            ai_glossary_set,
            update_check,
            update_install,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Marginalia");
    app.run(|_app, _event| {
        // macOS passes opened files as an event, not on the command line.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = _event {
            let paths = urls
                .into_iter()
                .filter_map(|u| u.to_file_path().ok())
                .collect();
            open_files(_app, paths);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_paths_from_arguments() {
        let dir = std::env::temp_dir().join("marginalia-args-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.prc"), b"x").unwrap();
        let args = ["marginalia", "--flag", "a.prc", "missing.epub"].map(String::from);
        assert_eq!(file_args(args, &dir), [dir.join("a.prc")]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
