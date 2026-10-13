//! Notes ("marginalia") attached to a passage or a whole paragraph.
//! Stored per book as `notes/<book id>.json` in the data directory, so they
//! survive removing and re-adding the book.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Where a note is anchored. Offsets are character offsets into the
/// paragraph text as displayed; `quote` is kept as well so the anchor can
/// be found again if the text layout ever changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Anchor {
    pub chapter: usize,
    pub paragraph: usize,
    /// `None` = the whole paragraph.
    pub start: Option<usize>,
    /// Last paragraph of a selection spanning several paragraphs.
    #[serde(default)]
    pub end_paragraph: Option<usize>,
    pub end: Option<usize>,
    #[serde(default)]
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// Assigned on first save.
    #[serde(default)]
    pub id: String,
    pub anchor: Anchor,
    pub text: String,
    /// `amber`, `green`, `blue` or `rose`.
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub updated: u64,
}

fn default_color() -> String {
    "amber".into()
}

fn file(book_id: &str) -> Result<PathBuf> {
    // Book ids are hex hashes, but never trust them as path components.
    let safe: String = book_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Ok(data_dir()?.join("notes").join(format!("{safe}.json")))
}

fn data_dir() -> Result<PathBuf> {
    marginalia_tts::paths::data_dir()
}

/// Notes of a book, in reading order.
pub fn load(book_id: &str) -> Vec<Note> {
    let mut notes: Vec<Note> = file(book_id)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    sort(&mut notes);
    notes
}

fn sort(notes: &mut [Note]) {
    notes.sort_by_key(|n| {
        (
            n.anchor.chapter,
            n.anchor.paragraph,
            n.anchor.start.unwrap_or(0),
            n.created,
        )
    });
}

fn store(book_id: &str, notes: &[Note]) -> Result<()> {
    let path = file(book_id)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(notes)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

/// Create (empty `id`) or update a note; returns it as stored.
pub fn save(book_id: &str, mut note: Note) -> Result<Note> {
    let mut notes = load(book_id);
    let now = crate::store::now();
    note.updated = now;
    match notes
        .iter_mut()
        .find(|n| !note.id.is_empty() && n.id == note.id)
    {
        Some(existing) => {
            note.created = existing.created;
            *existing = note.clone();
        }
        None => {
            note.created = now;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            note.id = format!("{nanos:x}");
            while notes.iter().any(|n| n.id == note.id) {
                note.id.push('x');
            }
            notes.push(note.clone());
        }
    }
    sort(&mut notes);
    store(book_id, &notes)?;
    Ok(note)
}

pub fn delete(book_id: &str, note_id: &str) -> Result<()> {
    let mut notes = load(book_id);
    notes.retain(|n| n.id != note_id);
    store(book_id, &notes)
}

/// All notes of a book as Markdown, grouped by chapter.
pub fn to_markdown(title: &str, authors: &[String], chapters: &[String], notes: &[Note]) -> String {
    let mut md = format!("# {title} — notes\n\n");
    if !authors.is_empty() {
        md.push_str(&format!("*{}*\n\n", authors.join(", ")));
    }
    let mut chapter = None;
    for n in notes {
        if chapter != Some(n.anchor.chapter) {
            chapter = Some(n.anchor.chapter);
            let name = chapters
                .get(n.anchor.chapter)
                .cloned()
                .unwrap_or_else(|| format!("Chapter {}", n.anchor.chapter + 1));
            md.push_str(&format!("## {name}\n\n"));
        }
        let quote = n.anchor.quote.trim();
        if !quote.is_empty() {
            for line in quote.lines() {
                md.push_str(&format!("> {line}\n"));
            }
            md.push('\n');
        }
        if !n.text.trim().is_empty() {
            md.push_str(n.text.trim());
            md.push_str("\n\n");
        }
        md.push_str("---\n\n");
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(chapter: usize, quote: &str, text: &str) -> Note {
        Note {
            id: String::new(),
            anchor: Anchor {
                chapter,
                paragraph: 0,
                start: None,
                end_paragraph: None,
                end: None,
                quote: quote.into(),
            },
            text: text.into(),
            color: default_color(),
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn markdown_export() {
        let md = to_markdown(
            "Book",
            &["Ann".into()],
            &["One".into(), "Two".into()],
            &[
                note(0, "a line\nnext", "my note"),
                note(1, "", "paragraph note"),
            ],
        );
        assert_eq!(
            md,
            "# Book — notes\n\n*Ann*\n\n## One\n\n> a line\n> next\n\nmy note\n\n---\n\n\
             ## Two\n\nparagraph note\n\n---\n\n"
        );
    }

    #[test]
    fn ids_never_escape_the_notes_dir() {
        let p = file("../../etc/passwd").unwrap();
        assert_eq!(p.file_name().unwrap(), "etcpasswd.json");
    }
}
