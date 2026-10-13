//! Persistent app state: the library and the settings, stored as JSON in
//! the ZReader data directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

fn file(name: &str) -> Result<PathBuf> {
    Ok(zreader_tts::paths::data_dir()?.join(name))
}

fn load<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    file(name)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let path = file(name)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Write-then-rename so a crash never leaves a half-written file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where the reader is in a book.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Position {
    pub chapter: usize,
    pub paragraph: usize,
    pub sentence: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub format: String,
    pub chapters: usize,
    pub added: u64,
    pub last_opened: Option<u64>,
    #[serde(default)]
    pub position: Position,
    /// Fraction of the book read, 0..=1.
    #[serde(default)]
    pub progress: f32,
    #[serde(default)]
    pub has_cover: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    pub books: Vec<Entry>,
}

impl Library {
    pub fn load() -> Library {
        load("library.json")
    }

    pub fn save(&self) -> Result<()> {
        save("library.json", self)
    }

    pub fn id_for(path: &Path) -> String {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        // FNV-1a: stable across Rust versions, unlike DefaultHasher.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in canonical.to_string_lossy().bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.books.iter().find(|b| b.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.books.iter_mut().find(|b| b.id == id)
    }

    /// Most recently opened first, then most recently added.
    pub fn sorted(&self) -> Vec<Entry> {
        let mut books = self.books.clone();
        books.sort_by(|a, b| {
            b.last_opened
                .cmp(&a.last_opened)
                .then(b.added.cmp(&a.added))
        });
        books
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `dark`, `light` or `sepia`.
    pub theme: String,
    /// `serif` or `sans`.
    pub font: String,
    pub font_size: u32,
    pub line_height: f32,
    /// Reading column width in characters.
    pub width: u32,
    pub sidebar_width: u32,
    /// Piper voice key; `None` picks one for the book's language.
    pub voice: Option<String>,
    pub speed: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "dark".into(),
            font: "serif".into(),
            font_size: 19,
            line_height: 1.7,
            width: 70,
            sidebar_width: 280,
            voice: None,
            speed: 1.0,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        load("settings.json")
    }

    pub fn save(&self) -> Result<()> {
        save("settings.json", self)
    }
}
