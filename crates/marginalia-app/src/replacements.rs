//! Find-and-replace rules: per book in `replace/<book id>.json` and for
//! every book in `replace/_all.json`, in the data directory. They change
//! what is shown, searched and read aloud, never the book file.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use marginalia_core::replace::{Replacer, Rule as CoreRule};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub find: String,
    #[serde(default)]
    pub replace: String,
    #[serde(default = "yes")]
    pub whole_word: bool,
    #[serde(default)]
    pub match_case: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rules {
    /// Rules for this book.
    pub book: Vec<Rule>,
    /// Rules for every book.
    pub all: Vec<Rule>,
}

const ALL: &str = "_all";

fn file(name: &str) -> Result<PathBuf> {
    // Book ids are hex hashes, but never trust them as path components.
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Ok(marginalia_tts::paths::data_dir()?
        .join("replace")
        .join(format!("{safe}.json")))
}

fn load_list(name: &str) -> Vec<Rule> {
    file(name)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_list(name: &str, rules: &[Rule]) -> Result<()> {
    let path = file(name)?;
    if rules.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(rules)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn load(book_id: &str) -> Rules {
    Rules {
        book: load_list(book_id),
        all: load_list(ALL),
    }
}

pub fn save(book_id: &str, rules: &Rules) -> Result<()> {
    save_list(book_id, &rules.book)?;
    save_list(ALL, &rules.all)
}

/// The enabled rules for a book, ready to apply; book rules come first so
/// that, for the same text, they win over rules for every book.
pub fn replacer(book_id: &str) -> Replacer {
    let rules = load(book_id);
    let mut seen = std::collections::HashSet::new();
    let active: Vec<CoreRule> = rules
        .book
        .iter()
        .chain(&rules.all)
        .filter(|r| {
            r.enabled && seen.insert((r.find.trim().to_lowercase(), r.match_case, r.whole_word))
        })
        .map(|r| CoreRule {
            find: r.find.clone(),
            replace: r.replace.clone(),
            whole_word: r.whole_word,
            match_case: r.match_case,
        })
        .collect();
    Replacer::new(&active)
}
