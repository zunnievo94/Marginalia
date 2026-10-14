//! Find the proper names a book uses most: runs of two to four capitalised
//! words ("Lâm Phong", "Trương Cao Thiên", "Thanh Vân Môn"), counted and
//! grouped by their first word, so that a character translated in several
//! ways ("Trương Cao Thiên" / "Trương Cao Trời") shows up side by side.

use std::collections::HashMap;

use crate::{Block, Book};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub name: String,
    pub count: usize,
}

/// Names sharing their first word, most frequent first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameGroup {
    pub key: String,
    pub total: usize,
    pub names: Vec<Name>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '’' || c == '-'
}

fn capitalised(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
        // Not an all-caps shout or abbreviation ("TP.HCM", "OK").
        && word.chars().filter(|c| c.is_alphabetic()).count() > 1
        && word.chars().skip(1).any(char::is_lowercase)
}

/// Words of a paragraph, each with whether it starts a sentence. Any
/// punctuation between words is kept as an empty word, so that a name
/// never runs across a comma or a full stop.
fn words(text: &str) -> Vec<(&str, bool)> {
    const SENTENCE: &[char] = &[
        '.', '!', '?', ':', '…', '"', '“', '”', '«', '»', '(', '[', '—', '\n',
    ];
    let mut out = Vec::new();
    let mut sentence_start = true;
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_word_char(c) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            out.push((&text[s..i], sentence_start));
            sentence_start = false;
        }
        if !c.is_whitespace() {
            out.push(("", false));
            if SENTENCE.contains(&c) {
                sentence_start = true;
            }
        }
    }
    if let Some(s) = start {
        out.push((&text[s..], sentence_start));
    }
    out
}

/// The most frequent names of a book: at most `limit` groups, each name
/// seen at least `min_count` times.
pub fn names(book: &Book, min_count: usize, limit: usize) -> Vec<NameGroup> {
    let paragraphs: Vec<&str> = book
        .chapters
        .iter()
        .flat_map(|c| &c.blocks)
        .filter_map(|b| match b {
            Block::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();

    // Words also written in lower case are ordinary words when they are
    // capitalised only because they start a sentence.
    let mut lower: HashMap<String, usize> = HashMap::new();
    for p in &paragraphs {
        for (w, _) in words(p) {
            if w.chars().next().is_some_and(char::is_lowercase) {
                *lower.entry(w.to_string()).or_default() += 1;
            }
        }
    }
    let common = |w: &str| lower.get(&w.to_lowercase()).copied().unwrap_or(0) >= 3;

    let mut counts: HashMap<String, usize> = HashMap::new();
    for p in &paragraphs {
        let ws = words(p);
        let mut i = 0;
        while i < ws.len() {
            if !capitalised(ws[i].0) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < ws.len() && capitalised(ws[j].0) && j - i < 4 {
                j += 1;
            }
            let mut run = &ws[i..j];
            // "Nhưng Lâm Phong": drop a sentence-initial ordinary word.
            if run[0].1 && common(run[0].0) {
                run = &run[1..];
            }
            if run.len() >= 2 {
                let name = run.iter().map(|(w, _)| *w).collect::<Vec<_>>().join(" ");
                *counts.entry(name).or_default() += 1;
            }
            i = j.max(i + 1);
        }
    }

    let mut groups: HashMap<String, Vec<Name>> = HashMap::new();
    for (name, count) in counts {
        if count < min_count {
            continue;
        }
        let key = name.split(' ').next().unwrap_or_default().to_string();
        groups.entry(key).or_default().push(Name { name, count });
    }
    let mut out: Vec<NameGroup> = groups
        .into_iter()
        .map(|(key, mut names)| {
            names.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
            NameGroup {
                total: names.iter().map(|n| n.count).sum(),
                key,
                names,
            }
        })
        .collect();
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.key.cmp(&b.key)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chapter;

    fn book(text: &str) -> Book {
        Book {
            format: crate::Format::Txt,
            metadata: Default::default(),
            chapters: vec![Chapter::from_text("c".into(), text.into())],
            images: Vec::new(),
            cover: None,
        }
    }

    #[test]
    fn finds_and_groups_names() {
        let text = "Trương Cao Thiên nhìn Lâm Phong. Nhưng Lâm Phong không nói.\n\n\
            Trương Cao Trời cười. Trương Cao Thiên đi về Thanh Vân Môn, nhưng ai cũng biết, \
            nhưng thôi, nhưng mà. Lâm Phong, Trương Cao Thiên! TP.HCM OK Hắn.";
        let groups = names(&book(text), 1, 10);
        let flat: Vec<(String, usize)> = groups
            .iter()
            .flat_map(|g| g.names.iter().map(|n| (n.name.clone(), n.count)))
            .collect();
        assert!(flat.contains(&("Trương Cao Thiên".into(), 3)));
        assert!(flat.contains(&("Lâm Phong".into(), 3)), "{flat:?}");
        assert!(flat.contains(&("Thanh Vân Môn".into(), 1)));
        assert!(!flat.iter().any(|(n, _)| n.starts_with("Nhưng")));
        assert!(!flat
            .iter()
            .any(|(n, _)| n.contains("TP") || n.contains("OK")));
        let truong = groups.iter().find(|g| g.key == "Trương").unwrap();
        assert_eq!(truong.names[0].name, "Trương Cao Thiên");
        assert_eq!(truong.names[1].name, "Trương Cao Trời");
        assert_eq!(truong.total, 4);
    }
}
