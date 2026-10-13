// Marginalia UI — plain JavaScript, no framework or build step.
// Talks to the Rust side through Tauri commands and events.

"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const CJK_END = /[\u3000-\u30ff\u3400-\u9fff\uf900-\ufaff\uff00-\uffef]["'”’」』）)]*$/;
const SPEEDS = [0.75, 1, 1.25, 1.5, 1.75, 2];
const VIEW_TITLES = {
  chapters: "Chapters",
  library: "Library",
  voices: "Voices",
  settings: "Settings",
};

const state = {
  settings: null,
  library: [],
  view: "chapters",
  book: null, // { entry, toc }
  chapter: -1,
  blocks: [], // chapter content: { kind: "text", sentences } | { kind: "image", image, alt }
  paragraphs: [], // text blocks only: [[sentence, ...], ...]
  cursor: null, // { chapter, paragraph, sentence } chosen by clicking
  speaking: null, // position currently being read aloud
  tts: "stopped", // loading | playing | paused | stopped
  voiceLabel: "",
  engine: null,
  catalog: null,
  catalogError: null,
  voiceLang: null,
  downloads: {}, // key -> { done, total }
  chapterFilter: "",
  notes: [], // notes of the open book
  notesFilter: "",
  editing: null, // note open in the editor
  pendingAnchor: null, // anchor of the current text selection
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

function debounce(fn, ms) {
  let t;
  return (...args) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
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

function toast(message, { error = false, actions = [], timeout = error ? 9000 : 4000 } = {}) {
  const node = el(`<div class="toast${error ? " error" : ""}">
    <div class="msg">${esc(message)}<div class="actions"></div></div>
    <button class="icon-btn" title="Close">×</button></div>`);
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

function applySettings() {
  const s = state.settings;
  document.documentElement.dataset.theme = s.theme;
  $("#sidebar").style.width = s.sidebar_width + "px";
  $("#notes-panel").style.width = s.notes_width + "px";
  document.body.classList.toggle("notes-open", !!s.notes_open);
  $("#st-speed").textContent = s.speed.toFixed(2).replace(/0$/, "") + "×";
  const r = document.querySelector(".reading");
  if (r) styleReading(r);
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
}

function renderSidebar() {
  document.querySelectorAll(".activity").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === state.view &&
      !document.body.classList.contains("sidebar-hidden")));
  $("#sidebar-title").innerHTML = `<span>${VIEW_TITLES[state.view]}</span><span class="grow"></span>`;
  const content = $("#sidebar-content");
  const scroll = content.scrollTop;
  content.replaceChildren();
  ({ chapters: renderChapters, library: renderLibrary, voices: renderVoices, settings: renderSettings })[state.view](content);
  content.scrollTop = scroll;
}

function renderChapters(root) {
  if (!state.book) {
    root.append(el(`<div class="hint">No book open.</div>`));
    const b = el(`<button class="btn block">Open Book…</button>`);
    b.onclick = pickBooks;
    root.append(b);
    return;
  }
  const { entry, toc } = state.book;
  root.append(el(`<div class="section-header"><span class="grow">${esc(entry.title)}</span></div>`));
  if (toc.length > 12) {
    const f = el(`<div class="filter"><input type="search" placeholder="Filter chapters" value="${esc(state.chapterFilter)}"></div>`);
    const input = f.querySelector("input");
    input.oninput = () => {
      state.chapterFilter = input.value;
      renderChapterList(list);
    };
    root.append(f);
  }
  const list = el(`<div class="toc"></div>`);
  root.append(list);
  renderChapterList(list);
  list.querySelector(".active")?.scrollIntoView({ block: "center" });
}

function renderChapterList(list) {
  const q = state.chapterFilter.trim().toLowerCase();
  const frag = document.createDocumentFragment();
  state.book.toc.forEach((c, i) => {
    if (q && !c.title.toLowerCase().includes(q)) return;
    const item = el(`<div class="list-item${i === state.chapter ? " active" : ""}" title="${esc(c.title)}">
      <span class="num">${i + 1}</span><span class="label">${esc(c.title)}</span>
      <span class="meta">${c.images && !c.words ? "🖼" : c.words >= 1000 ? Math.round(c.words / 100) / 10 + "k" : c.words}</span></div>`);
    item.onclick = () => loadChapter(i, { scroll: "top" });
    frag.append(item);
  });
  list.replaceChildren(frag);
}

function renderLibrary(root) {
  const b = el(`<button class="btn block">Open Book…</button>`);
  b.onclick = pickBooks;
  root.append(b);
  if (!state.library.length) {
    root.append(el(`<div class="hint">Your library is empty. Open EPUB, MOBI, PRC or AZW files, or drop them onto the window.</div>`));
    return;
  }
  root.append(el(`<div class="section-header"><span class="grow">Books</span><span class="badge">${state.library.length}</span></div>`));
  for (const book of state.library) {
    const pct = Math.round((book.progress || 0) * 100);
    const initial = (book.title.trim()[0] || "?").toUpperCase();
    const item = el(`<div class="list-item book-item${state.book?.entry.id === book.id ? " active" : ""}" title="${esc(book.path)}">
      <div class="cover" style="background:hsl(${hue(book.title)} 45% 40%)">${book.has_cover
        ? `<img src="${coverUrl(book.id)}" alt="" loading="lazy">` : esc(initial)}</div>
      <div class="info">
        <div class="title">${esc(book.title)}</div>
        <div class="meta">${esc(book.authors.join(", ") || "Unknown author")}</div>
        <div class="meta"><span class="badge">${esc(book.format)}</span> ${pct}% · ${book.chapters} ch.</div>
        <div class="progress-line"><div style="width:${pct}%"></div></div>
      </div>
      <div class="actions"><button class="icon-btn" title="Remove from library">×</button></div></div>`);
    item.onclick = () => openBook(book.id);
    item.querySelector(".actions button").onclick = async (e) => {
      e.stopPropagation();
      await call("library_remove", { id: book.id });
      if (state.book?.entry.id === book.id) closeBook();
      await refreshLibrary();
    };
    root.append(item);
  }
}

async function refreshEngine() {
  try {
    state.engine = await invoke("engine_status");
  } catch (e) {
    state.engine = null;
  }
  if (!state.catalog && !state.catalogError) {
    invoke("voices_catalog", { refresh: false })
      .then((c) => { state.catalog = c; })
      .catch((e) => { state.catalogError = String(e); })
      .finally(() => state.view === "voices" && renderSidebar());
  }
  if (state.view === "voices") renderSidebar();
}

function bookLanguage() {
  return (state.book?.entry.language || "en").replace("-", "_");
}

function renderVoices(root) {
  const eng = state.engine;
  root.append(el(`<div class="section-header"><span class="grow">Speech engine</span></div>`));
  if (!eng) {
    root.append(el(`<div class="hint">Checking…</div>`));
    return;
  }
  const piperDl = state.downloads.piper;
  if (eng.piper) {
    root.append(el(`<div class="hint">✓ Piper neural TTS is installed.</div>`));
  } else if (eng.piper_supported) {
    const box = el(`<div class="hint">Piper gives natural-sounding voices. Download the engine (~25 MB) once.</div>`);
    root.append(box);
    if (piperDl) {
      box.append(progressBar(piperDl));
    } else {
      const b = el(`<button class="btn block">Install Piper engine</button>`);
      b.onclick = installPiper;
      root.append(b);
    }
  } else {
    root.append(el(`<div class="hint">Piper has no prebuilt binary for this system.</div>`));
  }
  root.append(el(`<div class="hint">${eng.espeak ? "✓ eSpeak NG found (fallback voice)." : "eSpeak NG not found (optional fallback)."}</div>`));

  // Installed voices: pick the default.
  root.append(el(`<div class="section-header"><span class="grow">Installed voices</span><span class="badge">${eng.installed.length}</span></div>`));
  if (!eng.installed.length) {
    root.append(el(`<div class="hint">No voices yet — download one below.</div>`));
  }
  const auto = el(`<div class="list-item${state.settings.voice ? "" : " active"}"><span class="label">Automatic (match book language)</span></div>`);
  auto.onclick = () => { updateSetting("voice", null); renderSidebar(); };
  if (eng.installed.length) root.append(auto);
  for (const key of eng.installed) {
    const item = el(`<div class="list-item${state.settings.voice === key ? " active" : ""}">
      <span class="label">${esc(key)}</span>
      <span class="actions"><button class="icon-btn" title="Try this voice">▶</button><button class="icon-btn" title="Delete">×</button></span></div>`);
    item.onclick = () => { updateSetting("voice", key); renderSidebar(); };
    const [tryBtn, delBtn] = item.querySelectorAll(".actions button");
    tryBtn.onclick = (e) => {
      e.stopPropagation();
      updateSetting("voice", key);
      renderSidebar();
      if (state.book) play(currentStart());
    };
    delBtn.onclick = async (e) => {
      e.stopPropagation();
      await call("voice_remove", { key });
      if (state.settings.voice === key) updateSetting("voice", null);
      markInstalled(key, false);
      refreshEngine();
    };
    root.append(item);
  }

  // Catalog.
  root.append(el(`<div class="section-header"><span class="grow">Download voices</span></div>`));
  if (state.catalogError) {
    root.append(el(`<div class="hint error-text">Could not load the voice catalog: ${esc(state.catalogError)}</div>`));
    const retry = el(`<button class="btn block secondary">Retry</button>`);
    retry.onclick = () => { state.catalogError = null; refreshEngine(); };
    root.append(retry);
    return;
  }
  if (!state.catalog) {
    root.append(el(`<div class="hint">Loading catalog…</div>`));
    return;
  }
  const langs = new Map();
  for (const v of state.catalog) {
    langs.set(v.language_code, `${v.language} (${v.country || v.language_code})`);
  }
  if (!state.voiceLang) {
    const lang = bookLanguage();
    const family = lang.split("_")[0];
    const codes = [...langs.keys()];
    // "en" -> en_US, "fr" -> fr_FR, "de" -> de_DE ... when only a family is known.
    const main = family === "en" ? "en_US" : `${family}_${family.toUpperCase()}`;
    state.voiceLang = codes.find((c) => c.toLowerCase() === lang.toLowerCase())
      || codes.find((c) => c === main)
      || codes.find((c) => c.split("_")[0] === family) || "en_US";
  }
  const sel = el(`<div class="filter"><select></select></div>`);
  const select = sel.querySelector("select");
  for (const [code, name] of [...langs].sort((a, b) => a[1].localeCompare(b[1]))) {
    select.append(new Option(name, code, false, code === state.voiceLang));
  }
  select.onchange = () => { state.voiceLang = select.value; renderSidebar(); };
  root.append(sel);
  for (const v of state.catalog.filter((v) => v.language_code === state.voiceLang)) {
    const dl = state.downloads[v.key];
    const item = el(`<div class="list-item" style="flex-wrap:wrap">
      <span class="label">${esc(v.key.split("-").slice(1).join(" · "))}</span>
      <span class="meta">${mb(v.size)}</span><span class="slot"></span></div>`);
    const slot = item.querySelector(".slot");
    if (v.installed) {
      slot.append(el(`<span class="badge">installed</span>`));
    } else if (dl) {
      item.append(progressBar(dl));
    } else {
      const b = el(`<button class="btn small">Download</button>`);
      b.onclick = () => downloadVoice(v.key);
      slot.append(b);
    }
    item.title = `${v.key} — ${v.quality} quality${v.speakers > 1 ? `, ${v.speakers} speakers` : ""}`;
    root.append(item);
  }
  root.append(el(`<div class="hint">Voices come from the open <code>rhasspy/piper-voices</code> collection. Each voice has its own license (see its model card). Stored in <code>${esc(eng.data_dir)}</code>.</div>`));
}

function progressBar(dl) {
  const pct = dl.total ? Math.round((dl.done / dl.total) * 100) : 0;
  const n = el(`<div style="width:100%"><div class="bar"><div style="width:${pct}%"></div></div>
    <div class="meta">${dl.total ? `${pct}% of ${mb(dl.total)}` : mb(dl.done)}</div></div>`);
  return n;
}

function markInstalled(key, installed) {
  const v = state.catalog?.find((v) => v.key === key);
  if (v) v.installed = installed;
}

async function installPiper() {
  state.downloads.piper = { done: 0, total: null };
  renderSidebar();
  try {
    await invoke("engine_install");
    toast("Piper engine installed.");
  } catch (e) {
    toast("Installing Piper failed: " + e, { error: true });
  }
  delete state.downloads.piper;
  refreshEngine();
}

async function downloadVoice(key) {
  state.downloads[key] = { done: 0, total: null };
  renderSidebar();
  try {
    await invoke("voice_download", { key });
    markInstalled(key, true);
    toast(`Voice ${key} downloaded.`);
  } catch (e) {
    toast(`Downloading ${key} failed: ${e}`, { error: true });
  }
  delete state.downloads[key];
  refreshEngine();
}

function renderSettings(root) {
  const s = state.settings;
  const add = (html, bind) => {
    const n = el(`<div class="setting">${html}</div>`);
    bind(n);
    root.append(n);
  };
  const select = (key, label, options) =>
    add(`<label>${label}</label><select>${options.map(([v, t]) =>
      `<option value="${v}"${s[key] === v ? " selected" : ""}>${t}</option>`).join("")}</select>`,
    (n) => { n.querySelector("select").onchange = (e) => updateSetting(key, e.target.value); });
  const range = (key, label, min, max, step, fmt) =>
    add(`<label>${label}<span class="value">${fmt(s[key])}</span></label>
      <input type="range" min="${min}" max="${max}" step="${step}" value="${s[key]}">`,
    (n) => {
      const input = n.querySelector("input");
      input.oninput = () => {
        updateSetting(key, Number(input.value));
        n.querySelector(".value").textContent = fmt(Number(input.value));
      };
    });

  root.append(el(`<div class="section-header"><span class="grow">Appearance</span></div>`));
  select("theme", "Color theme", [["dark", "Dark"], ["light", "Light"], ["sepia", "Sepia"]]);
  select("font", "Font", [["serif", "Serif"], ["sans", "Sans-serif"]]);
  range("font_size", "Font size", 12, 36, 1, (v) => v + " px");
  range("line_height", "Line spacing", 1.2, 2.4, 0.1, (v) => v.toFixed(1));
  range("width", "Column width", 40, 120, 5, (v) => v + " ch");
  root.append(el(`<div class="section-header"><span class="grow">Read aloud</span></div>`));
  range("speed", "Speaking speed", 0.5, 2.5, 0.05, (v) => v.toFixed(2) + "×");
  root.append(el(`<div class="hint">Shortcuts: <kbd>Space</kbd> play/pause · <kbd>←</kbd> <kbd>→</kbd> chapters ·
    <kbd>Ctrl</kbd>+<kbd>B</kbd> sidebar · <kbd>Ctrl</kbd>+<kbd>O</kbd> open · <kbd>Ctrl</kbd>+<kbd>+</kbd>/<kbd>-</kbd> font size.
    Double-click a sentence to read aloud from there.</div>`));
}

// ------------------------------------------------------------ library

async function refreshLibrary() {
  state.library = await invoke("library_list");
  if (state.view === "library") renderSidebar();
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
  state.notes = await invoke("notes_list", { id }).catch(() => []);
  state.notesFilter = "";
  closeNoteEditor();
  state.cursor = null;
  state.speaking = null;
  state.chapterFilter = "";
  state.voiceLang = null;
  const pos = view.entry.position;
  await loadChapter(Math.min(pos.chapter, Math.max(view.toc.length - 1, 0)), {
    scroll: "position", paragraph: pos.paragraph,
  });
  state.view = "chapters";
  renderSidebar();
  renderNotesPanel();
  refreshLibrary();
}

function setViewIfHidden(view) {
  document.body.classList.remove("sidebar-hidden");
  state.view = view;
  renderSidebar();
}

function closeBook() {
  stopSpeech();
  state.book = null;
  state.chapter = -1;
  state.blocks = [];
  state.paragraphs = [];
  state.notes = [];
  closeNoteEditor();
  renderContent();
  renderSidebar();
  renderNotesPanel();
  renderStatus();
}

async function loadChapter(index, { scroll = "top", paragraph = 0 } = {}) {
  if (!state.book || index < 0 || index >= state.book.toc.length) return;
  if (index !== state.chapter) {
    state.blocks = await call("chapter_get", { id: state.book.entry.id, index });
    state.paragraphs = state.blocks.filter((b) => b.kind === "text").map((b) => b.sentences);
    state.chapter = index;
    renderContent();
  }
  const content = $("#content");
  if (scroll === "top") {
    content.scrollTop = 0;
    savePosition({ chapter: index, paragraph: 0, sentence: 0 });
  } else if (scroll === "position") {
    const p = content.querySelector(`p[data-p="${paragraph}"]`);
    if (p) content.scrollTop = p.offsetTop - 24;
  }
  if (state.view === "chapters") {
    document.querySelectorAll(".toc .list-item").forEach((n) => n.classList.remove("active"));
    renderSidebar();
  }
  renderStatus();
}

function renderContent() {
  const content = $("#content");
  const tabs = $("#tabs");
  if (!state.book) {
    tabs.replaceChildren(el(`<div class="tab"><span class="label">Welcome</span></div>`));
    $("#crumbs").textContent = "";
    $("#btn-prev").disabled = $("#btn-next").disabled = true;
    content.replaceChildren(renderWelcome());
    return;
  }
  const { entry, toc } = state.book;
  const title = toc[state.chapter]?.title || "";
  tabs.replaceChildren(el(`<div class="tab" title="${esc(entry.path)}"><span class="label">📖 ${esc(entry.title)}</span>
    <button class="icon-btn" title="Close book">×</button></div>`));
  tabs.querySelector("button").onclick = closeBook;
  $("#crumbs").innerHTML = `${esc(entry.title)}<span class="sep">›</span>${esc(title)}`;
  $("#btn-prev").disabled = state.chapter <= 0;
  $("#btn-next").disabled = state.chapter >= toc.length - 1;

  const reading = el(`<article class="reading"></article>`);
  // The language lets the webview pick the right glyphs (Vietnamese
  // diacritics, Simplified vs Traditional Chinese).
  if (entry.language) reading.lang = entry.language.replace("_", "-");
  styleReading(reading);
  const norm = (s) => s.toLowerCase().replace(/\s+/g, " ").trim();
  let pi = -1; // index among text paragraphs (what positions and speech use)
  state.blocks.forEach((block) => {
    if (block.kind === "image") {
      const fig = el(`<figure><img loading="lazy" alt="${esc(block.alt)}" title="${esc(block.alt)}"></figure>`);
      const img = fig.querySelector("img");
      img.src = imageUrl(entry.id, block.image);
      img.onerror = () => fig.remove();
      reading.append(fig);
      return;
    }
    const sentences = block.sentences;
    pi += 1;
    const p = document.createElement("p");
    p.dataset.p = pi;
    if (pi === 0 && norm(sentences.join(" ")) === norm(title)) p.className = "heading";
    p.append(el(`<span class="gutter add" title="Add a note to this paragraph"></span>`));
    sentences.forEach((s, si) => {
      // Chinese/Japanese sentences are not separated by spaces.
      if (si && !CJK_END.test(sentences[si - 1])) p.append(" ");
      const span = document.createElement("span");
      span.className = "s";
      span.dataset.s = si;
      span.textContent = s;
      p.append(span);
    });
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
  content.replaceChildren(reading);
  applyNoteMarks();
  if (state.speaking?.chapter === state.chapter) highlight(state.speaking, false);
}

function renderWelcome() {
  const w = el(`<div class="welcome">
    <div class="brand"><img src="logo.svg" alt=""><h1>Marginalia</h1></div>
    <div class="sub">Read, annotate and listen to EPUB, MOBI and PRC books.</div>
    <div class="cols">
      <div>
        <h2>Start</h2>
        <a id="w-open">Open Book…</a>
        <a id="w-voices">Download a voice…</a>
        <a id="w-settings">Settings</a>
      </div>
      <div class="recent"><h2>Recent</h2></div>
    </div>
    <div class="drop">Drop ebook files here to add them to your library</div>
  </div>`);
  w.querySelector("#w-open").onclick = pickBooks;
  w.querySelector("#w-voices").onclick = () => setViewIfHidden("voices") || refreshEngine();
  w.querySelector("#w-settings").onclick = () => setViewIfHidden("settings");
  const recent = w.querySelector(".recent");
  const books = state.library.slice(0, 8);
  if (!books.length) recent.append(el(`<div class="hint" style="padding:0">No books yet.</div>`));
  for (const b of books) {
    const a = el(`<a>${esc(b.title)}<span>${esc(b.authors[0] || "")}</span></a>`);
    a.onclick = () => openBook(b.id);
    recent.append(a);
  }
  return w;
}

// Fraction of the book's words before `pos`.
function progressOf(pos) {
  const toc = state.book.toc;
  const words = toc.reduce((n, c) => n + c.words, 0) || 1;
  let before = toc.slice(0, pos.chapter).reduce((n, c) => n + c.words, 0);
  if (pos.chapter === state.chapter && state.paragraphs.length) {
    before += (toc[pos.chapter]?.words || 0) * (pos.paragraph / state.paragraphs.length);
  }
  return Math.min(1, before / words);
}

function renderStatus() {
  const b = state.book;
  $("#st-book").textContent = b ? b.entry.title : "";
  $("#st-chapter").textContent = b ? `Chapter ${state.chapter + 1}/${b.toc.length}` : "";
  $("#st-progress").textContent = b ? Math.round(progressOf(b.entry.position) * 100) + "%" : "";
  $("#st-notes").textContent = b ? `✎ ${state.notes.length} note${state.notes.length === 1 ? "" : "s"}` : "";
  const play = $("#st-play");
  play.textContent = { loading: "… Preparing", playing: "❚❚ Pause", paused: "▶ Resume", stopped: "▶ Read aloud" }[state.tts];
  play.classList.toggle("playing", state.tts === "playing");
  $("#st-stop").style.display = state.tts === "stopped" ? "none" : "";
  $("#st-voice").textContent = state.tts !== "stopped" ? state.voiceLabel : "";
}

// Where the reader currently is: the topmost visible paragraph.
function visiblePosition() {
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
  invoke("position_save", { id: state.book.entry.id, position, progress }).catch(() => {});
}

const onScroll = debounce(() => {
  if (state.book && state.tts === "stopped") savePosition(visiblePosition());
}, 500);

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

function clearHighlight() {
  document.querySelectorAll(".s.speaking").forEach((n) => n.classList.remove("speaking"));
}

function highlight(pos, follow = true) {
  clearHighlight();
  const span = document.querySelector(`p[data-p="${pos.paragraph}"] .s[data-s="${pos.sentence}"]`);
  if (!span) return;
  span.classList.add("speaking");
  if (!follow) return;
  const box = $("#content").getBoundingClientRect();
  const r = span.getBoundingClientRect();
  if (r.top < box.top + 40 || r.bottom > box.bottom - 60) {
    span.scrollIntoView({ block: "center", behavior: "smooth" });
  }
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
  if (ev.voice) state.voiceLabel = "🔊 " + ev.voice;
  if (ev.state === "stopped") clearHighlight();
  if (ev.error === "NO_VOICE") {
    const lang = state.book?.entry.language || "en";
    toast(`No voice for this book's language (${lang}) yet. Download one to read aloud.`, {
      actions: [{ label: "Open Voices", run: () => { state.voiceLang = null; setViewIfHidden("voices"); refreshEngine(); } }],
    });
  } else if (ev.error) {
    toast("Read aloud stopped: " + ev.error, { error: true });
  }
  renderStatus();
}

function changeSpeed() {
  const s = state.settings.speed;
  const next = SPEEDS.find((v) => v > s + 0.01) ?? SPEEDS[0];
  updateSetting("speed", next);
  if (state.view === "settings") renderSidebar();
  // Speed is applied when synthesizing, so restart from the current sentence.
  if ((state.tts === "playing" || state.tts === "paused") && state.speaking) play(state.speaking);
}

// --------------------------------------------------------------- wiring

function wire() {
  document.querySelectorAll(".activity").forEach((b) => { b.onclick = () => setView(b.dataset.view); });
  $("#btn-prev").onclick = () => loadChapter(state.chapter - 1);
  $("#btn-next").onclick = () => loadChapter(state.chapter + 1);
  $("#st-play").onclick = togglePlay;
  $("#st-stop").onclick = stopSpeech;
  $("#st-speed").onclick = changeSpeed;
  $("#st-book").onclick = () => setViewIfHidden("library");
  $("#content").addEventListener("scroll", onScroll, { passive: true });

  wireNotes();

  // Click a sentence to place the read-aloud cursor; double-click to start there.
  const content = $("#content");
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
  });
  content.addEventListener("dblclick", (e) => {
    const pos = sentenceAt(e.target);
    if (!pos) return;
    window.getSelection().removeAllRanges();
    state.cursor = pos;
    play(pos);
  });

  // Sidebar resizing.
  const sash = $("#sash");
  sash.addEventListener("mousedown", (e) => {
    e.preventDefault();
    sash.classList.add("dragging");
    const startX = e.clientX;
    const startW = $("#sidebar").offsetWidth;
    const move = (ev) => {
      const w = Math.max(170, Math.min(window.innerWidth * 0.6, startW + ev.clientX - startX));
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
    if (mod && e.shiftKey && e.key.toLowerCase() === "m") { e.preventDefault(); toggleNotesPanel(); return; }
    if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); document.body.classList.toggle("sidebar-hidden"); renderSidebar(); }
    else if (mod && e.key.toLowerCase() === "o") { e.preventDefault(); pickBooks(); }
    else if (mod && e.key === "1") { e.preventDefault(); setViewIfHidden("chapters"); }
    else if (mod && e.key === "2") { e.preventDefault(); setViewIfHidden("library"); }
    else if (mod && e.key === "3") { e.preventDefault(); setViewIfHidden("voices"); refreshEngine(); }
    else if (mod && e.key === ",") { e.preventDefault(); setViewIfHidden("settings"); }
    else if (mod && (e.key === "=" || e.key === "+")) { e.preventDefault(); updateSetting("font_size", Math.min(36, state.settings.font_size + 1)); }
    else if (mod && e.key === "-") { e.preventDefault(); updateSetting("font_size", Math.max(12, state.settings.font_size - 1)); }
    else if (typing || mod || e.altKey) return;
    else if (e.key === " ") { e.preventDefault(); togglePlay(); }
    else if (e.key === "ArrowLeft") loadChapter(state.chapter - 1);
    else if (e.key === "ArrowRight") loadChapter(state.chapter + 1);
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
    state.downloads[key] = { done, total };
    if (state.view === "voices") renderSidebar();
  });
  listen("tauri://drag-enter", () => document.body.classList.add("dragging"));
  listen("tauri://drag-leave", () => document.body.classList.remove("dragging"));
  listen("tauri://drag-drop", (e) => {
    document.body.classList.remove("dragging");
    const paths = (e.payload.paths || []).filter((p) => /\.(epub|mobi|prc|azw3?|pdb)$/i.test(p));
    if (paths.length) addPaths(paths);
    else toast("Only EPUB, MOBI, PRC, AZW and PDB files can be opened.", { error: true });
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
  $("#sel-note").hidden = true;
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
    rect = { left: c.left + c.width / 2 - 180, bottom: c.top + 80, top: c.top + 80 };
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
  ed.querySelectorAll(".colors button").forEach((b) => b.classList.toggle("on", b.dataset.color === color));
}

function closeNoteEditor() {
  state.editing = null;
  const ed = $("#note-editor");
  if (ed) ed.hidden = true;
}

async function saveEditingNote() {
  const ed = $("#note-editor");
  const note = {
    ...state.editing,
    text: ed.querySelector("textarea").value,
    color: ed.dataset.color || "amber",
  };
  if (!note.text.trim() && !note.anchor.quote.trim()) return closeNoteEditor();
  const saved = await call("note_save", { id: state.book.entry.id, note });
  state.notes = state.notes.filter((n) => n.id !== saved.id).concat(saved).sort(noteOrder);
  closeNoteEditor();
  window.getSelection().removeAllRanges();
  applyNoteMarks();
  renderNotesPanel([saved.id]);
  renderStatus();
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
  $("#notes-count").textContent = state.notes.length;
  $("#notes-export").disabled = !state.notes.length;
  list.replaceChildren();
  if (!state.book) {
    list.append(el(`<div class="hint">Open a book to see its notes.</div>`));
    return;
  }
  const q = state.notesFilter.trim().toLowerCase();
  const notes = state.notes.filter((n) => !q || (n.text + " " + n.anchor.quote).toLowerCase().includes(q));
  if (!state.notes.length) {
    list.append(el(`<div class="hint">No notes yet.<br><br>Select some text and press <b>✎ Note</b>,
      or click the <b>+</b> in the margin next to a paragraph.</div>`));
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
    const card = el(`<div class="note-card${activeIds.includes(note.id) ? " active" : ""}" data-color="${esc(note.color)}">
      ${quote ? `<div class="q">${esc(quote)}</div>` : ""}
      ${note.text.trim() ? `<div class="t">${esc(note.text.trim())}</div>` : ""}
      <div class="when">${new Date(note.updated * 1000).toLocaleString()}</div>
      <div class="actions"><button class="icon-btn" title="Edit">✎</button><button class="icon-btn" title="Delete">×</button></div>
    </div>`);
    card.onclick = () => goToNote(note);
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
  const content = $("#content");
  const box = content.getBoundingClientRect();
  const r = p.getBoundingClientRect();
  if (r.top < box.top + 20 || r.top > box.bottom - 80) content.scrollTop += r.top - box.top - box.height / 3;
  const range = noteRange(note);
  if (range && HAS_HIGHLIGHTS) {
    CSS.highlights.set("note-flash", new Highlight(range));
    setTimeout(() => CSS.highlights.delete("note-flash"), 1200);
  } else {
    p.classList.remove("flash");
    void p.offsetWidth;
    p.classList.add("flash");
  }
}

function wireNotes() {
  const content = $("#content");
  const selBtn = $("#sel-note");
  const showSelectionButton = () => {
    const anchor = state.book ? selectionAnchor() : null;
    state.pendingAnchor = anchor;
    if (!anchor) {
      selBtn.hidden = true;
      return;
    }
    const rects = window.getSelection().getRangeAt(0).getClientRects();
    const last = rects[rects.length - 1];
    if (!last) return;
    selBtn.hidden = false;
    selBtn.style.left = Math.min(window.innerWidth - 90, last.right + 4) + "px";
    selBtn.style.top = Math.min(window.innerHeight - 60, last.bottom + 6) + "px";
  };
  content.addEventListener("mouseup", () => setTimeout(showSelectionButton, 0));
  content.addEventListener("keyup", (e) => e.shiftKey && showSelectionButton());
  content.addEventListener("scroll", () => { selBtn.hidden = true; }, { passive: true });
  document.addEventListener("selectionchange", () => {
    if (window.getSelection().isCollapsed) selBtn.hidden = true;
  });
  selBtn.addEventListener("mousedown", (e) => e.preventDefault()); // keep the selection
  selBtn.onclick = () => {
    if (!state.pendingAnchor) return;
    openNoteEditor({ id: "", anchor: state.pendingAnchor, text: "", color: "amber" });
  };

  // "+" in the margin: note on the whole paragraph.
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
    if (!ed.hidden && !ed.contains(e.target) && !e.target.closest(".gutter, #sel-note, .note-card")) closeNoteEditor();
  });

  $("#btn-notes").onclick = toggleNotesPanel;
  $("#st-notes").onclick = toggleNotesPanel;
  $("#notes-close").onclick = () => setNotesOpen(false);
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

main();
