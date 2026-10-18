//! What the `record` jig shares with its binary test: the trailing
//! arguments as headers and at most one `@FILE` body object. The jig
//! includes this file with `#[path]`, and `tests/record_jig.rs` does the
//! same, so the tests below run inside that binary.

/// The trailing arguments: headers to send, and at most one `@FILE` path
/// holding the `extra_body` object.
pub(crate) type Parsed = (Vec<(String, String)>, Option<String>);

/// The trailing arguments: headers, and at most one `@FILE` holding the
/// `extra_body` object. Anything else is rejected before any request is
/// sent.
pub(crate) fn extras(args: &[String]) -> Result<Parsed, String> {
    let mut headers = Vec::new();
    let mut body_file = None;
    for arg in args {
        if let Some(path) = arg.strip_prefix('@') {
            if path.is_empty() {
                return Err("record: @FILE names a file holding a JSON object".into());
            }
            if body_file.is_some() {
                return Err("record: one @FILE only".into());
            }
            body_file = Some(path.to_owned());
            continue;
        }
        let Some((name, value)) = arg.split_once(':') else {
            return Err("record: a header is NAME:VALUE".into());
        };
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    Ok((headers, body_file))
}

/// The `@FILE` JSON object, sent as the endpoint's `extra_body`.
pub(crate) fn extra_body(path: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("record: {path} (@FILE) must hold a JSON object"))
}

#[cfg(test)]
mod tests {
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
}
