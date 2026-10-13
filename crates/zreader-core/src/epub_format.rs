use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use epub::doc::{EpubDoc, NavPoint};

use crate::{html, Book, Chapter, Error, Format, Metadata, Result};

pub(crate) fn parse(bytes: &[u8]) -> Result<Book> {
    let mut doc = EpubDoc::from_reader(Cursor::new(bytes.to_vec()))
        .map_err(|e| Error::Epub(e.to_string()))?;

    let mut titles = HashMap::new();
    collect_toc_titles(&doc.toc, &mut titles);

    let metadata = Metadata {
        title: doc.get_title().unwrap_or_default().trim().to_string(),
        authors: doc
            .metadata
            .iter()
            .filter(|m| m.property == "creator")
            .map(|m| m.value.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        language: doc.mdata("language").map(|m| m.value.clone()),
    };

    let spine: Vec<String> = doc
        .spine
        .iter()
        .filter(|s| s.linear)
        .map(|s| s.idref.clone())
        .collect();

    let mut chapters = Vec::new();
    for idref in spine {
        let Some(path) = doc.resources.get(&idref).map(|r| r.path.clone()) else {
            continue;
        };
        let Some((content, mime)) = doc.get_resource_str(&idref) else {
            continue;
        };
        if !mime.contains("html") && !mime.contains("xml") {
            continue;
        }
        let converted = html::convert(&content);
        if converted.text.trim().is_empty() {
            continue; // cover pages, image-only pages
        }
        let title = titles
            .get(&normalize(&path))
            .cloned()
            .or(converted.heading)
            .unwrap_or_else(|| format!("Section {}", chapters.len() + 1));
        chapters.push(Chapter {
            title,
            text: converted.text,
        });
    }

    Ok(Book {
        format: Format::Epub,
        metadata,
        chapters,
    })
}

/// Map each content file to the first table-of-contents label pointing at it.
fn collect_toc_titles(points: &[NavPoint], out: &mut HashMap<PathBuf, String>) {
    for p in points {
        let key = normalize(&p.content);
        let label = p.label.trim();
        if !label.is_empty() {
            out.entry(key).or_insert_with(|| label.to_string());
        }
        collect_toc_titles(&p.children, out);
    }
}

/// Strip any `#fragment` so TOC entries match spine resource paths.
fn normalize(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    PathBuf::from(s.split('#').next().unwrap_or_default())
}
