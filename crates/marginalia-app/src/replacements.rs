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

/// Read a rule list: one `find=replace` per line (`→`, `=>` or a tab also
/// separate them). Empty lines and lines starting with `#` or `//` are
/// skipped.
pub fn parse_list(text: &str) -> Vec<Rule> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"))
        .filter_map(|l| {
            let (find, replace) = ["=>", "→", "=", "\t"]
                .iter()
                .find_map(|sep| l.split_once(sep))?;
            let find = find.trim();
            (!find.is_empty()).then(|| Rule {
                find: find.to_string(),
                replace: replace.trim().to_string(),
                whole_word: true,
                match_case: false,
                enabled: true,
            })
        })
        .collect()
}

/// Add `new` rules to `rules`; a rule for the same text replaces the old one.
pub fn merge(rules: &mut Vec<Rule>, new: Vec<Rule>) -> usize {
    let n = new.len();
    for rule in new {
        match rules.iter_mut().find(|r| r.find.trim() == rule.find.trim()) {
            Some(old) => old.replace = rule.replace,
            None => rules.push(rule),
        }
    }
    n
}

/// Write rules as a list `parse_list` reads back.
pub fn to_list(title: &str, rules: &[Rule]) -> String {
    let mut out =
        format!("# Marginalia replacements: {title}\n# One rule per line: find=replace\n\n");
    for r in rules {
        if !r.enabled {
            out.push_str("# (off) ");
        }
        out.push_str(&format!("{}={}\n", r.find.trim(), r.replace.trim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_lists() {
        let rules = parse_list("# names\nTrương Cao Thiên=Trương Cao Trời\n\nTP.HCM => thành phố Hồ Chí Minh\nA→B\nx\ty\nno separator\n");
        let pairs: Vec<(&str, &str)> = rules
            .iter()
            .map(|r| (r.find.as_str(), r.replace.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("Trương Cao Thiên", "Trương Cao Trời"),
                ("TP.HCM", "thành phố Hồ Chí Minh"),
                ("A", "B"),
                ("x", "y")
            ]
        );
        let mut list = vec![rules[0].clone()];
        merge(
            &mut list,
            parse_list("Trương Cao Thiên=Trương Tam\nLâm=Lam"),
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].replace, "Trương Tam");
        assert_eq!(parse_list(&to_list("Book", &list)), list);
    }
}
