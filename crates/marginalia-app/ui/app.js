// Marginalia UI — plain JavaScript, no framework or build step.
// Talks to the Rust side through Tauri commands and events.

"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const CJK_END = /[　-ヿ㐀-鿿豈-﫿＀-￯]["'”’」』）)]*$/;
const SPEEDS = [0.75, 1, 1.25, 1.5, 1.75, 2];
const QUICK_SPEEDS = [0.75, 1, 1.25, 1.5, 2];
const BOOK_FILE = /\.(epub|mobi|prc|azw3?|pdb|fb2|fbz|fb2\.zip|txt|md|markdown|pdf)$/i;
const LAYOUTS = ["margin", "book", "workbench"];
const THEMES = [["paper", "Paper"], ["night", "Night"], ["sepia", "Sepia"]];
const VIEW_TITLES = {
  chapters: "Contents",
  search: "Search",
  library: "Library",
  voices: "Voices",
  settings: "Settings",
};

const state = {
  settings: null,
  library: [],
  view: "chapters",
  book: null, // { entry, toc }
  openBooks: [], // ids shown as tabs in the workbench layout
  chapter: -1,
  blocks: [], // chapter content: { kind: "text", sentences } | { kind: "image", image, alt }
  paragraphs: [], // text blocks only: [[sentence, ...], ...]
  cursor: null, // { chapter, paragraph, sentence } chosen by clicking
  speaking: null, // position currently being read aloud
  tts: "stopped", // loading | playing | paused | stopped
  voiceLabel: "",
  engine: null,
  models: null, // voice models for voiceLang
  current: null, // voice used for voiceLang
  allLangs: null,
  voiceLang: null, // language family shown in the Voices view
  downloads: {}, // model id -> { done, total, name }
  chapterFilter: "",
  notes: [], // notes of the open book
  bookmarks: [], // bookmarks of the open book
  panelTab: "notes", // right panel: notes | bookmarks
  search: { query: "", hits: [], truncated: false, active: -1 },
  notesFilter: "",
  editing: null, // note open in the editor
  pendingAnchor: null, // anchor of the current text selection
  book2: { spread: 0, spreads: 1, step: 1 }, // two-page layout
  palette: { items: [], active: 0 },
  replace: { book: [], all: [], dirty: false }, // find-and-replace rules
};

// ------------------------------------------------------------- helpers

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

function el(html) {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}

function icon(name) {
  return `<svg class="i" aria-hidden="true"><use href="#i-${name}"></use></svg>`;
}

function debounce(fn, ms) {
  let t;
  return (...args) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
}

// Lower-case without accents, so "thung lung" matches "thung lũng".
function fold(s) {
  return String(s).normalize("NFD").replace(/[̀-ͯ]/g, "").replace(/[đĐ]/g, "d").toLowerCase();
}

// Images are served by the app's `mgimg` protocol (Windows exposes custom
// protocols as http://<scheme>.localhost).
const IMG_BASE = navigator.userAgent.includes("Windows") ? "http://mgimg.localhost/" : "mgimg://localhost/";
const imageUrl = (bookId, image) => `${IMG_BASE}${bookId}/${image}`;
const coverUrl = (bookId) => `${IMG_BASE}cover/${bookId}`;

function mb(bytes) {
  return (bytes / 1e6).toFixed(bytes < 1e7 ? 1 : 0) + " MB";
}

function hue(text) {
  let h = 0;
  for (const c of text) h = (h * 31 + c.codePointAt(0)) % 360;
  return h;
}

// Words (or CJK characters) per minute read aloud at 1×.
function speechRate() {
  const lang = langFamily(state.book?.entry.language || "en");
  if (["zh", "ja", "ko"].includes(lang)) return 260;
  if (lang === "vi") return 190;
  return 165;
}

function minutes(words) {
  const m = words / (speechRate() * (state.settings?.speed || 1));
  if (m < 1) return "<1 min";
  if (m < 60) return Math.round(m) + " min";
  return `${Math.floor(m / 60)} h ${Math.round(m % 60)} min`;
}

function toast(message, { error = false, actions = [], timeout = error ? 9000 : 4000 } = {}) {
  const node = el(`<div class="toast${error ? " error" : ""}" role="${error ? "alert" : "status"}">
    <div class="msg">${esc(message)}<div class="actions"></div></div>
    <button class="icon-btn small" aria-label="Close">${icon("close")}</button></div>`);
  const close = () => node.remove();
  node.querySelector("button").onclick = close;
  const bar = node.querySelector(".actions");
  if (!actions.length) bar.remove();
  for (const a of actions) {
    const b = el(`<button class="btn small">${esc(a.label)}</button>`);
    b.onclick = () => { close(); a.run(); };
    bar.append(b);
  }
  $("#toasts").append(node);
  if (timeout) setTimeout(close, timeout);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e), { error: true });
    throw e;
  }
}

// ------------------------------------------------------------ settings

function layout() {
  return LAYOUTS.includes(state.settings?.layout) ? state.settings.layout : "margin";
}

function isBook() {
  return layout() === "book" && !!document.querySelector(".book-stage");
}

function applySettings() {
  const s = state.settings;
  document.documentElement.dataset.theme = THEMES.some(([t]) => t === s.theme) ? s.theme : "paper";
  for (const l of LAYOUTS) document.body.classList.toggle("layout-" + l, layout() === l);
  document.body.classList.toggle("hand-notes", !!s.hand_notes);
  document.querySelectorAll("#layout-switch button").forEach((b) =>
    b.setAttribute("aria-checked", String(b.dataset.layout === layout())));
  $("#sidebar").style.width = s.sidebar_width + "px";
  $("#notes-panel").style.width = s.notes_width + "px";
  document.body.classList.toggle("notes-open", !!s.notes_open);
  $("#st-speed").textContent = formatSpeed(s.speed);
  const r = document.querySelector(".reading");
  if (r) styleReading(r);
  relayout();
  renderPlayer();
}

function formatSpeed(v) {
  return (Math.round(v * 100) / 100).toString() + "×";
}

function styleReading(r) {
  const s = state.settings;
  r.style.setProperty("--font-size", s.font_size + "px");
  r.style.setProperty("--line-height", s.line_height);
  r.style.setProperty("--width", s.width + "ch");
  r.classList.toggle("sans", s.font === "sans");
}

const persistSettings = debounce(() => invoke("settings_set", { settings: state.settings }), 300);

function updateSetting(key, value) {
  state.settings[key] = value;
  applySettings();
  persistSettings();
}

// Switching layout rebuilds the page around the same reading position.
function setLayout(l) {
  if (l === layout()) return;
  const pos = state.book ? visiblePosition() : null;
  updateSetting("layout", l);
  if (state.book) {
    renderContent();
    goToParagraph(pos.paragraph);
  }
  renderTabs();
  renderStatus();
}

function setTheme(t) {
  updateSetting("theme", t);
  if (state.view === "settings") renderSidebar();
}

// ------------------------------------------------------------- sidebar

function setView(view) {
  if (state.view === view && !document.body.classList.contains("sidebar-hidden")) {
    document.body.classList.add("sidebar-hidden"); // click active icon to hide, like VS Code
  } else {
    document.body.classList.remove("sidebar-hidden");
    state.view = view;
  }
  if (view === "voices") refreshEngine();
  renderSidebar();
  relayoutSoon();
}

function renderSidebar() {
  document.querySelectorAll(".activity").forEach((b) => {
    const on = b.dataset.view === state.view && !document.body.classList.contains("sidebar-hidden");
    b.classList.toggle("active", on);
    b.setAttribute("aria-pressed", String(on));
  });
  const title = $("#sidebar-title");
  title.innerHTML = `<span class="grow">${VIEW_TITLES[state.view]}</span>`;
  if (state.view === "chapters" && state.book) {
    const mode = state.settings.toc_mode === "map" ? "map" : "list";
    const seg = el(`<div class="seg" role="radiogroup" aria-label="Contents view">
      <button role="radio" data-mode="list" aria-checked="${mode === "list"}" aria-label="Chapter list" title="Chapter list">${icon("toc")}</button>
      <button role="radio" data-mode="map" aria-checked="${mode === "map"}" aria-label="Book map" title="Book map: the whole book drawn to length">${icon("map")}</button></div>`);
    seg.querySelectorAll("button").forEach((b) => {
      b.onclick = () => {
        updateSetting("toc_mode", b.dataset.mode);
        renderSidebar();
      };
    });
    title.append(seg);
  }
  const content = $("#sidebar-content");
  const scroll = content.scrollTop;
  content.replaceChildren();
  ({ chapters: renderChapters, search: renderSearch, library: renderLibrary, voices: renderVoices, settings: renderSettings })[state.view](content);
  content.scrollTop = scroll;
}

function renderBookHead(root) {
  const { entry, toc } = state.book;
  const pct = Math.round(progressOf(entry.position) * 100);
  const words = toc.reduce((n, c) => n + c.words, 0);
  root.append(el(`<div class="book-head">
    <h2>${esc(entry.title)}</h2>
    <p class="by">${esc(entry.authors.join(", ") || "Unknown author")}</p>
    <div class="meter" role="progressbar" aria-label="Read" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><div style="width:${pct}%"></div></div>
    <div class="meta">${pct}% read · ${toc.length} chapter${toc.length === 1 ? "" : "s"} · ${minutes(words)} to listen</div></div>`));
}

function renderChapters(root) {
  if (!state.book) {
    root.append(el(`<div class="hint">No book open.</div>`));
    const b = el(`<button class="btn block">Open book…</button>`);
    b.onclick = pickBooks;
    root.append(b);
    return;
  }
  renderBookHead(root);
  if (state.settings.toc_mode === "map") return renderMap(root);
  const { toc } = state.book;
  if (toc.length > 12) {
    const f = el(`<div class="filter"><input type="search" placeholder="Filter chapters" aria-label="Filter chapters" value="${esc(state.chapterFilter)}"></div>`);
    const input = f.querySelector("input");
    input.oninput = () => {
      state.chapterFilter = input.value;
      renderChapterList(list);
    };
    root.append(f);
  } else {
    root.append(el(`<div style="height:10px"></div>`));
  }
  const list = el(`<div class="toc" role="list"></div>`);
  root.append(list);
  renderChapterList(list);
  list.querySelector(".active")?.scrollIntoView({ block: "center" });
}

// How much of chapter `i` has been read, 0..1.
function chapterRead(i) {
  const pos = state.book.entry.position;
  if (i < pos.chapter) return 1;
  if (i > pos.chapter) return 0;
  const n = state.book.toc[i]?.paragraphs || 1;
  return Math.min(1, pos.paragraph / n);
}

function renderChapterList(list) {
  const q = fold(state.chapterFilter.trim());
  const counts = new Map();
  for (const n of state.notes) counts.set(n.anchor.chapter, (counts.get(n.anchor.chapter) || 0) + 1);
  const frag = document.createDocumentFragment();
  state.book.toc.forEach((c, i) => {
    if (q && !fold(c.title).includes(q)) return;
    const read = chapterRead(i);
    const notes = counts.get(i);
    const meta = c.words ? minutes(c.words) : c.images ? icon("image") : "";
    const item = el(`<div class="list-item${i === state.chapter ? " active" : ""}" role="listitem" tabindex="0" title="${esc(c.title)}">
      <span class="num">${i + 1}</span><span class="label">${esc(c.title)}</span>
      ${notes ? `<span class="note-count" title="${notes} note${notes > 1 ? "s" : ""}">${notes}</span>` : ""}
      <span class="meta">${meta}</span>
      ${read > 0 && read < 1 ? `<span class="read"><div style="width:${Math.round(read * 100)}%"></div></span>` : ""}</div>`);
    if (i === state.chapter) item.setAttribute("aria-current", "true");
    item.onclick = () => loadChapter(i, { scroll: "top" });
    item.onkeydown = (e) => { if (e.key === "Enter") loadChapter(i, { scroll: "top" }); };
    frag.append(item);
  });
  list.replaceChildren(frag);
}

// ------------------------------------------------------------ book map

// The whole book, one row per chapter drawn to its length, with search
// matches, notes, bookmarks and the reading position on it.
function renderMap(root) {
  const box = el(`<div class="search-box">
    <input type="search" placeholder="Search in this book" aria-label="Search in this book" autocomplete="off" spellcheck="false">
    <div class="meta" id="map-meta"></div></div>`);
  const input = box.querySelector("input");
  input.value = state.search.query;
  input.oninput = () => {
    state.search.query = input.value;
    runSearch();
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      nextHit(e.shiftKey ? -1 : 1);
    }
  };
  root.append(box);
  root.append(el(`<ol class="map" id="map-rows" aria-label="Chapters drawn to length"></ol>`));
  root.append(el(`<div class="map-legend" aria-hidden="true">
    <span><i class="l-hit"></i>Search match</span><span><i class="l-note"></i>Note</span>
    <span><i class="l-bm"></i>Bookmark</span><span><i class="l-here"></i>You are here</span></div>`));
  renderMapRows();
  document.querySelector(".map-row.active")?.scrollIntoView({ block: "center" });
}

function renderMapRows() {
  const rows = $("#map-rows");
  if (!rows || !state.book) return;
  const { toc, entry } = state.book;
  const { hits, active, query } = state.search;
  const meta = $("#map-meta");
  if (meta) {
    const chapters = new Set(hits.map((h) => h.chapter)).size;
    meta.textContent = !query.trim()
      ? "Click a row to jump there. Matches, notes and bookmarks show where they are in the book."
      : hits.length
        ? `${hits.length}${state.search.truncated ? "+" : ""} match${hits.length === 1 ? "" : "es"} in ${chapters} chapter${chapters === 1 ? "" : "s"}`
        : "No matches.";
  }
  const longest = Math.max(1, ...toc.map((c) => c.words));
  const at = (chapter, paragraph) => ((paragraph + 0.5) / Math.max(1, toc[chapter]?.paragraphs || 1)) * 100;
  const byChapter = (list, key) => {
    const m = new Map();
    list.forEach((x, i) => {
      const c = key(x);
      if (!m.has(c)) m.set(c, []);
      m.get(c).push([x, i]);
    });
    return m;
  };
  const hitsBy = byChapter(hits, (h) => h.chapter);
  const notesBy = byChapter(state.notes, (n) => n.anchor.chapter);
  const marksBy = byChapter(state.bookmarks, (b) => b.position.chapter);
  const frag = document.createDocumentFragment();
  toc.forEach((c, i) => {
    const w = Math.max(3, (c.words / longest) * 100);
    const read = chapterRead(i) * 100;
    const ch = hitsBy.get(i) || [];
    const nt = notesBy.get(i) || [];
    const bm = marksBy.get(i) || [];
    const label = `${c.title}${ch.length ? ` · ${ch.length} match${ch.length > 1 ? "es" : ""}` : ""}${nt.length ? ` · ${nt.length} note${nt.length > 1 ? "s" : ""}` : ""}`;
    const row = el(`<li class="map-row${i === state.chapter ? " active" : ""}" title="${esc(label)}">
      <span class="num">${i + 1}</span>
      <span class="lane"><span class="track" style="width:${w}%">
        <span class="done" style="width:${read}%"></span>
        ${ch.map(([h, hi]) => `<span class="hit${hi === active ? " current" : ""}" data-hit="${hi}" style="left:${at(i, h.paragraph)}%"></span>`).join("")}
        ${nt.map(([n]) => `<span class="nt" data-color="${esc(n.color)}" style="left:${at(i, n.anchor.paragraph)}%"></span>`).join("")}
        ${bm.map(([b]) => `<span class="bm" style="left:${at(i, b.position.paragraph)}%"></span>`).join("")}
        ${i === entry.position.chapter ? `<span class="here" style="left:${Math.min(100, at(i, entry.position.paragraph))}%"></span>` : ""}
      </span></span></li>`);
    row.onclick = (e) => {
      const hit = e.target.closest("[data-hit]");
      if (hit) return goToHit(Number(hit.dataset.hit));
      const track = row.querySelector(".track");
      const r = track.getBoundingClientRect();
      const f = e.target.closest(".track") ? Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) : 0;
      const paragraph = Math.floor(f * (c.paragraphs || 1));
      loadChapter(i, { scroll: "position", paragraph }).then(() => savePosition({ chapter: i, paragraph, sentence: 0 }));
    };
    frag.append(row);
  });
  rows.replaceChildren(frag);
}

// ------------------------------------------------------------- library

function renderLibrary(root) {
  const b = el(`<button class="btn block">${icon("plus")}Open book…</button>`);
  b.onclick = pickBooks;
  root.append(b);
  if (!state.library.length) {
    root.append(el(`<div class="hint">Your library is empty. Open EPUB, MOBI, AZW, FB2, TXT, Markdown or PDF files, or drop them onto the window.</div>`));
    return;
  }
  root.append(el(`<div class="section-header"><span class="grow">Books</span><span class="badge">${state.library.length}</span></div>`));
  for (const book of state.library) {
    const pct = Math.round((book.progress || 0) * 100);
    const initial = (book.title.trim()[0] || "?").toUpperCase();
    const item = el(`<div class="list-item book-item${state.book?.entry.id === book.id ? " active" : ""}" tabindex="0" title="${esc(book.path)}">
      <div class="cover" style="background:hsl(${hue(book.title)} 35% 34%)">${book.has_cover
        ? `<img src="${coverUrl(book.id)}" alt="" loading="lazy">` : esc(initial)}</div>
      <div class="info">
        <div class="title">${esc(book.title)}</div>
        <div class="meta">${esc(book.authors.join(", ") || "Unknown author")}</div>
        <div class="meta"><span class="badge">${esc(book.format)}</span> ${pct}% · ${book.chapters} ch.</div>
        <div class="progress-line"><div style="width:${pct}%"></div></div>
      </div>
      <div class="actions"><button class="icon-btn small" aria-label="Remove from library" title="Remove from library">${icon("close")}</button></div></div>`);
    item.onclick = () => openBook(book.id);
    item.onkeydown = (e) => { if (e.key === "Enter") openBook(book.id); };
    item.querySelector(".actions button").onclick = async (e) => {
      e.stopPropagation();
      await call("library_remove", { id: book.id });
      closeTab(book.id);
      await refreshLibrary();
    };
    root.append(item);
  }
}

// --------------------------------------------------------------- voices

async function refreshEngine() {
  if (!state.voiceLang) state.voiceLang = langFamily(state.book?.entry.language || "en");
  try {
    const [engine, models, current] = await Promise.all([
      invoke("engine_status"),
      invoke("models_list", { lang: state.voiceLang }),
      invoke("voice_current", { lang: state.voiceLang }),
    ]);
    Object.assign(state, { engine, models, current });
    if (!state.allLangs) {
      const all = await invoke("models_list", { lang: null });
      state.allLangs = [...new Set(all.flatMap((m) => m.languages.map(langFamily)))];
    }
  } catch (e) {
    state.engine = null;
    toast(String(e), { error: true });
  }
  if (state.view === "voices") renderSidebar();
  renderStatus();
}

function langFamily(lang) {
  return String(lang).replace("-", "_").split("_")[0].toLowerCase();
}

const LANG_NAMES = (() => {
  try {
    return new Intl.DisplayNames(["en"], { type: "language" });
  } catch {
    return null;
  }
})();

function langName(code) {
  try {
    return LANG_NAMES?.of(code.replace("_", "-")) || code;
  } catch {
    return code;
  }
}

const SAMPLES = {
  vi: "Xin chào! Đây là giọng đọc của Marginalia. Chúc bạn đọc sách vui vẻ.",
  en: "Hello! This is how Marginalia sounds when it reads your books aloud.",
  zh: "你好！这是Marginalia为你朗读书籍的声音。",
  fr: "Bonjour ! Voici la voix de Marginalia.",
  de: "Hallo! So klingt Marginalia, wenn es dir vorliest.",
  es: "¡Hola! Así suena Marginalia cuando te lee un libro.",
  ja: "こんにちは。これはマージナリアの声です。",
  ko: "안녕하세요. 마지널리아의 목소리입니다.",
};

const KOKORO_ACCENTS = { a: "American", b: "British", z: "Chinese", e: "Spanish", f: "French", h: "Hindi", i: "Italian", j: "Japanese", p: "Portuguese" };

// Speakers worth offering for a language, as [index, label] pairs.
function speakerOptions(model, lang) {
  const names = model.speakers;
  if (names.length <= 1) return [];
  const opts = names.map((n, i) => [i, n]);
  if (model.kind === "kokoro") {
    const prefixes = lang === "zh" ? ["z"] : lang === "en" ? ["a", "b"] : Object.keys(KOKORO_ACCENTS);
    return opts
      .filter(([, n]) => prefixes.includes(n[0]))
      .map(([i, n]) => [i, `${n.slice(3).replace(/^./, (c) => c.toUpperCase())} — ${KOKORO_ACCENTS[n[0]] || ""} ${n[1] === "f" ? "female" : "male"}`]);
  }
  if (model.kind === "supertonic") {
    return opts.map(([i, n]) => [i, `${n[0] === "F" ? "Female" : "Male"} ${n.slice(1)}`]);
  }
  const notes = model.speaker_notes || [];
  return opts.map(([i, n]) => [i, /^\d+$/.test(n) ? `Speaker ${n}` : notes[i] ? `${n} — ${notes[i]}` : n]);
}

function setVoice(model, speaker) {
  const voices = { ...(state.settings.voices || {}) };
  voices[state.voiceLang] = { model, speaker };
  updateSetting("voices", voices);
  invoke("voice_current", { lang: state.voiceLang }).then((c) => {
    state.current = c;
    if (state.view === "voices") renderSidebar();
    renderPlayer();
  });
}

function previewVoice(model, speaker) {
  const text = SAMPLES[state.voiceLang] || SAMPLES.en;
  invoke("tts_preview", { text, lang: state.voiceLang, model, speaker }).catch((e) => toast(String(e), { error: true }));
}

function renderVoices(root) {
  const eng = state.engine;
  if (!eng || !state.models) {
    root.append(el(`<div class="hint">Loading voices…</div>`));
    return;
  }
  const lang = state.voiceLang;
  const name = langName(lang);

  // Language picker.
  const sel = el(`<div class="filter"><select aria-label="Language"></select></div>`);
  const select = sel.querySelector("select");
  const langs = [...new Set([...(state.allLangs || []), lang])]
    .map((c) => [c, langName(c)])
    .sort((a, b) => a[1].localeCompare(b[1]));
  for (const [code, label] of langs) select.append(new Option(label, code, false, code === lang));
  select.onchange = () => {
    state.voiceLang = select.value;
    state.models = null;
    renderSidebar();
    refreshEngine();
  };
  root.append(sel);

  // The voice currently used for this language.
  root.append(el(`<div class="section-header"><span class="grow">Voice for ${esc(name)}</span></div>`));
  const installed = state.models.filter((m) => m.installed);
  if (!installed.length) {
    root.append(el(`<div class="hint">No voice for ${esc(name)} yet. Download one below; the starred one is a good start.</div>`));
  }
  for (const m of installed) {
    const active = state.current?.model === m.id;
    const item = el(`<div class="list-item voice-choice${active ? " active" : ""}" role="radio" aria-checked="${active}" tabindex="0">
      <span class="radio" aria-hidden="true"></span>
      <span class="label">${esc(m.name)} <span class="meta">${esc(m.kind)}</span></span>
      <button class="icon-btn small" aria-label="Listen to ${esc(m.name)}" title="Listen">${icon("play")}</button></div>`);
    item.onclick = () => setVoice(m.id, null);
    const speakers = speakerOptions(m, lang);
    const currentSpeaker = active ? state.current.speaker : null;
    item.querySelector("button").onclick = (e) => {
      e.stopPropagation();
      previewVoice(m.id, currentSpeaker);
    };
    if (active && speakers.length) {
      const s = el(`<select aria-label="Speaker"></select>`);
      for (const [i, label] of speakers) s.append(new Option(label, i, false, i === currentSpeaker));
      if (!speakers.some(([i]) => i === currentSpeaker)) s.prepend(new Option("Default", "", true, true));
      s.onclick = (e) => e.stopPropagation();
      s.onchange = () => {
        const sp = s.value === "" ? null : Number(s.value);
        setVoice(m.id, sp);
        previewVoice(m.id, sp);
      };
      item.append(s);
    }
    root.append(item);
  }

  // Models that speak this language.
  root.append(el(`<div class="section-header"><span class="grow">Voice models for ${esc(name)}</span></div>`));
  if (!state.models.length) root.append(el(`<div class="hint">No voice model speaks ${esc(name)}.</div>`));
  for (const m of state.models) {
    const dl = state.downloads[m.id];
    const item = el(`<div class="model-card${m.recommended ? " recommended" : ""}">
      <div class="row"><span class="label">${m.recommended ? icon("star") : ""}${esc(m.name)}</span>
        <span class="badge">${esc(m.kind)}</span><span class="meta" style="margin:0">${mb(m.size)}</span></div>
      ${m.note ? `<div class="meta">${esc(m.note)}</div>` : ""}
      <div class="meta">${m.speakers.length > 1 ? `${m.speakers.length} speakers · ` : ""}${m.languages.length > 3 ? `${m.languages.length} languages` : esc(m.languages.join(", "))} · ${esc(m.license)}</div>
      <div class="slot"></div></div>`);
    const slot = item.querySelector(".slot");
    if (dl) {
      slot.append(progressBar(dl));
    } else if (m.installed) {
      const rm = el(`<button class="btn small quiet">Remove</button>`);
      rm.onclick = async () => {
        await call("model_remove", { id: m.id });
        refreshEngine();
      };
      slot.append(rm);
    } else {
      const b = el(`<button class="btn small">${icon("export")}Download</button>`);
      b.onclick = () => downloadModel(m.id, m.name);
      slot.append(b);
    }
    root.append(item);
  }
  root.append(el(`<div class="hint">Voices run on this computer with <code>sherpa-onnx</code>; nothing is sent online.
    ${eng.espeak ? "eSpeak NG is used as a fallback." : ""} Models are stored in <code>${esc(eng.data_dir)}</code>.</div>`));
}

function progressBar(dl) {
  const pct = dl.total ? Math.round((dl.done / dl.total) * 100) : 0;
  return el(`<div style="width:100%"><div class="bar" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><div style="width:${pct}%"></div></div>
    <div class="meta">${dl.total ? `${pct}% of ${mb(dl.total)}` : mb(dl.done)}</div></div>`);
}

async function downloadModel(id, name) {
  state.downloads[id] = { done: 0, total: null, name: name || id };
  renderSidebar();
  renderDownloads();
  try {
    await invoke("model_download", { id });
    toast(`${name || "Voice model"} downloaded.`);
  } catch (e) {
    toast(`Downloading ${name || id} failed: ${e}`, { error: true });
  }
  delete state.downloads[id];
  renderDownloads();
  refreshEngine();
}

function renderDownloads() {
  const list = Object.values(state.downloads);
  $(".activity[data-view=voices] .dot").hidden = !list.length;
  const st = $("#st-download");
  if (!list.length) {
    st.replaceChildren();
    return;
  }
  const d = list[0];
  const pct = d.total ? Math.round((d.done / d.total) * 100) : 0;
  st.innerHTML = `Downloading ${esc(d.name)}${list.length > 1 ? ` (+${list.length - 1})` : ""}
    <span class="bar" aria-hidden="true"><div style="width:${pct}%"></div></span>${d.total ? pct + "%" : mb(d.done)}`;
}

// -------------------------------------------------------------- settings view

function renderSettings(root) {
  const s = state.settings;
  const add = (html, bind) => {
    const n = el(`<div class="setting">${html}</div>`);
    bind(n);
    root.append(n);
  };
  let uid = 0;
  const select = (key, label, options, onChange = (v) => updateSetting(key, v)) => {
    const id = "set-" + ++uid;
    add(`<label for="${id}">${label}</label><select id="${id}">${options.map(([v, t]) =>
      `<option value="${v}"${s[key] === v ? " selected" : ""}>${t}</option>`).join("")}</select>`,
    (n) => { n.querySelector("select").onchange = (e) => onChange(e.target.value); });
  };
  const range = (key, label, min, max, step, fmt) => {
    const id = "set-" + ++uid;
    add(`<label for="${id}">${label}<span class="value">${fmt(s[key])}</span></label>
      <input id="${id}" type="range" min="${min}" max="${max}" step="${step}" value="${s[key]}">`,
    (n) => {
      const input = n.querySelector("input");
      input.oninput = () => {
        updateSetting(key, Number(input.value));
        n.querySelector(".value").textContent = fmt(Number(input.value));
      };
    });
  };
  const check = (key, label) => {
    const n = el(`<div class="setting check"><label><input type="checkbox"${s[key] ? " checked" : ""}>${label}</label></div>`);
    n.querySelector("input").onchange = (e) => updateSetting(key, e.target.checked);
    root.append(n);
  };

  root.append(el(`<div class="section-header"><span class="grow">Appearance</span></div>`));
  select("theme", "Theme", THEMES, setTheme);
  select("layout", "Layout", [["margin", "Margin: notes beside the text"], ["book", "Book: two-page spread"], ["workbench", "Workbench: tabs, notes panel, status bar"]], setLayout);
  select("font", "Font", [["serif", "Serif (Literata)"], ["sans", "Sans-serif (Be Vietnam Pro)"]]);
  range("font_size", "Font size", 12, 36, 1, (v) => v + " px");
  range("line_height", "Line spacing", 1.2, 2.4, 0.1, (v) => v.toFixed(1));
  range("width", "Column width", 40, 120, 5, (v) => v + " ch");
  check("hand_notes", "Handwritten margin notes");
  root.append(el(`<div class="section-header"><span class="grow">Read aloud</span></div>`));
  range("speed", "Speaking speed", 0.5, 2.5, 0.05, (v) => v.toFixed(2) + "×");
  root.append(el(`<div class="hint"><kbd>Ctrl</kbd> <kbd>K</kbd> search or jump · <kbd>Space</kbd> play/pause ·
    <kbd>←</kbd> <kbd>→</kbd> chapters (pages in the book layout) · <kbd>Ctrl</kbd> <kbd>B</kbd> side bar ·
    <kbd>Ctrl</kbd> <kbd>O</kbd> open · <kbd>Ctrl</kbd> <kbd>+</kbd>/<kbd>-</kbd> font size.
    Double-click a sentence to read aloud from there.</div>`));
}

// ------------------------------------------------------------ library

async function refreshLibrary() {
  state.library = await invoke("library_list");
  if (state.view === "library") renderSidebar();
  renderTabs();
}

async function pickBooks() {
  const res = await call("library_pick");
  await handleAdded(res);
}

async function addPaths(paths) {
  const res = await call("library_add", { paths });
  await handleAdded(res);
}

async function handleAdded(res) {
  for (const e of res.errors) toast(e, { error: true });
  await refreshLibrary();
  if (res.added.length) await openBook(res.added[0].id);
}

// -------------------------------------------------------------- reader

async function openBook(id) {
  if (state.book?.entry.id === id) {
    setViewIfHidden("chapters");
    return;
  }
  stopSpeech();
  const view = await call("book_open", { id });
  state.book = view;
  if (!state.openBooks.includes(id)) state.openBooks.push(id);
  state.notes = await invoke("notes_list", { id }).catch(() => []);
  state.bookmarks = await invoke("bookmarks_list", { id }).catch(() => []);
  const rules = await invoke("replacements_get", { id }).catch(() => ({ book: [], all: [] }));
  state.replace = { book: rules.book, all: rules.all, dirty: false };
  state.search = { query: "", hits: [], truncated: false, active: -1 };
  state.notesFilter = "";
  closeNoteEditor();
  state.cursor = null;
  state.speaking = null;
  state.chapter = -1;
  state.chapterFilter = "";
  state.voiceLang = null;
  state.models = null;
  state.voiceLabel = "";
  const pos = view.entry.position;
  await loadChapter(Math.min(pos.chapter, Math.max(view.toc.length - 1, 0)), {
    scroll: "position", paragraph: pos.paragraph,
  });
  state.view = "chapters";
  renderSidebar();
  renderNotesPanel();
  refreshLibrary();
  refreshEngine();
}

function setViewIfHidden(view) {
  document.body.classList.remove("sidebar-hidden");
  state.view = view;
  renderSidebar();
  relayoutSoon();
}

function closeBook() {
  stopSpeech();
  state.book = null;
  state.chapter = -1;
  state.blocks = [];
  state.paragraphs = [];
  state.notes = [];
  state.bookmarks = [];
  state.search = { query: "", hits: [], truncated: false, active: -1 };
  closeNoteEditor();
  renderContent();
  renderSidebar();
  renderNotesPanel();
  renderStatus();
}

function closeTab(id) {
  const i = state.openBooks.indexOf(id);
  if (i >= 0) state.openBooks.splice(i, 1);
  if (state.book?.entry.id === id) {
    const next = state.openBooks[Math.min(i, state.openBooks.length - 1)];
    if (next) openBook(next);
    else closeBook();
  }
  renderTabs();
}

function renderTabs() {
  const tabs = $("#tabs");
  const books = state.openBooks.map((id) => state.library.find((b) => b.id === id)).filter(Boolean);
  if (!books.length) {
    tabs.replaceChildren(el(`<div class="tab active" role="tab" aria-selected="true"><span class="label">Welcome</span></div>`));
    return;
  }
  tabs.replaceChildren(...books.map((b) => {
    const active = b.id === state.book?.entry.id;
    const t = el(`<div class="tab${active ? " active" : ""}" role="tab" aria-selected="${active}" tabindex="${active ? 0 : -1}" title="${esc(b.path)}">
      <span class="label">${esc(b.title)}</span>
      <button class="icon-btn" aria-label="Close ${esc(b.title)}">${icon("close")}</button></div>`);
    t.onclick = () => openBook(b.id);
    t.querySelector("button").onclick = (e) => {
      e.stopPropagation();
      closeTab(b.id);
    };
    return t;
  }));
}

async function loadChapter(index, { scroll = "top", paragraph = 0 } = {}) {
  if (!state.book || index < 0 || index >= state.book.toc.length) return;
  if (index !== state.chapter) {
    state.blocks = await call("chapter_get", { id: state.book.entry.id, index });
    state.paragraphs = state.blocks.filter((b) => b.kind === "text").map((b) => b.sentences);
    state.chapter = index;
    renderContent();
  }
  if (scroll === "top") {
    goToParagraph(0);
    savePosition({ chapter: index, paragraph: 0, sentence: 0 });
  } else if (scroll === "position") {
    goToParagraph(paragraph);
  } else if (scroll === "end" && isBook()) {
    setSpread(state.book2.spreads - 1);
  }
  if (state.view === "chapters") renderSidebar();
  renderStatus();
}

function goToParagraph(paragraph) {
  const p = paragraphEl(paragraph);
  if (isBook()) {
    if (p && paragraph > 0) reveal(p);
    else setSpread(0);
  } else {
    $("#content").scrollTop = p && paragraph > 0 ? p.offsetTop - 24 : 0;
  }
}

function renderContent() {
  const content = $("#content");
  renderTabs();
  if (!state.book) {
    $("#crumbs").textContent = "";
    $("#btn-prev").disabled = $("#btn-next").disabled = true;
    content.replaceChildren(renderWelcome());
    return;
  }
  const { entry, toc } = state.book;
  const title = toc[state.chapter]?.title || "";
  $("#crumbs").innerHTML = `${esc(entry.title)}<span class="sep" aria-hidden="true">/</span><span class="here">${esc(title)}</span>`;
  $("#btn-prev").disabled = state.chapter <= 0;
  $("#btn-next").disabled = state.chapter >= toc.length - 1;

  const reading = el(`<article class="reading"></article>`);
  // The language lets the webview pick the right glyphs (Vietnamese
  // diacritics, Simplified vs Traditional Chinese) and hyphenate.
  if (entry.language) reading.lang = entry.language.replace("_", "-");
  styleReading(reading);
  const norm = (s) => s.toLowerCase().replace(/\s+/g, " ").trim();
  const book = layout() === "book";
  let pi = -1; // index among text paragraphs (what positions and speech use)
  let first = true;
  state.blocks.forEach((block) => {
    if (block.kind === "table") {
      reading.append(renderTable(block));
      return;
    }
    if (block.kind === "image") {
      const fig = el(`<figure><img alt="${esc(block.alt)}" title="${esc(block.alt)}"></figure>`);
      const img = fig.querySelector("img");
      if (!book) img.loading = "lazy";
      img.src = imageUrl(entry.id, block.image);
      img.onerror = () => { fig.remove(); relayoutSoon(); };
      img.onload = relayoutSoon;
      reading.append(fig);
      return;
    }
    const sentences = block.sentences;
    const style = block.style;
    pi += 1;
    const p = document.createElement("p");
    p.dataset.p = pi;
    if (style?.heading || (pi === 0 && norm(sentences.join(" ")) === norm(title))) {
      p.className = "heading";
      if (style?.heading) p.classList.add("h" + style.heading);
    } else if (first && !style?.quote) {
      p.className = "first";
      first = false;
    }
    if (style?.quote) p.classList.add("quote");
    p.append(el(`<span class="gutter add" title="Add a note to this paragraph">${icon("plus")}</span>`));
    sentences.forEach((s, si) => {
      // Chinese/Japanese sentences are not separated by spaces.
      if (si && !CJK_END.test(sentences[si - 1])) p.append(" ");
      const span = document.createElement("span");
      span.className = "s";
      span.dataset.s = si;
      span.textContent = s;
      p.append(span);
    });
    for (const [kind, start, end] of style?.spans || []) wrapRange(p, start, end, INLINE_TAGS[kind] || "span");
    reading.append(p);
  });
  if (!state.blocks.length) reading.append(el(`<p class="hint">This chapter is empty.</p>`));

  const nav = el(`<div class="chapter-nav"></div>`);
  if (state.chapter > 0) {
    const b = el(`<button><small>Previous</small>${esc(toc[state.chapter - 1].title)}</button>`);
    b.onclick = () => loadChapter(state.chapter - 1);
    nav.append(b);
  }
  if (state.chapter < toc.length - 1) {
    const b = el(`<button class="next"><small>Next</small>${esc(toc[state.chapter + 1].title)}</button>`);
    b.onclick = () => loadChapter(state.chapter + 1);
    nav.append(b);
  }
  reading.append(nav);

  if (book) {
    reading.append(el(`<span class="end" aria-hidden="true"></span>`));
    const stage = el(`<div class="book-stage"><div class="spread">
      <div class="sheet left"></div><div class="sheet right"></div>
      <div class="head left"></div><div class="head right"></div>
      <div class="folio left"></div><div class="folio right"></div>
      <div class="viewport"></div></div></div>`);
    stage.querySelector(".head.left").textContent = entry.title;
    stage.querySelector(".head.right").textContent = title;
    stage.querySelector(".viewport").append(reading);
    content.replaceChildren(stage);
    state.book2.spread = 0;
    layoutBook();
  } else {
    const page = el(`<div class="page"></div>`);
    page.append(reading, el(`<aside class="margin" aria-label="Margin notes"></aside>`));
    content.replaceChildren(page);
    content.scrollTop = 0;
  }
  applyNoteMarks();
  applySearchMarks();
  applyBookmarkMarks();
  if (state.speaking?.chapter === state.chapter) highlight(state.speaking, false);
}

const INLINE_TAGS = { b: "strong", i: "em", u: "u", s: "s", sup: "sup", sub: "sub", code: "code" };

// Wrap characters [start, end) of a paragraph's text in <tag> elements,
// one per text node it crosses, so sentence spans stay intact.
function wrapRange(p, start, end, tag) {
  const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
  const pieces = [];
  let pos = 0;
  for (let n = walker.nextNode(); n && pos < end; n = walker.nextNode()) {
    const a = Math.max(start, pos);
    const b = Math.min(end, pos + n.length);
    if (a < b) pieces.push([n, a - pos, b - pos]);
    pos += n.length;
  }
  for (const [n, a, b] of pieces) {
    let node = n;
    if (b < node.length) node.splitText(b);
    if (a > 0) node = node.splitText(a);
    const w = document.createElement(tag);
    node.replaceWith(w);
    w.append(node);
  }
}

function renderTable(block) {
  const fig = el(`<figure class="table"><table></table></figure>`);
  const table = fig.querySelector("table");
  block.rows.forEach((row, i) => {
    const tr = document.createElement("tr");
    for (const cell of row) {
      const td = document.createElement(block.header && i === 0 ? "th" : "td");
      td.textContent = cell;
      tr.append(td);
    }
    table.append(tr);
  });
  return fig;
}

function renderWelcome() {
  const w = el(`<div class="welcome">
    <div class="brand"><img src="logo.svg" alt=""><h1>Marginalia</h1></div>
    <div class="sub">Read, annotate and listen to EPUB, MOBI, FB2, TXT, Markdown and PDF books.</div>
    <div class="cols">
      <div>
        <h2>Start</h2>
        <a id="w-open" tabindex="0">Open a book…</a>
        <a id="w-voices" tabindex="0">Download a voice…</a>
        <a id="w-settings" tabindex="0">Settings</a>
      </div>
      <div class="recent"><h2>Recent</h2></div>
    </div>
    <div class="drop">Drop ebook files here to add them to your library</div>
  </div>`);
  w.querySelector("#w-open").onclick = pickBooks;
  w.querySelector("#w-voices").onclick = () => { setViewIfHidden("voices"); refreshEngine(); };
  w.querySelector("#w-settings").onclick = () => setViewIfHidden("settings");
  const recent = w.querySelector(".recent");
  const books = state.library.slice(0, 8);
  if (!books.length) recent.append(el(`<div class="hint" style="padding:0">No books yet.</div>`));
  for (const b of books) {
    const a = el(`<a tabindex="0">${esc(b.title)}<span>${esc(b.authors[0] || "")}</span></a>`);
    a.onclick = () => openBook(b.id);
    recent.append(a);
  }
  return w;
}

// ------------------------------------------------------------- layouts

const relayoutSoon = debounce(() => relayout(), 60);

function relayout() {
  if (!state.book || !document.querySelector(".reading")) return;
  if (isBook()) {
    const pos = visiblePosition();
    layoutBook();
    if (pos.paragraph > 0) reveal(paragraphEl(pos.paragraph));
  } else {
    layoutMargin();
  }
}

// Margin layout: notes are written in the margin beside what they annotate.
function layoutMargin() {
  const page = document.querySelector(".page");
  if (!page) return;
  const content = $("#content");
  const fontSize = state.settings.font_size;
  const room = content.clientWidth - 80 - 32 - 240;
  const wide = layout() === "margin" && room >= fontSize * 26;
  page.classList.toggle("with-margin", wide);
  const margin = page.querySelector(".margin");
  margin.replaceChildren();
  if (!wide) return;
  const top0 = margin.getBoundingClientRect().top;
  const items = [];
  for (const note of state.notes) {
    if (note.anchor.chapter !== state.chapter) continue;
    const r = noteRange(note);
    const rect = (r && r.getClientRects()[0]) || paragraphEl(note.anchor.paragraph)?.getBoundingClientRect();
    if (!rect) continue;
    items.push({ note, top: rect.top - top0 });
  }
  items.sort((a, b) => a.top - b.top);
  let bottom = -Infinity;
  for (const { note, top } of items) {
    const quote = note.anchor.quote.trim();
    const n = el(`<div class="margin-note" data-color="${esc(note.color)}" tabindex="0" role="button" aria-label="Note: ${esc(quote.slice(0, 60))}">
      <div class="q"><span>${esc(quote.length > 40 ? quote.slice(0, 40) + "…" : quote || "Paragraph")}</span></div>
      ${note.text.trim() ? `<div class="t">${esc(note.text.trim())}</div>` : ""}</div>`);
    const y = Math.max(top - 2, bottom + 12);
    n.style.top = y + "px";
    n.onclick = () => openNoteEditor(note, n);
    n.onkeydown = (e) => { if (e.key === "Enter") openNoteEditor(note, n); };
    margin.append(n);
    bottom = y + n.offsetHeight;
  }
}

// Book layout: the chapter flows into columns, shown two pages at a time.
function layoutBook() {
  const stage = document.querySelector(".book-stage");
  if (!stage) return;
  const reading = stage.querySelector(".reading");
  const spread = stage.querySelector(".spread");
  const vp = stage.querySelector(".viewport");
  const fontSize = state.settings.font_size;
  const W = stage.clientWidth;
  const H = stage.clientHeight;
  const outer = Math.round(Math.max(36, Math.min(64, W * 0.04)));
  const inner = Math.round(outer * 0.8);
  const top = 58;
  const bottom = 54;
  const pageW = Math.max(220, Math.min((W - 48) / 2, fontSize * 34));
  const colW = Math.round(pageW - outer - inner);
  const colH = Math.max(160, Math.round(Math.min(H - 40, pageW * 1.5) - top - bottom));
  const gap = inner * 2;
  const width = colW * 2 + gap;
  spread.style.width = width + outer * 2 + "px";
  spread.style.height = colH + top + bottom + "px";
  Object.assign(vp.style, { left: outer + "px", top: top + "px", width: width + "px", height: colH + "px" });
  Object.assign(reading.style, { width: width + "px", height: colH + "px", columnCount: 2, columnGap: gap + "px" });
  reading.style.setProperty("--page-h", colH - 20 + "px");
  for (const side of ["left", "right"]) {
    const head = spread.querySelector(".head." + side);
    const folio = spread.querySelector(".folio." + side);
    const x = side === "left" ? outer : outer + colW + gap;
    Object.assign(head.style, { left: x + "px", width: colW + "px", top: "26px" });
    Object.assign(folio.style, { left: x + "px", width: colW + "px", bottom: "24px" });
  }
  const step = width + gap;
  const end = reading.querySelector(".end");
  const x = end ? end.getBoundingClientRect().left - reading.getBoundingClientRect().left : 0;
  state.book2.step = step;
  state.book2.colStep = colW + gap;
  state.book2.spreads = Math.max(1, Math.floor((x - 1) / step) + 1);
  setSpread(Math.min(state.book2.spread, state.book2.spreads - 1), { save: false });
}

function setSpread(k, { save = true } = {}) {
  const reading = document.querySelector(".book-stage .reading");
  if (!reading) return;
  const b = state.book2;
  b.spread = Math.max(0, Math.min(k, b.spreads - 1));
  reading.style.transform = `translateX(${-b.spread * b.step}px)`;
  // Pages actually used: the last spread may hold a single page.
  const end = reading.querySelector(".end");
  const lastX = end ? end.offsetLeft : 0;
  const pages = Math.max(1, Math.floor(lastX / (b.colStep || 1)) + 1);
  const left = b.spread * 2 + 1;
  $(".folio.left").textContent = left <= pages ? left : "";
  $(".folio.right").textContent = left + 1 <= pages ? left + 1 : "";
  $(".sheet.right").style.opacity = left + 1 <= pages ? "" : "0.55";
  // Measure once the page-turn transition has moved the text.
  clearTimeout(b.settle);
  b.settle = setTimeout(() => {
    if (save && state.tts === "stopped" && isBook()) savePosition(visiblePosition());
    updateBookmarkButton();
  }, 360);
  renderPlayer();
}

function turnPage(dir) {
  if (!isBook()) return loadChapter(state.chapter + dir);
  const b = state.book2;
  const next = b.spread + dir;
  if (next >= 0 && next < b.spreads) return setSpread(next);
  if (dir > 0) loadChapter(state.chapter + 1, { scroll: "top" });
  else if (state.chapter > 0) loadChapter(state.chapter - 1, { scroll: "end" });
}

// Bring a DOM node or range into view.
function reveal(target) {
  if (!target) return;
  if (isBook()) {
    const reading = document.querySelector(".book-stage .reading");
    const rect = target.getClientRects?.()[0] || target.getBoundingClientRect();
    const x = rect.left - reading.getBoundingClientRect().left + 1;
    setSpread(Math.floor(x / state.book2.step), { save: false });
    return;
  }
  const content = $("#content");
  const box = content.getBoundingClientRect();
  const r = target.getBoundingClientRect();
  if (r.top < box.top + 30 || r.bottom > box.bottom - 30) content.scrollTop += r.top - box.top - box.height / 3;
}

function inView(node) {
  const box = (isBook() ? document.querySelector(".viewport") : $("#content")).getBoundingClientRect();
  const r = node.getBoundingClientRect();
  return r.left >= box.left - 2 && r.right <= box.right + 2 && r.top >= box.top + 30 && r.bottom <= box.bottom - 40;
}

// ---------------------------------------------------------- position

// Fraction of the book's words before `pos`.
function progressOf(pos) {
  const toc = state.book.toc;
  const words = toc.reduce((n, c) => n + c.words, 0) || 1;
  let before = toc.slice(0, pos.chapter).reduce((n, c) => n + c.words, 0);
  const paragraphs = toc[pos.chapter]?.paragraphs || state.paragraphs.length;
  if (paragraphs) before += (toc[pos.chapter]?.words || 0) * (pos.paragraph / paragraphs);
  return Math.min(1, before / words);
}

function renderStatus() {
  const b = state.book;
  $("#st-book").textContent = b ? b.entry.title : "";
  $("#st-chapter").textContent = b ? `Chapter ${state.chapter + 1} of ${b.toc.length}` : "";
  $("#st-progress").textContent = b ? Math.round(progressOf(b.entry.position) * 100) + "%" : "";
  $("#st-notes").textContent = b ? `${state.notes.length} note${state.notes.length === 1 ? "" : "s"}` : "";
  $("#notes-badge").textContent = b && state.notes.length ? state.notes.length : "";
  const play = $("#st-play");
  const label = { loading: "Preparing…", playing: "Pause", paused: "Resume", stopped: "Read aloud" }[state.tts];
  play.innerHTML = icon(state.tts === "playing" ? "pause" : "play") + label;
  play.classList.toggle("playing", state.tts === "playing");
  play.disabled = !b;
  $("#st-stop").style.display = state.tts === "stopped" ? "none" : "";
  $("#st-voice").textContent = voiceName();
  $("#st-speed").textContent = formatSpeed(state.settings?.speed || 1);
  renderPlayer();
}

function voiceName() {
  if (state.voiceLabel) return state.voiceLabel;
  const c = state.current;
  if (c && state.models) {
    const m = state.models.find((x) => x.id === c.model);
    if (m) return m.name;
  }
  return "";
}

// The read-aloud bar under the text (margin and book layouts).
function renderPlayer() {
  if (!state.settings) return;
  const b = state.book;
  const playBtn = $("#pl-play");
  const playing = state.tts === "playing";
  playBtn.innerHTML = icon(playing ? "pause" : "play");
  playBtn.setAttribute("aria-label", playing ? "Pause" : state.tts === "paused" ? "Resume" : "Read aloud");
  playBtn.classList.toggle("loading", state.tts === "loading");
  playBtn.disabled = !b;
  $("#pl-prev").disabled = $("#pl-next").disabled = !b;
  $("#pl-stop").hidden = state.tts === "stopped";
  const line = $("#pl-line");
  const pos = state.tts !== "stopped" && state.speaking?.chapter === state.chapter ? state.speaking
    : state.cursor?.chapter === state.chapter ? state.cursor : null;
  const sentence = pos && state.paragraphs[pos.paragraph]?.[pos.sentence];
  if (!b) {
    line.textContent = "Open a book to read it aloud.";
  } else if (sentence) {
    line.textContent = sentence;
  } else {
    line.textContent = "Press Space, or double-click a sentence, to listen from there.";
  }
  line.classList.toggle("idle", !sentence);
  const paragraph = pos ? pos.paragraph : b ? (state.book.entry.position.chapter === state.chapter ? state.book.entry.position.paragraph : 0) : 0;
  const n = state.paragraphs.length || 1;
  const f = b ? Math.min(1, paragraph / n) : 0;
  $("#pl-track .fill").style.width = f * 100 + "%";
  $("#pl-track .knob").style.left = f * 100 + "%";
  $("#pl-track").setAttribute("aria-valuenow", String(Math.round(f * 100)));
  $("#pl-where").textContent = b ? `Chapter ${state.chapter + 1} of ${b.toc.length}` : "";
  const words = b ? (b.toc[state.chapter]?.words || 0) * (1 - f) : 0;
  $("#pl-left").textContent = b && words ? minutes(words) + " left" : "";
  const speed = state.settings.speed;
  const seg = $("#pl-speed");
  if (seg.childElementCount !== QUICK_SPEEDS.length) {
    seg.replaceChildren(...QUICK_SPEEDS.map((v) => {
      const btn = el(`<button role="radio" data-speed="${v}">${formatSpeed(v)}</button>`);
      btn.onclick = () => setSpeed(v);
      return btn;
    }));
  }
  seg.querySelectorAll("button").forEach((btn) =>
    btn.setAttribute("aria-checked", String(Math.abs(Number(btn.dataset.speed) - speed) < 0.01)));
  const vb = $("#pl-voice span");
  vb.textContent = voiceName() || "Choose a voice";
}

// Where the reader currently is: the first paragraph in view.
function visiblePosition() {
  if (isBook()) {
    const box = document.querySelector(".viewport").getBoundingClientRect();
    for (const p of document.querySelectorAll(".reading p[data-p]")) {
      for (const r of p.getClientRects()) {
        if (r.right > box.left + 2 && r.left < box.right - 2 && r.bottom > box.top) {
          return { chapter: state.chapter, paragraph: Number(p.dataset.p), sentence: 0 };
        }
      }
    }
    return { chapter: state.chapter, paragraph: 0, sentence: 0 };
  }
  const content = $("#content");
  const top = content.getBoundingClientRect().top;
  for (const p of content.querySelectorAll("p[data-p]")) {
    if (p.getBoundingClientRect().bottom > top + 8) {
      return { chapter: state.chapter, paragraph: Number(p.dataset.p), sentence: 0 };
    }
  }
  return { chapter: state.chapter, paragraph: 0, sentence: 0 };
}

function savePosition(position) {
  if (!state.book) return;
  state.book.entry.position = position;
  const progress = progressOf(position);
  const entry = state.library.find((b) => b.id === state.book.entry.id);
  if (entry) {
    entry.position = position;
    entry.progress = progress;
  }
  $("#st-progress").textContent = Math.round(progress * 100) + "%";
  if (state.view === "chapters" && state.settings.toc_mode === "map") renderMapRows();
  renderPlayer();
  invoke("position_save", { id: state.book.entry.id, position, progress }).catch(() => {});
}

const onScroll = debounce(() => {
  if (state.book && state.tts === "stopped" && !isBook()) savePosition(visiblePosition());
  updateBookmarkButton();
}, 300);

// --------------------------------------------------------------- speech

function currentStart() {
  if (state.cursor && state.cursor.chapter === state.chapter) return state.cursor;
  if (state.speaking && state.speaking.chapter === state.chapter) return state.speaking;
  return visiblePosition();
}

async function play(position) {
  if (!state.book) return;
  state.tts = "loading";
  renderStatus();
  await call("tts_play", { id: state.book.entry.id, position });
}

function togglePlay() {
  if (!state.book) return;
  if (state.tts === "playing") invoke("tts_pause");
  else if (state.tts === "paused") invoke("tts_resume");
  else if (state.tts === "stopped") play(currentStart());
}

function stopSpeech() {
  if (state.tts !== "stopped") invoke("tts_stop");
  state.tts = "stopped";
  clearHighlight();
  renderStatus();
}

// The sentence `dir` steps away from `pos` in this chapter.
function stepSentence(pos, dir) {
  let { paragraph, sentence } = pos;
  sentence += dir;
  while (paragraph >= 0 && paragraph < state.paragraphs.length) {
    const n = state.paragraphs[paragraph].length;
    if (sentence >= 0 && sentence < n) return { chapter: state.chapter, paragraph, sentence };
    paragraph += dir;
    sentence = dir > 0 ? 0 : (state.paragraphs[paragraph]?.length || 1) - 1;
  }
  return null;
}

function skipSentence(dir) {
  if (!state.book) return;
  const from = state.speaking?.chapter === state.chapter && state.tts !== "stopped" ? state.speaking : currentStart();
  const to = stepSentence(from, dir);
  if (!to) return;
  if (state.tts === "playing" || state.tts === "paused" || state.tts === "loading") {
    play(to);
  } else {
    setCursor(to);
  }
}

function setCursor(pos) {
  state.cursor = pos;
  document.querySelectorAll(".s.cursor").forEach((n) => n.classList.remove("cursor"));
  const span = document.querySelector(`p[data-p="${pos.paragraph}"] .s[data-s="${pos.sentence}"]`);
  if (span) {
    span.classList.add("cursor");
    if (!inView(span)) reveal(span);
  }
  renderPlayer();
}

function clearHighlight() {
  document.querySelectorAll(".s.speaking").forEach((n) => n.classList.remove("speaking"));
}

function highlight(pos, follow = true) {
  clearHighlight();
  const span = document.querySelector(`p[data-p="${pos.paragraph}"] .s[data-s="${pos.sentence}"]`);
  if (!span) return;
  span.classList.add("speaking");
  if (!follow || inView(span)) return;
  if (isBook()) reveal(span);
  else span.scrollIntoView({ block: "center", behavior: "smooth" });
}

async function onSpeechPosition(pos) {
  if (!state.book || pos.book_id !== state.book.entry.id) return;
  state.speaking = { chapter: pos.chapter, paragraph: pos.paragraph, sentence: pos.sentence };
  if (pos.chapter !== state.chapter) {
    state.cursor = null;
    await loadChapter(pos.chapter, { scroll: "none" });
  }
  highlight(state.speaking);
  savePosition(state.speaking);
}

function onSpeechState(ev) {
  state.tts = ev.state;
  if (ev.voice) state.voiceLabel = ev.voice;
  if (ev.state === "stopped") clearHighlight();
  if (ev.error === "NO_VOICE") {
    const lang = state.book?.entry.language || "en";
    toast(`No voice for this book's language (${lang}) yet. Download one to read aloud.`, {
      actions: [{ label: "Open Voices", run: openVoices }],
    });
  } else if (ev.error) {
    toast("Read aloud stopped: " + ev.error, { error: true });
  }
  renderStatus();
}

function openVoices() {
  state.voiceLang = null;
  state.models = null;
  setViewIfHidden("voices");
  refreshEngine();
}

function setSpeed(v) {
  updateSetting("speed", v);
  if (state.view === "settings") renderSidebar();
  renderStatus();
  // Speed is applied when synthesizing, so restart from the current sentence.
  if ((state.tts === "playing" || state.tts === "paused") && state.speaking) play(state.speaking);
}

function changeSpeed() {
  const s = state.settings.speed;
  setSpeed(SPEEDS.find((v) => v > s + 0.01) ?? SPEEDS[0]);
}

// ------------------------------------------------------------- palette

function openPalette(prefill = "") {
  const p = $("#palette");
  p.hidden = false;
  const input = $("#palette-input");
  input.value = prefill;
  renderPalette();
  input.focus();
  input.select();
}

function closePalette() {
  $("#palette").hidden = true;
}

function paletteItems(q) {
  const items = [];
  const fq = fold(q.trim());
  if (state.book) {
    if (fq) {
      items.push({ group: "Search", icon: "search", label: `Search for “${q.trim()}” in this book`, run: () => searchFor(q.trim()) });
    }
    const chapters = state.book.toc
      .map((c, i) => ({ c, i }))
      .filter(({ c, i }) => !fq || fold(c.title).includes(fq) || String(i + 1) === fq);
    const near = fq ? chapters : chapters.slice(Math.max(0, state.chapter - 2), state.chapter + 6);
    for (const { c, i } of near.slice(0, 8)) {
      items.push({ group: "Chapters", icon: "toc", label: c.title, meta: `${i + 1}${c.words ? " · " + minutes(c.words) : ""}`, run: () => loadChapter(i, { scroll: "top" }) });
    }
  }
  const commands = [
    { label: "Layout: Margin, notes beside the text", icon: "margin", run: () => setLayout("margin") },
    { label: "Layout: Book, two-page spread", icon: "book", run: () => setLayout("book") },
    { label: "Layout: Workbench, tabs and panels", icon: "panels", run: () => setLayout("workbench") },
    ...THEMES.map(([t, name]) => ({ label: `Theme: ${name}`, icon: "settings", run: () => setTheme(t) })),
    { label: "Book map", icon: "map", run: () => { updateSetting("toc_mode", "map"); setViewIfHidden("chapters"); } },
    { label: "Notes and bookmarks panel", icon: "note", run: toggleNotesPanel },
    { label: "Replace text in this book", icon: "replace", run: () => openReplace() },
    { label: "Bookmark this place", icon: "bookmark", run: toggleBookmark },
    { label: "Open a book…", icon: "plus", run: pickBooks },
    { label: "Library", icon: "library", run: () => setViewIfHidden("library") },
    { label: "Voices", icon: "voices", run: openVoices },
    { label: "Settings", icon: "settings", run: () => setViewIfHidden("settings") },
  ];
  const matching = commands.filter((c) => !fq || fold(c.label).includes(fq)).map((c) => ({ group: "Commands", ...c }));
  // A query naming a command ("night", "book") runs it on Enter.
  if (fq && matching.length) return [...matching, ...items];
  return [...items, ...matching];
}

function renderPalette() {
  const q = $("#palette-input").value;
  const items = paletteItems(q);
  state.palette = { items, active: 0 };
  const list = $("#palette-list");
  const frag = document.createDocumentFragment();
  let group = "";
  items.forEach((it, i) => {
    if (it.group !== group) {
      group = it.group;
      frag.append(el(`<div class="pal-group">${esc(group)}</div>`));
    }
    const node = el(`<div class="pal-item${i === 0 ? " active" : ""}" role="option" data-i="${i}">${icon(it.icon)}
      <span class="label">${esc(it.label)}</span>${it.meta ? `<span class="meta">${esc(it.meta)}</span>` : ""}</div>`);
    node.onmousemove = () => setPaletteActive(i);
    node.onclick = () => runPalette(i);
    frag.append(node);
  });
  if (!items.length) frag.append(el(`<div class="hint">Nothing matches.</div>`));
  list.replaceChildren(frag);
}

function setPaletteActive(i) {
  const n = state.palette.items.length;
  if (!n) return;
  state.palette.active = ((i % n) + n) % n;
  document.querySelectorAll(".pal-item").forEach((x) => x.classList.toggle("active", Number(x.dataset.i) === state.palette.active));
  document.querySelector(".pal-item.active")?.scrollIntoView({ block: "nearest" });
}

function runPalette(i) {
  const it = state.palette.items[i];
  closePalette();
  it?.run();
}

function searchFor(q) {
  state.search.query = q;
  if (state.settings.toc_mode === "map") setViewIfHidden("chapters");
  else setViewIfHidden("search");
  runSearch();
}

// --------------------------------------------------------------- wiring

function wire() {
  document.querySelectorAll(".activity").forEach((b) => { b.onclick = () => setView(b.dataset.view); });
  $("#btn-prev").onclick = () => loadChapter(state.chapter - 1);
  $("#btn-next").onclick = () => loadChapter(state.chapter + 1);
  document.querySelectorAll("#layout-switch button").forEach((b) => { b.onclick = () => setLayout(b.dataset.layout); });
  $("#btn-palette").onclick = () => openPalette();
  $("#st-play").onclick = togglePlay;
  $("#st-stop").onclick = stopSpeech;
  $("#st-speed").onclick = changeSpeed;
  $("#st-voice").onclick = openVoices;
  $("#st-book").onclick = () => setViewIfHidden("library");
  $("#pl-play").onclick = togglePlay;
  $("#pl-stop").onclick = stopSpeech;
  $("#pl-prev").onclick = () => skipSentence(-1);
  $("#pl-next").onclick = () => skipSentence(1);
  $("#pl-voice").onclick = openVoices;
  const track = $("#pl-track");
  const seekTo = (f) => {
    if (!state.book || !state.paragraphs.length) return;
    const paragraph = Math.min(state.paragraphs.length - 1, Math.floor(f * state.paragraphs.length));
    const pos = { chapter: state.chapter, paragraph, sentence: 0 };
    if (state.tts !== "stopped") play(pos);
    else {
      setCursor(pos);
      reveal(paragraphEl(paragraph));
      savePosition(pos);
    }
  };
  track.onclick = (e) => {
    const r = track.getBoundingClientRect();
    seekTo((e.clientX - r.left) / r.width);
  };
  track.onkeydown = (e) => {
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      e.stopPropagation();
      skipSentence(e.key === "ArrowRight" ? 1 : -1);
    }
  };

  const content = $("#content");
  content.addEventListener("scroll", onScroll, { passive: true });
  // In the book layout the wheel turns pages.
  let wheelAt = 0;
  content.addEventListener("wheel", (e) => {
    if (!isBook()) return;
    e.preventDefault();
    const d = Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
    if (Math.abs(d) < 4 || Date.now() - wheelAt < 380) return;
    wheelAt = Date.now();
    turnPage(d > 0 ? 1 : -1);
  }, { passive: false });
  new ResizeObserver(relayoutSoon).observe(content);
  document.fonts?.ready.then(relayoutSoon);

  wireNotes();

  // Click a sentence to place the read-aloud cursor; double-click to start there.
  const sentenceAt = (target) => {
    const s = target.closest?.(".s");
    if (!s) return null;
    return { chapter: state.chapter, paragraph: Number(s.parentElement.dataset.p), sentence: Number(s.dataset.s) };
  };
  content.addEventListener("click", (e) => {
    if (e.target.closest(".gutter")) return;
    const clickedNote = noteAtPoint(e.clientX, e.clientY);
    if (clickedNote && !window.getSelection().toString()) openNoteEditor(clickedNote);
    const pos = sentenceAt(e.target);
    if (!pos || window.getSelection().toString()) return;
    document.querySelectorAll(".s.cursor").forEach((n) => n.classList.remove("cursor"));
    e.target.closest(".s").classList.add("cursor");
    state.cursor = pos;
    renderPlayer();
  });
  content.addEventListener("dblclick", (e) => {
    const pos = sentenceAt(e.target);
    if (!pos) return;
    window.getSelection().removeAllRanges();
    state.cursor = pos;
    play(pos);
  });

  // Palette.
  const pin = $("#palette-input");
  pin.oninput = renderPalette;
  pin.onkeydown = (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); setPaletteActive(state.palette.active + 1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setPaletteActive(state.palette.active - 1); }
    else if (e.key === "Enter") { e.preventDefault(); runPalette(state.palette.active); }
    else if (e.key === "Escape") { e.preventDefault(); closePalette(); }
  };
  document.addEventListener("mousedown", (e) => {
    if (!$("#palette").hidden && !e.target.closest("#palette")) closePalette();
  });

  // Sidebar resizing.
  const sash = $("#sash");
  sash.addEventListener("mousedown", (e) => {
    e.preventDefault();
    sash.classList.add("dragging");
    const startX = e.clientX;
    const startW = $("#sidebar").offsetWidth;
    const move = (ev) => {
      const w = Math.max(200, Math.min(window.innerWidth * 0.6, startW + ev.clientX - startX));
      $("#sidebar").style.width = w + "px";
    };
    const up = () => {
      sash.classList.remove("dragging");
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      updateSetting("sidebar_width", $("#sidebar").offsetWidth);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });

  document.addEventListener("keydown", (e) => {
    const typing = /^(INPUT|SELECT|TEXTAREA)$/.test(e.target.tagName);
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "Escape" && !$("#note-editor").hidden) { e.preventDefault(); closeNoteEditor(); return; }
    if (mod && e.key.toLowerCase() === "k") { e.preventDefault(); openPalette(); return; }
    if (mod && e.shiftKey && e.key.toLowerCase() === "m") { e.preventDefault(); toggleNotesPanel(); return; }
    if (mod && e.key.toLowerCase() === "f") { e.preventDefault(); openSearch(); return; }
    if (mod && e.key.toLowerCase() === "d") { e.preventDefault(); toggleBookmark(); return; }
    if (e.key === "F3" || (mod && e.key.toLowerCase() === "g")) { e.preventDefault(); nextHit(e.shiftKey ? -1 : 1); return; }
    if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); document.body.classList.toggle("sidebar-hidden"); renderSidebar(); relayoutSoon(); }
    else if (mod && e.key.toLowerCase() === "o") { e.preventDefault(); pickBooks(); }
    else if (mod && e.key === "1") { e.preventDefault(); setViewIfHidden("chapters"); }
    else if (mod && e.key === "2") { e.preventDefault(); setViewIfHidden("library"); }
    else if (mod && e.key === "3") { e.preventDefault(); openVoices(); }
    else if (mod && e.key === ",") { e.preventDefault(); setViewIfHidden("settings"); }
    else if (mod && (e.key === "=" || e.key === "+")) { e.preventDefault(); updateSetting("font_size", Math.min(36, state.settings.font_size + 1)); }
    else if (mod && e.key === "-") { e.preventDefault(); updateSetting("font_size", Math.max(12, state.settings.font_size - 1)); }
    else if (typing || mod || e.altKey || !$("#palette").hidden) return;
    else if (e.key === " ") { e.preventDefault(); togglePlay(); }
    else if (e.key === "ArrowLeft") turnPage(-1);
    else if (e.key === "ArrowRight") turnPage(1);
    else if (isBook() && (e.key === "PageDown" || e.key === "PageUp")) { e.preventDefault(); turnPage(e.key === "PageDown" ? 1 : -1); }
    else if (e.key === "Escape") stopSpeech();
  });
}

async function main() {
  wire();
  state.settings = await invoke("settings_get");
  applySettings();
  await refreshLibrary();

  listen("tts-position", (e) => onSpeechPosition(e.payload));
  listen("tts-state", (e) => onSpeechState(e.payload));
  listen("download-progress", (e) => {
    const { key, done, total } = e.payload;
    if (!state.downloads[key]) return;
    Object.assign(state.downloads[key], { done, total });
    if (state.view === "voices") renderSidebar();
    renderDownloads();
  });
  listen("tauri://drag-enter", () => document.body.classList.add("dragging"));
  listen("tauri://drag-leave", () => document.body.classList.remove("dragging"));
  listen("tauri://drag-drop", (e) => {
    document.body.classList.remove("dragging");
    const paths = (e.payload.paths || []).filter((p) => BOOK_FILE.test(p));
    if (paths.length) addPaths(paths);
    else toast("Marginalia opens EPUB, MOBI, AZW, PRC, FB2, TXT, Markdown and PDF files.", { error: true });
  });

  // Reopen the last book where it was left.
  const last = state.library.find((b) => b.last_opened);
  if (last) {
    try {
      await openBook(last.id);
    } catch {
      renderContent();
    }
  } else {
    renderContent();
    state.view = "library";
  }
  renderSidebar();
  renderNotesPanel();
  renderStatus();
}


// ---------------------------------------------------------------- notes

const HAS_HIGHLIGHTS = typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight !== "undefined";
const NOTE_COLORS = ["amber", "green", "blue", "rose"];

function paragraphEl(i) {
  return document.querySelector(`.reading p[data-p="${i}"]`);
}

// Character offset of (node, offset) inside paragraph `p`'s text.
function textOffset(p, node, offset) {
  const r = document.createRange();
  r.setStart(p, 0);
  r.setEnd(node, offset);
  return r.toString().length;
}

// DOM position of character `offset` inside paragraph `p`.
function domPoint(p, offset) {
  const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
  let left = offset;
  let last = null;
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (left <= n.length) return [n, left];
    left -= n.length;
    last = n;
  }
  return last ? [last, last.length] : [p, 0];
}

// A DOM Range for a note in the current chapter, or null.
function noteRange(note) {
  const a = note.anchor;
  if (a.chapter !== state.chapter || a.start == null) return null;
  const p1 = paragraphEl(a.paragraph);
  const p2 = paragraphEl(a.end_paragraph ?? a.paragraph);
  if (!p1 || !p2) return null;
  let start = a.start;
  let end = a.end;
  // If the text moved, find the quote again within the paragraph.
  if (p1 === p2 && p1.textContent.slice(start, end) !== a.quote) {
    const at = p1.textContent.indexOf(a.quote);
    if (at < 0) return null;
    start = at;
    end = at + a.quote.length;
  }
  const r = document.createRange();
  r.setStart(...domPoint(p1, start));
  r.setEnd(...domPoint(p2, end));
  return r;
}

function applyNoteMarks() {
  if (HAS_HIGHLIGHTS) for (const c of NOTE_COLORS) CSS.highlights.delete("note-" + c);
  document.querySelectorAll(".reading .gutter.mark").forEach((n) => n.remove());
  document.querySelectorAll(".reading p.noted").forEach((p) => {
    p.classList.remove("noted", "whole");
    p.removeAttribute("data-color");
  });
  if (!state.book) return;
  const ranges = Object.fromEntries(NOTE_COLORS.map((c) => [c, []]));
  const byParagraph = new Map();
  for (const note of state.notes) {
    if (note.anchor.chapter !== state.chapter) continue;
    const list = byParagraph.get(note.anchor.paragraph) || [];
    list.push(note);
    byParagraph.set(note.anchor.paragraph, list);
    const r = noteRange(note);
    if (r) (ranges[note.color] || ranges.amber).push(r);
  }
  if (HAS_HIGHLIGHTS) {
    for (const c of NOTE_COLORS) {
      if (ranges[c].length) CSS.highlights.set("note-" + c, new Highlight(...ranges[c]));
    }
  }
  for (const [pi, notes] of byParagraph) {
    const p = paragraphEl(pi);
    if (!p) continue;
    p.classList.add("noted");
    if (notes.some((n) => n.anchor.start == null)) p.classList.add("whole");
    p.dataset.color = notes[0].color;
    const mark = el(`<span class="gutter mark" data-color="${esc(notes[0].color)}" data-count="${notes.length}"
      title="${notes.length} note${notes.length > 1 ? "s" : ""}"></span>`);
    mark.onclick = (e) => {
      e.stopPropagation();
      if (notes.length === 1) openNoteEditor(notes[0], mark);
      else {
        setNotesOpen(true);
        renderNotesPanel(notes.map((n) => n.id));
      }
    };
    p.prepend(mark);
  }
  if (!isBook()) layoutMargin();
  if (state.view === "chapters") {
    if (state.settings.toc_mode === "map") renderMapRows();
  }
}

// The note whose highlighted passage is under the pointer, if any.
function noteAtPoint(x, y) {
  let node;
  let offset;
  if (document.caretPositionFromPoint) {
    const c = document.caretPositionFromPoint(x, y);
    if (!c) return null;
    [node, offset] = [c.offsetNode, c.offset];
  } else if (document.caretRangeFromPoint) {
    const c = document.caretRangeFromPoint(x, y);
    if (!c) return null;
    [node, offset] = [c.startContainer, c.startOffset];
  } else return null;
  for (const note of state.notes) {
    const r = noteRange(note);
    try {
      if (r && r.isPointInRange(node, offset) && !(r.endContainer === node && r.endOffset === offset)) return note;
    } catch { /* node outside the document */ }
  }
  return null;
}

// Anchor for the current selection inside the reading view, or null.
function selectionAnchor() {
  const sel = window.getSelection();
  if (!sel.rangeCount || sel.isCollapsed) return null;
  const r = sel.getRangeAt(0);
  const p1 = (r.startContainer.nodeType === 1 ? r.startContainer : r.startContainer.parentElement)?.closest(".reading p[data-p]");
  const p2 = (r.endContainer.nodeType === 1 ? r.endContainer : r.endContainer.parentElement)?.closest(".reading p[data-p]");
  if (!p1 || !p2) return null;
  const quote = r.toString();
  if (!quote.trim()) return null;
  const start = textOffset(p1, r.startContainer, r.startOffset);
  const end = textOffset(p2, r.endContainer, r.endOffset);
  const paragraph = Number(p1.dataset.p);
  const endParagraph = Number(p2.dataset.p);
  return {
    chapter: state.chapter,
    paragraph,
    start,
    end_paragraph: endParagraph !== paragraph ? endParagraph : null,
    end,
    quote,
  };
}

function paragraphAnchor(p) {
  const text = p.textContent.trim();
  return {
    chapter: state.chapter,
    paragraph: Number(p.dataset.p),
    start: null,
    end_paragraph: null,
    end: null,
    quote: text.length > 280 ? text.slice(0, 280) + "…" : text,
  };
}

function openNoteEditor(note, near) {
  state.editing = note;
  $("#sel-tools").hidden = true;
  const ed = $("#note-editor");
  const quote = note.anchor.quote.trim();
  ed.querySelector(".quote").textContent = quote || "(paragraph)";
  ed.querySelector("textarea").value = note.text || "";
  ed.querySelector('[data-act="delete"]').hidden = !note.id;
  setEditorColor(note.color || "amber");
  ed.hidden = false;
  // Place it next to what it annotates, kept inside the window.
  let rect = near?.getBoundingClientRect?.() || noteRange(note)?.getBoundingClientRect()
    || paragraphEl(note.anchor.paragraph)?.getBoundingClientRect();
  if (!rect || (!rect.width && !rect.height)) {
    const c = $("#content").getBoundingClientRect();
    rect = { left: c.left + c.width / 2 - 190, bottom: c.top + 80, top: c.top + 80 };
  }
  const w = ed.offsetWidth;
  const h = ed.offsetHeight;
  const left = Math.max(12, Math.min(window.innerWidth - w - 12, rect.left));
  let top = rect.bottom + 8;
  if (top + h > window.innerHeight - 30) top = Math.max(12, rect.top - h - 8);
  ed.style.left = left + "px";
  ed.style.top = top + "px";
  ed.querySelector("textarea").focus();
}

function setEditorColor(color) {
  const ed = $("#note-editor");
  ed.dataset.color = color;
  ed.querySelector(".quote").dataset.color = color;
  ed.querySelectorAll(".colors button").forEach((b) => {
    b.classList.toggle("on", b.dataset.color === color);
    b.setAttribute("aria-checked", String(b.dataset.color === color));
  });
}

function closeNoteEditor() {
  state.editing = null;
  const ed = $("#note-editor");
  if (ed) ed.hidden = true;
}

async function saveNote(note) {
  const saved = await call("note_save", { id: state.book.entry.id, note });
  state.notes = state.notes.filter((n) => n.id !== saved.id).concat(saved).sort(noteOrder);
  applyNoteMarks();
  renderNotesPanel([saved.id]);
  renderStatus();
  if (state.view === "chapters" && state.settings.toc_mode !== "map") renderSidebar();
  return saved;
}

async function saveEditingNote() {
  const ed = $("#note-editor");
  const note = {
    ...state.editing,
    text: ed.querySelector("textarea").value,
    color: ed.dataset.color || "amber",
  };
  if (!note.text.trim() && !note.anchor.quote.trim()) return closeNoteEditor();
  closeNoteEditor();
  window.getSelection().removeAllRanges();
  await saveNote(note);
}

async function deleteNote(note) {
  await call("note_delete", { id: state.book.entry.id, noteId: note.id });
  state.notes = state.notes.filter((n) => n.id !== note.id);
  if (state.editing?.id === note.id) closeNoteEditor();
  applyNoteMarks();
  renderNotesPanel();
  renderStatus();
}

function noteOrder(a, b) {
  const x = a.anchor;
  const y = b.anchor;
  return x.chapter - y.chapter || x.paragraph - y.paragraph || (x.start ?? 0) - (y.start ?? 0) || a.created - b.created;
}

function setNotesOpen(open) {
  updateSetting("notes_open", open);
  if (open) renderNotesPanel();
}

function toggleNotesPanel() {
  setNotesOpen(!state.settings.notes_open);
}

function renderNotesPanel(activeIds = []) {
  const list = $("#notes-list");
  if (!list) return;
  $("#notes-count").textContent = state.notes.length || "";
  $("#bookmarks-count").textContent = state.bookmarks.length || "";
  document.querySelectorAll(".panel-tab").forEach((t) => {
    t.classList.toggle("active", t.dataset.tab === state.panelTab);
    t.setAttribute("aria-selected", String(t.dataset.tab === state.panelTab));
  });
  $("#notes-export").hidden = state.panelTab !== "notes";
  $("#notes-export").disabled = !state.notes.length;
  $("#notes-filter-box").hidden = state.panelTab === "replace";
  const active = state.replace.book.filter((r) => r.enabled).length + state.replace.all.filter((r) => r.enabled).length;
  $("#replace-count").textContent = active || "";
  if (state.panelTab === "bookmarks") return renderBookmarks(list);
  if (state.panelTab === "replace") return renderReplace(list);
  list.replaceChildren();
  if (!state.book) {
    list.append(el(`<div class="hint">Open a book to see its notes.</div>`));
    return;
  }
  const q = fold(state.notesFilter.trim());
  const notes = state.notes.filter((n) => !q || fold(n.text + " " + n.anchor.quote).includes(q));
  if (!state.notes.length) {
    list.append(el(`<div class="hint">No notes yet.<br><br>Select some text and choose <b>Add note</b>,
      or click the <b>+</b> left of a paragraph.</div>`));
    return;
  }
  if (!notes.length) list.append(el(`<div class="hint">No notes match.</div>`));
  let chapter = -1;
  let firstActive = null;
  for (const note of notes) {
    if (note.anchor.chapter !== chapter) {
      chapter = note.anchor.chapter;
      list.append(el(`<div class="note-group">${esc(state.book.toc[chapter]?.title || `Chapter ${chapter + 1}`)}</div>`));
    }
    const quote = note.anchor.quote.trim();
    const card = el(`<div class="note-card${activeIds.includes(note.id) ? " active" : ""}" data-color="${esc(note.color)}" tabindex="0">
      <div class="where">${esc(new Date(note.updated * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" }))}</div>
      ${quote ? `<div class="q">${esc(quote)}</div>` : ""}
      ${note.text.trim() ? `<div class="t">${esc(note.text.trim())}</div>` : ""}
      <div class="actions"><button class="icon-btn small" aria-label="Edit note" title="Edit">${icon("edit")}</button><button class="icon-btn small" aria-label="Delete note" title="Delete">${icon("close")}</button></div>
    </div>`);
    card.onclick = () => goToNote(note);
    card.onkeydown = (e) => { if (e.key === "Enter") goToNote(note); };
    const [edit, del] = card.querySelectorAll(".actions button");
    edit.onclick = async (e) => {
      e.stopPropagation();
      await goToNote(note);
      openNoteEditor(note);
    };
    del.onclick = (e) => {
      e.stopPropagation();
      deleteNote(note);
    };
    if (activeIds.includes(note.id) && !firstActive) firstActive = card;
    list.append(card);
  }
  firstActive?.scrollIntoView({ block: "nearest" });
}

async function goToNote(note) {
  const a = note.anchor;
  await loadChapter(a.chapter, { scroll: "none" });
  const p = paragraphEl(a.paragraph);
  if (!p) return;
  const range = noteRange(note);
  reveal(range || p);
  if (range && HAS_HIGHLIGHTS) {
    CSS.highlights.set("note-flash", new Highlight(range));
    setTimeout(() => CSS.highlights.delete("note-flash"), 1200);
  } else {
    p.classList.remove("flash");
    void p.offsetWidth;
    p.classList.add("flash");
  }
}

function sentenceOfSelection() {
  const sel = window.getSelection();
  if (!sel.rangeCount) return null;
  const node = sel.getRangeAt(0).startContainer;
  const s = (node.nodeType === 1 ? node : node.parentElement)?.closest(".s");
  if (!s) return null;
  return { chapter: state.chapter, paragraph: Number(s.parentElement.dataset.p), sentence: Number(s.dataset.s) };
}

function wireNotes() {
  const content = $("#content");
  const tools = $("#sel-tools");
  const showSelectionTools = () => {
    const anchor = state.book ? selectionAnchor() : null;
    state.pendingAnchor = anchor;
    if (!anchor) {
      tools.hidden = true;
      return;
    }
    const rects = window.getSelection().getRangeAt(0).getClientRects();
    const first = rects[0];
    const last = rects[rects.length - 1];
    if (!first) return;
    tools.hidden = false;
    const w = tools.offsetWidth;
    const box = content.getBoundingClientRect();
    let top = first.top - tools.offsetHeight - 8;
    if (top < box.top + 4) top = last.bottom + 8;
    tools.style.left = Math.max(8, Math.min(window.innerWidth - w - 8, first.left)) + "px";
    tools.style.top = top + "px";
  };
  content.addEventListener("mouseup", () => setTimeout(showSelectionTools, 0));
  content.addEventListener("keyup", (e) => e.shiftKey && showSelectionTools());
  content.addEventListener("scroll", () => { tools.hidden = true; }, { passive: true });
  document.addEventListener("selectionchange", () => {
    if (window.getSelection().isCollapsed) tools.hidden = true;
  });
  tools.addEventListener("mousedown", (e) => e.preventDefault()); // keep the selection
  tools.querySelectorAll("button").forEach((b) => {
    b.onclick = async () => {
      const anchor = state.pendingAnchor;
      if (!anchor) return;
      const act = b.dataset.act;
      if (act === "note") {
        openNoteEditor({ id: "", anchor, text: "", color: "amber" });
        return;
      }
      if (act === "replace") {
        tools.hidden = true;
        window.getSelection().removeAllRanges();
        openReplace(anchor.quote.trim().split(/\s+/).join(" "));
        return;
      }
      tools.hidden = true;
      if (act === "highlight") {
        window.getSelection().removeAllRanges();
        await saveNote({ id: "", anchor, text: "", color: "amber" });
      } else if (act === "read") {
        const pos = sentenceOfSelection();
        window.getSelection().removeAllRanges();
        if (pos) {
          state.cursor = pos;
          play(pos);
        }
      } else if (act === "copy") {
        try {
          await navigator.clipboard.writeText(anchor.quote);
          toast("Copied.", { timeout: 1500 });
        } catch {
          document.execCommand("copy");
        }
      }
    };
  });

  // "+" left of a paragraph: note on the whole paragraph.
  content.addEventListener("click", (e) => {
    const add = e.target.closest(".gutter.add");
    if (!add) return;
    e.stopPropagation();
    const p = add.closest("p[data-p]");
    openNoteEditor({ id: "", anchor: paragraphAnchor(p), text: "", color: "amber" }, p);
  });

  const ed = $("#note-editor");
  ed.querySelector('[data-act="save"]').onclick = saveEditingNote;
  ed.querySelector('[data-act="cancel"]').onclick = closeNoteEditor;
  ed.querySelector('[data-act="delete"]').onclick = () => state.editing && deleteNote(state.editing);
  ed.querySelectorAll(".colors button").forEach((b) => { b.onclick = () => setEditorColor(b.dataset.color); });
  ed.querySelector("textarea").addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      saveEditingNote();
    }
  });
  document.addEventListener("mousedown", (e) => {
    if (!ed.hidden && !ed.contains(e.target) && !e.target.closest(".gutter, #sel-tools, .note-card, .margin-note")) closeNoteEditor();
  });

  $("#btn-notes").onclick = toggleNotesPanel;
  $("#st-notes").onclick = toggleNotesPanel;
  $("#notes-close").onclick = () => setNotesOpen(false);
  document.querySelectorAll(".panel-tab").forEach((t) => {
    t.onclick = () => {
      state.panelTab = t.dataset.tab;
      renderNotesPanel();
    };
  });
  $("#btn-bookmark").onclick = toggleBookmark;
  $("#notes-filter").oninput = (e) => {
    state.notesFilter = e.target.value;
    renderNotesPanel();
  };
  $("#notes-export").onclick = async () => {
    if (!state.book) return;
    const path = await call("notes_export", { id: state.book.entry.id });
    if (path) toast("Notes exported to " + path);
  };
}

// -------------------------------------------------------------- replace

// Find-and-replace rules change what is shown, searched and read aloud;
// the book file is never modified.
function newRule(find = "") {
  return { find, replace: "", whole_word: true, match_case: false, enabled: true };
}

function openReplace(find) {
  if (!state.book) return;
  state.panelTab = "replace";
  if (find) {
    if (find.length > 200) find = find.slice(0, 200);
    state.replace.book.push(newRule(find));
    state.replace.dirty = true;
  }
  setNotesOpen(true);
  renderNotesPanel();
  const inputs = document.querySelectorAll('#notes-list .rule[data-list="book"] .to');
  if (find && inputs.length) inputs[inputs.length - 1]?.focus();
}

function renderReplace(list) {
  list.replaceChildren();
  if (!state.book) {
    list.append(el(`<div class="hint">Open a book to replace words in it.</div>`));
    return;
  }
  list.append(el(`<div class="hint">Change a word or name everywhere: in what you read, search and hear.
    The book file is not modified, and longer phrases win over shorter ones.</div>`));
  const section = (key, title) => {
    const rules = state.replace[key];
    list.append(el(`<div class="note-group">${esc(title)}</div>`));
    rules.forEach((r, i) => list.append(ruleRow(rules, r, i)));
    const add = el(`<button class="btn small quiet add-rule">${icon("plus")}Add rule</button>`);
    add.onclick = () => {
      rules.push(newRule());
      state.replace.dirty = true;
      renderNotesPanel();
      const rows = list.querySelectorAll(`.rule[data-list="${key}"] .from`);
      rows[rows.length - 1]?.focus();
    };
    list.append(add);
  };
  section("book", "This book");
  section("all", "All books");
  const bar = el(`<div class="replace-bar">
    <button class="btn" data-act="apply">${icon("check")}Apply</button>
    <button class="btn quiet" data-act="revert">Revert</button></div>`);
  bar.querySelector('[data-act="apply"]').disabled = !state.replace.dirty;
  bar.querySelector('[data-act="revert"]').disabled = !state.replace.dirty;
  bar.querySelector('[data-act="apply"]').onclick = applyReplacements;
  bar.querySelector('[data-act="revert"]').onclick = async () => {
    const rules = await invoke("replacements_get", { id: state.book.entry.id });
    state.replace = { book: rules.book, all: rules.all, dirty: false };
    renderNotesPanel();
  };
  list.append(bar);
}

function ruleRow(rules, r, i) {
  const key = rules === state.replace.book ? "book" : "all";
  const row = el(`<div class="rule${r.enabled ? "" : " off"}" data-list="${key}">
    <div class="line">
      <input type="checkbox" aria-label="Use this rule"${r.enabled ? " checked" : ""}>
      <input type="text" class="from" placeholder="Find" aria-label="Find" spellcheck="false">
      <button class="icon-btn small" aria-label="Delete rule" title="Delete">${icon("close")}</button>
    </div>
    <div class="line">
      <span class="arrow" aria-hidden="true">${icon("right")}</span>
      <input type="text" class="to" placeholder="Replace with (empty removes it)" aria-label="Replace with" spellcheck="false">
      <span class="seg" role="group" aria-label="Options">
        <button aria-pressed="${r.match_case}" title="Match case" aria-label="Match case">Aa</button>
        <button aria-pressed="${r.whole_word}" title="Whole word only" aria-label="Whole word only"><u>ab</u></button>
      </span>
    </div></div>`);
  const [enabled, from, del] = row.querySelectorAll(".line:first-child > *");
  const to = row.querySelector(".to");
  from.value = r.find;
  to.value = r.replace;
  const dirty = () => {
    state.replace.dirty = true;
    document.querySelectorAll(".replace-bar .btn").forEach((b) => { b.disabled = false; });
  };
  enabled.onchange = () => { r.enabled = enabled.checked; row.classList.toggle("off", !r.enabled); dirty(); };
  from.oninput = () => { r.find = from.value; dirty(); };
  to.oninput = () => { r.replace = to.value; dirty(); };
  for (const input of [from, to]) {
    input.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); applyReplacements(); } };
  }
  del.onclick = () => { rules.splice(i, 1); dirty(); renderNotesPanel(); };
  const [caseBtn, wordBtn] = row.querySelectorAll(".seg button");
  caseBtn.onclick = () => { r.match_case = !r.match_case; caseBtn.setAttribute("aria-pressed", r.match_case); dirty(); };
  wordBtn.onclick = () => { r.whole_word = !r.whole_word; wordBtn.setAttribute("aria-pressed", r.whole_word); dirty(); };
  return row;
}

async function applyReplacements() {
  if (!state.book) return;
  const clean = (rules) => rules.filter((r) => r.find.trim());
  const rules = { book: clean(state.replace.book), all: clean(state.replace.all) };
  const id = state.book.entry.id;
  await call("replacements_set", { id, rules });
  state.replace = { ...rules, dirty: false };
  // The text changed: reload the book where the reader is.
  stopSpeech();
  const pos = visiblePosition();
  const view = await call("book_open", { id });
  state.book.toc = view.toc;
  state.chapter = -1;
  await loadChapter(pos.chapter, { scroll: "position", paragraph: pos.paragraph });
  if (state.search.query.trim()) runSearch();
  renderSidebar();
  renderNotesPanel();
  const n = rules.book.filter((r) => r.enabled).length + rules.all.filter((r) => r.enabled).length;
  toast(n ? `${n} replacement rule${n === 1 ? "" : "s"} in use.` : "No replacements in use.", { timeout: 2500 });
}

// --------------------------------------------------------------- search

function openSearch() {
  if (!state.book) return;
  const selected = window.getSelection().toString().trim();
  if (selected && selected.length < 80 && !selected.includes("\n")) state.search.query = selected;
  setViewIfHidden("search");
  const input = $("#search-input");
  input?.focus();
  input?.select();
  if (selected && state.search.query === selected) runSearch();
}

function renderSearch(root) {
  if (!state.book) {
    root.append(el(`<div class="hint">Open a book to search in it.</div>`));
    return;
  }
  const box = el(`<div class="search-box">
    <input type="search" id="search-input" placeholder="Search in this book" aria-label="Search in this book" autocomplete="off" spellcheck="false">
    <div class="meta" id="search-meta"></div></div>`);
  const input = box.querySelector("input");
  input.value = state.search.query;
  input.oninput = () => {
    state.search.query = input.value;
    runSearch();
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      nextHit(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape") {
      input.value = "";
      state.search.query = "";
      runSearch();
    }
  };
  root.append(box);
  root.append(el(`<div id="search-results"></div>`));
  renderSearchResults();
  setTimeout(() => input.focus(), 0);
}

const runSearch = debounce(async () => {
  if (!state.book) return;
  const query = state.search.query;
  if (!query.trim()) {
    Object.assign(state.search, { hits: [], truncated: false, active: -1 });
  } else {
    const r = await invoke("book_search", { id: state.book.entry.id, query }).catch((e) => {
      toast(String(e), { error: true });
      return null;
    });
    if (!r || state.search.query !== query) return; // a newer search is running
    Object.assign(state.search, { hits: r.hits, truncated: r.truncated, active: -1 });
  }
  renderSearchResults();
  renderMapRows();
  applySearchMarks();
}, 200);

function renderSearchResults() {
  const results = $("#search-results");
  const meta = $("#search-meta");
  if (!results || !meta) return;
  const { query, hits, truncated, active } = state.search;
  results.replaceChildren();
  if (!query.trim()) {
    meta.textContent = "Ignores case and accents: “thung lung” finds “thung lũng”. Enter = next, Shift+Enter = previous.";
    return;
  }
  const chapters = new Set(hits.map((h) => h.chapter)).size;
  meta.textContent = hits.length
    ? `${hits.length}${truncated ? "+" : ""} result${hits.length === 1 ? "" : "s"} in ${chapters} chapter${chapters === 1 ? "" : "s"}`
    : "No results.";
  const frag = document.createDocumentFragment();
  let chapter = -1;
  hits.forEach((h, i) => {
    if (h.chapter !== chapter) {
      chapter = h.chapter;
      const count = hits.filter((x) => x.chapter === chapter).length;
      frag.append(el(`<div class="search-group"><span>${esc(state.book.toc[chapter]?.title || `Chapter ${chapter + 1}`)}</span><span class="badge">${count}</span></div>`));
    }
    const lead = h.before.length >= 40 ? "…" + h.before.trimStart() : h.before;
    const item = el(`<div class="search-hit${i === active ? " active" : ""}" data-i="${i}" tabindex="0">${esc(lead)}<mark>${esc(h.matched)}</mark>${esc(h.after)}${h.after.length >= 40 ? "…" : ""}</div>`);
    item.onclick = () => goToHit(i);
    item.onkeydown = (e) => { if (e.key === "Enter") goToHit(i); };
    frag.append(item);
  });
  results.append(frag);
}

// DOM range of a search hit in the displayed chapter, or null.
function hitRange(h) {
  if (h.chapter !== state.chapter) return null;
  const p = paragraphEl(h.paragraph);
  if (!p) return null;
  let { start, end } = h;
  const text = p.textContent;
  if (text.slice(start, end) !== h.matched) {
    // Fall back to the nearest occurrence if the text layout differs.
    const at = text.indexOf(h.matched);
    if (at < 0) return null;
    start = at;
    end = at + h.matched.length;
  }
  const r = document.createRange();
  r.setStart(...domPoint(p, start));
  r.setEnd(...domPoint(p, end));
  return r;
}

function applySearchMarks() {
  if (!HAS_HIGHLIGHTS) return;
  CSS.highlights.delete("search-hit");
  CSS.highlights.delete("search-current");
  const { hits, active } = state.search;
  const ranges = [];
  hits.forEach((h, i) => {
    if (i === active) return;
    const r = hitRange(h);
    if (r) ranges.push(r);
  });
  if (ranges.length) CSS.highlights.set("search-hit", new Highlight(...ranges));
  const current = hits[active] && hitRange(hits[active]);
  if (current) CSS.highlights.set("search-current", new Highlight(current));
}

async function goToHit(i) {
  const h = state.search.hits[i];
  if (!h) return;
  state.search.active = i;
  await loadChapter(h.chapter, { scroll: "none" });
  reveal(hitRange(h) || paragraphEl(h.paragraph));
  applySearchMarks();
  document.querySelectorAll(".search-hit").forEach((n) => n.classList.toggle("active", Number(n.dataset.i) === i));
  document.querySelector(".search-hit.active")?.scrollIntoView({ block: "nearest" });
  renderMapRows();
  // Reading aloud from here starts at the found sentence.
  state.cursor = { chapter: h.chapter, paragraph: h.paragraph, sentence: 0 };
  renderPlayer();
}

function nextHit(dir) {
  const n = state.search.hits.length;
  if (!n) return;
  let i = state.search.active;
  if (i < 0) i = dir > 0 ? -1 : 0;
  goToHit((((i + dir) % n) + n) % n);
}

// ------------------------------------------------------------ bookmarks

function bookmarkPosition() {
  if (state.speaking && state.speaking.chapter === state.chapter && state.tts !== "stopped") return state.speaking;
  return visiblePosition();
}

function bookmarkAt(pos) {
  return state.bookmarks.find((b) => b.position.chapter === pos.chapter && b.position.paragraph === pos.paragraph);
}

async function toggleBookmark() {
  if (!state.book) return;
  const pos = bookmarkPosition();
  const id = state.book.entry.id;
  const existing = bookmarkAt(pos);
  if (existing) {
    await call("bookmark_remove", { id, bookmarkId: existing.id });
    state.bookmarks = state.bookmarks.filter((b) => b.id !== existing.id);
    toast("Bookmark removed.");
  } else {
    const label = paragraphEl(pos.paragraph)?.textContent.trim() || "";
    const b = await call("bookmark_add", { id, position: { ...pos, sentence: 0 }, label });
    state.bookmarks = state.bookmarks.concat(b).sort((x, y) =>
      x.position.chapter - y.position.chapter || x.position.paragraph - y.position.paragraph);
    toast("Bookmarked.", { timeout: 1500 });
  }
  applyBookmarkMarks();
  renderMapRows();
  if (state.panelTab === "bookmarks") renderNotesPanel();
  else $("#bookmarks-count").textContent = state.bookmarks.length || "";
}

function applyBookmarkMarks() {
  document.querySelectorAll(".reading p.bookmarked").forEach((p) => p.classList.remove("bookmarked"));
  for (const b of state.bookmarks) {
    if (b.position.chapter === state.chapter) paragraphEl(b.position.paragraph)?.classList.add("bookmarked");
  }
  updateBookmarkButton();
}

function updateBookmarkButton() {
  const btn = $("#btn-bookmark");
  if (!btn) return;
  btn.disabled = !state.book;
  const on = !!(state.book && document.querySelector(".reading") && bookmarkAt(visiblePosition()));
  btn.classList.toggle("on", on);
  btn.setAttribute("aria-pressed", String(on));
}

function renderBookmarks(list) {
  list.replaceChildren();
  if (!state.book) {
    list.append(el(`<div class="hint">Open a book to see its bookmarks.</div>`));
    return;
  }
  if (!state.bookmarks.length) {
    list.append(el(`<div class="hint">No bookmarks yet.<br><br>Press <kbd>Ctrl</kbd> <kbd>D</kbd> or the ribbon button above the text to bookmark the place you are reading.</div>`));
    return;
  }
  const q = fold(state.notesFilter.trim());
  for (const b of state.bookmarks) {
    if (q && !fold(b.label).includes(q)) continue;
    const title = state.book.toc[b.position.chapter]?.title || `Chapter ${b.position.chapter + 1}`;
    const card = el(`<div class="bookmark-card" tabindex="0">
      <div class="where">${icon("bookmark")}${esc(title)}</div><div class="lbl">${esc(b.label || "(empty paragraph)")}</div>
      <div class="actions"><button class="icon-btn small" aria-label="Remove bookmark" title="Remove">${icon("close")}</button></div></div>`);
    const go = async () => {
      await loadChapter(b.position.chapter, { scroll: "position", paragraph: b.position.paragraph });
      const p = paragraphEl(b.position.paragraph);
      if (p) {
        p.classList.remove("flash");
        void p.offsetWidth;
        p.classList.add("flash");
      }
      savePosition(b.position);
    };
    card.onclick = go;
    card.onkeydown = (e) => { if (e.key === "Enter") go(); };
    card.querySelector("button").onclick = async (e) => {
      e.stopPropagation();
      await call("bookmark_remove", { id: state.book.entry.id, bookmarkId: b.id });
      state.bookmarks = state.bookmarks.filter((x) => x.id !== b.id);
      applyBookmarkMarks();
      renderMapRows();
      renderNotesPanel();
    };
    list.append(card);
  }
}

main();
