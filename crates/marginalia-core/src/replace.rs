//! Find-and-replace rules applied to a book's text, for renaming
//! characters ("Trương Cao Thiên" → "Trương Cao Trời") or fixing how a
//! word is read aloud. The book file is never changed.
//!
//! All rules run in one pass: at each position the longest matching rule
//! wins, and replaced text is never matched again, so rules do not chain.

use regex::{Captures, Regex};

use crate::{Block, Book};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub find: String,
    pub replace: String,
    /// Only match whole words: "Thiên" does not match inside "Thiênhạ".
    pub whole_word: bool,
    /// Match upper and lower case exactly.
    pub match_case: bool,
}

pub struct Replacer {
    regex: Option<Regex>,
    /// Replacement for each capture group, in group order.
    replacements: Vec<String>,
}

impl Replacer {
    /// Compile `rules`; rules with an empty `find` are ignored. Line breaks
    /// in replacements become spaces so paragraphs stay as they are.
    pub fn new(rules: &[Rule]) -> Replacer {
        let mut rules: Vec<&Rule> = rules.iter().filter(|r| !r.find.trim().is_empty()).collect();
        // Longest first, so the alternation prefers "Trương Cao Thiên"
        // over "Thiên" at the same position.
        rules.sort_by_key(|r| std::cmp::Reverse(r.find.chars().count()));
        let pattern = rules
            .iter()
            .map(|r| {
                let mut p = regex::escape(r.find.trim());
                if r.whole_word {
                    p = format!(r"\b{p}\b");
                }
                if !r.match_case {
                    p = format!("(?i:{p})");
                }
                format!("({p})")
            })
            .collect::<Vec<_>>()
            .join("|");
        Replacer {
            regex: (!rules.is_empty()).then(|| Regex::new(&pattern).expect("escaped pattern")),
            replacements: rules
                .iter()
                .map(|r| r.replace.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regex.is_none()
    }

    pub fn apply(&self, text: &str) -> String {
        self.apply_mapped(text).0
    }

    fn replacement(&self, caps: &Captures) -> &str {
        (1..caps.len())
            .find(|&i| caps.get(i).is_some())
            .map_or("", |i| &self.replacements[i - 1])
    }

    /// Replace, and return how character offsets move: for each match,
    /// (old start, old end, new start, new end) in characters.
    fn apply_mapped(&self, text: &str) -> (String, Vec<[usize; 4]>) {
        let Some(regex) = &self.regex else {
            return (text.to_string(), Vec::new());
        };
        let mut out = String::with_capacity(text.len());
        let mut moves = Vec::new();
        let (mut last, mut old_chars, mut new_chars) = (0usize, 0usize, 0usize);
        for caps in regex.captures_iter(text) {
            let m = caps.get(0).expect("whole match");
            let between = &text[last..m.start()];
            out.push_str(between);
            let n = between.chars().count();
            old_chars += n;
            new_chars += n;
            let rep = self.replacement(&caps);
            out.push_str(rep);
            let (old_len, new_len) = (m.as_str().chars().count(), rep.chars().count());
            moves.push([
                old_chars,
                old_chars + old_len,
                new_chars,
                new_chars + new_len,
            ]);
            old_chars += old_len;
            new_chars += new_len;
            last = m.end();
        }
        out.push_str(&text[last..]);
        (out, moves)
    }

    /// Number of matches in `text`.
    pub fn count(&self, text: &str) -> usize {
        self.regex.as_ref().map_or(0, |r| r.find_iter(text).count())
    }

    /// Apply the rules to every chapter title, paragraph and image caption.
    pub fn apply_to_book(&self, book: &mut Book) {
        if self.is_empty() {
            return;
        }
        for chapter in &mut book.chapters {
            chapter.title = self.apply(&chapter.title);
            chapter.text = self.apply(&chapter.text);
            let mut paragraph = 0;
            for block in &mut chapter.blocks {
                match block {
                    Block::Text(t) => {
                        let (new, moves) = self.apply_mapped(t);
                        *t = new;
                        if let Some(style) = chapter.styles.get_mut(paragraph) {
                            for span in &mut style.spans {
                                span.start = moved(&moves, span.start, true);
                                span.end = moved(&moves, span.end, false);
                            }
                            style.spans.retain(|s| s.start < s.end);
                        }
                        paragraph += 1;
                    }
                    Block::Image { alt, .. } => *alt = self.apply(alt),
                    Block::Table(table) => {
                        for cell in table.rows.iter_mut().flatten() {
                            *cell = self.apply(cell);
                        }
                    }
                }
            }
        }
    }
}

/// Where character offset `at` lands after the replacements in `moves`.
/// An offset inside a replaced stretch moves to its start (for a span's
/// start) or its end (for a span's end), so the styling covers it.
fn moved(moves: &[[usize; 4]], at: usize, start: bool) -> usize {
    let Some(&[os, oe, ns, ne]) = moves.iter().rev().find(|m| m[0] <= at) else {
        return at;
    };
    if at == os {
        ns
    } else if at < oe {
        if start {
            ns
        } else {
            ne
        }
    } else {
        ne + (at - oe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(find: &str, replace: &str, whole_word: bool, match_case: bool) -> Rule {
        Rule {
            find: find.into(),
            replace: replace.into(),
            whole_word,
            match_case,
        }
    }

    #[test]
    fn longest_rule_wins_and_rules_do_not_chain() {
        let r = Replacer::new(&[
            rule("Thiên", "Trời", true, false),
            rule("Trương Cao Thiên", "Trương Cao Trời", true, false),
            rule("Trời", "Đất", true, false),
        ]);
        assert_eq!(
            r.apply("Trương Cao Thiên nhìn trời. Thiên hạ thái bình."),
            "Trương Cao Trời nhìn Đất. Trời hạ thái bình."
        );
    }

    #[test]
    fn whole_word_and_case() {
        let whole = Replacer::new(&[rule("an", "AN", true, true)]);
        assert_eq!(whole.apply("an bình, bàn an, An"), "AN bình, bàn AN, An");
        let any = Replacer::new(&[rule("an", "x", false, false)]);
        assert_eq!(any.apply("An banh"), "x bxh");
        // Vietnamese letters count as word characters.
        let vi = Replacer::new(&[rule("Hà", "Hồ", true, true)]);
        assert_eq!(vi.apply("Hà Hàn Hà."), "Hồ Hàn Hồ.");
    }

    #[test]
    fn special_characters_are_literal() {
        let r = Replacer::new(&[
            rule("TP.HCM", "thành phố Hồ Chí Minh", false, true),
            rule("(?)", "", false, true),
        ]);
        assert_eq!(r.apply("Ở TP.HCM (?)"), "Ở thành phố Hồ Chí Minh ");
        assert_eq!(r.apply("TPxHCM"), "TPxHCM");
    }

    #[test]
    fn styles_follow_replacements() {
        use crate::{Chapter, Inline, ParaStyle, Span};
        let mut book = crate::Book {
            format: crate::Format::Epub,
            metadata: Default::default(),
            chapters: vec![Chapter::from_text("c".into(), "Thiên nói với Lan.".into())],
            images: Vec::new(),
            cover: None,
        };
        // "Lan" is italic: characters 14..17.
        book.chapters[0].styles = vec![ParaStyle {
            spans: vec![Span {
                kind: Inline::Italic,
                start: 14,
                end: 17,
            }],
            ..Default::default()
        }];
        Replacer::new(&[rule("Thiên", "Trương Cao Trời", true, true)]).apply_to_book(&mut book);
        let Block::Text(t) = &book.chapters[0].blocks[0] else {
            panic!()
        };
        let s = book.chapters[0].styles[0].spans[0];
        let italic: String = t.chars().skip(s.start).take(s.end - s.start).collect();
        assert_eq!(italic, "Lan");
        // A styled word that is itself replaced stays styled.
        Replacer::new(&[rule("Lan", "Ngọc Lan", true, true)]).apply_to_book(&mut book);
        let Block::Text(t) = &book.chapters[0].blocks[0] else {
            panic!()
        };
        let s = book.chapters[0].styles[0].spans[0];
        let italic: String = t.chars().skip(s.start).take(s.end - s.start).collect();
        assert_eq!(italic, "Ngọc Lan");
    }

    #[test]
    fn replacements_keep_paragraphs() {
        let r = Replacer::new(&[rule("a", "b\n\nc", false, true)]);
        assert_eq!(r.apply("a"), "b c");
        assert!(Replacer::new(&[rule("  ", "x", false, true)]).is_empty());
    }
}
