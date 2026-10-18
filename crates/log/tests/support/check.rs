//! What the `check` jig shares with its binary test: a one-line summary
//! of a sound session directory, or its first problem. The jig includes
//! this file with `#[path]`, and `tests/check_jig.rs` does the same, so
//! the tests below run inside that binary.

use std::path::Path;

use contract::events::Event;

/// A one-line summary of a sound session directory, or its first problem.
pub(crate) fn check(dir: &Path) -> Result<String, String> {
    let lines = log::read(dir).map_err(|e| e.to_string())?;
    for (due, line) in (0u64..).zip(&lines) {
        let n = due + 1;
        Event::from_envelope(line)
            .map_err(|e| format!("line {n}: a {} payload that does not parse: {e}", line.kind))?;
        match line.seq {
            Some(seq) if seq.0 == due => {}
            Some(seq) => return Err(format!("line {n}: seq {} where {due} was due", seq.0)),
            None => {
                return Err(format!(
                    "line {n}: no seq, but the log holds only durable lines"
                ));
            }
        }
    }
    Ok(format!("ok: {} lines, seq contiguous from 0", lines.len()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use super::*;
    use crate::common::*;
    use log::Log;

    /// A session directory the log wrote, of three durable lines.
    fn session(tmp: &TestDir) -> std::path::PathBuf {
        let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
        log.append(&session_started(), None, None).unwrap();
        log.append(&delta("x"), None, None).unwrap();
        log.append(&empty("step_started"), None, None).unwrap();
        log.append(&empty("assistant_message_started"), None, None)
            .unwrap();
        tmp.session(&id("s_1"))
    }

    fn append(dir: &Path, bytes: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events.jsonl"))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn a_session_the_log_wrote_passes() {
        let tmp = TestDir::new("check-ok");
        assert_eq!(
            check(&session(&tmp)),
            Ok("ok: 3 lines, seq contiguous from 0".to_owned())
        );
    }

    #[test]
    fn a_gap_in_seq_fails_naming_the_line() {
        let tmp = TestDir::new("check-gap");
        let dir = session(&tmp);
        let path = dir.join("events.jsonl");
        let text = fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| l)
            .collect();
        fs::write(&path, kept.join("\n") + "\n").unwrap();
        assert_eq!(check(&dir), Err("line 2: seq 2 where 1 was due".to_owned()));
    }

    #[test]
    fn a_line_that_does_not_parse_fails_naming_the_line() {
        let tmp = TestDir::new("check-json");
        let dir = session(&tmp);
        append(&dir, b"not json\n");
        let problem = check(&dir).unwrap_err();
        assert!(problem.contains("line 4"), "{problem}");
    }

    #[test]
    fn a_payload_that_does_not_parse_fails_naming_the_line() {
        let tmp = TestDir::new("check-payload");
        let dir = session(&tmp);
        append(
            &dir,
            br#"{"kind":"turn_completed","session_id":"s_1","ts":1,"schema_version":1,"seq":3,"payload":{"outcome":5}}
"#,
        );
        let problem = check(&dir).unwrap_err();
        assert!(
            problem.starts_with("line 4: a turn_completed payload"),
            "{problem}"
        );
    }

    #[test]
    fn a_line_without_seq_fails() {
        let tmp = TestDir::new("check-noseq");
        let dir = session(&tmp);
        append(
            &dir,
            br#"{"kind":"step_started","session_id":"s_1","ts":1,"schema_version":1,"payload":{}}
"#,
        );
        assert_eq!(
            check(&dir),
            Err("line 4: no seq, but the log holds only durable lines".to_owned())
        );
    }

    #[test]
    fn a_missing_directory_fails() {
        let tmp = TestDir::new("check-missing");
        assert!(check(&tmp.session(&id("s_none"))).is_err());
    }
}
