//! End-to-end parsing of small synthetic books for each format.

use std::io::Write;

use marginalia_core::{open_bytes, Block, Error, Format};

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];
const JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 0xFF, 0xD9,
];

fn build_epub() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let files: &[(&str, &str)] = &[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        ),
        (
            "OEBPS/content.opf",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Test Book</dc:title>
    <dc:creator>Jane Doe</dc:creator>
    <dc:language>en</dc:language>
    <dc:identifier id="id">urn:uuid:1234</dc:identifier>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="cv" href="text/cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover-img" href="images/cover.jpg" media-type="image/jpeg"/>
    <item id="pic" href="images/pic.png" media-type="image/png"/>
  </manifest>
  <spine toc="ncx"><itemref idref="cv"/><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#,
        ),
        (
            "OEBPS/toc.ncx",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head></head><docTitle><text>Test Book</text></docTitle>
  <navMap>
    <navPoint id="n1" playOrder="1"><navLabel><text>The Beginning</text></navLabel><content src="ch1.xhtml"/></navPoint>
  </navMap>
</ncx>"#,
        ),
        (
            "OEBPS/ch1.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c1</title></head>
<body><h1>One</h1><p>It was a dark night.</p></body></html>"#,
        ),
        (
            "OEBPS/ch2.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c2</title></head>
<body><h2>Two</h2><p>Morning came.</p><img src="images/pic.png" alt="Sunrise"/></body></html>"#,
        ),
        (
            "OEBPS/text/cover.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="../images/cover.jpg"/></svg></body></html>"#,
        ),
    ];
    for (name, body) in files {
        zip.start_file(*name, stored).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    for (name, data) in [
        ("OEBPS/images/cover.jpg", JPEG),
        ("OEBPS/images/pic.png", PNG),
    ] {
        zip.start_file(name, stored).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    buf.into_inner()
}

/// Assemble a Palm database from a type/creator and records.
fn palm_db(kind: &[u8; 8], records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 78];
    out[..9].copy_from_slice(b"Palm_Name");
    out[60..68].copy_from_slice(kind);
    out[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
    let mut offset = 78 + records.len() * 8 + 2;
    for (i, r) in records.iter().enumerate() {
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, i as u8]);
        offset += r.len();
    }
    out.extend_from_slice(&[0, 0]);
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

fn build_mobi(encryption: u16) -> Vec<u8> {
    let text = "<html><body><h1>Chapter 1</h1><p>Caf\u{e9} opens.</p>\
                <mbp:pagebreak/><h2>Chapter 2</h2><p>It closes.</p><img recindex=\"00001\"/></body></html>";
    let text = text.as_bytes();

    let header_len = 0xE8usize;
    let mut rec0 = vec![0u8; 16 + header_len];
    rec0[0..2].copy_from_slice(&1u16.to_be_bytes()); // no compression
    rec0[4..8].copy_from_slice(&(text.len() as u32).to_be_bytes());
    rec0[8..10].copy_from_slice(&1u16.to_be_bytes()); // one text record
    rec0[10..12].copy_from_slice(&4096u16.to_be_bytes());
    rec0[12..14].copy_from_slice(&encryption.to_be_bytes());
    rec0[16..20].copy_from_slice(b"MOBI");
    rec0[20..24].copy_from_slice(&(header_len as u32).to_be_bytes());
    rec0[28..32].copy_from_slice(&65001u32.to_be_bytes()); // UTF-8
    rec0[128..132].copy_from_slice(&0x40u32.to_be_bytes()); // has EXTH
    rec0[0xF2..0xF4].copy_from_slice(&1u16.to_be_bytes()); // multibyte trailer
    rec0[0x6C..0x70].copy_from_slice(&2u32.to_be_bytes()); // first image record

    // EXTH with an author and a cover (image offset 1 => record 3).
    let author = b"John Roe";
    rec0.extend_from_slice(b"EXTH");
    rec0.extend_from_slice(&((12 + 8 + author.len() + 12) as u32).to_be_bytes());
    rec0.extend_from_slice(&2u32.to_be_bytes());
    rec0.extend_from_slice(&100u32.to_be_bytes());
    rec0.extend_from_slice(&((8 + author.len()) as u32).to_be_bytes());
    rec0.extend_from_slice(author);
    rec0.extend_from_slice(&201u32.to_be_bytes());
    rec0.extend_from_slice(&12u32.to_be_bytes());
    rec0.extend_from_slice(&1u32.to_be_bytes());

    // Full name.
    let name = b"Mobi Book";
    let name_off = rec0.len() as u32;
    rec0.extend_from_slice(name);
    rec0[84..88].copy_from_slice(&name_off.to_be_bytes());
    rec0[88..92].copy_from_slice(&(name.len() as u32).to_be_bytes());

    let mut rec1 = text.to_vec();
    rec1.push(0x00); // multibyte trailer: (0 & 3) + 1 = 1 byte

    palm_db(b"BOOKMOBI", &[rec0, rec1, PNG.to_vec(), JPEG.to_vec()])
}

#[test]
fn epub() {
    let book = open_bytes(&build_epub()).unwrap();
    assert_eq!(book.format, Format::Epub);
    assert_eq!(book.metadata.title, "Test Book");
    assert_eq!(book.metadata.authors, ["Jane Doe"]);
    assert_eq!(book.metadata.language.as_deref(), Some("en"));
    // A picture-only cover page is kept, as its own section.
    assert_eq!(book.chapters.len(), 3);
    assert_eq!(book.chapters[0].text, "");
    assert_eq!(
        book.chapters[0].blocks,
        [Block::Image {
            image: 0,
            alt: String::new()
        }]
    );
    // Title from the NCX, then falling back to the first heading.
    assert_eq!(book.chapters[1].title, "The Beginning");
    assert_eq!(book.chapters[2].title, "Two");
    assert_eq!(book.chapters[1].text, "One\n\nIt was a dark night.");
    assert_eq!(
        book.chapters[2].blocks.last(),
        Some(&Block::Image {
            image: 1,
            alt: "Sunrise".into()
        })
    );
    assert_eq!(book.images.len(), 2);
    assert_eq!(book.images[1].media_type, "image/png");
    assert_eq!(book.images[1].data, PNG);
    assert_eq!(book.cover_image().unwrap().media_type, "image/jpeg");
}

#[test]
fn mobi() {
    let book = open_bytes(&build_mobi(0)).unwrap();
    assert_eq!(book.format, Format::Mobi);
    assert_eq!(book.metadata.title, "Mobi Book");
    assert_eq!(book.metadata.authors, ["John Roe"]);
    let titles: Vec<_> = book.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Chapter 1", "Chapter 2"]);
    assert_eq!(book.chapters[0].text, "Chapter 1\n\nCaf\u{e9} opens.");
    assert_eq!(book.chapters[1].text, "Chapter 2\n\nIt closes.");
    assert!(matches!(
        book.chapters[1].blocks.last(),
        Some(Block::Image { .. })
    ));
    assert_eq!(book.images[0].data, PNG);
    assert_eq!(book.cover_image().unwrap().data, JPEG);
}

#[test]
fn encrypted_mobi_is_rejected() {
    assert!(matches!(open_bytes(&build_mobi(2)), Err(Error::Encrypted)));
}

#[test]
fn palmdoc_prc() {
    let text = b"Prologue\r\nOnce upon a time.\r\nChapter 1\r\nThe story.";
    let mut rec0 = vec![0u8; 16];
    rec0[0..2].copy_from_slice(&1u16.to_be_bytes());
    rec0[4..8].copy_from_slice(&(text.len() as u32).to_be_bytes());
    rec0[8..10].copy_from_slice(&1u16.to_be_bytes());
    let book = open_bytes(&palm_db(b"TEXtREAd", &[rec0, text.to_vec()])).unwrap();
    assert_eq!(book.format, Format::PalmDoc);
    assert_eq!(book.metadata.title, "Palm Name");
    let titles: Vec<_> = book.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Prologue", "Chapter 1"]);
}

#[test]
fn unknown_format() {
    assert!(matches!(
        open_bytes(b"not a book"),
        Err(Error::UnsupportedFormat)
    ));
}
