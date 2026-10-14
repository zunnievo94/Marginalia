//! Text utilities used by the reader and the TTS pipeline.

/// Chinese/Japanese/Korean characters, which are written without spaces
/// and so are counted (and spaced) one by one.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF)
}

/// Word count that also works for scripts without spaces: every CJK
/// character counts as one word.
pub fn word_count(text: &str) -> usize {
    let mut count = 0;
    let mut in_word = false;
    for c in text.chars() {
        if is_cjk(c) {
            count += 1;
            in_word = false;
        } else if c.is_alphanumeric() {
            if !in_word {
                count += 1;
                in_word = true;
            }
        } else if c.is_whitespace() || !matches!(c, '\'' | '’' | '-') {
            in_word = false;
        }
    }
    count
}

/// Split text into speakable chunks: sentences, never longer than
/// `max_chars` (long sentences are broken at commas or spaces).
///
/// Chunking keeps latency low (playback starts after the first sentence is
/// synthesized) and lets a reader resume at sentence granularity.
pub fn speech_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(20);
    let mut chunks = Vec::new();
    for paragraph in text.split("\n\n") {
        for sentence in sentences(paragraph) {
            split_long(&sentence, max_chars, &mut chunks);
        }
    }
    chunks
}

/// Paragraphs of a chapter, each split into sentences. This is the unit
/// used both to display text and to track what is being read aloud, so a
/// paragraph without any sentence (e.g. `* * *`) is kept whole.
pub fn paragraph_sentences(text: &str) -> Vec<Vec<String>> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(sentences_or_whole)
        .collect()
}

/// A paragraph as the reader shows it: sentences joined by a space, except
/// after a CJK sentence (Chinese/Japanese are written without spaces).
pub fn display_paragraph(sentences: &[String]) -> String {
    let mut out = String::new();
    for (i, s) in sentences.iter().enumerate() {
        if i > 0 {
            let prev_cjk = sentences[i - 1]
                .trim_end_matches(['"', '\'', '”', '’', '」', '』', '）', ')'])
                .chars()
                .last()
                // Same ranges as the reader UI's CJK_END (not Hangul:
                // Korean separates sentences with spaces).
                .is_some_and(|c| {
                    matches!(c, '\u{3000}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ff00}'..='\u{ffef}')
                });
            if !prev_cjk {
                out.push(' ');
            }
        }
        out.push_str(s);
    }
    out
}

/// Map character offsets in a paragraph to offsets in its
/// [`display_paragraph`] form, which collapses whitespace, joins sentences
/// and may drop stray punctuation. Entry `i` is where the paragraph's
/// character `i` lands (the last entry is the end of the text).
pub fn display_offsets(paragraph: &str, display: &str) -> Vec<usize> {
    let p: Vec<char> = paragraph.chars().collect();
    let d: Vec<char> = display.chars().collect();
    let mut map = Vec::with_capacity(p.len() + 1);
    let mut j = 0;
    for &c in &p {
        if !c.is_whitespace() {
            // A space the display added between sentences.
            if d.get(j).is_some_and(|x| x.is_whitespace()) && d.get(j + 1) == Some(&c) {
                j += 1;
            }
        }
        map.push(j);
        match d.get(j) {
            Some(&x) if x == c || (x.is_whitespace() && c.is_whitespace()) => j += 1,
            _ => {} // collapsed whitespace or a dropped character
        }
    }
    map.push(j);
    map
}

/// Sentences of one paragraph, or the paragraph itself if it has none.
pub fn sentences_or_whole(paragraph: &str) -> Vec<String> {
    let s = sentences(paragraph);
    if s.is_empty() {
        vec![paragraph.trim().to_string()]
    } else {
        s
    }
}

/// Split one paragraph into sentences.
pub fn sentences(paragraph: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = paragraph.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        current.push(if c == '\n' { ' ' } else { c });
        if is_terminator(c) {
            // Swallow trailing closing quotes/brackets and repeated marks.
            while i + 1 < chars.len() && (is_terminator(chars[i + 1]) || is_closer(chars[i + 1])) {
                i += 1;
                current.push(chars[i]);
            }
            let at_boundary =
                chars.get(i + 1).is_none_or(|n| n.is_whitespace()) || is_cjk_terminator(c);
            if at_boundary && !(c == '.' && ends_with_abbreviation(&current)) {
                push_trimmed(&mut out, &mut current);
            }
        }
        i += 1;
    }
    push_trimmed(&mut out, &mut current);
    out
}

fn push_trimmed(out: &mut Vec<String>, current: &mut String) {
    let s = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().any(char::is_alphanumeric) {
        out.push(s);
    }
    current.clear();
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…') || is_cjk_terminator(c)
}

fn is_cjk_terminator(c: char) -> bool {
    matches!(c, '。' | '！' | '？')
}

fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '”' | '’' | '」' | '』' | '）')
}

fn ends_with_abbreviation(s: &str) -> bool {
    const ABBREVIATIONS: &[&str] = &[
        "mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "st.", "vs.", "etc.", "e.g.", "i.e.",
        "no.", "fig.", "mt.",
    ];
    let last = s
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_start_matches(|c: char| !c.is_alphanumeric());
    // Single capital initials such as "J." in "J. R. R. Tolkien" (but not a
    // one-letter word ending a sentence, like Vietnamese "ở.").
    let is_initial = last.chars().count() == 2 && last.starts_with(char::is_uppercase);
    is_initial || ABBREVIATIONS.contains(&last.to_lowercase().as_str())
}

fn split_long(sentence: &str, max_chars: usize, out: &mut Vec<String>) {
    let mut rest = sentence.trim();
    while rest.chars().count() > max_chars {
        let limit = rest
            .char_indices()
            .nth(max_chars)
            .map_or(rest.len(), |(i, _)| i);
        let window = &rest[..limit];
        let cut = window
            .rfind([',', ';', ':', '，', '、'])
            .map(|i| i + window[i..].chars().next().map_or(1, char::len_utf8))
            .or_else(|| window.rfind(' '))
            .filter(|&i| i > 0)
            .unwrap_or(limit);
        out.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences() {
        let s = sentences("Hello there. Mr. Smith said \"Hi!\" Then left... Did he? Yes.");
        assert_eq!(
            s,
            vec![
                "Hello there.",
                "Mr. Smith said \"Hi!\"",
                "Then left...",
                "Did he?",
                "Yes."
            ]
        );
    }

    #[test]
    fn keeps_decimals_and_initials() {
        let s = sentences("Pi is 3.14 roughly. J. R. R. Tolkien wrote it.");
        assert_eq!(s, vec!["Pi is 3.14 roughly.", "J. R. R. Tolkien wrote it."]);
    }

    #[test]
    fn paragraphs_keep_separators() {
        let p = paragraph_sentences("One. Two.\n\n* * *\n\nThree.");
        assert_eq!(p, vec![vec!["One.", "Two."], vec!["* * *"], vec!["Three."]]);
    }

    #[test]
    fn cjk() {
        assert_eq!(sentences("你好。世界！"), vec!["你好。", "世界！"]);
        assert_eq!(
            sentences("他说：“你好。”然后走了。"),
            vec!["他说：“你好。”", "然后走了。"]
        );
    }

    #[test]
    fn vietnamese() {
        assert_eq!(
            sentences("Tôi ở đây. Anh ở. Ông Nguyễn đến lúc 3.5 giờ chiều!"),
            vec!["Tôi ở đây.", "Anh ở.", "Ông Nguyễn đến lúc 3.5 giờ chiều!"]
        );
    }

    #[test]
    fn counts_words_in_any_script() {
        assert_eq!(word_count("Hello, world! It's fine."), 4);
        assert_eq!(word_count("Xin chào thế giới."), 4);
        assert_eq!(word_count("你好，世界。"), 4);
        assert_eq!(word_count("Marginalia 读书"), 3);
    }

    #[test]
    fn long_chinese_sentence_is_split_at_commas() {
        let s = "这是一个很长的句子，".repeat(30) + "结束。";
        let chunks = speech_chunks(&s, 50);
        assert!(chunks.iter().all(|c| c.chars().count() <= 50));
        assert!(chunks[0].ends_with('，'));
        assert_eq!(chunks.concat(), s);
    }

    #[test]
    fn chunks_are_bounded() {
        let long = "word ".repeat(100);
        let chunks = speech_chunks(&long, 50);
        assert!(chunks.iter().all(|c| c.chars().count() <= 50));
        assert_eq!(chunks.join(" ").split_whitespace().count(), 100);
    }

    #[test]
    fn display_offsets_follow_collapsed_text() {
        let p = "Một  ngày.\nHai câu!";
        let d = display_paragraph(&sentences_or_whole(p));
        assert_eq!(d, "Một ngày. Hai câu!");
        let map = display_offsets(p, &d);
        let pc: Vec<char> = p.chars().collect();
        let dc: Vec<char> = d.chars().collect();
        // "ngày" and "câu" keep their place.
        let at = |w: &str| {
            pc.iter()
                .collect::<String>()
                .find(w)
                .map(|b| p[..b].chars().count())
                .unwrap()
        };
        for w in ["ngày", "Hai", "câu"] {
            let s = map[at(w)];
            assert_eq!(dc[s..s + w.chars().count()].iter().collect::<String>(), w);
        }
        assert_eq!(*map.last().unwrap(), dc.len());
        // CJK sentences are joined without a space.
        let p = "你好。再見。";
        let d = display_paragraph(&sentences_or_whole(p));
        assert_eq!(display_offsets(p, &d)[3], 3);
    }
}
