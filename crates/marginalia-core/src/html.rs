//! A small, forgiving HTML/XHTML to plain-text converter.
//!
//! Ebook markup (especially MOBI) is frequently not well-formed, so this is a
//! tolerant tokenizer rather than a real parser. It keeps paragraph structure,
//! drops scripts/styles/head, and decodes entities.

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
}

const LINE_BREAK: char = '\u{1}';

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

    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_depth == 0 && svg_depth == 0 {
                out.text(rest);
                if in_heading {
                    heading_buf.push_str(rest);
                }
            }
            break;
        };
        let (txt, after) = rest.split_at(lt);
        if skip_depth == 0 && svg_depth == 0 {
            out.text(txt);
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
                out.text(&r[..end]);
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
                out.text("<");
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

        if name == "br" {
            out.line_break();
        } else if is_block(&name) {
            out.paragraph_break();
        }
    }
    out.paragraph_break();

    let (text, blocks) = out.finish();
    Converted {
        text,
        heading,
        blocks,
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
    current: String,
}

impl Out {
    fn text(&mut self, s: &str) {
        self.current.push_str(s);
    }

    fn line_break(&mut self) {
        self.current.push(LINE_BREAK);
    }

    fn paragraph_break(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let raw = std::mem::take(&mut self.current);
        let decoded = decode(&raw);
        let lines: Vec<String> = decoded
            .split(LINE_BREAK)
            .map(collapse)
            .filter(|l| !l.is_empty())
            .collect();
        if !lines.is_empty() {
            let p = lines.join("\n");
            self.blocks.push(RawBlock::Text(p.clone()));
            self.paragraphs.push(p);
        }
    }

    fn image(&mut self, src: String, alt: String) {
        self.paragraph_break();
        self.blocks.push(RawBlock::Image { src, alt });
    }

    fn finish(self) -> (String, Vec<RawBlock>) {
        (self.paragraphs.join("\n\n"), self.blocks)
    }
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
}
