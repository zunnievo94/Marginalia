//! Full-text search inside a book.
//!
//! Matching ignores case and diacritics ("thung lung" finds "thung lũng",
//! "dang" finds "Đặng"), so readers can type without accents.

use unicode_normalization::char::decompose_canonical;

use crate::text::{display_paragraph, sentences_or_whole};
use crate::{Block, Book};

/// One match. Offsets are character offsets into the paragraph's display
/// text (see [`display_paragraph`]); `paragraph` counts text blocks only,
/// like reading positions do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub chapter: usize,
    pub paragraph: usize,
    pub start: usize,
    pub end: usize,
    pub before: String,
    pub matched: String,
    pub after: String,
}

/// Fold one character for matching: lower-case, without combining marks.
fn fold_char(c: char, out: &mut String) {
    let mut base = None;
    decompose_canonical(c, |d| {
        if base.is_none() {
            base = Some(d);
        }
    });
    let base = match base.unwrap_or(c) {
        'đ' | 'Đ' => 'd',
        b => b,
    };
    out.extend(base.to_lowercase());
}

/// Fold a string, returning it with a map from folded char index to the
/// original char index.
fn fold(s: &str) -> (Vec<char>, Vec<usize>) {
    let mut chars = Vec::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len());
    let mut buf = String::new();
    for (i, c) in s.chars().enumerate() {
        buf.clear();
        fold_char(c, &mut buf);
        for f in buf.chars() {
            chars.push(f);
            map.push(i);
        }
    }
    (chars, map)
}

/// Folded form of a query, with whitespace collapsed.
pub fn fold_query(q: &str) -> Vec<char> {
    let collapsed = q.split_whitespace().collect::<Vec<_>>().join(" ");
    fold(&collapsed).0
}

/// Find up to `limit` matches of `query` in reading order.
pub fn search(book: &Book, query: &str, limit: usize) -> Vec<Hit> {
    const CONTEXT: usize = 40;
    let needle = fold_query(query);
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (ci, chapter) in book.chapters.iter().enumerate() {
        let texts = chapter.blocks.iter().filter_map(|b| match b {
            Block::Text(t) => Some(t),
            Block::Image { .. } => None,
        });
        for (pi, text) in texts.enumerate() {
            let display = display_paragraph(&sentences_or_whole(text));
            let (hay, map) = fold(&display);
            if hay.len() < needle.len() {
                continue;
            }
            let chars: Vec<char> = display.chars().collect();
            let mut i = 0;
            while i + needle.len() <= hay.len() {
                if hay[i..i + needle.len()] == needle[..] {
                    let start = map[i];
                    let end = map[i + needle.len() - 1] + 1;
                    let from = start.saturating_sub(CONTEXT);
                    let to = (end + CONTEXT).min(chars.len());
                    hits.push(Hit {
                        chapter: ci,
                        paragraph: pi,
                        start,
                        end,
                        before: chars[from..start].iter().collect(),
                        matched: chars[start..end].iter().collect(),
                        after: chars[end..to].iter().collect(),
                    });
                    if hits.len() >= limit {
                        return hits;
                    }
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chapter, Format, Metadata};

    fn book(chapters: &[&str]) -> Book {
        Book {
            format: Format::Epub,
            metadata: Metadata::default(),
            chapters: chapters
                .iter()
                .map(|t| Chapter::from_text("c".into(), t.to_string()))
                .collect(),
            images: Vec::new(),
            cover: None,
        }
    }

    #[test]
    fn ignores_case_and_diacritics() {
        let b = book(&[
            "Buổi sáng, sương mù phủ kín thung lũng.\n\nĐặng Thùy Trâm.",
            "Không có gì.",
        ]);
        let hits = search(&b, "THUNG lung", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched, "thung lũng");
        assert_eq!((hits[0].chapter, hits[0].paragraph), (0, 0));
        assert_eq!(hits[0].before, "Buổi sáng, sương mù phủ kín ");
        let d = search(&b, "dang thuy", 10);
        assert_eq!(d[0].matched, "Đặng Thùy");
        assert_eq!(d[0].paragraph, 1);
        assert_eq!(search(&b, "khong", 10)[0].chapter, 1);
    }

    #[test]
    fn offsets_index_display_text() {
        let b = book(&["One.  Two words\nhere. One more."]);
        let hits = search(&b, "one", 10);
        assert_eq!(hits.len(), 2);
        let display: Vec<char> = "One. Two words here. One more.".chars().collect();
        for h in &hits {
            let s: String = display[h.start..h.end].iter().collect();
            assert_eq!(s, h.matched);
        }
    }

    #[test]
    fn cjk_and_limits() {
        let b = book(&["桃花源記。桃花林。", "桃花"]);
        assert_eq!(search(&b, "桃花", 10).len(), 3);
        assert_eq!(search(&b, "桃花", 2).len(), 2);
        assert!(search(&b, "  ", 10).is_empty());
    }
}
