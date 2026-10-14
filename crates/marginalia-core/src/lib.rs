//! Core ebook model and format parsers for Marginalia.
//!
//! Every supported format is converted into a [`Book`]: metadata plus an
//! ordered list of [`Chapter`]s holding plain text. Plain text is what both
//! the text renderer and the text-to-speech pipeline consume.

mod book;
mod epub_format;
mod fb2_format;
pub mod html;
mod huff;
mod kf8;
mod markdown_format;
mod mobi_format;
pub mod names;
mod pdf_format;
pub mod replace;
pub mod search;
pub mod text;
mod txt_format;

use std::path::Path;

pub use book::{
    sniff_image, Block, Book, Chapter, Format, Image, Inline, Metadata, ParaStyle, Span, Table,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported or unrecognised file format")]
    UnsupportedFormat,
    #[error("invalid EPUB: {0}")]
    Epub(String),
    #[error("invalid MOBI/PRC: {0}")]
    Mobi(String),
    #[error("invalid FB2: {0}")]
    Fb2(String),
    #[error("could not read the PDF: {0}")]
    Pdf(String),
    #[error("book is DRM-encrypted (or corrupt); Marginalia only opens DRM-free books")]
    Encrypted,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Open an ebook, detecting its format from the file contents (and, for
/// plain text and Markdown, the file name).
pub fn open(path: impl AsRef<Path>) -> Result<Book> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    open_named(&bytes, name.as_deref())
}

/// Parse an ebook from memory, detecting its format from the contents.
pub fn open_bytes(bytes: &[u8]) -> Result<Book> {
    open_named(bytes, None)
}

/// Parse an ebook from memory; `name` (a file name) tells plain text from
/// Markdown and gives books without metadata a title.
pub fn open_named(bytes: &[u8], name: Option<&str>) -> Result<Book> {
    let ext = name
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match Format::detect(bytes) {
        Some(Format::Epub) => match fb2_format::zipped(bytes) {
            Some(fb2) => fb2_format::parse(&fb2),
            None => epub_format::parse(bytes),
        },
        Some(Format::Mobi) | Some(Format::PalmDoc) => mobi_format::parse(bytes),
        Some(Format::Fb2) => fb2_format::parse(bytes),
        Some(Format::Pdf) => pdf_format::parse(bytes, name),
        _ if matches!(ext.as_str(), "md" | "markdown" | "mkd") => {
            markdown_format::parse(bytes, name)
        }
        _ if matches!(ext.as_str(), "txt" | "text")
            || (name.is_some() && ext.is_empty() && txt_format::looks_like_text(bytes)) =>
        {
            txt_format::parse(bytes, name)
        }
        _ => Err(Error::UnsupportedFormat),
    }
}

/// Decode a text file in any common encoding (BOM, UTF-8, or a guess
/// such as GBK, Big5 or windows-1258).
pub fn decode_text(bytes: &[u8]) -> String {
    txt_format::decode(bytes)
}

/// A title from a file name: without the extension, underscores as spaces.
pub(crate) fn title_from_name(name: Option<&str>) -> String {
    let name = name.unwrap_or("Untitled");
    let stem = name.split('/').next_back().unwrap_or(name);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    stem.replace('_', " ").trim().to_string()
}
