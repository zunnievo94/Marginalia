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
        let Some(regex) = &self.regex else {
            return text.to_string();
        };
        regex
            .replace_all(text, |caps: &Captures| {
                (1..caps.len())
                    .find(|&i| caps.get(i).is_some())
                    .map(|i| self.replacements[i - 1].clone())
                    .unwrap_or_default()
            })
            .into_owned()
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
            for block in &mut chapter.blocks {
                match block {
                    Block::Text(t) => *t = self.apply(t),
                    Block::Image { alt, .. } => *alt = self.apply(alt),
                }
            }
        }
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
    fn replacements_keep_paragraphs() {
        let r = Replacer::new(&[rule("a", "b\n\nc", false, true)]);
        assert_eq!(r.apply("a"), "b c");
        assert!(Replacer::new(&[rule("  ", "x", false, true)]).is_empty());
    }
}
