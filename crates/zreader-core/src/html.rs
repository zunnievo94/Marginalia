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
}

const LINE_BREAK: char = '\u{1}';

/// Convert HTML to plain text.
pub fn to_text(html: &str) -> String {
    convert(html).text
}

/// Convert HTML to plain text and pick up the first heading.
pub fn convert(html: &str) -> Converted {
    let mut out = Out::default();
    let mut skip_depth = 0usize; // inside <script>, <style>, <head>...
    let mut heading: Option<String> = None;
    let mut in_heading = false;
    let mut heading_buf = String::new();

    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_depth == 0 {
                out.text(rest);
                if in_heading {
                    heading_buf.push_str(rest);
                }
            }
            break;
        };
        let (txt, after) = rest.split_at(lt);
        if skip_depth == 0 {
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
            if skip_depth == 0 {
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
            if skip_depth == 0 {
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

        if matches!(name.as_str(), "script" | "style" | "head" | "svg") {
            if closing {
                skip_depth = skip_depth.saturating_sub(1);
            } else if !self_closing {
                skip_depth += 1;
            }
            continue;
        }
        if skip_depth > 0 {
            continue;
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

    Converted {
        text: out.finish(),
        heading,
    }
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
            self.paragraphs.push(lines.join("\n"));
        }
    }

    fn finish(self) -> String {
        self.paragraphs.join("\n\n")
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
    fn tolerates_broken_markup() {
        assert_eq!(to_text("a < b <p>c"), "a < b\n\nc");
        assert_eq!(to_text("<p>unterminated <b"), "unterminated");
    }
}
