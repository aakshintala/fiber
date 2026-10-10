//! Tests beside [`super::run`]: the PDF mode's counting, cutting and exit codes.

use std::ffi::OsString;
use std::io::Write;

use super::{What, parse_what, run};

/// A stdout whose writes or flushes fail on request.
struct Stream {
    fail_write: bool,
    fail_flush: bool,
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.fail_write {
            return Err(std::io::Error::other("write refused"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.fail_flush {
            return Err(std::io::Error::other("flush refused"));
        }
        Ok(())
    }
}

fn pdf_bytes(pages: usize) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let texts: Vec<String> = (0..pages).map(|n| format!("page {n}")).collect();
    let mut document = Document::with_version("1.5");
    let info_id = document.add_object(dictionary! {
        "Title" => Object::string_literal("test"),
        "CreationDate" => Object::string_literal("D:19700101000000Z"),
    });
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });
    let kids: Vec<Object> = texts
        .iter()
        .map(|text| {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 48.into()]),
                    Operation::new("Td", vec![100.into(), 600.into()]),
                    Operation::new("Tj", vec![Object::string_literal(text.as_str())]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id =
                document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            document
                .add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "Contents" => content_id,
                })
                .into()
        })
        .collect();
    let pages_dict = dictionary! {
        "Type" => "Pages",
        "Kids" => kids,
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    document
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);
    document.compress();
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn load(bytes: &[u8]) -> lopdf::Result<lopdf::Document> {
    lopdf::Document::load_mem_with_options(bytes, super::load_options())
}

fn child(arguments: &[OsString]) -> (i32, String, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(arguments, &mut stdout, &mut stderr);
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

fn write_input(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

// A one-page PDF whose objects the writer packs into a single Flate object
// stream (plus a cross-reference stream) decoding to exactly `decompressed`
// bytes. The padding string is the only part that grows, so a small probe
// build measures the fixed overhead and the real build sizes its padding to
// hit `decompressed` exactly. The page dict is packed on purpose: lopdf
// loads leniently, so a stream past the limit is dropped instead of failing
// the load, and without its page the file counts zero pages, which `run`
// refuses.
fn objstm_pdf(decompressed: usize) -> Vec<u8> {
    use lopdf::{Document, Object, SaveOptions, dictionary};
    fn build(pad: usize) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let catalog_id = document.new_object_id();
        let pages_id = document.new_object_id();
        let page_id = document.new_object_id();
        let pad_id = document.new_object_id();
        document.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! {
                "Type" => "Catalog",
                "Pages" => pages_id,
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        document.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        document
            .objects
            .insert(pad_id, Object::string_literal("A".repeat(pad).as_str()));
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document
            .save_with_options(
                &mut bytes,
                SaveOptions {
                    use_object_streams: true,
                    use_xref_streams: true,
                    ..Default::default()
                },
            )
            .unwrap();
        bytes
    }
    fn packed_size(bytes: &[u8]) -> usize {
        let document = load(bytes).unwrap();
        let mut sizes = Vec::new();
        for object in document.objects.values() {
            if let Object::Stream(stream) = object {
                let is_packed = stream.dict.get(b"Type").and_then(Object::as_name).ok()
                    == Some(b"ObjStm".as_slice());
                if is_packed {
                    sizes.push(stream.get_plain_content().unwrap().len());
                }
            }
        }
        assert_eq!(sizes.len(), 1, "the writer must pack one object stream");
        sizes[0]
    }
    // The probe carries a tiny padding: small enough to load under the
    // limit, so its packed size measures the fixed overhead.
    let probe_pad = 16;
    let overhead = packed_size(&build(probe_pad)) - probe_pad;
    let bytes = build(decompressed - overhead);
    if decompressed <= super::MAX_DECOMPRESSED_BYTES {
        assert_eq!(packed_size(&bytes), decompressed);
    }
    bytes
}

// Like `objstm_pdf`, but the writer packs the objects into two Flate
// object streams: the first holds the catalog, the page tree and two page
// dicts with small filler objects, and the second holds one padding string
// alone, sized so the second stream decodes to exactly `decompressed`
// bytes. The padding sits last, so growing it never moves another object's
// offset and the sizing overhead stays fixed. A lenient load drops an
// over-limit second stream and keeps both pages, so without a refusal the
// file would run with objects silently missing.
fn objstm_pdf_with_plain_pages(decompressed: usize) -> Vec<u8> {
    use lopdf::{Document, Object, SaveOptions, dictionary};
    fn build(pad: usize) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let catalog_id = document.new_object_id();
        let pages_id = document.new_object_id();
        let page1_id = document.new_object_id();
        let page2_id = document.new_object_id();
        document.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! {
                "Type" => "Catalog",
                "Pages" => pages_id,
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page1_id), Object::Reference(page2_id)],
                "Count" => 2,
            }),
        );
        for page_id in [page1_id, page2_id] {
            document.objects.insert(
                page_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                }),
            );
        }
        // Ninety-four fillers and two tails take the first stream to
        // exactly 100 objects with the four above, so the padding below
        // fills the second stream alone.
        for n in 0..94 {
            let filler_id = document.new_object_id();
            document.objects.insert(
                filler_id,
                Object::string_literal(format!("filler {n}").as_str()),
            );
        }
        for n in 0..2 {
            let filler_id = document.new_object_id();
            document.objects.insert(
                filler_id,
                Object::string_literal(format!("tail {n}").as_str()),
            );
        }
        let pad_id = document.new_object_id();
        document
            .objects
            .insert(pad_id, Object::string_literal("A".repeat(pad).as_str()));
        document.trailer.set("Root", catalog_id);
        // The padding is referenced from the trailer so the reference check
        // reaches it: without the reference a dropped second stream would
        // leave both pages loading and nothing to refuse.
        document.trailer.set("Pad", pad_id);
        let mut bytes = Vec::new();
        document
            .save_with_options(
                &mut bytes,
                SaveOptions {
                    use_object_streams: true,
                    use_xref_streams: true,
                    ..Default::default()
                },
            )
            .unwrap();
        bytes
    }
    fn stream_sizes(bytes: &[u8], pad: usize) -> (usize, usize) {
        let document = load(bytes).unwrap();
        let expected = Object::string_literal("A".repeat(pad).as_str());
        let mut plain = None;
        let mut padded = None;
        for object in document.objects.values() {
            if let Object::Stream(stream) = object {
                let is_packed = stream.dict.get(b"Type").and_then(Object::as_name).ok()
                    == Some(b"ObjStm".as_slice());
                if is_packed {
                    let size = stream.get_plain_content().unwrap().len();
                    let holds_pad = lopdf::ObjectStream::new(stream)
                        .unwrap()
                        .objects
                        .values()
                        .any(|object| object == &expected);
                    if holds_pad {
                        assert!(padded.replace(size).is_none());
                    } else {
                        assert!(plain.replace(size).is_none());
                    }
                }
            }
        }
        (
            plain.expect("a first stream of pages"),
            padded.expect("a second stream of padding"),
        )
    }
    // The probe carries a tiny padding: small enough to load under the
    // limit, so the padded stream's size measures the fixed overhead.
    let probe_pad = 16;
    let (_, probe_padded) = stream_sizes(&build(probe_pad), probe_pad);
    let overhead = probe_padded - probe_pad;
    let bytes = build(decompressed - overhead);
    if decompressed <= super::MAX_DECOMPRESSED_BYTES {
        let (_, padded) = stream_sizes(&bytes, decompressed - overhead);
        assert_eq!(padded, decompressed);
    }
    bytes
}

// A one-page PDF saved without object streams, with its content stream's
// id: every object has a `Normal` cross-reference entry, so corrupting one
// object's bytes exercises the reference check on an ordinary object.
fn plain_pdf() -> (Vec<u8>, lopdf::ObjectId) {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 48.into()]),
            Operation::new("Td", vec![100.into(), 600.into()]),
            Operation::new("Tj", vec![Object::string_literal("page")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        "Resources" => dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        },
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    // A plain cross-reference table, so the test below can add an entry.
    document.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    document
        .save_with_options(
            &mut bytes,
            lopdf::SaveOptions {
                use_object_streams: false,
                use_xref_streams: false,
                ..Default::default()
            },
        )
        .unwrap();
    (bytes, content_id)
}

// `bytes` with one object's header overwritten in place: the object's offset
// still points at it, so the lenient load skips only it.
fn pdf_with_broken_object(bytes: &[u8], id: lopdf::ObjectId) -> Vec<u8> {
    let marker = format!("{} {} obj", id.0, id.1).into_bytes();
    let at = bytes
        .windows(marker.len())
        .position(|window| window == marker.as_slice())
        .expect("the object header");
    let mut broken = bytes.to_vec();
    for byte in &mut broken[at..at + marker.len()] {
        *byte = b'X';
    }
    broken
}

// `bytes` with one more cross-reference entry past the last object, pointing
// at the file header: no object loads for it and nothing references it. The
// fresh save writes one `0 <count>` section, so the new entry takes the
// next id and both the section count and `/Size` grow by one. Bytes are
// only inserted past every object, so no listed offset moves.
fn pdf_with_dangling_entry(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let xref = out
        .windows(5)
        .position(|window| window == b"xref\n")
        .expect("an xref table");
    let header_start = xref + 5;
    let header_end = header_start
        + out[header_start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .unwrap();
    let header = std::str::from_utf8(&out[header_start..header_end]).unwrap();
    let (start, count) = header.split_once(' ').unwrap();
    assert_eq!(start, "0");
    let count: u32 = count.parse().unwrap();
    out.splice(header_start..header_end, format!("0 {}", count + 1).bytes());
    let trailer = out
        .windows(7)
        .position(|window| window == b"trailer")
        .expect("a trailer");
    out.splice(trailer..trailer, b"0000000000 00000 n \n".iter().copied());
    let size = out
        .windows(6)
        .position(|window| window == b"/Size ")
        .expect("a /Size entry");
    let digits = size + 6;
    let end = digits
        + out[digits..]
            .iter()
            .position(|&byte| !byte.is_ascii_digit())
            .unwrap();
    assert_eq!(
        std::str::from_utf8(&out[digits..end]).unwrap(),
        count.to_string()
    );
    out.splice(digits..end, (count + 1).to_string().bytes());
    out
}

// A one-page PDF whose catalog references two dictionaries that reference
// each other: the reference walk must terminate on the cycle.
fn pdf_with_cycle() -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let first_id = document.new_object_id();
    let second_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, b"page".to_vec()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    document.objects.insert(
        first_id,
        Object::Dictionary(dictionary! {
            "Next" => Object::Reference(second_id),
        }),
    );
    document.objects.insert(
        second_id,
        Object::Dictionary(dictionary! {
            "Next" => Object::Reference(first_id),
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Cycle" => first_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn whole_at_ten_pages_writes_the_input_bytes_unchanged() {
    let dir = fakes::TempDir::new("fiber-pdf-whole");
    let bytes = pdf_bytes(10);
    let input = write_input(dir.path(), "in.pdf", &bytes);
    // Build the real 5-argument form: pdf <input> <dir> <stem> <what>.
    let real = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_whole10"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&real);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(10)));
    assert_eq!(value.get("total"), Some(&serde_json::json!(10)));
    assert_eq!(value.get("file"), Some(&serde_json::json!("p_whole10.pdf")));
    assert_eq!(
        std::fs::read(dir.path().join("p_whole10.pdf")).unwrap(),
        bytes
    );
}

#[test]
fn whole_at_eleven_pages_exits_4_with_the_total_and_writes_nothing() {
    let dir = fakes::TempDir::new("fiber-pdf-whole-many");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(11));
    let real = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_many"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, _) = child(&real);
    assert_eq!(code, 4);
    assert_eq!(stdout.trim(), "{\"total\":11}");
    assert!(!dir.path().join("p_many.pdf").exists());
}

#[test]
fn pages_two_to_three_of_five_writes_two_pages() {
    let dir = fakes::TempDir::new("fiber-pdf-cut");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(5));
    let real = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_cut"),
        OsString::from("pages=2-3"),
    ];
    let (code, stdout, stderr) = child(&real);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(2)));
    assert_eq!(value.get("total"), Some(&serde_json::json!(5)));
    let cut = std::fs::read(dir.path().join("p_cut.pdf")).unwrap();
    let reloaded = load(&cut).unwrap();
    assert_eq!(reloaded.get_pages().len(), 2);
}

#[test]
fn pages_last_to_last_succeeds_and_past_the_end_exits_4() {
    let dir = fakes::TempDir::new("fiber-pdf-edges");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(5));
    let ok = vec![
        OsString::from("pdf"),
        input.clone().into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_last"),
        OsString::from("pages=5-5"),
    ];
    assert_eq!(child(&ok).0, 0);
    let cut = std::fs::read(dir.path().join("p_last.pdf")).unwrap();
    assert_eq!(load(&cut).unwrap().get_pages().len(), 1);
    for (n, what) in ["pages=5-6", "pages=6-6"].iter().enumerate() {
        let stem = format!("p_past{n}");
        let arguments = vec![
            OsString::from("pdf"),
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from(&stem),
            OsString::from(what),
        ];
        let (code, stdout, _) = child(&arguments);
        assert_eq!(code, 4, "{what}");
        assert_eq!(stdout.trim(), "{\"total\":5}", "{what}");
        assert!(!dir.path().join(format!("{stem}.pdf")).exists());
    }
}

#[test]
fn non_pdf_truncated_and_zero_page_pdfs_exit_1() {
    let dir = fakes::TempDir::new("fiber-pdf-refused");
    let input = write_input(dir.path(), "x.pdf", b"not a pdf");
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_bad"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());

    let truncated = pdf_bytes(3);
    let input = write_input(dir.path(), "t.pdf", &truncated[..truncated.len() / 2]);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_trunc"),
        OsString::from("whole=10"),
    ];
    let (code, _, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(!stderr.is_empty());

    let mut empty = lopdf::Document::with_version("1.4");
    let mut bytes = Vec::new();
    empty.save_to(&mut bytes).unwrap();
    let input = write_input(dir.path(), "e.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_empty"),
        OsString::from("whole=10"),
    ];
    let (code, _, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(!stderr.is_empty());
}

#[test]
fn an_object_stream_past_the_limit_is_refused_and_writes_nothing() {
    let dir = fakes::TempDir::new("fiber-pdf-objstm-over");
    let bytes = objstm_pdf(super::MAX_DECOMPRESSED_BYTES + 1);
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_over"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
    assert!(!dir.path().join("p_over.pdf").exists());
}

#[test]
fn an_object_stream_at_the_limit_loads_and_writes_the_input_bytes() {
    let dir = fakes::TempDir::new("fiber-pdf-objstm-at");
    let bytes = objstm_pdf(super::MAX_DECOMPRESSED_BYTES);
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_at"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(1)));
    assert_eq!(std::fs::read(dir.path().join("p_at.pdf")).unwrap(), bytes);
}

#[test]
fn an_object_stream_under_the_limit_loads_and_cuts() {
    let dir = fakes::TempDir::new("fiber-pdf-objstm-under");
    let input = write_input(dir.path(), "in.pdf", &objstm_pdf(1024));
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_under"),
        OsString::from("pages=1-1"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(1)));
    let cut = std::fs::read(dir.path().join("p_under.pdf")).unwrap();
    assert_eq!(load(&cut).unwrap().get_pages().len(), 1);
}

#[test]
fn an_over_limit_object_stream_beside_two_plain_pages_is_refused_and_writes_nothing() {
    let bytes = objstm_pdf_with_plain_pages(super::MAX_DECOMPRESSED_BYTES + 1);
    // Both pages survive the lenient load: without the refusal the file
    // would run with objects silently missing.
    assert_eq!(load(&bytes).unwrap().get_pages().len(), 2);
    let dir = fakes::TempDir::new("fiber-pdf-objstm-over-beside");
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_over_beside"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
    assert!(!dir.path().join("p_over_beside.pdf").exists());
}

#[test]
fn an_object_stream_at_the_limit_beside_two_plain_pages_loads_two_pages() {
    let bytes = objstm_pdf_with_plain_pages(super::MAX_DECOMPRESSED_BYTES);
    let dir = fakes::TempDir::new("fiber-pdf-objstm-at-beside");
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_at_beside"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(2)));
    assert_eq!(
        std::fs::read(dir.path().join("p_at_beside.pdf")).unwrap(),
        bytes
    );
}

#[test]
fn a_malformed_content_stream_the_page_needs_is_refused() {
    let (bytes, content_id) = plain_pdf();
    let broken = pdf_with_broken_object(&bytes, content_id);
    // The page itself still loads, so only the reference check can refuse
    // the file: the dropped object is an ordinary one, not a packed one.
    let document = load(&broken).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    assert!(!document.objects.contains_key(&content_id));
    let dir = fakes::TempDir::new("fiber-pdf-bad-content");
    let input = write_input(dir.path(), "in.pdf", &broken);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_bad_content"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("failed to load"));
    assert!(!dir.path().join("p_bad_content.pdf").exists());
}

#[test]
fn an_unreferenced_dangling_xref_entry_still_loads() {
    let (bytes, _) = plain_pdf();
    let dangling = pdf_with_dangling_entry(&bytes);
    assert_eq!(load(&dangling).unwrap().get_pages().len(), 1);
    let dir = fakes::TempDir::new("fiber-pdf-dangling");
    let input = write_input(dir.path(), "in.pdf", &dangling);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_dangling"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(1)));
    assert_eq!(
        std::fs::read(dir.path().join("p_dangling.pdf")).unwrap(),
        dangling
    );
}

#[test]
fn a_reference_cycle_terminates_and_loads() {
    let bytes = pdf_with_cycle();
    assert_eq!(load(&bytes).unwrap().get_pages().len(), 1);
    let dir = fakes::TempDir::new("fiber-pdf-cycle");
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_cycle"),
        OsString::from("whole=10"),
    ];
    let (code, stdout, stderr) = child(&arguments);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value.get("page_count"), Some(&serde_json::json!(1)));
}

#[test]
fn bad_requests_bad_stems_and_wrong_counts_exit_2() {
    let dir = fakes::TempDir::new("fiber-pdf-usage");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(2));
    for what in ["whole=", "pages=3", "pages=0-2", "pages=3-2", "x=1"] {
        let arguments = vec![
            OsString::from("pdf"),
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from("p_ok"),
            OsString::from(what),
        ];
        let (code, _, stderr) = child(&arguments);
        assert_eq!(code, 2, "{what}");
        assert!(!stderr.is_empty(), "{what}");
    }
    for stem in ["", "../x", "a.b"] {
        let arguments = vec![
            OsString::from("pdf"),
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from(stem),
            OsString::from("whole=10"),
        ];
        assert_eq!(child(&arguments).0, 2, "{stem:?}");
    }
    for count in [0, 1, 2, 3, 4, 6] {
        let mut arguments: Vec<OsString> = vec![OsString::from("pdf")];
        for n in 0..count {
            arguments.push(OsString::from(n.to_string()));
        }
        assert_eq!(child(&arguments).0, 2, "count {count}");
    }
}

#[test]
fn an_existing_target_is_never_overwritten() {
    let dir = fakes::TempDir::new("fiber-pdf-exists");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(2));
    std::fs::write(dir.path().join("p_kept.pdf"), b"old").unwrap();
    let arguments = vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_kept"),
        OsString::from("whole=10"),
    ];
    assert_eq!(child(&arguments).0, 3);
    assert_eq!(
        std::fs::read(dir.path().join("p_kept.pdf")).unwrap(),
        b"old"
    );
}

#[test]
fn the_fixture_pdf_counts_two_pages() {
    let bytes = std::fs::read(format!(
        "{}/../../research/pdf-tool-results/text.pdf",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let document = load(&bytes).unwrap();
    assert_eq!(document.get_pages().len(), 2);
}

#[test]
fn a_stdout_that_refuses_the_line_exits_3_for_whole_and_pages() {
    let dir = fakes::TempDir::new("fiber-pdf-stdout");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(5));
    for (stem, what, fail_write, fail_flush) in [
        ("p_w_write", "whole=10", true, false),
        ("p_w_flush", "whole=10", false, true),
        ("p_r_write", "pages=2-3", true, false),
        ("p_r_flush", "pages=2-3", false, true),
    ] {
        let arguments = vec![
            OsString::from("pdf"),
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from(stem),
            OsString::from(what),
        ];
        let mut stdout = Stream {
            fail_write,
            fail_flush,
        };
        let mut stderr = Vec::new();
        let code = run(&arguments, &mut stdout, &mut stderr);
        assert_eq!(code, 3, "{what} write={fail_write} flush={fail_flush}");
    }
}

#[test]
fn a_cut_counts_its_pages_from_first_to_last() {
    let dir = fakes::TempDir::new("fiber-pdf-count");
    let input = write_input(dir.path(), "in.pdf", &pdf_bytes(5));
    for (n, (what, expected)) in [("pages=1-3", 3), ("pages=2-5", 4)].into_iter().enumerate() {
        let arguments = vec![
            OsString::from("pdf"),
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from(format!("p_count{n}")),
            OsString::from(what),
        ];
        let (code, stdout, stderr) = child(&arguments);
        assert_eq!((code, stderr.as_str()), (0, ""), "{what}");
        let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(
            value.get("page_count"),
            Some(&serde_json::json!(expected)),
            "{what}"
        );
    }
}

#[test]
fn what_accepts_a_range_from_page_one_and_refuses_a_sign_or_an_empty_half() {
    assert_eq!(
        parse_what("pages=1-1"),
        Some(What::Pages { first: 1, last: 1 })
    );
    assert_eq!(
        parse_what("pages=1-3"),
        Some(What::Pages { first: 1, last: 3 })
    );
    assert_eq!(parse_what("pages=0-3"), None);
    for text in [
        "whole=+3",
        "pages=+1-3",
        "pages=1-+3",
        "pages=-3",
        "pages=3-",
        "whole=",
    ] {
        assert_eq!(parse_what(text), None, "{text}");
    }
}
