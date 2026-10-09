//! Writes the PDFs the lopdf probe rows use, deterministically, into
//! argv[1]: `scan-<pages>p.pdf`, the cap-sized `scan-cap.pdf`, and the
//! many-small-objects `manyobjs-100p.pdf`.
//!
//! Each `scan-` page holds one full-page 640x854 8-bit gray image XObject with
//! incompressible deterministic bytes (a per-page-seeded LCG), stored raw,
//! so the file size is dominated by image streams as in a real scan.
//! A page costs 546,560 bytes of image stream: 100 pages are ~52 MiB.
//!
//! `scan-cap.pdf` is the same kind of file topped up with an Info padding
//! string to exactly the 100 MiB cap, and `manyobjs-100p.pdf` carries ~200
//! small annotation-like objects per page in Flate object streams with a
//! cross-reference stream, so its peak comes from object count and
//! decompression, not raw bytes.

use lopdf::{Document, LoadOptions, Object, Stream, dictionary};

/// The image child's per-stream object-stream decompression limit, separate
/// from the 100 MiB file cap (`docs/tools.md`, "read"): the probe loads
/// with the same limit.
const MAX_DECOMPRESSED_BYTES: usize = 67_108_864;
/// The file cap in bytes: `scan-cap.pdf` is exactly this long.
const CAP_BYTES: usize = 104_857_600;
/// Pages for the cap-sized fixture: raw image bytes alone stay under the
/// cap, leaving room for the padding string that tops the file up to it.
const CAP_PAGES: u32 = 191;
/// Pages of the many-small-objects fixture and annots per page.
const MANY_PAGES: u32 = 100;
const MANY_PER_PAGE: u32 = 200;

/// Loads bytes the way the image child does.
fn load(bytes: &[u8]) -> Document {
    Document::load_mem_with_options(
        bytes,
        LoadOptions::with_max_decompressed_size(MAX_DECOMPRESSED_BYTES),
    )
    .unwrap()
}

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

fn scanned_doc(pages: u32) -> Document {
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
    doc
}

fn scanned_pdf(pages: u32) -> Vec<u8> {
    let mut doc = scanned_doc(pages);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

/// A scanned-like PDF of exactly [`CAP_BYTES`] bytes: [`CAP_PAGES`] pages of
/// raw image, plus an Info padding string sized so the saved file hits the
/// cap byte-for-byte. The first pass measures the pages alone and sizes the
/// entry past the cap on purpose; later passes only grow or shrink the
/// string, which moves the length exactly 1:1 (fixed-width xref entries and
/// a same-digit startxref), so the loop converges instead of assuming sizes.
fn scan_cap_pdf() -> Vec<u8> {
    let mut doc = scanned_doc(CAP_PAGES);
    let info_id = doc
        .trailer
        .get(b"Info")
        .and_then(Object::as_reference)
        .unwrap();
    let mut pad = 0usize;
    for _ in 0..10 {
        if pad > 0 {
            let info = doc
                .objects
                .get_mut(&info_id)
                .unwrap()
                .as_dict_mut()
                .unwrap();
            info.set("CapPad", Object::string_literal("A".repeat(pad).as_str()));
        }
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        if bytes.len() == CAP_BYTES {
            return bytes;
        }
        if pad == 0 {
            assert!(
                bytes.len() < CAP_BYTES,
                "the pages alone must stay under the cap"
            );
            // Deliberately past the cap: the entry itself costs bytes around
            // the padding, and the next pass trims exactly.
            pad = CAP_BYTES - bytes.len();
        } else {
            pad = pad.saturating_add_signed((CAP_BYTES as i64 - bytes.len() as i64) as isize);
        }
    }
    panic!("the padding did not converge on {CAP_BYTES} bytes");
}

/// Deterministic incompressible hex: the low nibble of an LCG seeded by page
/// and index, so packed annotation strings cannot collapse under Flate.
fn lcg_hex(page: u32, n: u32, len: usize) -> String {
    let mut state = 0x9e3779b9u64
        ^ (u64::from(page).wrapping_mul(0xbf58476d1ce4e5b9))
        ^ (u64::from(n).wrapping_mul(0x94d049bb133111eb));
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push(char::from_digit((state >> 56) as u32 % 16, 16).unwrap());
    }
    out
}

/// A [`MANY_PAGES`]-page PDF with [`MANY_PER_PAGE`] small annotation-like
/// objects per page, packed by the writer into Flate object streams with a
/// cross-reference stream: a few MiB on disk, so load peak comes from object
/// count and decompression, not raw bytes.
fn many_objects_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let catalog_id = doc.new_object_id();
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let mut kids = Vec::new();
    for page in 1..=MANY_PAGES {
        let mut annots = Vec::with_capacity(MANY_PER_PAGE as usize);
        for n in 0..MANY_PER_PAGE {
            let note = format!("note {page}-{n} {}", lcg_hex(page, n, 256));
            annots.push(Object::from(doc.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Text",
                "Rect" => vec![
                    Object::Integer(i64::from(n % 20) * 25),
                    Object::Integer(i64::from(n / 20) * 70),
                    Object::Integer(i64::from(n % 20) * 25 + 20),
                    Object::Integer(i64::from(n / 20) * 70 + 20),
                ],
                "Contents" => Object::string_literal(note.as_str()),
                "C" => vec![1.into(), 0.into(), 0.into()],
            })));
        }
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            format!("page {page} annots {}", annots.len()).into_bytes(),
        ));
        kids.push(Object::from(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => font_id,
                },
            },
            "Contents" => content_id,
            "Annots" => annots,
        })));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => i64::from(MANY_PAGES),
        }),
    );
    doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }),
    );
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_with_options(
        &mut bytes,
        lopdf::SaveOptions {
            use_object_streams: true,
            use_xref_streams: true,
            ..Default::default()
        },
    )
    .unwrap();
    bytes
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: gen <out-dir>");
    for pages in FIXTURES {
        let bytes = scanned_pdf(*pages);
        let path = format!("{dir}/scan-{pages}p.pdf");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            load(&bytes).get_pages().len(),
            *pages as usize,
            "round-trip of {path}"
        );
        eprintln!(
            "{path}: {pages} pages, {} bytes ({:.1} MiB)",
            bytes.len(),
            bytes.len() as f64 / 1_048_576.0
        );
    }
    let capped = scan_cap_pdf();
    let path = format!("{dir}/scan-cap.pdf");
    std::fs::write(&path, &capped).unwrap();
    assert_eq!(capped.len(), CAP_BYTES, "{path} must be exactly the cap");
    assert_eq!(
        load(&capped).get_pages().len(),
        CAP_PAGES as usize,
        "round-trip of {path}"
    );
    eprintln!(
        "{path}: {CAP_PAGES} pages, {} bytes ({:.1} MiB)",
        capped.len(),
        capped.len() as f64 / 1_048_576.0
    );
    let many = many_objects_pdf();
    let path = format!("{dir}/manyobjs-100p.pdf");
    std::fs::write(&path, &many).unwrap();
    assert_eq!(
        load(&many).get_pages().len(),
        MANY_PAGES as usize,
        "round-trip of {path}"
    );
    eprintln!(
        "{path}: {MANY_PAGES} pages, {} bytes ({:.1} MiB)",
        many.len(),
        many.len() as f64 / 1_048_576.0
    );
}
