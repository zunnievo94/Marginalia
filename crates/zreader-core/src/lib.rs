//! Core ebook model and format parsers for ZReader.
//!
//! Every supported format is converted into a [`Book`]: metadata plus an
//! ordered list of [`Chapter`]s holding plain text. Plain text is what both
//! the text renderer and the text-to-speech pipeline consume.

mod book;
mod epub_format;
pub mod html;
mod huff;
mod mobi_format;
pub mod text;

use std::path::Path;

pub use book::{Book, Chapter, Format, Metadata};

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
    #[error("book is DRM-encrypted (or corrupt); ZReader only opens DRM-free books")]
    Encrypted,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Open an ebook, detecting its format from the file contents.
pub fn open(path: impl AsRef<Path>) -> Result<Book> {
    let bytes = std::fs::read(path.as_ref())?;
    open_bytes(&bytes)
}

/// Parse an ebook from memory, detecting its format from the contents.
pub fn open_bytes(bytes: &[u8]) -> Result<Book> {
    match Format::detect(bytes) {
        Some(Format::Epub) => epub_format::parse(bytes),
        Some(Format::Mobi) | Some(Format::PalmDoc) => mobi_format::parse(bytes),
        None => Err(Error::UnsupportedFormat),
    }
}
