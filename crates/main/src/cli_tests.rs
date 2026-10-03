use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use super::{Invocation, MENU, Run, command, parse_from, usage_sentence, version_line};

fn menu() -> String {
    format!("{MENU}\n")
}

fn usage(args: &[&str]) -> (bool, String) {
    let parsed = parse_from(args.iter().copied());
    if let Invocation::Usage { ask, sentence } = parsed {
        (ask, sentence)
    } else {
        panic!("expected a usage error, got {parsed:?}");
    }
}

fn sentence(args: &[&str]) -> String {
    usage(args).1
}

#[test]
fn the_menu_is_hand_grouped_and_names_every_subcommand() {
    let mut cmd = command();
    let long = cmd.render_long_help().to_string();
    let short = cmd.render_help().to_string();
    assert_eq!(long, menu());
    assert_eq!(short, menu());
    for sub in cmd.get_subcommands() {
        assert!(
            long.contains(sub.get_name()),
            "{} is missing from the menu\n{long}",
            sub.get_name()
        );
    }
    assert!(
        cmd.get_subcommands().any(|sub| sub.get_name() == "help"),
        "the help command is missing from the parser"
    );

    for args in [&["fiber", "--help"][..], &["fiber", "-h"]] {
        let parsed = parse_from(args.iter().copied());
        if let Invocation::Help(error) = parsed {
            assert_eq!(error.to_string(), menu(), "{args:?}");
        } else {
            panic!("{args:?} did not print help: {parsed:?}");
        }
    }
    let parsed = parse_from(["fiber", "help"]);
    assert!(
        matches!(parsed, Invocation::Run(Run::Help(None))),
        "{parsed:?}"
    );
    assert_eq!(super::render_help(None).unwrap(), menu());
}

#[test]
fn version_is_the_package_version_and_there_is_no_capital_v() {
    let line = "fiber 0.0.0\n";
    assert_eq!(version_line(), line);
    for args in [&["fiber", "-v"][..], &["fiber", "--version"]] {
        let parsed = parse_from(args.iter().copied());
        if let Invocation::Version(error) = parsed {
            assert_eq!(error.to_string(), line, "{args:?}");
        } else {
            panic!("{args:?}: {parsed:?}");
        }
    }
    let parsed = parse_from(["fiber", "version"]);
    assert!(
        matches!(parsed, Invocation::Run(Run::Version)),
        "{parsed:?}"
    );
    assert_eq!(
        sentence(&["fiber", "-V"]),
        "Unexpected argument '-V' found. Run `fiber --help` for usage."
    );
}

#[test]
fn an_unknown_subcommand_keeps_claps_suggestion() {
    assert_eq!(
        sentence(&["fiber", "instal", "x"]),
        "Unrecognized subcommand 'instal'; did you mean 'install'? Run `fiber --help` for usage."
    );
}

#[test]
fn an_unknown_flag_keeps_claps_suggestion() {
    assert_eq!(
        sentence(&["fiber", "ask", "--modle", "x", "hi"]),
        "Unexpected argument '--modle' found; did you mean '--model'? Run `fiber --help` for usage."
    );
}

#[test]
fn an_unknown_flag_drops_the_double_dash_tip() {
    let error = command()
        .try_get_matches_from(["fiber", "ask", "--zzzzzzzz"])
        .unwrap_err();
    let raw = error.to_string();
    assert!(
        raw.contains("to pass '--zzzzzzzz' as a value, use '-- --zzzzzzzz'"),
        "{raw}"
    );
    let sentence = usage_sentence(&error);
    assert_eq!(
        sentence,
        "Unexpected argument '--zzzzzzzz' found. Run `fiber --help` for usage."
    );
    assert!(!sentence.contains("to pass"), "{sentence}");
    assert!(!sentence.contains("Usage:"), "{sentence}");
    assert!(!sentence.contains('\n'), "{sentence}");
}

#[test]
fn a_missing_required_argument_names_the_value() {
    assert_eq!(
        sentence(&["fiber", "install"]),
        "The following required arguments were not provided: <name or path>. Run `fiber --help` for usage."
    );
}

#[test]
fn other_parse_errors_are_one_sentence() {
    let missing_model = sentence(&["fiber", "ask", "--model"]);
    assert_eq!(
        missing_model,
        "A value is required for '--model <model>' but none was supplied. Run `fiber --help` for usage."
    );
    assert!(!missing_model.contains("Usage:"));
    assert!(!missing_model.contains('\n'));

    let args = vec![
        OsString::from("fiber"),
        OsString::from("ask"),
        OsString::from_vec(vec![0xff, 0xfe]),
    ];
    let parsed = parse_from(args);
    if let Invocation::Usage {
        ask: true,
        sentence,
    } = parsed
    {
        assert_eq!(
            sentence,
            "Invalid UTF-8 was detected in one or more arguments. Run `fiber --help` for usage."
        );
        assert!(!sentence.contains('\n'));
    } else {
        panic!("{parsed:?}");
    }
}

#[test]
fn ask_takes_one_prompt_then_an_optional_dash() {
    let parsed = parse_from(["fiber", "ask", "hi", "-"]);
    if let Invocation::Run(Run::Ask {
        model: None,
        prompt: Some(prompt),
        dash: true,
    }) = parsed
    {
        assert_eq!(prompt, "hi");
    } else {
        panic!("{parsed:?}");
    }

    let parsed = parse_from(["fiber", "ask", "-"]);
    assert!(
        matches!(
            parsed,
            Invocation::Run(Run::Ask {
                prompt: None,
                dash: true,
                ..
            })
        ),
        "{parsed:?}"
    );

    let parsed = parse_from(["fiber", "ask", "hi"]);
    if let Invocation::Run(Run::Ask {
        prompt: Some(prompt),
        dash: false,
        ..
    }) = parsed
    {
        assert_eq!(prompt, "hi");
    } else {
        panic!("{parsed:?}");
    }

    assert!(matches!(parse_from(["fiber"]), Invocation::Run(Run::Bare)));

    let shape = "`fiber ask` takes one prompt, then an optional `-`; quote the prompt. Run `fiber --help` for usage.";
    for args in [
        &["fiber", "ask", "a", "b"][..],
        &["fiber", "ask", "-", "a"],
        &["fiber", "ask", "-", "-"],
    ] {
        let (ask, sentence) = usage(args);
        assert!(ask, "{args:?}");
        assert_eq!(sentence, shape, "{args:?}");
    }
}
