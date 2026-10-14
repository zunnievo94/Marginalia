//! A small, forgiving HTML/XHTML to plain-text converter.
//!
//! Ebook markup (especially MOBI) is frequently not well-formed, so this is a
//! tolerant tokenizer rather than a real parser. It keeps paragraph structure,
//! drops scripts/styles/head, and decodes entities. Basic styling (bold,
//! italic, headings, quotes) is recorded beside the text as [`ParaStyle`]s,
//! and tables become [`RawBlock::Table`], so the text itself stays plain.

use crate::book::{Inline, ParaStyle, Span, Table};

/// Result of converting one HTML document.
#[derive(Debug, Default, Clone)]
pub struct Converted {
    /// Plain text, paragraphs separated by `"\n\n"`, line breaks by `"\n"`.
    pub text: String,
    /// Text of the first `<h1>`..`<h3>` heading, if any.
    pub heading: Option<String>,
    /// Paragraphs and images in document order. The text blocks are exactly
    /// the paragraphs of `text`.
    pub blocks: Vec<RawBlock>,
    /// Styling of each text block, in the same order.
    pub styles: Vec<ParaStyle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawBlock {
    Text(String),
    /// An image as referenced by the markup: a URL/path from `src` (or SVG
    /// `href`), or `recindex:N` for Mobipocket image records.
    Image {
        src: String,
        alt: String,
    },
    /// A table; its text is not part of the paragraphs.
    Table(Table),
}

const LINE_BREAK: char = '\u{1}';
/// Private-use characters marking where an inline style starts and ends
/// inside a paragraph being collected; removed before the text is used.
const OPEN: u32 = 0xE000;
const CLOSE: u32 = 0xE010;

/// Inline styles of an element, if it is one that styles text.
fn inline_styles(name: &str, tag: &str) -> Option<Vec<Inline>> {
    let fixed = match name {
        "b" | "strong" => Some(Inline::Bold),
        "i" | "em" | "cite" | "dfn" | "var" | "emphasis" => Some(Inline::Italic),
        "u" | "ins" => Some(Inline::Underline),
        "s" | "strike" | "del" | "strikethrough" => Some(Inline::Strike),
        "sup" => Some(Inline::Sup),
        "sub" => Some(Inline::Sub),
        "code" | "kbd" | "samp" | "tt" => Some(Inline::Code),
        "span" | "font" => None,
        _ => return None,
    };
    let mut out: Vec<Inline> = fixed.into_iter().collect();
    // Inline CSS, the usual way converted books mark emphasis.
    if let Some(style) = attr(tag, "style") {
        let style = style.to_ascii_lowercase().replace(' ', "");
        if style.contains("font-style:italic") || style.contains("font-style:oblique") {
            out.push(Inline::Italic);
        }
        if [
            "font-weight:bold",
            "font-weight:600",
            "font-weight:700",
            "font-weight:800",
            "font-weight:900",
        ]
        .iter()
        .any(|k| style.contains(k))
        {
            out.push(Inline::Bold);
        }
        if style.contains("underline") {
            out.push(Inline::Underline);
        }
        if style.contains("line-through") {
            out.push(Inline::Strike);
        }
    }
    Some(out)
}

/// A table being collected: rows of cells, as raw text.
#[derive(Default)]
struct TableBuf {
    depth: usize,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Option<String>,
    header: bool,
    header_row: bool,
}

impl TableBuf {
    fn end_cell(&mut self) {
        if let Some(c) = self.cell.take() {
            self.row
                .push(collapse(&decode(&c.replace(LINE_BREAK, " "))));
        }
    }

    fn end_row(&mut self) {
        self.end_cell();
        if !self.row.is_empty() {
            if self.rows.is_empty() {
                self.header = self.header_row;
            }
            self.rows.push(std::mem::take(&mut self.row));
        }
        self.header_row = false;
    }
}

/// Decode an (X)HTML file to text, honouring a BOM, the XML declaration's
/// `encoding` or a `<meta charset>`; defaults to UTF-8. Ebooks in Chinese
/// sometimes use GBK/GB18030 or Big5, and some files are UTF-16.
pub fn decode_markup(bytes: &[u8]) -> String {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom_len..])
            .0
            .into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let declared = ["encoding=", "charset="].iter().find_map(|key| {
        let at = head.find(key)? + key.len();
        let value: String = head[at..]
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        encoding_rs::Encoding::for_label(value.as_bytes())
    });
    let enc = match declared {
        // A file that claims a legacy encoding but is valid UTF-8 is UTF-8.
        Some(e) if e != encoding_rs::UTF_8 && std::str::from_utf8(bytes).is_err() => e,
        _ => encoding_rs::UTF_8,
    };
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// Convert HTML to plain text.
pub fn to_text(html: &str) -> String {
    convert(html).text
}

/// Convert HTML to plain text and pick up the first heading.
pub fn convert(html: &str) -> Converted {
    let mut out = Out::default();
    let mut skip_depth = 0usize; // inside <script>, <style>, <head>...
    let mut svg_depth = 0usize;
    let mut heading: Option<String> = None;
    let mut in_heading = false;
    let mut heading_buf = String::new();
    // Elements that may style text, innermost last: (name, styles).
    let mut inline_stack: Vec<(String, Vec<Inline>)> = Vec::new();
    let mut table = TableBuf::default();
    let mut heading_level: u8 = 0;
    let mut quote_depth = 0usize;

    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_depth == 0 && svg_depth == 0 {
                text_to(&mut out, &mut table, rest);
                if in_heading {
                    heading_buf.push_str(rest);
                }
            }
            break;
        };
        let (txt, after) = rest.split_at(lt);
        if skip_depth == 0 && svg_depth == 0 {
            text_to(&mut out, &mut table, txt);
            if in_heading {
                heading_buf.push_str(txt);
            }
        }
        rest = after;

        // Comments, CDATA, doctype, processing instructions.
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |i| &r[i + 3..]);
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>").unwrap_or(r.len());
            if skip_depth == 0 && svg_depth == 0 {
                text_to(&mut out, &mut table, &r[..end]);
            }
            rest = r.get(end + 3..).unwrap_or("");
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
            continue;
        }

        let starts_tag = rest[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
        if !starts_tag {
            // A stray '<' in text, e.g. "a < b".
            if skip_depth == 0 && svg_depth == 0 {
                text_to(&mut out, &mut table, "<");
                if in_heading {
                    heading_buf.push('<');
                }
            }
            rest = &rest[1..];
            continue;
        }

        let end = tag_end(rest);
        let tag = &rest[1..end];
        rest = rest.get(end + 1..).unwrap_or("");

        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let self_closing = tag.ends_with('/');

        if matches!(name.as_str(), "script" | "style" | "head") {
            if closing {
                skip_depth = skip_depth.saturating_sub(1);
            } else if !self_closing {
                skip_depth += 1;
            }
            continue;
        }
        if name == "svg" {
            if closing {
                svg_depth = svg_depth.saturating_sub(1);
            } else if !self_closing {
                svg_depth += 1;
            }
            continue;
        }
        if skip_depth > 0 {
            continue;
        }
        if !closing && (name == "img" || (svg_depth > 0 && name == "image")) {
            let src = attr(tag, "src")
                .or_else(|| attr(tag, "xlink:href"))
                .or_else(|| attr(tag, "href"))
                .or_else(|| {
                    // Mobipocket: <img recindex="00001">
                    let n: usize = attr(tag, "recindex")?.trim().parse().ok()?;
                    Some(format!("recindex:{n}"))
                });
            if let Some(src) = src.filter(|s| !s.trim().is_empty()) {
                let alt = attr(tag, "alt").map(|a| collapse(&a)).unwrap_or_default();
                out.image(src.trim().to_string(), alt);
            }
            continue;
        }
        if svg_depth > 0 {
            continue; // SVG text/shapes are not readable prose
        }

        if matches!(name.as_str(), "h1" | "h2" | "h3") {
            if closing && in_heading {
                in_heading = false;
                let h = collapse(&decode(&heading_buf));
                if heading.is_none() && !h.is_empty() {
                    heading = Some(h);
                }
            } else if !closing && heading.is_none() {
                in_heading = true;
                heading_buf.clear();
            }
        }

        // Tables: cells are collected apart from the paragraphs.
        if name == "table" {
            if closing {
                table.depth = table.depth.saturating_sub(1);
                if table.depth == 0 {
                    table.end_row();
                    let t = std::mem::take(&mut table);
                    if t.rows.iter().flatten().any(|c| !c.is_empty()) {
                        out.blocks.push(RawBlock::Table(Table {
                            rows: t.rows,
                            header: t.header,
                        }));
                    }
                }
            } else if !self_closing {
                if table.depth == 0 {
                    out.paragraph_break();
                }
                table.depth += 1;
            }
            continue;
        }
        if table.depth > 0 {
            if table.depth == 1 {
                match (name.as_str(), closing) {
                    ("tr", false) => table.end_row(),
                    ("tr", true) => table.end_row(),
                    ("td" | "th", false) => {
                        table.end_cell();
                        table.cell = Some(String::new());
                        table.header_row |= name == "th";
                    }
                    ("td" | "th", true) => table.end_cell(),
                    _ => {
                        if name == "br" || is_block(&name) {
                            text_to(&mut out, &mut table, " ");
                        }
                    }
                }
            } else if name == "br" || is_block(&name) || matches!(name.as_str(), "td" | "th") {
                text_to(&mut out, &mut table, " ");
            }
            continue;
        }

        // Inline styles.
        if !self_closing {
            if closing {
                if let Some(at) = inline_stack.iter().rposition(|(n, _)| *n == name) {
                    for (_, styles) in inline_stack.drain(at..).rev() {
                        for s in styles.into_iter().rev() {
                            out.close(s);
                        }
                    }
                }
            } else if let Some(styles) = inline_styles(&name, tag) {
                for &s in &styles {
                    out.open(s);
                }
                inline_stack.push((name.clone(), styles));
            }
        }

        if name == "br" {
            out.line_break();
        } else if is_block(&name) {
            out.paragraph_break();
            // Block-level styling applies to the paragraphs that follow.
            if let Some(level) = name.strip_prefix('h').and_then(|l| l.parse::<u8>().ok()) {
                if (1..=6).contains(&level) {
                    heading_level = if closing { 0 } else { level };
                }
            } else if name == "blockquote" {
                quote_depth = if closing {
                    quote_depth.saturating_sub(1)
                } else {
                    quote_depth + 1
                };
            }
            out.heading = heading_level;
            out.quote = quote_depth > 0;
        }
    }
    out.paragraph_break();

    let (text, blocks, styles) = out.finish();
    Converted {
        text,
        heading,
        blocks,
        styles,
    }
}

/// Send text to the table being collected, or to the paragraphs.
fn text_to(out: &mut Out, table: &mut TableBuf, s: &str) {
    if table.depth > 0 {
        // Whitespace between cells is markup layout, not content.
        if table.cell.is_none() && s.trim().is_empty() {
            return;
        }
        table.cell.get_or_insert_with(String::new).push_str(s);
    } else {
        out.text(s);
    }
}

/// Value of attribute `name` in a raw tag such as `img src="a.png" alt=x`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let b = tag.as_bytes();
    // Skip the tag name.
    let mut i = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'/' | b'>') {
            i += 1;
        }
        let key = &tag[start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = None;
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let (vstart, vend) = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let s = i + 1;
                let e = tag[s..].find(q as char).map_or(tag.len(), |p| s + p);
                i = (e + 1).min(tag.len());
                (s, e)
            } else {
                let s = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                (s, i)
            };
            value = Some(&tag[vstart..vend]);
        }
        if key.is_empty() {
            i += 1;
            continue;
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(decode(value.unwrap_or("")));
        }
    }
    None
}

/// Byte index of the `>` that closes the tag starting at `s[0] == '<'`,
/// honouring quoted attribute values.
fn tag_end(s: &str) -> usize {
    let mut quote: Option<u8> = None;
    for (i, b) in s.bytes().enumerate().skip(1) {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return i,
            _ => {}
        }
    }
    s.len()
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "dl"
            | "dt"
            | "dd"
            | "tr"
            | "table"
            | "blockquote"
            | "pre"
            | "hr"
            | "section"
            | "article"
            | "aside"
            | "header"
            | "footer"
            | "nav"
            | "figure"
            | "figcaption"
            | "body"
            | "html"
            | "mbp:pagebreak"
    )
}

fn decode(s: &str) -> String {
    html_escape::decode_html_entities(s).into_owned()
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Out {
    paragraphs: Vec<String>,
    blocks: Vec<RawBlock>,
    styles: Vec<ParaStyle>,
    current: String,
    /// How many times each inline style is open, by `Inline as usize`.
    active: [usize; Inline::COUNT],
    heading: u8,
    quote: bool,
}

impl Out {
    fn text(&mut self, s: &str) {
        self.current.push_str(s);
    }

    fn line_break(&mut self) {
        self.current.push(LINE_BREAK);
    }

    fn open(&mut self, s: Inline) {
        self.active[s as usize] += 1;
        self.current.push(char::from_u32(OPEN + s as u32).unwrap());
    }

    fn close(&mut self, s: Inline) {
        let n = &mut self.active[s as usize];
        if *n > 0 {
            *n -= 1;
            self.current.push(char::from_u32(CLOSE + s as u32).unwrap());
        }
    }

    fn paragraph_break(&mut self) {
        // Styles still open continue in the next paragraph.
        let reopen: String = Inline::ALL
            .iter()
            .filter(|s| self.active[**s as usize] > 0)
            .map(|s| char::from_u32(OPEN + *s as u32).unwrap())
            .collect();
        if self
            .current
            .chars()
            .all(|c| c as u32 >= OPEN && c as u32 <= CLOSE + 16)
        {
            self.current = reopen;
            return;
        }
        let raw = std::mem::replace(&mut self.current, reopen);
        let (p, spans) = clean_paragraph(&decode(&raw));
        if !p.is_empty() {
            self.blocks.push(RawBlock::Text(p.clone()));
            self.styles.push(ParaStyle {
                heading: self.heading,
                quote: self.quote,
                spans,
            });
            self.paragraphs.push(p);
        }
    }

    fn image(&mut self, src: String, alt: String) {
        self.paragraph_break();
        self.blocks.push(RawBlock::Image { src, alt });
    }

    fn finish(self) -> (String, Vec<RawBlock>, Vec<ParaStyle>) {
        (self.paragraphs.join("\n\n"), self.blocks, self.styles)
    }
}

/// Collapse whitespace in each line, drop empty lines and turn the style
/// markers into spans (character offsets into the cleaned text).
fn clean_paragraph(raw: &str) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut len = 0usize; // chars in `text`
    let mut events: Vec<(usize, Inline, bool)> = Vec::new();
    for line in raw.split(LINE_BREAK) {
        let mut buf = String::new();
        let mut n = 0usize;
        let mut space = false;
        let mut line_events = Vec::new();
        for c in line.chars() {
            let code = c as u32;
            if (OPEN..OPEN + Inline::COUNT as u32).contains(&code) {
                line_events.push((
                    n + usize::from(space && n > 0),
                    Inline::ALL[(code - OPEN) as usize],
                    true,
                ));
            } else if (CLOSE..CLOSE + Inline::COUNT as u32).contains(&code) {
                line_events.push((n, Inline::ALL[(code - CLOSE) as usize], false));
            } else if c.is_whitespace() {
                space = n > 0;
            } else {
                if space {
                    buf.push(' ');
                    n += 1;
                    space = false;
                }
                buf.push(c);
                n += 1;
            }
        }
        let base = if n > 0 && len > 0 { len + 1 } else { len };
        for (at, s, open) in line_events {
            events.push((base + at.min(n), s, open));
        }
        if n > 0 {
            if len > 0 {
                text.push('\n');
                len += 1;
            }
            text.push_str(&buf);
            len += n;
        }
    }
    let mut spans = Vec::new();
    let mut depth = [0usize; Inline::COUNT];
    let mut start = [0usize; Inline::COUNT];
    for (at, s, open) in events {
        let k = s as usize;
        if open {
            if depth[k] == 0 {
                start[k] = at;
            }
            depth[k] += 1;
        } else if depth[k] > 0 {
            depth[k] -= 1;
            if depth[k] == 0 && at.min(len) > start[k] {
                spans.push(Span {
                    kind: s,
                    start: start[k],
                    end: at.min(len),
                });
            }
        }
    }
    for s in Inline::ALL {
        let k = s as usize;
        if depth[k] > 0 && len > start[k] {
            spans.push(Span {
                kind: s,
                start: start[k],
                end: len,
            });
        }
    }
    spans.sort_by_key(|s| (s.start, s.end));
    (text, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_entities() {
        let html = r#"<?xml version="1.0"?><!DOCTYPE html><html><head><title>x</title>
            <style>p { color: red }</style></head><body>
            <h1 class="c">Chapter&nbsp;One</h1>
            <p>Hello,   <b>world</b> &amp; friends.</p>
            <!-- a comment <p>not here</p> -->
            <p>Line one<br/>Line two</p><script>alert(1)</script>
            <p title="a > b">5 &lt; 6</p></body></html>"#;
        let c = convert(html);
        assert_eq!(c.heading.as_deref(), Some("Chapter One"));
        assert_eq!(
            c.text,
            "Chapter One\n\nHello, world & friends.\n\nLine one\nLine two\n\n5 < 6"
        );
    }

    #[test]
    fn images_in_order() {
        let c = convert(
            r#"<p>Before</p><div><img alt="A map" src="../img/map%20one.png"/></div>
               <svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href='cover.jpg'/><text>no</text></svg>
               <p>After <img recindex="00003"> text</p>"#,
        );
        let img = |src: &str, alt: &str| RawBlock::Image {
            src: src.into(),
            alt: alt.into(),
        };
        assert_eq!(
            c.blocks,
            vec![
                RawBlock::Text("Before".into()),
                img("../img/map%20one.png", "A map"),
                img("cover.jpg", ""),
                RawBlock::Text("After".into()),
                img("recindex:3", ""),
                RawBlock::Text("text".into()),
            ]
        );
        assert_eq!(c.text, "Before\n\nAfter\n\ntext");
    }

    #[test]
    fn decodes_legacy_encodings() {
        let html = "<?xml version=\"1.0\" encoding=\"GBK\"?><p>你好，世界</p>";
        let (gbk, _, _) = encoding_rs::GBK.encode(html);
        assert_eq!(to_text(&decode_markup(&gbk)), "你好，世界");

        let mut utf16 = vec![0xFF, 0xFE];
        for u in "<p>Tiếng Việt</p>".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(to_text(&decode_markup(&utf16)), "Tiếng Việt");

        let mislabelled = "<meta charset=\"iso-8859-1\"><p>Tiếng Việt</p>";
        assert_eq!(
            to_text(&decode_markup(mislabelled.as_bytes())),
            "Tiếng Việt"
        );
    }

    #[test]
    fn tolerates_broken_markup() {
        assert_eq!(to_text("a < b <p>c"), "a < b\n\nc");
        assert_eq!(to_text("<p>unterminated <b"), "unterminated");
    }

    fn styled(html: &str) -> Vec<(String, Vec<(Inline, String)>)> {
        let c = convert(html);
        let texts: Vec<String> = c
            .blocks
            .iter()
            .filter_map(|b| match b {
                RawBlock::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        texts
            .into_iter()
            .zip(c.styles)
            .map(|(t, st)| {
                let chars: Vec<char> = t.chars().collect();
                let spans = st
                    .spans
                    .iter()
                    .map(|s| (s.kind, chars[s.start..s.end].iter().collect()))
                    .collect();
                (t, spans)
            })
            .collect()
    }

    #[test]
    fn inline_styles_become_spans() {
        let got = styled("<p>Một <b>ngày</b> nọ, <i>Kiều  <b>đi</b></i> chơi.</p>");
        assert_eq!(got[0].0, "Một ngày nọ, Kiều đi chơi.");
        assert_eq!(
            got[0].1,
            vec![
                (Inline::Bold, "ngày".into()),
                (Inline::Italic, "Kiều đi".into()),
                (Inline::Bold, "đi".into()),
            ]
        );
        // CSS emphasis, entities and a style running across paragraphs.
        let got = styled(
            "<p><span style=\"font-style: italic\">a &amp; b</span> c</p><i><p>x</p><p>y</p></i>",
        );
        assert_eq!(got[0].1, vec![(Inline::Italic, "a & b".into())]);
        assert_eq!(got[1].1, vec![(Inline::Italic, "x".into())]);
        assert_eq!(got[2].1, vec![(Inline::Italic, "y".into())]);
    }

    #[test]
    fn headings_quotes_and_tables() {
        let c = convert(
            "<h2>Title</h2><p>Body</p><blockquote><p>Quoted</p></blockquote>\
             <table><tr><th>Name</th><th>Age</th></tr><tr><td><b>An</b></td><td>20</td></tr></table><p>After</p>",
        );
        assert_eq!(c.text, "Title\n\nBody\n\nQuoted\n\nAfter");
        assert_eq!(c.styles[0].heading, 2);
        assert_eq!(c.styles[1], ParaStyle::default());
        assert!(c.styles[2].quote);
        let table = c.blocks.iter().find_map(|b| match b {
            RawBlock::Table(t) => Some(t.clone()),
            _ => None,
        });
        assert_eq!(
            table,
            Some(Table {
                rows: vec![
                    vec!["Name".into(), "Age".into()],
                    vec!["An".into(), "20".into()]
                ],
                header: true,
            })
        );
    }
}
