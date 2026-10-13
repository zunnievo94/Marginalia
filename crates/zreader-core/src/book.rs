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

/// A piece of chapter content, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// One paragraph of text (the n-th text block is the n-th paragraph of
    /// [`Chapter::text`]).
    Text(String),
    /// An image, by index into [`Book::images`].
    Image { image: usize, alt: String },
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
    /// Paragraphs and images in reading order.
    pub blocks: Vec<Block>,
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
        }
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
            })
            .collect();
        Chapter {
            title,
            text: converted.text,
            blocks,
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
