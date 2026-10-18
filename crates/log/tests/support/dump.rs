//! What the `dump` jig shares with its binary test: the session's
//! events, one JSON line each. The jig includes this file with `#[path]`,
//! and `tests/dump_jig.rs` does the same, so the tests below run inside
//! that binary.

use std::path::Path;

/// The session's events, one JSON line each.
pub(crate) fn dump(dir: &Path) -> Result<String, log::Error> {
    let mut out = String::new();
    for line in log::read(dir)? {
        out.push_str(&serde_json::to_string(&line)?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::common::*;
    use log::Log;

    #[test]
    fn it_prints_the_log_a_session_left_one_event_per_line() {
        let tmp = TestDir::new("dump");
        let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
        log.append(&session_started(), None, None).unwrap();
        log.append(&delta("x"), None, None).unwrap();
        log.append(&empty("step_started"), None, None).unwrap();
        let dir = tmp.session(&id("s_1"));
        let out = dump(&dir).unwrap();
        assert_eq!(out, fs::read_to_string(dir.join("events.jsonl")).unwrap());
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn a_missing_directory_fails() {
        let tmp = TestDir::new("dump-missing");
        assert!(dump(&tmp.session(&id("s_none"))).is_err());
    }
}
