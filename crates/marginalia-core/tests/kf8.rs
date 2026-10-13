//! KF8 (AZW3) books produced by calibre.

use marginalia_core::{open, Block};

fn data(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn kf8_chapters_follow_the_ncx() {
    let book = open(data("kieu.azw3")).unwrap();
    assert_eq!(book.metadata.title, "Truyện Kiều");
    assert_eq!(book.metadata.authors, ["Nguyễn Du"]);
    let titles: Vec<_> = book.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(&titles[..2], ["Hồi thứ nhất", "Hồi thứ hai"]);
    assert!(book.chapters[0]
        .text
        .contains("Trăm năm trong cõi người ta, chữ tài chữ mệnh khéo là ghét nhau."));
    assert!(book.chapters[1]
        .text
        .contains("Rằng năm Gia Tĩnh triều Minh"));
}

#[test]
fn kf8_images_and_cover() {
    for file in ["illus.azw3", "illus-both.mobi"] {
        let book = open(data(file)).unwrap();
        let chapter = &book.chapters[0];
        assert_eq!(chapter.title, "Chương 1: Thung lũng", "{file}");
        let images: Vec<usize> = chapter
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { image, .. } => Some(*image),
                Block::Text(_) => None,
            })
            .collect();
        assert_eq!(images.len(), 1, "{file}: one inline picture");
        let picture = &book.images[images[0]];
        assert!(picture.media_type.starts_with("image/"), "{file}");
        let cover = book.cover_image().expect("cover");
        assert_eq!(cover.media_type, "image/jpeg", "{file}");
        assert_ne!(cover.data, picture.data, "{file}: cover is not the picture");
        // The picture sits between the two paragraphs.
        assert!(
            matches!(chapter.blocks.first(), Some(Block::Text(_))),
            "{file}"
        );
    }
}
