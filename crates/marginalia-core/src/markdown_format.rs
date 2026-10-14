//! Markdown (`.md`): rendered to HTML and converted like any other book,
//! one chapter per top-level heading.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use crate::{Book, Chapter, Format, Metadata, Result};

pub fn parse(bytes: &[u8], name: Option<&str>) -> Result<Book> {
    let text = crate::txt_format::decode(bytes);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_SMART_PUNCTUATION;
    let events: Vec<Event> = Parser::new_ext(&text, options).collect();

    // One H1 is the book's title and chapters start at H2; several H1s
    // are chapters themselves.
    let level_of = |e: &Event| match e {
        Event::Start(Tag::Heading { level, .. }) => Some(*level),
        _ => None,
    };
    let h1 = events
        .iter()
        .filter(|e| level_of(e) == Some(HeadingLevel::H1))
        .count();
    let split = if h1 >= 2 {
        HeadingLevel::H1
    } else {
        HeadingLevel::H2
    };

    let mut title = crate::title_from_name(name);
    if h1 == 1 {
        let at = events
            .iter()
            .position(|e| level_of(e) == Some(HeadingLevel::H1))
            .unwrap_or(0);
        let heading: String = events[at..]
            .iter()
            .take_while(|e| !matches!(e, Event::End(pulldown_cmark::TagEnd::Heading(_))))
            .filter_map(|e| match e {
                Event::Text(t) | Event::Code(t) => Some(t.to_string()),
                _ => None,
            })
            .collect();
        if !heading.trim().is_empty() {
            title = heading.trim().to_string();
        }
    }

    let mut parts: Vec<Vec<Event>> = vec![Vec::new()];
    for e in events {
        if level_of(&e) == Some(split) && parts.last().is_some_and(|p| !p.is_empty()) {
            parts.push(Vec::new());
        }
        parts.last_mut().expect("never empty").push(e);
    }

    let mut chapters = Vec::new();
    for part in parts {
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, part.into_iter());
        let converted = crate::html::convert(&html);
        if converted.blocks.is_empty() {
            continue;
        }
        let chapter_title = converted.heading.clone().unwrap_or_else(|| {
            if chapters.is_empty() {
                title.clone()
            } else {
                format!("Part {}", chapters.len() + 1)
            }
        });
        // Images in Markdown point at files next to it, which a reader
        // that only has the text cannot load.
        chapters.push(Chapter::from_html(chapter_title, converted, |_| None));
    }
    if chapters.is_empty() {
        chapters.push(Chapter::from_text(title.clone(), String::new()));
    }
    Ok(Book {
        format: Format::Markdown,
        metadata: Metadata {
            title,
            ..Default::default()
        },
        chapters,
        images: Vec::new(),
        cover: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Inline};

    #[test]
    fn chapters_styles_and_tables() {
        let md = "# My Notes\n\nIntro with **bold**.\n\n## One\n\nSome *italic* text.\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n## Two\n\nEnd.\n";
        let book = parse(md.as_bytes(), Some("notes.md")).unwrap();
        assert_eq!(book.metadata.title, "My Notes");
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["My Notes", "One", "Two"]);
        let one = &book.chapters[1];
        assert!(one
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Table(t) if t.header && t.rows.len() == 2)));
        let italic = one
            .styles
            .iter()
            .flat_map(|s| &s.spans)
            .find(|s| s.kind == Inline::Italic);
        assert!(italic.is_some());
        assert_eq!(one.styles[0].heading, 2);
    }
}
