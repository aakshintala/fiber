use super::{extra_body, extras};

#[test]
fn extras_reads_headers_and_one_body_file() {
    let arg = |s: &str| s.to_owned();
    let parsed = extras(&[arg("x-a: 1")]).unwrap();
    assert_eq!(parsed.0, vec![("x-a".to_owned(), "1".to_owned())]);
    assert_eq!(parsed.1, None);
    let parsed = extras(&[arg("@f.json"), arg("x-a:1")]).unwrap();
    assert_eq!(parsed.0, vec![("x-a".to_owned(), "1".to_owned())]);
    assert_eq!(parsed.1, Some("f.json".to_owned()));
    assert_eq!(
        extras(&[arg("@a.json"), arg("@b.json")]).unwrap_err(),
        "record: one @FILE only"
    );
    assert_eq!(
        extras(&[arg("@")]).unwrap_err(),
        "record: @FILE names a file holding a JSON object"
    );
    assert_eq!(
        extras(&[arg("no-colon")]).unwrap_err(),
        "record: a header is NAME:VALUE"
    );
}

#[test]
fn extra_body_must_be_a_json_object() {
    let dir = fakes::TempDir::new("fiber-record-extra-body");
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path.display().to_string()
    };
    let object = write("object.json", br#"{"store": false}"#);
    assert_eq!(
        extra_body(&object)
            .unwrap()
            .get("store")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    for name in ["array.json", "string.json", "null.json"] {
        let contents: &[u8] = match name {
            "array.json" => b"[1]",
            "string.json" => br#""x""#,
            _ => b"null",
        };
        let path = write(name, contents);
        assert_eq!(
            extra_body(&path).unwrap_err(),
            format!("record: {path} (@FILE) must hold a JSON object")
        );
    }
    let bad = write("bad.json", b"{oops");
    let error = extra_body(&bad).unwrap_err();
    assert!(error.starts_with(&format!("{bad}: ")), "{error}");
}
