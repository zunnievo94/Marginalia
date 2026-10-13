use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use epub::doc::{EpubDoc, NavPoint};

use crate::book::sniff_image;
use crate::{html, Book, Chapter, Error, Format, Image, Metadata, Result};

type Doc = EpubDoc<Cursor<Vec<u8>>>;

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
        language: doc.mdata("language").map(|m| m.value.trim().to_string()),
    };

    let spine: Vec<String> = doc
        .spine
        .iter()
        .filter(|s| s.linear)
        .map(|s| s.idref.clone())
        .collect();

    let mut images = Images::default();
    let mut chapters = Vec::new();
    for idref in spine {
        let Some(path) = doc.resources.get(&idref).map(|r| r.path.clone()) else {
            continue;
        };
        let Some((bytes, mime)) = doc.get_resource(&idref) else {
            continue;
        };
        if !mime.contains("html") && !mime.contains("xml") {
            continue;
        }
        let converted = html::convert(&html::decode_markup(&bytes));
        let fallback_title = titles
            .get(&normalize(&path))
            .cloned()
            .or_else(|| converted.heading.clone())
            .unwrap_or_else(|| format!("Section {}", chapters.len() + 1));
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let chapter = Chapter::from_html(fallback_title, converted, |src| {
            images.add(&mut doc, &resolve(&base, src)?)
        });
        // Skip blank pages; keep picture-only pages (covers, illustrations).
        if chapter.text.trim().is_empty() && !chapter.has_images() {
            continue;
        }
        chapters.push(chapter);
    }

    let cover = doc
        .get_cover_id()
        .and_then(|id| doc.resources.get(&id).map(|r| r.path.clone()))
        .and_then(|path| images.add(&mut doc, &path))
        .or_else(|| (!images.list.is_empty()).then_some(0));

    Ok(Book {
        format: Format::Epub,
        metadata,
        chapters,
        images: images.list,
        cover,
    })
}

/// Images loaded from the archive, each stored once.
#[derive(Default)]
struct Images {
    list: Vec<Image>,
    by_path: HashMap<PathBuf, Option<usize>>,
}

impl Images {
    fn add(&mut self, doc: &mut Doc, path: &Path) -> Option<usize> {
        if let Some(found) = self.by_path.get(path) {
            return *found;
        }
        let index = doc.get_resource_by_path(path).and_then(|data| {
            let media_type = doc
                .get_resource_mime_by_path(path)
                .filter(|m| m.starts_with("image/"))
                .or_else(|| sniff_image(&data).map(str::to_string))?;
            self.list.push(Image { media_type, data });
            Some(self.list.len() - 1)
        });
        self.by_path.insert(path.to_path_buf(), index);
        index
    }
}

/// Resolve an `src` relative to the chapter's directory inside the archive.
/// Archive paths always use `/`, whatever the OS (so no `Path::join`, which
/// would use `\\` on Windows).
fn resolve(base: &Path, src: &str) -> Option<PathBuf> {
    if src.contains("://") || src.starts_with("data:") {
        return None; // remote or inline images are not supported
    }
    let src = percent_decode(src.split(['#', '?']).next()?);
    let base = base.to_string_lossy().replace('\\', "/");
    let mut parts: Vec<&str> = if src.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for segment in src.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Some(PathBuf::from(parts.join("/")))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths() {
        let base = Path::new("OEBPS/Text");
        assert_eq!(
            resolve(base, "../Images/a%20b.png#x"),
            Some(PathBuf::from("OEBPS/Images/a b.png"))
        );
        assert_eq!(
            resolve(base, "c.jpg"),
            Some(PathBuf::from("OEBPS/Text/c.jpg"))
        );
        assert_eq!(
            resolve(base, "/img/d.gif"),
            Some(PathBuf::from("img/d.gif"))
        );
        assert_eq!(resolve(base, "https://x/y.png"), None);
        // Windows-style base paths still produce archive (`/`) paths.
        assert_eq!(
            resolve(Path::new("OEBPS\\Text"), "../img/e.png")
                .map(|p| p.to_string_lossy().into_owned()),
            Some("OEBPS/img/e.png".to_string())
        );
    }
}
