# ZReader

An open-source ebook reader for **EPUB** and **MOBI / PRC / AZW** files that
can **read your books aloud** with open-source neural text-to-speech. Voice
models are downloaded on demand, so nothing big ships with the app.

> **About this project.** ZReader is built for its owner's own needs: a
> reader that opens the formats they use and can read to them. It is not
> aimed at any particular audience, market, or business goal. It is shared
> openly (MIT) in case it is useful to someone else, but features and
> priorities follow what the owner needs.

## Status

Early stage. What works today:

| Area | Status |
| --- | --- |
| EPUB 2 / 3 | ✅ metadata, spine order, chapter titles from the TOC |
| MOBI / PRC / AZW | ✅ uncompressed, PalmDOC and HUFF/CDIC compression, EXTH metadata, UTF-8 and CP1252 |
| Plain PalmDOC (`.pdb`, old `.prc`) | ✅ plain-text and HTML content |
| DRM-protected books | ❌ refused with a clear error (DRM-free books only) |
| TTS: Piper neural voices | ✅ catalog of 100+ voices in ~40 languages, downloaded on demand with MD5 checks |
| TTS: eSpeak NG fallback | ✅ uses a system `espeak-ng` when Piper is unavailable |
| Playback | ✅ gapless playback while the next sentence is synthesized, or export to WAV |
| GUI | 🚧 planned (see [Roadmap](#roadmap)); today's interface is the `zreader` CLI |

## Install

You need a [Rust toolchain](https://rustup.rs) (1.82+).

```sh
git clone https://github.com/zunnievo94/ZReader
cd ZReader
cargo install --path crates/zreader-cli
```

On Linux, audio playback needs the ALSA headers (`sudo apt install
libasound2-dev` / `sudo dnf install alsa-lib-devel`). To build without audio
output (WAV export only), run `cargo install --path crates/zreader-cli
--no-default-features`.

## Usage

```sh
zreader info book.epub          # title, authors, language, word count
zreader toc  book.mobi          # numbered chapter list
zreader read book.prc -c 3      # print chapter 3 as plain text

zreader speak book.epub                 # read aloud from chapter 1
zreader speak book.epub -c 5 --speed 1.2
zreader speak book.epub -c 5 --chapters 1 --out chapter5.wav
zreader say "Testing a voice" --voice en_GB-alan-medium
```

The first time you use `speak` or `say`, ZReader picks a voice for the
book's language, asks before downloading it (~20–100 MB), and asks before
downloading the Piper engine (~25 MB). Add `--yes` to skip the questions.

### Managing voices

```sh
zreader voices list --lang en         # browse the catalog
zreader voices download en_US-amy-medium
zreader voices list --installed
zreader voices remove en_US-amy-medium
zreader engine install                # fetch the Piper runtime up front
zreader engine status                 # what is installed and where
```

Engines can be chosen with `--engine piper|espeak|auto` (default `auto`:
Piper, falling back to eSpeak NG).

Downloads are stored in the platform data directory
(`~/.local/share/zreader` on Linux, `~/Library/Application
Support/org.zreader.ZReader` on macOS, `%APPDATA%\zreader\ZReader\data` on
Windows). Set `ZREADER_DATA_DIR` to use another location, or `ZREADER_PIPER`
to point at your own Piper build.

## How it works

```
crates/
  zreader-core   Book model + parsers. EPUB via the `epub` crate; MOBI/PRC
                 (Palm database, PalmDOC LZ77, HUFF/CDIC, EXTH) implemented
                 here. HTML is converted to clean paragraphs; text is split
                 into sentences for speech.
  zreader-tts    Voice catalog + downloader (Hugging Face rhasspy/piper-voices,
                 pinned to v1.0.0, MD5-verified), Piper runtime installer and
                 driver, eSpeak NG driver, WAV writer, audio player (rodio).
  zreader-cli    The `zreader` command.
```

Speech is produced one sentence at a time on a worker thread. Piper is kept
running as a single child process, so the model loads once per session, and
audio plays while the next sentence is being synthesized.

## Why Rust (and not Go)

Both were considered; Rust was chosen because:

1. **App UI options.** [Tauri 2](https://tauri.app) gives one Rust codebase
   for Windows, macOS, Linux, **Android and iOS**, with a small binary. Rust
   also has native toolkits (Slint, egui, iced). Go's desktop and mobile UI
   story (Fyne, Wails, gomobile) is thinner, especially on mobile.
2. **On-device TTS.** Neural TTS runs on ONNX Runtime and eSpeak NG, both
   C/C++ libraries. Rust links them with zero-cost FFI (`ort`, `sherpa-rs`);
   Go needs cgo, which complicates cross-compiling and mobile builds.
3. **Real-time audio.** No garbage collector means no GC pauses in the
   playback path, and `cpal`/`rodio` cover every desktop and mobile platform.
4. **Ebook parsing.** Mature crates exist (`epub`, `zip`, `quick-xml`,
   `encoding_rs`), and Rust's memory safety matters when parsing untrusted
   files from the internet.

Go would build faster and is simpler to learn, which suits a backend or a
pure CLI better than a cross-platform reading app with audio.

## Roadmap

- [ ] GUI app with Tauri 2 (desktop first, then Android/iOS) on top of
      `zreader-core` and `zreader-tts`: library view, paged reader,
      highlight-the-sentence-being-read
- [ ] Remember reading position and resume reading aloud
- [ ] Run Piper in-process (ONNX Runtime via `sherpa-rs`/`ort`) instead of a
      child process; also add Kokoro voices
- [ ] Full KF8 (AZW3) structure: chapter splitting from the NCX index
- [ ] Images and basic styling in the reader view
- [ ] FB2, TXT and (maybe) PDF text

## Licenses

ZReader is MIT licensed (see [LICENSE](LICENSE)). Downloaded components keep
their own licenses: the [Piper](https://github.com/rhasspy/piper) engine is
MIT, each [Piper voice](https://huggingface.co/rhasspy/piper-voices) has a
`MODEL_CARD` with its dataset license, and
[eSpeak NG](https://github.com/espeak-ng/espeak-ng) (GPL-3.0) is only run as
a separate program, never linked.
