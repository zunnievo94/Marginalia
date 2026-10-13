//! Bookmarks: saved reading positions, stored per book as
//! `bookmarks/<book id>.json` in the data directory.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::Position;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub position: Position,
    /// The start of the bookmarked paragraph, for the list.
    pub label: String,
    pub created: u64,
}

fn file(book_id: &str) -> Result<PathBuf> {
    let safe: String = book_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Ok(marginalia_tts::paths::data_dir()?
        .join("bookmarks")
        .join(format!("{safe}.json")))
}

pub fn load(book_id: &str) -> Vec<Bookmark> {
    let mut list: Vec<Bookmark> = file(book_id)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    list.sort_by_key(|b| {
        (
            b.position.chapter,
            b.position.paragraph,
            b.position.sentence,
        )
    });
    list
}

fn store(book_id: &str, list: &[Bookmark]) -> Result<()> {
    let path = file(book_id)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

/// Add a bookmark (one per paragraph: adding again returns the existing one).
pub fn add(book_id: &str, position: Position, label: String) -> Result<Bookmark> {
    let mut list = load(book_id);
    if let Some(existing) = list.iter().find(|b| {
        b.position.chapter == position.chapter && b.position.paragraph == position.paragraph
    }) {
        return Ok(existing.clone());
    }
    let now = crate::store::now();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let label: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = match label.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &label[..i]),
        None => label,
    };
    let bookmark = Bookmark {
        id: format!("{nanos:x}"),
        position,
        label,
        created: now,
    };
    list.push(bookmark.clone());
    store(book_id, &list)?;
    Ok(bookmark)
}

pub fn remove(book_id: &str, bookmark_id: &str) -> Result<()> {
    let mut list = load(book_id);
    list.retain(|b| b.id != bookmark_id);
    store(book_id, &list)
}
