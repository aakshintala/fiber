//! What the `provider-server` jig shares with its binary test: one
//! argument as a response, and a request as the jig prints it. The jig
//! includes this file with `#[path]`, and `tests/provider_server_jig.rs`
//! does the same, so the tests below run inside that binary.

use fakes::{Request, Response};

/// One argument as a response: `FILE` is a 200 stream, `STATUS:FILE` a JSON
/// body with that status.
pub(crate) fn response(arg: &str) -> Result<Response, String> {
    let (status, file) = match arg.split_once(':').map(|(s, f)| (s.parse::<u16>(), f)) {
        Some((Ok(s), f)) => (Some(s), f),
        Some((Err(_), _)) | None => (None, arg),
    };
    let body = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    Ok(match status {
        Some(s) => Response::status(s, body),
        None => Response::stream(body),
    })
}

/// A request as the jig prints it: the request line, the headers, and the
/// body as text, then a blank line.
pub(crate) fn show(request: &Request) -> String {
    let mut out = format!("{} {}\n", request.method, request.path);
    for (name, value) in &request.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push_str(&String::from_utf8_lossy(&request.body));
    out.push_str("\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(dir: &fakes::TempDir, name: &str, bytes: &[u8]) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_bare_file_is_a_stream_and_a_status_prefix_sets_the_status() {
        let dir = fakes::TempDir::new("fakes-jig-body");
        let file = temp_file(&dir, "body", b"data: x\n\n");

        assert_eq!(response(&file).unwrap(), Response::stream("data: x\n\n"));
        assert_eq!(
            response(&format!("429:{file}")).unwrap(),
            Response::status(429, "data: x\n\n")
        );
        assert!(
            response("/no/such/file")
                .unwrap_err()
                .contains("/no/such/file")
        );
    }

    #[test]
    fn it_prints_the_request_line_headers_and_body() {
        let request = Request {
            method: "POST".to_owned(),
            path: "/v1/messages".to_owned(),
            headers: vec![("x-api-key".to_owned(), "sha256:746b4ad1".to_owned())],
            body: b"{}".to_vec(),
            body_len: 2,
        };

        assert_eq!(
            show(&request),
            "POST /v1/messages\nx-api-key: sha256:746b4ad1\n{}\n\n"
        );
    }
}
