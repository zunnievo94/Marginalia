//! PDF, as text: the text layer of each page is extracted and its lines
//! joined back into paragraphs. Layout, pictures and scanned pages (which
//! have no text layer) are not supported.

use crate::{Book, Chapter, Error, Format, Metadata, Result};

/// Pages per chapter: PDFs carry no reliable chapter structure.
const PAGES_PER_CHAPTER: usize = 10;

pub fn parse(bytes: &[u8], name: Option<&str>) -> Result<Book> {
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
        .map_err(|_| Error::Pdf("the file could not be read".into()))?
        .map_err(|e| Error::Pdf(e.to_string()))?;
    if pages.iter().all(|p| p.trim().is_empty()) {
        return Err(Error::Pdf(
            "it has no text to read (it may be scanned pages, which Marginalia cannot read)".into(),
        ));
    }
    let mut chapters = Vec::new();
    for (i, group) in pages.chunks(PAGES_PER_CHAPTER).enumerate() {
        let first = i * PAGES_PER_CHAPTER + 1;
        let last = first + group.len() - 1;
        let title = if first == last {
            format!("Page {first}")
        } else {
            format!("Pages {first}–{last}")
        };
        let paragraphs: Vec<String> = group.iter().flat_map(|p| reflow(p)).collect();
        chapters.push(Chapter::from_text(title, paragraphs.join("\n\n")));
    }
    Ok(Book {
        format: Format::Pdf,
        metadata: Metadata {
            title: crate::title_from_name(name),
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

/// Join a page's lines into paragraphs: a blank line ends a paragraph, and
/// so does a short line ending a sentence; a hyphen at a line end joins the
/// word back together.
fn reflow(page: &str) -> Vec<String> {
    let lines: Vec<&str> = page.lines().map(str::trim).collect();
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut out = Vec::new();
    let mut current = String::new();
    for line in lines {
        if line.is_empty() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if current.is_empty() {
            current.push_str(line);
        } else if current.ends_with('-')
            && current[..current.len() - 1].ends_with(char::is_alphabetic)
        {
            current.pop();
            current.push_str(line);
        } else if current.ends_with(is_cjk) {
            current.push_str(line);
        } else {
            current.push(' ');
            current.push_str(line);
        }
        let short = line.chars().count() * 10 < longest * 7;
        let ends_sentence = line.ends_with([
            '.', '!', '?', ':', '…', '"', '”', '»', ')', '。', '！', '？', '」',
        ]);
        if short && ends_sentence {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflows_lines_into_paragraphs() {
        let page = "This is a long line of text that wraps onto\nthe next line and ends here.\nA new para-\ngraph starts, and goes on for a while longer\nbefore it ends.\n\nLast one.";
        assert_eq!(
            reflow(page),
            [
                "This is a long line of text that wraps onto the next line and ends here.",
                "A new paragraph starts, and goes on for a while longer before it ends.",
                "Last one."
            ]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"%PDF-1.4 not really", None).is_err());
    }
}
