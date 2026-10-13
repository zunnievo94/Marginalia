/// Container formats ZReader understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// EPUB 2/3 (zip container).
    Epub,
    /// Mobipocket / Kindle (`.mobi`, `.prc`, `.azw`, `.azw3`).
    Mobi,
    /// Plain PalmDOC (`.pdb`, older `.prc`) without a MOBI header.
    PalmDoc,
}

impl Format {
    /// Sniff the format from the first bytes of a file.
    pub fn detect(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"PK\x03\x04") {
            return Some(Format::Epub);
        }
        // Palm database: type + creator live at offset 60..68.
        match bytes.get(60..68)? {
            b"BOOKMOBI" => Some(Format::Mobi),
            b"TEXtREAd" => Some(Format::PalmDoc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: String,
    pub authors: Vec<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    /// Readable plain text; paragraphs are separated by blank lines.
    pub text: String,
}

impl Chapter {
    /// Paragraphs of the chapter, in reading order.
    pub fn paragraphs(&self) -> impl Iterator<Item = &str> {
        self.text
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct Book {
    pub format: Format,
    pub metadata: Metadata,
    pub chapters: Vec<Chapter>,
}

impl Book {
    pub fn word_count(&self) -> usize {
        self.chapters
            .iter()
            .map(|c| c.text.split_whitespace().count())
            .sum()
    }
}
