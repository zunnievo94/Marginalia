/// Container formats Marginalia understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// EPUB 2/3 (zip container).
    Epub,
    /// Mobipocket / Kindle (`.mobi`, `.prc`, `.azw`, `.azw3`).
    Mobi,
    /// Plain PalmDOC (`.pdb`, older `.prc`) without a MOBI header.
    PalmDoc,
    /// FictionBook 2 (`.fb2`, or zipped as `.fb2.zip` / `.fbz`).
    Fb2,
    /// Plain text (`.txt`).
    Txt,
    /// Markdown (`.md`).
    Markdown,
    /// PDF, as extracted text.
    Pdf,
}

impl Format {
    /// Sniff the format from the first bytes of a file. Plain text and
    /// Markdown have no signature; [`crate::open`] tells them apart by the
    /// file name.
    pub fn detect(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"PK\x03\x04") {
            return Some(Format::Epub);
        }
        if bytes.starts_with(b"%PDF-") {
            return Some(Format::Pdf);
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        if head.contains("<FictionBook") {
            return Some(Format::Fb2);
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

/// A piece of chapter content, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// One paragraph of text (the n-th text block is the n-th paragraph of
    /// [`Chapter::text`]).
    Text(String),
    /// An image, by index into [`Book::images`].
    Image { image: usize, alt: String },
    /// A table. Its text is not part of [`Chapter::text`], so it is shown
    /// but not read aloud or searched.
    Table(Table),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// The first row holds column headings.
    pub header: bool,
}

/// An inline text style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Inline {
    Bold,
    Italic,
    Underline,
    Strike,
    Sup,
    Sub,
    Code,
}

impl Inline {
    pub const COUNT: usize = 7;
    pub const ALL: [Inline; Inline::COUNT] = [
        Inline::Bold,
        Inline::Italic,
        Inline::Underline,
        Inline::Strike,
        Inline::Sup,
        Inline::Sub,
        Inline::Code,
    ];

    /// Short name: `b`, `i`, `u`, `s`, `sup`, `sub` or `code`.
    pub fn name(self) -> &'static str {
        match self {
            Inline::Bold => "b",
            Inline::Italic => "i",
            Inline::Underline => "u",
            Inline::Strike => "s",
            Inline::Sup => "sup",
            Inline::Sub => "sub",
            Inline::Code => "code",
        }
    }
}

/// A styled stretch of a paragraph, in character offsets into its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub kind: Inline,
    pub start: usize,
    pub end: usize,
}

/// How a paragraph is styled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParaStyle {
    /// Heading level 1–6, or 0 for body text.
    pub heading: u8,
    /// Inside a block quote (also used for epigraphs and poems).
    pub quote: bool,
    pub spans: Vec<Span>,
}

impl ParaStyle {
    pub fn is_plain(&self) -> bool {
        self.heading == 0 && !self.quote && self.spans.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Image {
    /// e.g. `image/jpeg`
    pub media_type: String,
    pub data: Vec<u8>,
}

/// Guess an image's media type from its first bytes.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    Some(match data {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'B', b'M', ..] => "image/bmp",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        d if d.starts_with(b"<svg") || d.starts_with(b"<?xml") => "image/svg+xml",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    /// Readable plain text; paragraphs are separated by blank lines.
    pub text: String,
    /// Paragraphs, images and tables in reading order.
    pub blocks: Vec<Block>,
    /// Styling of the n-th text block; missing entries are plain.
    pub styles: Vec<ParaStyle>,
}

impl Chapter {
    /// A text-only chapter.
    pub fn from_text(title: String, text: String) -> Chapter {
        let blocks = text
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| Block::Text(p.to_string()))
            .collect();
        Chapter {
            title,
            text,
            blocks,
            styles: Vec::new(),
        }
    }

    /// Style of the n-th text block.
    pub fn style(&self, paragraph: usize) -> Option<&ParaStyle> {
        self.styles.get(paragraph).filter(|s| !s.is_plain())
    }

    /// Build a chapter from converted HTML, resolving image references to
    /// indices in the book's image list (unresolvable images are dropped).
    pub(crate) fn from_html(
        title: String,
        converted: crate::html::Converted,
        mut resolve: impl FnMut(&str) -> Option<usize>,
    ) -> Chapter {
        let blocks = converted
            .blocks
            .into_iter()
            .filter_map(|b| match b {
                crate::html::RawBlock::Text(t) => Some(Block::Text(t)),
                crate::html::RawBlock::Image { src, alt } => {
                    resolve(&src).map(|image| Block::Image { image, alt })
                }
                crate::html::RawBlock::Table(t) => Some(Block::Table(t)),
            })
            .collect();
        Chapter {
            title,
            text: converted.text,
            blocks,
            styles: converted.styles,
        }
    }

    pub fn has_images(&self) -> bool {
        self.blocks.iter().any(|b| matches!(b, Block::Image { .. }))
    }

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
    /// Images referenced by chapters (and the cover).
    pub images: Vec<Image>,
    /// Cover image, by index into `images`.
    pub cover: Option<usize>,
}

impl Book {
    pub fn cover_image(&self) -> Option<&Image> {
        self.images.get(self.cover?)
    }

    pub fn word_count(&self) -> usize {
        self.chapters
            .iter()
            .map(|c| crate::text::word_count(&c.text))
            .sum()
    }
}
