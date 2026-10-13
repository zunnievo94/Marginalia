//! Text utilities used by the TTS pipeline.

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
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    // Single initials such as "J." in "J. R. R. Tolkien".
    let is_initial = last.chars().count() == 2 && last.starts_with(char::is_alphabetic);
    is_initial || ABBREVIATIONS.contains(&last.as_str())
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
    fn cjk() {
        assert_eq!(sentences("你好。世界！"), vec!["你好。", "世界！"]);
    }

    #[test]
    fn chunks_are_bounded() {
        let long = "word ".repeat(100);
        let chunks = speech_chunks(&long, 50);
        assert!(chunks.iter().all(|c| c.chars().count() <= 50));
        assert_eq!(chunks.join(" ").split_whitespace().count(), 100);
    }
}
