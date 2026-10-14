//! Plain text (`.txt`): guesses the encoding, rebuilds paragraphs and
//! splits chapters at headings such as "Chương 1", "第一章" or "Chapter 1".

use std::sync::OnceLock;

use regex::Regex;

use crate::{Book, Chapter, Format, Metadata, Result};

/// Text files have no NUL bytes (UTF-16 ones start with a BOM).
pub fn looks_like_text(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    encoding_rs::Encoding::for_bom(head).is_some() || !head.contains(&0)
}

/// Decode text in any common encoding: a BOM wins, then UTF-8, then a
/// guess (GBK/Big5 for Chinese novels, windows-125x...).
pub fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    detector
        .guess(None, chardetng::Utf8Detection::Allow)
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

pub fn parse(bytes: &[u8], name: Option<&str>) -> Result<Book> {
    let text = decode(bytes).replace("\r\n", "\n").replace('\r', "\n");
    let title = crate::title_from_name(name);
    let chapters = split_chapters(paragraphs(&text), &title);
    Ok(Book {
        format: Format::Txt,
        metadata: Metadata {
            title,
            ..Default::default()
        },
        chapters,
        images: Vec::new(),
        cover: None,
    })
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ff00}'..='\u{ffef}')
}

/// Paragraphs of a text file. Files with blank lines between paragraphs
/// may wrap lines inside a paragraph; files without them (common for
/// Chinese and Vietnamese novels) have one paragraph per line.
pub(crate) fn paragraphs(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let blank = lines.iter().filter(|l| l.trim().is_empty()).count();
    let filled = lines.len() - blank;
    if blank * 3 < filled {
        return lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() && !current.ends_with(is_cjk) {
            current.push(' ');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)^(
                第\s*[0-9０-９零〇一二三四五六七八九十百千万两]+\s*[章回节節卷集部篇幕]
              | (chương|chuong|hồi|hoi|quyển|quyen|phần|phan|tập|tap|tiết|tiet)\s+([0-9]+|[ivxlcdm]+)\b
              | (chapter|part|book)\s+([0-9]+|[ivxlcdm]+|one|two|three|four|five|six|seven|eight|nine|ten)\b
              | (prologue|epilogue|lời\s+mở\s+đầu|lời\s+tựa|vĩ\s+thanh|ngoại\s+truyện|phần\s+kết)
              | (序章|序言|楔子|尾声|尾聲|番外|后记|後記)
            )",
        )
        .expect("valid regex")
    })
}

/// Whether a paragraph looks like a chapter heading.
pub(crate) fn is_heading(p: &str) -> bool {
    p.chars().count() <= 80 && heading_re().is_match(p)
}

/// Chapters at headings; without at least two headings, parts of about
/// 5,000 words so a long file stays easy to move through.
pub(crate) fn split_chapters(paragraphs: Vec<String>, book_title: &str) -> Vec<Chapter> {
    let headings = paragraphs.iter().filter(|p| is_heading(p)).count();
    let mut chapters = Vec::new();
    let mut title = String::new();
    let mut body: Vec<String> = Vec::new();
    let mut words = 0;
    let flush = |title: &mut String, body: &mut Vec<String>, chapters: &mut Vec<Chapter>| {
        if body.is_empty() {
            return;
        }
        let t = if title.is_empty() {
            if chapters.is_empty() {
                book_title.to_string()
            } else {
                format!("Part {}", chapters.len() + 1)
            }
        } else {
            std::mem::take(title)
        };
        chapters.push(Chapter::from_text(t, std::mem::take(body).join("\n\n")));
    };
    for p in paragraphs {
        if headings >= 2 && is_heading(&p) {
            flush(&mut title, &mut body, &mut chapters);
            title = p.clone();
        } else if headings < 2 && words >= 5000 {
            flush(&mut title, &mut body, &mut chapters);
            words = 0;
        }
        words += crate::text::word_count(&p);
        body.push(p);
    }
    flush(&mut title, &mut body, &mut chapters);
    if chapters.is_empty() {
        chapters.push(Chapter::from_text(book_title.to_string(), String::new()));
    }
    chapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapters_at_headings() {
        let text = "Tác giả: Ai đó.\n\nChương 1: Gặp gỡ\nHai người gặp nhau.\nRồi chia tay.\nChương 2\nHết.";
        let book = parse(text.as_bytes(), Some("truyen_hay.txt")).unwrap();
        assert_eq!(book.metadata.title, "truyen hay");
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["truyen hay", "Chương 1: Gặp gỡ", "Chương 2"]);
        assert_eq!(book.chapters[1].paragraphs().count(), 3);
    }

    #[test]
    fn wrapped_paragraphs_and_chinese() {
        let p = paragraphs("One line\nwrapped here.\n\nNext.\n");
        assert_eq!(p, ["One line wrapped here.", "Next."]);
        let book = parse("第一章 開始\n　　他說。\n第二章\n　　好。".as_bytes(), None).unwrap();
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "第一章 開始");
        assert_eq!(book.chapters[0].paragraphs().nth(1), Some("他說。"));
    }

    #[test]
    fn legacy_encodings() {
        let (gbk, _, _) =
            encoding_rs::GBK.encode("第一章 你好，世界。这是一个测试，用来检查编码。");
        assert!(decode(&gbk).starts_with("第一章 你好"));
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("Xin chào".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode(&utf16), "Xin chào");
        assert!(looks_like_text(&utf16));
    }
}
