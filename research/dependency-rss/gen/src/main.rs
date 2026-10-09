//! Writes the scanned-document-like PDFs the lopdf probe rows use,
//! deterministically, into argv[1]: `scan-<pages>p.pdf`.
//!
//! Each page holds one full-page 640x854 8-bit gray image XObject with
//! incompressible deterministic bytes (a per-page-seeded LCG), stored raw,
//! so the file size is dominated by image streams as in a real scan.
//! A page costs 546,560 bytes of image stream: 100 pages are ~52 MiB.

use lopdf::{Document, Object, Stream, dictionary};

/// Page image size: A4 ratio, ~0.52 MiB of gray bytes per page raw.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 854;
/// Pages per fixture, in run.sh order: ~16, 32, 100-page, 64, 128, 256, 512 MiB.
const FIXTURES: &[u32] = &[30, 60, 100, 120, 240, 480, 960];

/// Deterministic incompressible bytes: the high byte of an LCG.
fn page_bytes(page: u32, len: usize) -> Vec<u8> {
    let mut state = 0x9e3779b9u64 ^ (u64::from(page).wrapping_mul(0xbf58476d1ce4e5b9));
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 56) as u8);
    }
    out
}

fn scanned_pdf(pages: u32) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("probe"),
        "CreationDate" => Object::string_literal("D:19700101000000Z"),
    });
    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(pages as usize);
    for n in 1..=pages {
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(WIDTH),
                "Height" => i64::from(HEIGHT),
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            page_bytes(n, (WIDTH as usize) * (HEIGHT as usize)),
        ));
        let content = format!("q {WIDTH} 0 0 {HEIGHT} 0 0 cm /Im{n} Do Q");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        kids.push(Object::from(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! {
                    format!("Im{n}") => image_id,
                },
            },
            "Contents" => content_id,
        })));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => i64::from(pages),
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: gen <out-dir>");
    for pages in FIXTURES {
        let bytes = scanned_pdf(*pages);
        let path = format!("{dir}/scan-{pages}p.pdf");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            Document::load_mem(&bytes).unwrap().get_pages().len(),
            *pages as usize,
            "round-trip of {path}"
        );
        eprintln!(
            "{path}: {pages} pages, {} bytes ({:.1} MiB)",
            bytes.len(),
            bytes.len() as f64 / 1_048_576.0
        );
    }
}
