//! KF8 (AZW3) books.
//!
//! A KF8 book stores its XHTML files cut into pieces: each file's
//! "skeleton" (the outer markup) and its "fragments" (the content) are
//! listed in two INDX indexes, and the table of contents in a third (NCX).
//! We reassemble the files, use each as a chapter, and title chapters from
//! the NCX. The layout follows the format as documented by the MobileRead
//! wiki and implemented in KindleUnpack and calibre.

use std::collections::HashMap;

use crate::book::sniff_image;
use crate::mobi_format::{decode, err, read_text, u16_at, u32_at, MobiHeader, PalmDb};
use crate::{html, Book, Chapter, Format, Image, Metadata, Result};

/// One INDX entry: its label and tag values.
#[derive(Debug, Default)]
pub(crate) struct IndexEntry {
    pub(crate) label: Vec<u8>,
    pub(crate) tags: HashMap<u8, Vec<u32>>,
}

/// Forward variable-width integer: 7 bits per byte, high bit marks the
/// last byte. Returns (bytes consumed, value).
fn forward_varint(data: &[u8], mut pos: usize) -> Option<(usize, u32)> {
    let start = pos;
    let mut value: u32 = 0;
    loop {
        let b = *data.get(pos)?;
        pos += 1;
        value = (value << 7) | (b & 0x7f) as u32;
        if b & 0x80 != 0 || pos - start >= 5 {
            return Some((pos - start, value));
        }
    }
}

struct IndxHeader {
    /// Header length (the TAGX section follows it in the main record).
    len: usize,
    /// IDXT position (entry records) .
    start: usize,
    /// Number of entry records (main record) or entries (entry record).
    count: usize,
    /// Number of CNCX string records after the entry records.
    nctoc: usize,
}

fn indx_header(data: &[u8]) -> Result<IndxHeader> {
    if !data.starts_with(b"INDX") {
        return Err(err("bad INDX record"));
    }
    let word = |i: usize| u32_at(data, 4 + i * 4).map(|v| v as usize).unwrap_or(0);
    // len, nul1, type, gen, start, count, code, lng, total, ordt, ligt, nligt, nctoc
    Ok(IndxHeader {
        len: word(0),
        start: word(4),
        count: word(5),
        nctoc: word(12),
    })
}

/// (tag, values per entry, mask, end flag)
type TagxEntry = (u8, u8, u8, u8);

fn read_tagx(data: &[u8], start: usize) -> (usize, Vec<TagxEntry>) {
    if data.get(start..start + 4) != Some(b"TAGX") {
        return (0, Vec::new());
    }
    let first_entry = u32_at(data, start + 4).unwrap_or(12) as usize;
    let control_bytes = u32_at(data, start + 8).unwrap_or(0) as usize;
    let mut tags = Vec::new();
    let mut i = 12;
    while i + 4 <= first_entry {
        if let Some(t) = data.get(start + i..start + i + 4) {
            tags.push((t[0], t[1], t[2], t[3]));
        }
        i += 4;
    }
    (control_bytes, tags)
}

fn tag_map(
    control_bytes: usize,
    table: &[TagxEntry],
    data: &[u8],
    start: usize,
) -> HashMap<u8, Vec<u32>> {
    // First pass: how many values each present tag has.
    enum Count {
        Values(u32),
        Bytes(u32),
    }
    let mut present: Vec<(u8, Count, u8)> = Vec::new();
    let mut control_index = 0;
    let mut pos = start + control_bytes;
    for &(tag, per_entry, mask, end_flag) in table {
        if end_flag == 1 {
            control_index += 1;
            continue;
        }
        let Some(&cbyte) = data.get(start + control_index) else {
            break;
        };
        let value = cbyte & mask;
        if value == 0 {
            continue;
        }
        if value == mask {
            if mask.count_ones() > 1 {
                // A varint giving the byte length of the values follows.
                let Some((used, n)) = forward_varint(data, pos) else {
                    break;
                };
                pos += used;
                present.push((tag, Count::Bytes(n), per_entry));
            } else {
                present.push((tag, Count::Values(1), per_entry));
            }
        } else {
            present.push((
                tag,
                Count::Values((value >> mask.trailing_zeros()) as u32),
                per_entry,
            ));
        }
    }
    // Second pass: read the values.
    let mut out = HashMap::new();
    for (tag, count, per_entry) in present {
        let mut values = Vec::new();
        match count {
            Count::Values(n) => {
                for _ in 0..n * per_entry.max(1) as u32 {
                    let Some((used, v)) = forward_varint(data, pos) else {
                        break;
                    };
                    pos += used;
                    values.push(v);
                }
            }
            Count::Bytes(n) => {
                let end = pos + n as usize;
                while pos < end {
                    let Some((used, v)) = forward_varint(data, pos) else {
                        break;
                    };
                    pos += used;
                    values.push(v);
                }
            }
        }
        out.insert(tag, values);
    }
    out
}

/// Read an index whose main record is `db[idx]`: its entries and the CNCX
/// strings (by offset).
pub(crate) fn read_index(
    db: &PalmDb,
    idx: usize,
) -> Result<(Vec<IndexEntry>, HashMap<u32, Vec<u8>>)> {
    let main = db.record(idx).ok_or_else(|| err("missing INDX record"))?;
    let header = indx_header(main)?;
    let (control_bytes, tagx) = read_tagx(main, header.len);

    let mut cncx = HashMap::new();
    for j in 0..header.nctoc {
        let Some(data) = db.record(idx + header.count + 1 + j) else {
            break;
        };
        let base = (j as u32) << 16;
        let mut off = 0;
        while off < data.len() && data[off] != 0 {
            let Some((used, len)) = forward_varint(data, off) else {
                break;
            };
            let start = off + used;
            let end = (start + len as usize).min(data.len());
            cncx.insert(base + off as u32, data[start..end].to_vec());
            off = end;
        }
    }

    let mut entries = Vec::new();
    for i in 0..header.count {
        let data = db
            .record(idx + 1 + i)
            .ok_or_else(|| err("missing INDX entry record"))?;
        let h = indx_header(data)?;
        let positions: Vec<usize> = (0..h.count)
            .filter_map(|j| u16_at(data, h.start + 4 + 2 * j).map(|p| p as usize))
            .collect();
        for &pos in &positions {
            let Some(&len) = data.get(pos) else { continue };
            let label_end = (pos + 1 + len as usize).min(data.len());
            entries.push(IndexEntry {
                label: data[pos + 1..label_end].to_vec(),
                tags: tag_map(control_bytes, &tagx, data, label_end),
            });
        }
    }
    Ok((entries, cncx))
}

fn tag(entry: &IndexEntry, t: u8, i: usize) -> Option<usize> {
    entry.tags.get(&t)?.get(i).map(|&v| v as usize)
}

/// Decode a `kindle:embed:XXXX` base-32 resource number (1-based).
pub(crate) fn kindle_embed_index(src: &str) -> Option<usize> {
    let code = src.strip_prefix("kindle:embed:")?;
    let code = code.split(['?', '#']).next()?;
    let mut n = 0usize;
    for c in code.chars() {
        n = n * 32 + c.to_digit(32)? as usize;
    }
    Some(n)
}

/// Parse the KF8 book whose header is record `base`.
/// `first_resource` overrides the header's first-resource record: in hybrid
/// files the images are stored once, in the MOBI 7 part, and only the
/// MOBI 7 header points at them.
pub(crate) fn parse(
    db: &PalmDb,
    base: usize,
    metadata: Metadata,
    cover_offset: Option<usize>,
    first_resource: Option<usize>,
) -> Result<Book> {
    let rec0 = db.record(base).ok_or_else(|| err("missing KF8 header"))?;
    if rec0.get(16..20) != Some(b"MOBI") {
        return Err(err("missing KF8 MOBI header"));
    }
    let header = MobiHeader::parse(rec0);
    let text_length = u32_at(rec0, 4).unwrap_or(u32::MAX) as usize;
    let mut raw = read_text(db, base, rec0, &header)?;
    raw.truncate(text_length);

    // Flow 0 is the XHTML; other flows hold CSS/SVG.
    let flow0 = match header.fdst.and_then(|i| db.record(base + i)) {
        Some(fdst) if fdst.starts_with(b"FDST") => {
            let start = u32_at(fdst, 12).unwrap_or(0) as usize;
            let end = (u32_at(fdst, 16).unwrap_or(raw.len() as u32) as usize).min(raw.len());
            &raw[start.min(end)..end]
        }
        _ => &raw[..],
    };

    let skeletons = header
        .skeletons
        .map(|i| read_index(db, base + i))
        .transpose()?
        .map(|(e, _)| e)
        .unwrap_or_default();
    let fragments = header
        .fragments
        .map(|i| read_index(db, base + i))
        .transpose()?
        .map(|(e, _)| e)
        .unwrap_or_default();
    if skeletons.is_empty() {
        return Err(err("KF8 book without skeleton index"));
    }

    // Rebuild each file: insert its fragments into its skeleton.
    let mut parts: Vec<Vec<u8>> = Vec::new();
    // Fragment index -> part number, to place NCX entries.
    let mut fragment_part: Vec<usize> = Vec::new();
    let mut frag_ptr = 0;
    for (part_no, skel) in skeletons.iter().enumerate() {
        let frag_count = tag(skel, 1, 0).unwrap_or(0);
        let skel_pos = tag(skel, 6, 0).unwrap_or(0);
        let skel_len = tag(skel, 6, 1).unwrap_or(0);
        let skel_end = (skel_pos + skel_len).min(flow0.len());
        let mut part = flow0[skel_pos.min(skel_end)..skel_end].to_vec();
        let mut base_ptr = skel_end;
        for _ in 0..frag_count {
            let Some(frag) = fragments.get(frag_ptr) else {
                break;
            };
            frag_ptr += 1;
            fragment_part.push(part_no);
            let insert_pos: usize = std::str::from_utf8(&frag.label)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(skel_pos + part.len());
            let length = tag(frag, 6, 1).unwrap_or(0);
            let slice_end = (base_ptr + length).min(flow0.len());
            let slice = &flow0[base_ptr.min(slice_end)..slice_end];
            let at = insert_pos.saturating_sub(skel_pos).min(part.len());
            part.splice(at..at, slice.iter().copied());
            base_ptr = slice_end;
        }
        parts.push(part);
    }

    // Table of contents: first (shallowest) entry pointing into each part.
    let mut part_titles: HashMap<usize, (usize, String)> = HashMap::new();
    if let Some(ncx) = header.ncx {
        if let Ok((entries, cncx)) = read_index(db, base + ncx) {
            for e in &entries {
                let Some(name) = tag(e, 3, 0).and_then(|o| cncx.get(&(o as u32))) else {
                    continue;
                };
                let title = decode(name, true).trim().to_string();
                let level = tag(e, 4, 0).unwrap_or(0);
                let Some(part) = tag(e, 6, 0).and_then(|fid| fragment_part.get(fid)) else {
                    continue;
                };
                match part_titles.get(part) {
                    Some((l, _)) if *l <= level => {}
                    _ => {
                        part_titles.insert(*part, (level, title));
                    }
                }
            }
        }
    }

    // Images: kindle:embed:XXXX counts from the first resource record.
    let first = first_resource.unwrap_or(header.first_image);
    let mut images = Vec::new();
    let mut by_record: HashMap<usize, Option<usize>> = HashMap::new();
    let mut load_image = |record: usize| -> Option<usize> {
        *by_record.entry(record).or_insert_with(|| {
            let data = db.record(record)?;
            let media_type = sniff_image(data)?;
            images.push(Image {
                media_type: media_type.to_string(),
                data: data.to_vec(),
            });
            Some(images.len() - 1)
        })
    };
    let resource_record = |n: usize| (first > 0 && n > 0).then(|| first + n - 1);

    let mut chapters = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let markup = decode(part, true);
        let converted = html::convert(&markup);
        let title = part_titles
            .get(&i)
            .map(|(_, t)| t.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| converted.heading.clone())
            .unwrap_or_else(|| format!("Section {}", chapters.len() + 1));
        let chapter = Chapter::from_html(title, converted, |src| {
            let n = kindle_embed_index(src)?;
            load_image(resource_record(n)?)
        });
        if chapter.text.trim().is_empty() && !chapter.has_images() {
            continue;
        }
        chapters.push(chapter);
    }
    if chapters.is_empty() {
        return Err(err("KF8 book has no readable text"));
    }
    let cover = cover_offset
        .and_then(|off| resource_record(off + 1))
        .and_then(&mut load_image);

    Ok(Book {
        format: Format::Mobi,
        metadata,
        chapters,
        images,
        cover,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_embeds() {
        assert_eq!(forward_varint(&[0x81], 0), Some((1, 1)));
        assert_eq!(forward_varint(&[0x01, 0x80], 0), Some((2, 128)));
        assert_eq!(
            kindle_embed_index("kindle:embed:0001?mime=image/jpeg"),
            Some(1)
        );
        assert_eq!(kindle_embed_index("kindle:embed:000A"), Some(10));
        assert_eq!(kindle_embed_index("kindle:embed:0010"), Some(32));
        assert_eq!(kindle_embed_index("images/a.png"), None);
    }
}
