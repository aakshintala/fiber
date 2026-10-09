//! Tests beside [`super::run`]: the PDF mode's counting, cutting and exit codes.

use std::ffi::OsString;

use super::run;

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

fn args(values: &[&std::path::Path], extra: &[&str]) -> Vec<OsString> {
    let mut out: Vec<OsString> = values
        .iter()
        .map(|value| value.as_os_str().to_owned())
        .collect();
    out.extend(extra.iter().map(OsString::from));
    out
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

#[test]
fn whole_at_ten_pages_writes_the_input_bytes_unchanged() {
    let dir = fakes::TempDir::new("fiber-pdf-whole");
    let bytes = pdf_bytes(10);
    let input = write_input(dir.path(), "in.pdf", &bytes);
    let arguments = args(
        &[&input, dir.path(), "p_whole".as_ref()],
        &["pdf", "placeholder"],
    );
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
    drop(arguments);
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
    let reloaded = lopdf::Document::load_mem(&cut).unwrap();
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
    assert_eq!(
        lopdf::Document::load_mem(&cut).unwrap().get_pages().len(),
        1
    );
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
    let document = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(document.get_pages().len(), 2);
}
