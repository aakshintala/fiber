//! The deliberately narrow grammar for a shell builtin `kill` whose targets
//! may take the session-owned-process fast path.

use rustix::process::Signal;

use super::syntax::Stage;

/// One parsed target. `group` selects `kill(-pgid, signal)` instead of
/// `kill(pid, signal)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Target {
    /// The process or process group id.
    pub pgid: u32,
    /// Whether the target is a process group.
    pub group: bool,
}

/// A signal that may be sent by the builtin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SignalChoice {
    /// Signal zero checks existence without changing process state.
    None,
    /// A named OS signal.
    Some(Signal),
}

/// A parsed, bare builtin invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Parsed {
    /// The signal to send.
    pub signal: SignalChoice,
    /// Every parsed target, in command order.
    pub targets: Vec<Target>,
}

/// Parses only the bare `kill` forms the shell fast path understands.
pub(super) fn parse(command: &str) -> Option<Parsed> {
    let script = super::syntax::parse(command)?;
    let [element] = script.elements.as_slice() else {
        return None;
    };
    let [Stage::Simple(simple)] = element.stages.as_slice() else {
        return None;
    };
    if !simple.assignments.is_empty() || !simple.redirects.is_empty() {
        return None;
    }
    let words = &simple.words;
    if words
        .iter()
        .any(|word| word.substituted || word.raw != word.cooked)
        || words.first().is_none_or(|word| word.cooked != "kill")
    {
        return None;
    }

    let mut index = 1;
    let mut signal = SignalChoice::Some(Signal::TERM);
    if let Some(word) = words.get(index)
        && word.cooked.starts_with('-')
        && word.cooked != "--"
    {
        signal = signal_word(word.cooked.strip_prefix('-')?)?;
        index += 1;
    }
    let delimited = words.get(index).is_some_and(|word| word.cooked == "--");
    if delimited {
        index += 1;
    }

    let mut targets = Vec::new();
    for word in words.get(index..)? {
        let raw = word.cooked.as_str();
        let (raw, group) = if delimited {
            match raw.strip_prefix('-') {
                Some(pgid) => (pgid, true),
                None => (raw, false),
            }
        } else {
            if raw.starts_with('-') {
                return None;
            }
            (raw, false)
        };
        let pgid = raw.parse::<u32>().ok()?;
        if support::group::refused(pgid) {
            return None;
        }
        targets.push(Target { pgid, group });
    }
    if targets.is_empty() {
        return None;
    }
    Some(Parsed { signal, targets })
}

/// Parses the targets without exposing the signal-choice details to the
/// classifier.
pub(super) fn targets(command: &str) -> Option<Vec<Target>> {
    Some(parse(command)?.targets)
}

fn signal_word(raw: &str) -> Option<SignalChoice> {
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = raw.parse::<i32>().ok()?;
        return if number == 0 {
            Some(SignalChoice::None)
        } else {
            Signal::from_named_raw(number).map(SignalChoice::Some)
        };
    }
    let name = raw.strip_prefix("SIG").unwrap_or(raw).to_ascii_uppercase();
    let signal = match name.as_str() {
        "HUP" => Signal::HUP,
        "INT" => Signal::INT,
        "QUIT" => Signal::QUIT,
        "ILL" => Signal::ILL,
        "TRAP" => Signal::TRAP,
        "ABRT" | "IOT" => Signal::ABORT,
        "BUS" => Signal::BUS,
        "FPE" => Signal::FPE,
        "KILL" => Signal::KILL,
        "USR1" => Signal::USR1,
        "SEGV" => Signal::SEGV,
        "USR2" => Signal::USR2,
        "PIPE" => Signal::PIPE,
        "ALRM" => Signal::ALARM,
        "TERM" => Signal::TERM,
        "CHLD" | "CLD" => Signal::CHILD,
        "CONT" => Signal::CONT,
        "STOP" => Signal::STOP,
        "TSTP" => Signal::TSTP,
        "TTIN" => Signal::TTIN,
        "TTOU" => Signal::TTOU,
        "URG" => Signal::URG,
        "XCPU" => Signal::XCPU,
        "XFSZ" => Signal::XFSZ,
        "VTALRM" => Signal::VTALARM,
        "PROF" => Signal::PROF,
        "WINCH" => Signal::WINCH,
        "IO" | "POLL" => Signal::IO,
        "SYS" => Signal::SYS,
        _ => return None,
    };
    Some(SignalChoice::Some(signal))
}

/// Builds a process-group command in pieces because `signal-sites` rejects
/// any one literal containing both the builtin name and group-target syntax.
#[cfg(test)]
pub(super) fn group_kill_command(options: &str, targets: &str) -> String {
    ["kill ", options, "-- -", targets].concat()
}

#[cfg(test)]
mod tests {
    use rustix::process::Signal;

    use super::{Parsed, SignalChoice, Target, parse, targets};

    #[test]
    fn kill_targets_accepts_only_positive_pids_and_delimited_groups() {
        for (command, expected) in [
            (
                "kill 2".to_owned(),
                vec![Target {
                    pgid: 2,
                    group: false,
                }],
            ),
            (
                "kill -9 2".to_owned(),
                vec![Target {
                    pgid: 2,
                    group: false,
                }],
            ),
            (
                "kill -TERM 2".to_owned(),
                vec![Target {
                    pgid: 2,
                    group: false,
                }],
            ),
            (
                "kill -- 2".to_owned(),
                vec![Target {
                    pgid: 2,
                    group: false,
                }],
            ),
            (
                super::group_kill_command("", "2"),
                vec![Target {
                    pgid: 2,
                    group: true,
                }],
            ),
            (
                super::group_kill_command("-KILL ", "2 3"),
                vec![
                    Target {
                        pgid: 2,
                        group: true,
                    },
                    Target {
                        pgid: 3,
                        group: false,
                    },
                ],
            ),
        ] {
            assert_eq!(targets(&command), Some(expected), "{command}");
        }
    }

    #[test]
    fn kill_targets_refuses_invalid_ids_special_options_and_indirect_commands() {
        for command in [
            "kill".to_owned(),
            "kill 0".to_owned(),
            "kill 1".to_owned(),
            super::group_kill_command("", "0"),
            super::group_kill_command("", "1"),
            super::group_kill_command("", ""),
            "kill -s TERM 2".to_owned(),
            "kill -l".to_owned(),
            "kill %1".to_owned(),
            "kill -2".to_owned(),
            "kill 4294967296".to_owned(),
            "killall 2".to_owned(),
            "pkill -g 2".to_owned(),
            "env kill 2".to_owned(),
            "timeout 1 kill 2".to_owned(),
            "xargs kill 2".to_owned(),
            "sudo kill 2".to_owned(),
            "command kill 2".to_owned(),
            "/bin/kill 2".to_owned(),
            "'kill' 2".to_owned(),
            "kill 2 | cat".to_owned(),
            "kill $(echo 2)".to_owned(),
            "kill 2 >out".to_owned(),
            "FOO=bar kill 2".to_owned(),
        ] {
            assert_eq!(targets(&command), None, "{command}");
        }
    }

    #[test]
    fn kill_signal_words_are_parsed() {
        for (word, expected) in [
            ("HUP", Signal::HUP),
            ("INT", Signal::INT),
            ("QUIT", Signal::QUIT),
            ("ILL", Signal::ILL),
            ("TRAP", Signal::TRAP),
            ("ABRT", Signal::ABORT),
            ("IOT", Signal::ABORT),
            ("BUS", Signal::BUS),
            ("FPE", Signal::FPE),
            ("KILL", Signal::KILL),
            ("USR1", Signal::USR1),
            ("SEGV", Signal::SEGV),
            ("USR2", Signal::USR2),
            ("PIPE", Signal::PIPE),
            ("ALRM", Signal::ALARM),
            ("TERM", Signal::TERM),
            ("CHLD", Signal::CHILD),
            ("CLD", Signal::CHILD),
            ("CONT", Signal::CONT),
            ("STOP", Signal::STOP),
            ("TSTP", Signal::TSTP),
            ("TTIN", Signal::TTIN),
            ("TTOU", Signal::TTOU),
            ("URG", Signal::URG),
            ("XCPU", Signal::XCPU),
            ("XFSZ", Signal::XFSZ),
            ("VTALRM", Signal::VTALARM),
            ("PROF", Signal::PROF),
            ("WINCH", Signal::WINCH),
            ("IO", Signal::IO),
            ("POLL", Signal::IO),
            ("SYS", Signal::SYS),
        ] {
            for spelling in [
                word.to_owned(),
                format!("SIG{word}"),
                word.to_ascii_lowercase(),
            ] {
                assert_eq!(
                    parse(&format!("kill -{spelling} 2")).unwrap().signal,
                    SignalChoice::Some(expected),
                    "{spelling}"
                );
            }
        }
        for (command, signal) in [
            ("kill 2", SignalChoice::Some(Signal::TERM)),
            ("kill -9 2", SignalChoice::Some(Signal::KILL)),
            ("kill -KILL 2", SignalChoice::Some(Signal::KILL)),
            ("kill -SIGTERM 2", SignalChoice::Some(Signal::TERM)),
            ("kill -0 2", SignalChoice::None),
        ] {
            assert_eq!(
                parse(command),
                Some(Parsed {
                    signal,
                    targets: vec![Target {
                        pgid: 2,
                        group: false
                    }],
                }),
                "{command}"
            );
        }
    }
}
