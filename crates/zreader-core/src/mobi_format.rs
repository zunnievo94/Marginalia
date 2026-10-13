//! MOBI / PRC / AZW and plain PalmDOC parsing.
//!
//! Both formats are Palm databases: a header, a record table, and record 0
//! describing how the text records are compressed. We read uncompressed and
//! PalmDOC (LZ77) and HUFF/CDIC compression are all supported.

use crate::huff::HuffReader;
use crate::{html, Book, Chapter, Error, Format, Metadata, Result};

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_PALMDOC: u16 = 2;
const COMPRESSION_HUFF: u16 = 17480;

fn err(msg: impl Into<String>) -> Error {
    Error::Mobi(msg.into())
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

/// The record table of a Palm database.
struct PalmDb<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
}

impl<'a> PalmDb<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        let count = u16_at(bytes, 76).ok_or_else(|| err("truncated header"))? as usize;
        let offsets = (0..count)
            .map(|i| u32_at(bytes, 78 + i * 8).map(|o| o as usize))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| err("truncated record table"))?;
        if offsets.is_empty() {
            return Err(err("no records"));
        }
        Ok(PalmDb { bytes, offsets })
    }

    fn name(&self) -> String {
        let raw = &self.bytes[..32.min(self.bytes.len())];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).replace('_', " ")
    }

    fn record(&self, i: usize) -> Option<&'a [u8]> {
        let start = *self.offsets.get(i)?;
        let end = self
            .offsets
            .get(i + 1)
            .copied()
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        self.bytes.get(start..end.max(start))
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Book> {
    let db = PalmDb::parse(bytes)?;
    let rec0 = db.record(0).ok_or_else(|| err("missing record 0"))?;

    let compression = u16_at(rec0, 0).ok_or_else(|| err("short record 0"))?;
    let text_length = u32_at(rec0, 4).unwrap_or(u32::MAX) as usize;
    let record_count = u16_at(rec0, 8).unwrap_or(0) as usize;
    let encryption = u16_at(rec0, 12).unwrap_or(0);
    let is_mobi = Format::detect(bytes) == Some(Format::Mobi);
    let has_mobi_header = rec0.get(16..20) == Some(b"MOBI");

    // In plain PalmDOC these bytes are a reading position, not encryption.
    if is_mobi && encryption != 0 {
        return Err(Error::Encrypted);
    }

    let header = has_mobi_header.then(|| MobiHeader::parse(rec0));
    let extra_flags = header.as_ref().map_or(0, |h| h.extra_flags);

    let mut huff = if compression == COMPRESSION_HUFF {
        let h = header
            .as_ref()
            .ok_or_else(|| err("HUFF book without MOBI header"))?;
        let records: Vec<&[u8]> = (h.first_huff..h.first_huff + h.huff_count)
            .map(|i| db.record(i).ok_or_else(|| err("missing HUFF record")))
            .collect::<Result<_>>()?;
        Some(HuffReader::new(&records)?)
    } else {
        None
    };

    let mut raw = Vec::with_capacity(text_length.min(64 << 20));
    for i in 1..=record_count {
        let Some(rec) = db.record(i) else { break };
        let rec = strip_trailing_entries(rec, extra_flags);
        match compression {
            COMPRESSION_NONE => raw.extend_from_slice(rec),
            COMPRESSION_PALMDOC => palmdoc_decompress(rec, &mut raw),
            COMPRESSION_HUFF => raw.extend(huff.as_mut().expect("set above").unpack(rec)?),
            other => return Err(err(format!("unknown compression {other}"))),
        }
    }
    raw.truncate(text_length);
    let utf8 = header.as_ref().is_some_and(|h| h.encoding == 65001);
    let text = decode(&raw, utf8);

    if looks_binary(&text) {
        // Old Mobipocket DRM leaves the PalmDOC header untouched.
        return Err(Error::Encrypted);
    }

    let mut metadata = Metadata {
        title: db.name(),
        ..Metadata::default()
    };
    if let Some(h) = &header {
        h.apply(rec0, &mut metadata);
    }
    let chapters = if header.is_some() || looks_like_html(&text) {
        html_chapters(&text)
    } else {
        plain_text_chapters(&text)
    };

    Ok(Book {
        format: if is_mobi {
            Format::Mobi
        } else {
            Format::PalmDoc
        },
        metadata,
        chapters,
    })
}

struct MobiHeader {
    header_len: usize,
    encoding: u32,
    extra_flags: u16,
    first_huff: usize,
    huff_count: usize,
}

impl MobiHeader {
    fn parse(rec0: &[u8]) -> Self {
        let header_len = u32_at(rec0, 20).unwrap_or(0) as usize;
        let encoding = u32_at(rec0, 28).unwrap_or(1252);
        let extra_flags = if header_len >= 0xE4 {
            u16_at(rec0, 0xF2).unwrap_or(0)
        } else {
            0
        };
        MobiHeader {
            header_len,
            encoding,
            extra_flags,
            first_huff: u32_at(rec0, 0x70).unwrap_or(0) as usize,
            huff_count: u32_at(rec0, 0x74).unwrap_or(0) as usize,
        }
    }

    fn apply(&self, rec0: &[u8], meta: &mut Metadata) {
        let utf8 = self.encoding == 65001;
        // Full name.
        if let (Some(off), Some(len)) = (u32_at(rec0, 84), u32_at(rec0, 88)) {
            if let Some(name) = rec0.get(off as usize..(off + len) as usize) {
                let name = decode(name, utf8);
                if !name.trim().is_empty() {
                    meta.title = name.trim().to_string();
                }
            }
        }
        // EXTH block.
        let has_exth = u32_at(rec0, 128).unwrap_or(0) & 0x40 != 0;
        let exth = 16 + self.header_len;
        if !has_exth || rec0.get(exth..exth + 4) != Some(b"EXTH") {
            return;
        }
        let count = u32_at(rec0, exth + 8).unwrap_or(0);
        let mut pos = exth + 12;
        for _ in 0..count {
            let (Some(kind), Some(len)) = (u32_at(rec0, pos), u32_at(rec0, pos + 4)) else {
                break;
            };
            let len = len as usize;
            if len < 8 {
                break;
            }
            let Some(data) = rec0.get(pos + 8..pos + len) else {
                break;
            };
            let value = decode(data, utf8).trim().to_string();
            match kind {
                100 if !value.is_empty() => meta.authors.push(value),
                503 if !value.is_empty() => meta.title = value,
                524 if !value.is_empty() => meta.language = Some(value),
                _ => {}
            }
            pos += len;
        }
    }
}

fn decode(bytes: &[u8], utf8: bool) -> String {
    let enc = if utf8 {
        encoding_rs::UTF_8
    } else {
        encoding_rs::WINDOWS_1252
    };
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// Encrypted or corrupt text decodes to lots of control characters.
fn looks_binary(text: &str) -> bool {
    let sample: Vec<char> = text.chars().take(4096).collect();
    let bad = sample
        .iter()
        .filter(|&&c| (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || c == '\u{fffd}')
        .count();
    !sample.is_empty() && bad * 20 > sample.len()
}

fn looks_like_html(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(4096)
        .collect::<String>()
        .to_ascii_lowercase();
    head.contains("<html") || head.contains("<body") || head.contains("<p>") || head.contains("<p ")
}

/// Remove the "trailing entries" MOBI appends to each text record.
fn strip_trailing_entries(mut rec: &[u8], flags: u16) -> &[u8] {
    for bit in (1..16).rev() {
        if flags & (1 << bit) == 0 {
            continue;
        }
        // Size is a backward-encoded varint in the last (up to) 4 bytes.
        let tail = &rec[rec.len().saturating_sub(4)..];
        let mut size = 0usize;
        for &b in tail {
            if b & 0x80 != 0 {
                size = 0;
            }
            size = (size << 7) | (b & 0x7f) as usize;
        }
        rec = &rec[..rec.len().saturating_sub(size)];
    }
    if flags & 1 != 0 {
        if let Some(&last) = rec.last() {
            let n = (last & 0x3) as usize + 1;
            rec = &rec[..rec.len().saturating_sub(n)];
        }
    }
    rec
}

/// PalmDOC LZ77 decompression, appending to `out`.
pub(crate) fn palmdoc_decompress(input: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            0x01..=0x08 => {
                let end = (i + c as usize).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0x00 | 0x09..=0x7f => out.push(c),
            0x80..=0xbf => {
                let Some(&next) = input.get(i) else { break };
                i += 1;
                let pair = ((c as usize) << 8) | next as usize;
                let distance = (pair >> 3) & 0x7ff;
                let length = (pair & 0x7) + 3;
                let produced = out.len() - start;
                if distance == 0 || distance > produced {
                    continue; // corrupt back-reference; skip it
                }
                let from = out.len() - distance;
                for k in 0..length {
                    out.push(out[from + k]);
                }
            }
            0xc0..=0xff => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
        }
    }
}

/// MOBI text is HTML; chapters are separated by `<mbp:pagebreak/>`.
fn html_chapters(markup: &str) -> Vec<Chapter> {
    let lower = markup.to_ascii_lowercase();
    let mut cuts = vec![0];
    cuts.extend(lower.match_indices("<mbp:pagebreak").map(|(i, _)| i));
    cuts.push(markup.len());

    let mut chapters = Vec::new();
    for w in cuts.windows(2) {
        let converted = html::convert(&markup[w[0]..w[1]]);
        if converted.text.trim().is_empty() {
            continue;
        }
        chapters.push(Chapter {
            title: converted
                .heading
                .unwrap_or_else(|| format!("Section {}", chapters.len() + 1)),
            text: converted.text,
        });
    }
    chapters
}

/// PalmDOC books are plain text. Paragraphs are blank-line separated when the
/// book uses blank lines, otherwise one per line; "Chapter ..." lines start a
/// new chapter.
fn plain_text_chapters(text: &str) -> Vec<Chapter> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let blank_separated = text.contains("\n\n");
    let paragraphs: Vec<String> = if blank_separated {
        text.split("\n\n")
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect()
    } else {
        text.lines().map(|l| l.trim().to_string()).collect()
    };

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current: Option<Chapter> = None;
    for p in paragraphs.into_iter().filter(|p| !p.is_empty()) {
        let is_heading = p.len() < 80 && {
            let l = p.to_lowercase();
            l.starts_with("chapter ")
                || l.starts_with("part ")
                || l == "prologue"
                || l == "epilogue"
        };
        if is_heading || current.is_none() {
            if let Some(c) = current.take() {
                chapters.push(c);
            }
            current = Some(Chapter {
                title: if is_heading {
                    p.clone()
                } else {
                    format!("Section {}", chapters.len() + 1)
                },
                text: String::new(),
            });
        }
        let c = current.as_mut().expect("set above");
        if !c.text.is_empty() {
            c.text.push_str("\n\n");
        }
        c.text.push_str(&p);
    }
    chapters.extend(current);
    chapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palmdoc_lz77() {
        // "abcabcabc" + "x y": literals, a back-reference (distance 3,
        // length 6) and a space+char byte.
        let pair: u16 = (0x8000) | (3 << 3) | (6 - 3);
        let mut input = b"abc".to_vec();
        input.extend_from_slice(&pair.to_be_bytes());
        input.push(b'x');
        input.push(b'y' ^ 0x80);
        input.extend_from_slice(&[0x02, 0xff, 0xfe]);
        let mut out = Vec::new();
        palmdoc_decompress(&input, &mut out);
        assert_eq!(out, b"abcabcabcx y\xff\xfe");
    }

    #[test]
    fn trailing_entries() {
        // One extra-data entry of size 3 (incl. its size byte) plus a
        // multibyte entry of 1+1 bytes.
        let rec = [b'h', b'i', 0xAA, 0x01, 0xBB, 0xCC, 0x83];
        // Strip order: bit 1 entry first (0x83 => size 3), then multibyte.
        assert_eq!(strip_trailing_entries(&rec, 0b11), b"hi");
    }

    #[test]
    fn plain_text_headings() {
        let ch = plain_text_chapters("Intro text\nChapter 1\nIt began.\nCHAPTER 2\nThe end.");
        let titles: Vec<_> = ch.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Section 1", "Chapter 1", "CHAPTER 2"]);
        assert_eq!(ch[1].text, "Chapter 1\n\nIt began.");
    }
}
