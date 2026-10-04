use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use clap::error::ContextValue;

use super::{
    Commands, ExtensionCommands, Invocation, MENU, command, parse_from, usage_sentence,
    version_line,
};

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
        if let Invocation::Print(error) = parsed {
            assert_eq!(error.to_string(), menu(), "{args:?}");
        } else {
            panic!("{args:?} did not print help: {parsed:?}");
        }
    }
    let parsed = parse_from(["fiber", "help"]);
    assert!(
        matches!(
            parsed,
            Invocation::Run(Some(Commands::Help { command: None }))
        ),
        "{parsed:?}"
    );
    assert_eq!(super::render_help(None).unwrap(), menu());
}

#[test]
fn version_text_names_the_commit_when_one_was_recorded() {
    assert_eq!(
        super::version_text("0.0.0", Some("4f2a9c1")),
        "0.0.0 (4f2a9c1)"
    );
}

#[test]
fn version_text_is_the_version_alone_when_no_commit_was_recorded() {
    assert_eq!(super::version_text("0.0.0", None), "0.0.0");
    assert_eq!(super::version_text("0.0.0", Some("")), "0.0.0");
}

#[test]
fn version_is_the_package_version_and_there_is_no_capital_v() {
    let line = format!(
        "fiber {}\n",
        super::version_text(env!("CARGO_PKG_VERSION"), option_env!("FIBER_COMMIT"))
    );
    assert_eq!(version_line(), line);
    for args in [&["fiber", "-v"][..], &["fiber", "--version"]] {
        let parsed = parse_from(args.iter().copied());
        if let Invocation::Print(error) = parsed {
            assert_eq!(error.to_string(), line, "{args:?}");
        } else {
            panic!("{args:?}: {parsed:?}");
        }
    }
    let parsed = parse_from(["fiber", "version"]);
    assert!(
        matches!(parsed, Invocation::Run(Some(Commands::Version))),
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
        sentence(&["fiber", "extension", "instal", "x"]),
        "Unrecognized subcommand 'instal'; did you mean 'install'? Run `fiber --help` for usage."
    );
    assert_eq!(
        sentence(&["fiber", "extension", "i"]),
        "Unrecognized subcommand 'i'; did you mean 'install'? Run `fiber --help` for usage."
    );
}

#[test]
fn extension_update_parses_an_optional_name() {
    let Invocation::Run(Some(Commands::Extension(ExtensionCommands::Update { name }))) =
        parse_from(["fiber", "extension", "update"])
    else {
        panic!("update with no name");
    };
    assert_eq!(name, None);
    let Invocation::Run(Some(Commands::Extension(ExtensionCommands::Update { name }))) =
        parse_from(["fiber", "extension", "update", "x"])
    else {
        panic!("update with name");
    };
    assert_eq!(name.as_deref(), Some("x"));
}

#[test]
fn extension_alone_is_a_one_line_usage_sentence() {
    assert_eq!(
        sentence(&["fiber", "extension"]),
        "'fiber extension' requires a subcommand but one was not provided [subcommands: install, update, remove, list]. Run `fiber --help` for usage."
    );
}

#[test]
fn extension_help_matches_help_extension() {
    let rendered = super::render_help(Some("extension")).unwrap();
    let Invocation::Print(error) = parse_from(["fiber", "extension", "--help"]) else {
        panic!("extension --help did not print");
    };
    assert_eq!(rendered, error.to_string());
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("Usage: fiber extension")),
        "{rendered}"
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
        sentence(&["fiber", "extension", "install"]),
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
    let Invocation::Run(Some(Commands::Ask(args))) = parsed else {
        panic!("{parsed:?}");
    };
    assert_eq!(args.model, None);
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), true)
    );

    let Invocation::Run(Some(Commands::Ask(args))) = parse_from(["fiber", "ask", "-"]) else {
        panic!("dash");
    };
    assert_eq!(super::ask_parts(&args.prompt).unwrap(), (None, true));

    let Invocation::Run(Some(Commands::Ask(args))) = parse_from(["fiber", "ask", "hi"]) else {
        panic!("prompt");
    };
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), false)
    );

    assert!(matches!(parse_from(["fiber"]), Invocation::Run(None)));

    let shape = "`fiber ask` takes one prompt, then an optional `-`; quote the prompt. Run `fiber --help` for usage.";
    for args in [
        &["fiber", "ask", "a", "b"][..],
        &["fiber", "ask", "-", "a"],
        &["fiber", "ask", "-", "-"],
    ] {
        let Invocation::Run(Some(Commands::Ask(ask))) = parse_from(args.iter().copied()) else {
            panic!("{args:?}");
        };
        assert_eq!(
            super::ask_parts(&ask.prompt).unwrap_err(),
            shape,
            "{args:?}"
        );
    }
}

#[test]
fn a_commands_help_matches_its_flag_and_names_fiber() {
    let cmd = command();
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    assert!(!names.is_empty());
    for name in &names {
        let rendered = super::render_help(Some(name)).unwrap();
        let Invocation::Print(error) = parse_from(["fiber", name, "--help"]) else {
            panic!("{name} did not print help");
        };
        assert_eq!(rendered, error.to_string(), "{name}");
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with(&format!("Usage: fiber {name}"))),
            "{name}\n{rendered}"
        );
    }
    assert_eq!(
        super::render_help(Some("nope")).unwrap_err(),
        sentence(&["fiber", "nope"])
    );
}

#[test]
fn suggestions_are_the_text_clap_stored() {
    assert_eq!(
        super::all_suggestions(&ContextValue::String("install".to_owned())),
        ["install".to_owned()]
    );
    assert_eq!(
        super::all_suggestions(&ContextValue::Strings(vec![
            "list".to_owned(),
            "install".to_owned()
        ])),
        ["list".to_owned(), "install".to_owned()]
    );
    for value in [
        ContextValue::None,
        ContextValue::Bool(true),
        ContextValue::Number(1),
        ContextValue::StyledStr("install".into()),
        ContextValue::StyledStrs(vec!["install".into()]),
    ] {
        assert!(
            super::all_suggestions(&value).is_empty(),
            "{value:?} is not a suggestion"
        );
    }
}

#[test]
fn ask_resume_takes_a_session_id() {
    let Invocation::Run(Some(Commands::Ask(args))) =
        parse_from(["fiber", "ask", "--resume", "s_abc", "hi"])
    else {
        panic!("resume with a prompt");
    };
    assert_eq!(args.resume.as_deref(), Some("s_abc"));
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), false)
    );

    let Invocation::Run(Some(Commands::Ask(args))) =
        parse_from(["fiber", "ask", "--resume", "s_abc"])
    else {
        panic!("resume without a prompt");
    };
    assert_eq!(args.resume.as_deref(), Some("s_abc"));

    let Invocation::Run(Some(Commands::Ask(args))) = parse_from(["fiber", "ask", "hi"]) else {
        panic!("no resume");
    };
    assert_eq!(args.resume, None);
}

#[test]
fn ask_resume_without_an_id_is_a_usage_error() {
    assert_eq!(
        sentence(&["fiber", "ask", "--resume"]),
        "A value is required for '--resume <id>' but none was supplied. Run `fiber --help` for usage."
    );
    let (ask, empty) = usage(&["fiber", "ask", "--resume", ""]);
    assert!(ask);
    assert_eq!(
        empty,
        "The argument '--resume <id>' requires a session id but none was given. Run `fiber --help` for usage."
    );
}

#[test]
fn the_menu_and_ask_help_show_resume() {
    assert!(
        menu().contains("[--resume <id>]"),
        "the menu shows --resume:\n{}",
        menu()
    );
    let rendered = super::render_help(Some("ask")).unwrap();
    assert!(
        rendered.contains("--resume <id>"),
        "ask's help shows --resume:\n{rendered}"
    );
}
