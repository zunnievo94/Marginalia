//! FictionBook 2 (`.fb2`, and zipped `.fb2.zip` / `.fbz`). The XML is
//! mapped onto HTML tags and read by the HTML converter; images are
//! base64 `<binary>` elements.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use base64::Engine;
use regex::Regex;

use crate::{Book, Chapter, Error, Format, Image, Metadata, Result};

/// The FB2 file inside a zip that is not an EPUB.
pub fn zipped(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    if zip.by_name("META-INF/container.xml").is_ok() {
        return None;
    }
    let name = zip
        .file_names()
        .find(|n| n.to_ascii_lowercase().ends_with(".fb2"))?
        .to_string();
    let mut out = Vec::new();
    zip.by_name(&name).ok()?.read_to_end(&mut out).ok()?;
    Some(out)
}

fn re(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// Text inside the first `<tag>…</tag>` of `xml`.
fn inner<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let mut from = 0;
    loop {
        let at = from + xml[from..].find(&open)?;
        let after = &xml[at + open.len()..];
        // Avoid matching a longer name such as <titles>.
        if after.starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
            let start = at + xml[at..].find('>')? + 1;
            if xml[..start].ends_with("/>") {
                return Some("");
            }
            let end = start + xml[start..].find(&format!("</{tag}>"))?;
            return Some(&xml[start..end]);
        }
        from = at + open.len();
    }
}

fn plain(xml: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let text = re(r"<[^>]*>", &TAGS).replace_all(xml, " ");
    html_escape::decode_html_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn attr<'a>(tag_attrs: &'a str, name: &str) -> Option<&'a str> {
    let at = tag_attrs.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag_attrs[at..];
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let end = rest[1..].find(q)?;
    Some(&rest[1..1 + end])
}

/// FB2 elements as the HTML the converter understands.
fn to_html(xml: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    let tag = re(
        r"<(/?)(title|subtitle|epigraph|cite|poem|stanza|v|annotation|text-author|empty-line|image|section|emphasis|strikethrough|date)((?:\s[^>]*?)?)(/?)>",
        &TAG,
    );
    let href = re(r#"\b[\w-]*:?href\s*="#, &HREF);
    tag.replace_all(xml, |c: &regex::Captures| {
        let html = match &c[2] {
            "title" => "h2",
            "subtitle" => "h3",
            "epigraph" | "cite" | "poem" | "annotation" => "blockquote",
            "stanza" | "section" => "div",
            "v" | "text-author" | "date" | "empty-line" => "p",
            "image" => "img",
            "emphasis" => "em",
            "strikethrough" => "s",
            _ => "span",
        };
        let attrs = if html == "img" {
            href.replace_all(&c[3], "src=").into_owned()
        } else {
            String::new()
        };
        format!("<{}{html}{attrs}{}>", &c[1], &c[4])
    })
    .into_owned()
}

/// Top-level `<section>` elements of `xml`, as (start, content start,
/// content end, end) byte offsets.
fn sections(xml: &str) -> Vec<(usize, usize, usize, usize)> {
    static SECTION: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut open = (0, 0);
    for m in re(r"<(/?)section\b[^>]*?(/?)>", &SECTION).captures_iter(xml) {
        let whole = m.get(0).expect("match");
        if !m[2].is_empty() {
            continue; // <section/>
        }
        if m[1].is_empty() {
            if depth == 0 {
                open = (whole.start(), whole.end());
            }
            depth += 1;
        } else if depth > 0 {
            depth -= 1;
            if depth == 0 {
                out.push((open.0, open.1, whole.start(), whole.end()));
            }
        }
    }
    out
}

struct Ctx<'a> {
    ids: &'a HashMap<String, usize>,
    chapters: Vec<Chapter>,
}

impl Ctx<'_> {
    fn chapter(&mut self, xml: &str) {
        if plain(xml).is_empty() {
            return;
        }
        let converted = crate::html::convert(&to_html(xml));
        if converted.blocks.is_empty() {
            return;
        }
        let title = converted
            .heading
            .clone()
            .unwrap_or_else(|| format!("Chapter {}", self.chapters.len() + 1));
        let ids = self.ids;
        self.chapters
            .push(Chapter::from_html(title, converted, |src| {
                ids.get(src.trim_start_matches('#')).copied()
            }));
    }

    /// A section with sub-sections gives its own text (if any beyond the
    /// title) as one chapter, then one chapter per sub-section.
    fn section(&mut self, xml: &str, depth: usize) {
        let children = sections(xml);
        if children.is_empty() || depth > 8 {
            self.chapter(xml);
            return;
        }
        let intro = &xml[..children[0].0];
        let intro_text = plain(intro);
        let title_text = inner(intro, "title").map(plain).unwrap_or_default();
        if intro_text.len() > title_text.len() + 1 {
            self.chapter(intro);
        }
        for (_, start, end, _) in children {
            self.section(&xml[start..end], depth + 1);
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Book> {
    let xml = crate::html::decode_markup(bytes);
    if !xml.contains("<FictionBook") {
        return Err(Error::Fb2("no <FictionBook> element".into()));
    }

    // Metadata.
    let info = inner(&xml, "title-info").unwrap_or("");
    let mut metadata = Metadata {
        title: inner(info, "book-title").map(plain).unwrap_or_default(),
        language: inner(info, "lang").map(plain).filter(|l| !l.is_empty()),
        ..Default::default()
    };
    let mut rest = info;
    while let Some(author) = inner(rest, "author") {
        let name: Vec<String> = ["first-name", "middle-name", "last-name"]
            .iter()
            .filter_map(|t| inner(author, t).map(plain))
            .filter(|n| !n.is_empty())
            .collect();
        let name = if name.is_empty() {
            inner(author, "nickname").map(plain).unwrap_or_default()
        } else {
            name.join(" ")
        };
        if !name.is_empty() {
            metadata.authors.push(name);
        }
        let at = rest.find("</author>").map_or(rest.len(), |i| i + 9);
        rest = &rest[at..];
    }

    // Images.
    static BINARY: OnceLock<Regex> = OnceLock::new();
    let mut images = Vec::new();
    let mut ids = HashMap::new();
    for c in re(r"(?s)<binary\b([^>]*)>(.*?)</binary>", &BINARY).captures_iter(&xml) {
        let b64: String = c[2].chars().filter(|c| !c.is_whitespace()).collect();
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            continue;
        };
        let media_type = crate::sniff_image(&data)
            .map(str::to_string)
            .or_else(|| attr(&c[1], "content-type").map(str::to_string))
            .unwrap_or_else(|| "application/octet-stream".into());
        if let Some(id) = attr(&c[1], "id") {
            ids.insert(id.to_string(), images.len());
        }
        images.push(Image { media_type, data });
    }
    static HREF_ID: OnceLock<Regex> = OnceLock::new();
    let cover = inner(info, "coverpage")
        .and_then(|c| re(r#"href\s*=\s*["']#?([^"']+)["']"#, &HREF_ID).captures(c))
        .and_then(|c| ids.get(&c[1]).copied());

    // Text: the main body, then notes and comments.
    static BODY: OnceLock<Regex> = OnceLock::new();
    let mut ctx = Ctx {
        ids: &ids,
        chapters: Vec::new(),
    };
    for c in re(r"(?s)<body\b([^>]*)>(.*?)</body>", &BODY).captures_iter(&xml) {
        let body = &c[2];
        if attr(&c[1], "name").is_some() {
            // Notes: one chapter, titled by the body's own title.
            ctx.chapter(body);
            continue;
        }
        let top = sections(body);
        if top.is_empty() {
            ctx.chapter(body);
        } else {
            // Text before the first section, unless it is only a title.
            let intro = &body[..top[0].0];
            let title = inner(intro, "title").map(plain).unwrap_or_default();
            if plain(intro).len() > title.len() + 1 {
                ctx.chapter(intro);
            }
            for (_, start, end, _) in top {
                ctx.section(&body[start..end], 0);
            }
        }
    }
    if ctx.chapters.is_empty() {
        return Err(Error::Fb2("the book has no text".into()));
    }
    if metadata.title.is_empty() {
        metadata.title = ctx.chapters[0].title.clone();
    }
    Ok(Book {
        format: Format::Fb2,
        metadata,
        chapters: ctx.chapters,
        images,
        cover,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Inline};

    const FB2: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
 <description><title-info>
  <author><first-name>Ngô</first-name><last-name>Thừa Ân</last-name></author>
  <book-title>Tây Du Ký</book-title><lang>vi</lang>
  <coverpage><image l:href="#cover.png"/></coverpage>
 </title-info></description>
 <body>
  <title><p>Tây Du Ký</p></title>
  <section><title><p>Hồi 1</p></title>
   <p>Đá thần <emphasis>nứt</emphasis> ra.</p>
   <section><title><p>Đoạn A</p></title><p>Khỉ đá ra đời.</p><image l:href="#cover.png"/></section>
   <section><title><p>Đoạn B</p></title><p>Tìm thầy <strong>học đạo</strong>.</p></section>
  </section>
  <section><title><p>Hồi 2</p></title><empty-line/><p>Hết.</p></section>
 </body>
 <body name="notes"><title><p>Chú thích</p></title><section id="n1"><p>Một chú thích.</p></section></body>
 <binary id="cover.png" content-type="image/png">iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=</binary>
</FictionBook>"##;

    #[test]
    fn parses_sections_styles_and_images() {
        let book = parse(FB2.as_bytes()).unwrap();
        assert_eq!(book.metadata.title, "Tây Du Ký");
        assert_eq!(book.metadata.authors, ["Ngô Thừa Ân"]);
        assert_eq!(book.metadata.language.as_deref(), Some("vi"));
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Hồi 1", "Đoạn A", "Đoạn B", "Hồi 2", "Chú thích"]);
        assert_eq!(book.images.len(), 1);
        assert_eq!(book.images[0].media_type, "image/png");
        assert_eq!(book.cover, Some(0));
        assert!(book.chapters[1]
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Image { image: 0, .. })));
        let hoi1 = &book.chapters[0];
        assert!(hoi1.text.contains("Đá thần nứt ra."));
        assert!(hoi1
            .styles
            .iter()
            .flat_map(|s| &s.spans)
            .any(|s| s.kind == Inline::Italic));
        assert!(crate::open_bytes(FB2.as_bytes()).is_ok());
    }
}
