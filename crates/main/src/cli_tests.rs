use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use clap::error::ContextValue;

use super::{
    Commands, ConfigCommands, ExtensionCommands, HubCommands, Invocation, MENU, SessionsCommands,
    command, parse_from, usage_sentence, version_line,
};

fn menu() -> String {
    format!("{MENU}\n")
}

/// The subcommands a person sees: the hidden search subcommands stay out
/// of the menu and the help tour.
fn visible() -> Vec<String> {
    command()
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .collect()
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
fn the_menu_is_hand_grouped_and_names_every_visible_subcommand() {
    let mut cmd = command();
    let long = cmd.render_long_help().to_string();
    let short = cmd.render_help().to_string();
    assert_eq!(long, menu());
    assert_eq!(short, menu());
    assert!(!long.contains("grep"), "{long}");
    assert!(!long.contains("find"), "{long}");
    for name in visible() {
        assert!(
            long.contains(&name),
            "{name} is missing from the menu\n{long}"
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
            Invocation::Run(Some(Commands::Help { ref command })) if command.as_slice().is_empty()
        ),
        "{parsed:?}"
    );
    assert_eq!(super::render_help::<&str>(&[]).unwrap(), menu());
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
fn approve_parses_an_optional_yes_and_nothing_else() {
    let Invocation::Run(Some(Commands::Approve(args))) = parse_from(["fiber", "approve"]) else {
        panic!("approve");
    };
    assert!(!args.yes);
    let Invocation::Run(Some(Commands::Approve(args))) = parse_from(["fiber", "approve", "--yes"])
    else {
        panic!("approve --yes");
    };
    assert!(args.yes);
    assert!(
        sentence(&["fiber", "approve", "extra"]).starts_with("Unexpected argument 'extra'"),
        "{}",
        sentence(&["fiber", "approve", "extra"])
    );
}

#[test]
fn the_menu_and_approve_help_say_what_approve_does() {
    let help = super::render_help(&["approve"]).unwrap();
    assert!(help.contains("--yes"), "{help}");
    assert!(help.contains("without asking"), "{help}");
    let menu = menu();
    let itself = menu
        .split("\n\n")
        .find(|group| group.starts_with("Fiber itself:"))
        .unwrap();
    let line = "  approve [--yes]                           Show what this repository ships and approve it";
    assert!(itself.lines().any(|l| l == line), "{line}\n{itself}");
}

#[test]
fn login_and_logout_take_an_optional_provider() {
    for (args, provider) in [
        (&["fiber", "login"][..], None),
        (&["fiber", "login", "openrouter"], Some("openrouter")),
    ] {
        let Invocation::Run(Some(Commands::Login(login))) = parse_from(args.iter().copied()) else {
            panic!("{args:?}");
        };
        assert_eq!(login.provider.as_deref(), provider, "{args:?}");
    }
    for (args, provider) in [
        (&["fiber", "logout"][..], None),
        (&["fiber", "logout", "opencode-zen"], Some("opencode-zen")),
    ] {
        let Invocation::Run(Some(Commands::Logout(logout))) = parse_from(args.iter().copied())
        else {
            panic!("{args:?}");
        };
        assert_eq!(logout.provider.as_deref(), provider, "{args:?}");
    }
    for args in [
        &["fiber", "login", "a", "b"][..],
        &["fiber", "logout", "a", "b"],
    ] {
        assert!(
            sentence(args).starts_with("Unexpected argument 'b'"),
            "{}",
            sentence(args)
        );
    }
}

#[test]
fn login_and_logout_take_as_and_logout_takes_all() {
    let Invocation::Run(Some(Commands::Login(login))) =
        parse_from(["fiber", "login", "acme", "--as", "work"])
    else {
        panic!("login --as");
    };
    assert_eq!(login.label.as_deref(), Some("work"));
    let Invocation::Run(Some(Commands::Login(login))) = parse_from(["fiber", "login", "acme"])
    else {
        panic!("login");
    };
    assert_eq!(login.label, None);
    for (args, label, all) in [
        (
            &["fiber", "logout", "acme", "--as", "work"][..],
            Some("work"),
            false,
        ),
        (&["fiber", "logout", "acme", "--all"], None, true),
        (&["fiber", "logout", "acme"], None, false),
    ] {
        let Invocation::Run(Some(Commands::Logout(logout))) = parse_from(args.iter().copied())
        else {
            panic!("{args:?}");
        };
        assert_eq!(logout.label.as_deref(), label, "{args:?}");
        assert_eq!(logout.all, all, "{args:?}");
    }
    let said = sentence(&["fiber", "logout", "acme", "--as", "w", "--all"]);
    assert!(said.contains("cannot be used with"), "{said}");
    assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");
}

#[test]
fn sessions_export_takes_an_id_and_an_optional_path() {
    let Invocation::Run(Some(Commands::Sessions(SessionsCommands::Export { id, path }))) =
        parse_from(["fiber", "sessions", "export", "s_abc"])
    else {
        panic!("export with an id");
    };
    assert_eq!(id, "s_abc");
    assert_eq!(path, None);
    let Invocation::Run(Some(Commands::Sessions(SessionsCommands::Export { id, path }))) =
        parse_from(["fiber", "sessions", "export", "s_abc", "out"])
    else {
        panic!("export with an id and a path");
    };
    assert_eq!(id, "s_abc");
    assert_eq!(path, Some(PathBuf::from("out")));
    assert!(
        sentence(&["fiber", "sessions", "export"])
            .starts_with("The following required arguments were not provided: <id>"),
        "{}",
        sentence(&["fiber", "sessions", "export"])
    );
}

#[test]
fn sessions_alone_is_a_one_line_usage_sentence() {
    assert_eq!(
        sentence(&["fiber", "sessions"]),
        "'fiber sessions' requires a subcommand but one was not provided [subcommands: export]. Run `fiber --help` for usage."
    );
}

#[test]
fn the_menu_lists_sessions_export_under_sessions() {
    let menu = menu();
    let sessions = menu
        .split("\n\n")
        .find(|group| group.starts_with("Sessions:"))
        .unwrap();
    assert!(
        sessions.lines().any(|l|
            l == "  sessions export <id> [<path>]                         Write the session's log and its artifacts to <path>"),
        "{sessions}"
    );
}

#[test]
fn models_takes_an_optional_search_and_json() {
    let Invocation::Run(Some(Commands::Models(models))) = parse_from(["fiber", "models"]) else {
        panic!("bare models");
    };
    assert_eq!(models.search, None);
    assert!(!models.json);
    let Invocation::Run(Some(Commands::Models(models))) =
        parse_from(["fiber", "models", "acme", "--json"])
    else {
        panic!("models with a search and --json");
    };
    assert_eq!(models.search.as_deref(), Some("acme"));
    assert!(models.json);
    assert!(
        sentence(&["fiber", "models", "a", "b"]).starts_with("Unexpected argument 'b'"),
        "{}",
        sentence(&["fiber", "models", "a", "b"])
    );
}

#[test]
fn the_menu_lists_models_under_sessions() {
    let menu = menu();
    let sessions = menu
        .split("\n\n")
        .find(|group| group.starts_with("Sessions:"))
        .unwrap();
    assert!(
        sessions.lines().any(|l|
            l == "  models [<search>] [--json]                            List the models the installed providers serve"),
        "{sessions}"
    );
    let help = super::render_help(&["models"]).unwrap();
    assert!(
        help.contains("List the models the installed providers serve"),
        "{help}"
    );
    assert!(
        help.lines()
            .any(|line| line.starts_with("Usage: fiber models")),
        "{help}"
    );
}

#[test]
fn the_menu_lists_login_and_logout_under_fiber_itself() {
    let menu = menu();
    let itself = menu
        .split("\n\n")
        .find(|group| group.starts_with("Fiber itself:"))
        .unwrap();
    for line in [
        "  login [<provider>] [--as <label>]         Store a provider's key",
        "  logout <provider> [--as <label> | --all]  Delete a provider's stored key",
    ] {
        assert!(itself.lines().any(|l| l == line), "{line}\n{itself}");
    }
    assert!(::cli::LOGOUT_SHAPE.ends_with(" Run `fiber --help` for usage."));
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
    let rendered = super::render_help(&["extension"]).unwrap();
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
fn help_noun_verb_matches_the_verbs_help_flag() {
    let cmd = command();
    let pairs: Vec<(String, String)> = cmd
        .get_subcommands()
        .filter(|noun| !noun.is_hide_set())
        .flat_map(|noun| {
            noun.get_subcommands()
                .map(|verb| (noun.get_name().to_owned(), verb.get_name().to_owned()))
        })
        .collect();
    assert!(pairs.len() >= 3, "{pairs:?}");
    for (noun, verb) in &pairs {
        let parsed = parse_from(["fiber", "help", noun, verb]);
        let Invocation::Run(Some(Commands::Help { command })) = parsed else {
            panic!("help {noun} {verb} did not parse: {parsed:?}");
        };
        let rendered = super::render_help(command.as_slice()).unwrap();
        let Invocation::Print(error) = parse_from(["fiber", noun, verb, "--help"]) else {
            panic!("{noun} {verb} --help did not print");
        };
        assert_eq!(rendered, error.to_string(), "{noun} {verb}");
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with(&format!("Usage: fiber {noun} {verb}"))),
            "{noun} {verb}\n{rendered}"
        );
    }
    assert_eq!(
        super::render_help(&["extension", "instal"]).unwrap_err(),
        sentence(&["fiber", "extension", "instal"])
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
    let names = visible();
    assert!(!names.is_empty());
    for name in &names {
        let rendered = super::render_help(&[name]).unwrap();
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
        super::render_help(&["nope"]).unwrap_err(),
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
    let rendered = super::render_help(&["ask"]).unwrap();
    assert!(
        rendered.contains("--resume <id>"),
        "ask's help shows --resume:\n{rendered}"
    );
}

#[test]
fn the_search_subcommands_stay_hidden_but_parse_everything_after() {
    assert_eq!(
        visible(),
        [
            "ask",
            "sessions",
            "models",
            "extension",
            "approve",
            "config",
            "login",
            "logout",
            "version",
            "help"
        ]
    );
    let Invocation::Run(Some(Commands::Grep { args })) =
        parse_from(["fiber", "grep", "needle", "a.txt"])
    else {
        panic!("grep with a pattern and a path");
    };
    assert_eq!(args, [OsString::from("needle"), OsString::from("a.txt")]);
    // Clap handles no help or version flag for them: all is operands.
    let Invocation::Run(Some(Commands::Grep { args })) =
        parse_from(["fiber", "grep", "--help", "-n"])
    else {
        panic!("grep with flags");
    };
    assert_eq!(args, [OsString::from("--help"), OsString::from("-n")]);
    let Invocation::Run(Some(Commands::Grep { args })) = parse_from(["fiber", "grep"]) else {
        panic!("bare grep");
    };
    assert!(args.is_empty());
    let Invocation::Run(Some(Commands::Grep { args })) =
        parse_from(["fiber", "grep", "--", "-needle", "a.txt"])
    else {
        panic!("grep keeps the argv delimiter");
    };
    assert_eq!(
        args,
        [
            OsString::from("--"),
            OsString::from("-needle"),
            OsString::from("a.txt")
        ]
    );
    let Invocation::Run(Some(Commands::Find { args })) =
        parse_from(["fiber", "find", ".", "-name", "*.rs", "-o"])
    else {
        panic!("find with an expression");
    };
    assert_eq!(
        args,
        [
            OsString::from("."),
            OsString::from("-name"),
            OsString::from("*.rs"),
            OsString::from("-o")
        ]
    );
}

#[test]
fn the_image_subcommand_is_hidden_and_passes_its_arguments_through() {
    assert_eq!(
        visible(),
        [
            "ask",
            "sessions",
            "models",
            "extension",
            "approve",
            "config",
            "login",
            "logout",
            "version",
            "help"
        ]
    );
    let Invocation::Run(Some(Commands::Image { args })) =
        parse_from(["fiber", "image", "a", "b", "c"])
    else {
        panic!("image with three operands");
    };
    assert_eq!(
        args,
        [
            OsString::from("a"),
            OsString::from("b"),
            OsString::from("c")
        ]
    );
    let Invocation::Run(Some(Commands::Image { args })) =
        parse_from(["fiber", "image", "--", "-a", "b", "c"])
    else {
        panic!("image keeps the argv delimiter");
    };
    assert_eq!(args.first(), Some(&OsString::from("--")));
    assert_eq!(args.len(), 4);
}

#[test]
fn find_keeps_the_argv_delimiter_like_grep() {
    // Clap consumes the argv delimiter `--`, so only the raw slice after
    // the subcommand restores it: without the find arm above, the parsed
    // args would miss it.
    let Invocation::Run(Some(Commands::Find { args })) =
        parse_from(["fiber", "find", "--", "-name", "*.rs"])
    else {
        panic!("find keeps the argv delimiter");
    };
    assert_eq!(
        args,
        [
            OsString::from("--"),
            OsString::from("-name"),
            OsString::from("*.rs")
        ]
    );
}

#[test]
fn session_parses_its_id_workspace_model_and_prompt() {
    let Invocation::Run(Some(Commands::Session(args))) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/home/u/proj",
        "--model",
        "fake/m",
        "--prompt",
        "hi",
    ]) else {
        panic!("session with all four flags");
    };
    assert_eq!(args.id, "s_0123456789abcdef");
    assert_eq!(args.workspace, PathBuf::from("/home/u/proj"));
    assert_eq!(args.model.as_deref(), Some("fake/m"));
    assert_eq!(args.prompt.as_deref(), Some("hi"));

    let Invocation::Run(Some(Commands::Session(args))) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/home/u/proj",
    ]) else {
        panic!("session with only the required flags");
    };
    assert_eq!(args.model, None);
    assert_eq!(args.prompt, None);
    assert!(!args.resume);
}

#[test]
fn session_resume_parses_and_conflicts_with_prompt() {
    let Invocation::Run(Some(Commands::Session(args))) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/home/u/proj",
        "--resume",
    ]) else {
        panic!("session with --resume");
    };
    assert!(args.resume);
    assert_eq!(args.prompt, None);

    let said = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--resume",
        "--prompt",
        "hi",
    ]);
    assert!(said.contains("--resume"), "{said}");
    assert!(said.contains("--prompt"), "{said}");
}

#[test]
fn session_without_an_id_or_a_workspace_is_a_usage_error() {
    let missing_id = sentence(&["fiber", "session", "--workspace", "/w"]);
    assert!(missing_id.contains("--id <session_id>"), "{missing_id}");
    let missing_workspace = sentence(&["fiber", "session", "--id", "s_0123456789abcdef"]);
    assert!(
        missing_workspace.contains("--workspace <path>"),
        "{missing_workspace}"
    );
}

#[test]
fn session_rejects_an_id_that_is_not_a_minted_session_id() {
    for id in [
        "../x",
        "s_ABCDEF0123456789",
        "s_0123456789abcde",
        "s_0123456789abcdef0",
        "s_0123456789abcdeg",
        "s_",
        "s_0123456789ABCDEF",
        "x_0123456789abcdef",
    ] {
        let said = sentence(&["fiber", "session", "--id", id, "--workspace", "/w"]);
        assert!(said.contains("session id"), "{id}: {said}");
    }
}

#[test]
fn the_menu_and_top_level_help_name_no_session_command() {
    // A command line is two spaces, the name, then a space: `  session `.
    // `  sessions export` stays, as does prose such as "one session of".
    let command_line = |text: &str| text.lines().any(|line| line.starts_with("  session "));
    assert!(
        !command_line(&menu()),
        "the menu names no session command:\n{}",
        menu()
    );
    assert!(
        menu().contains("sessions export"),
        "sessions export stays:\n{}",
        menu()
    );
    assert!(
        !command_line(&super::render_help::<&str>(&[]).unwrap()),
        "top-level help names no session command"
    );
    assert!(
        visible().iter().all(|name| name != "session"),
        "session stays hidden: {:?}",
        visible()
    );
}

#[test]
fn a_session_usage_error_fails_before_any_session_like_ask() {
    let (ask, missing) = usage(&["fiber", "session"]);
    assert!(ask);
    assert!(missing.ends_with("Run `fiber --help` for usage."));
    let (ask, bad_id) = usage(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcde",
        "--workspace",
        "/w",
    ]);
    assert!(ask);
    assert!(bad_id.contains("session id"), "{bad_id}");
}

#[test]
fn config_get_takes_a_key() {
    let Invocation::Run(Some(Commands::Config(ConfigCommands::Get { key }))) =
        parse_from(["fiber", "config", "get", "model"])
    else {
        panic!("config get model");
    };
    assert_eq!(key, "model");
}

#[test]
fn config_set_takes_scopes_a_key_and_a_value() {
    let Invocation::Run(Some(Commands::Config(ConfigCommands::Set {
        project,
        repo,
        key,
        value,
    }))) = parse_from(["fiber", "config", "set", "model", "a/b"])
    else {
        panic!("config set model a/b");
    };
    assert!(!project);
    assert!(!repo);
    assert_eq!(key, "model");
    assert_eq!(value, "a/b");
    let Invocation::Run(Some(Commands::Config(ConfigCommands::Set {
        project,
        repo,
        key,
        value,
    }))) = parse_from([
        "fiber",
        "config",
        "set",
        "--project",
        "handoff.tokens",
        "200000",
    ])
    else {
        panic!("config set --project");
    };
    assert!(project);
    assert!(!repo);
    assert_eq!(key, "handoff.tokens");
    assert_eq!(value, "200000");
    let Invocation::Run(Some(Commands::Config(ConfigCommands::Set { repo, .. }))) =
        parse_from(["fiber", "config", "set", "--repo", "model", "a/b"])
    else {
        panic!("config set --repo");
    };
    assert!(repo);
}

#[test]
fn config_set_with_both_scopes_is_a_usage_error() {
    assert!(
        sentence(&[
            "fiber",
            "config",
            "set",
            "--project",
            "--repo",
            "model",
            "a/b"
        ])
        .contains("--project"),
        "{}",
        sentence(&[
            "fiber",
            "config",
            "set",
            "--project",
            "--repo",
            "model",
            "a/b"
        ])
    );
}

#[test]
fn config_set_without_a_value_names_it() {
    assert!(
        sentence(&["fiber", "config", "set", "model"]).contains("<value>"),
        "{}",
        sentence(&["fiber", "config", "set", "model"])
    );
}

#[test]
fn the_menu_lists_config_get_and_set_under_configuration() {
    assert!(
        menu().contains(
            "Configuration:\n\
             \x20\x20config get <key>                              Print the effective value and the layer it came from\n\
             \x20\x20config set [--project | --repo] <key> <value>  Write one key in one layer's file\n"
        ),
        "{}",
        menu()
    );
}

#[test]
fn hub_serve_parses_and_stays_hidden() {
    let Invocation::Run(Some(Commands::Hub(HubCommands::Serve))) =
        parse_from(["fiber", "hub", "serve"])
    else {
        panic!("hub serve parses");
    };
    // `fiber hub` with no subcommand is a usage error, not the hub.
    let (_, missing) = usage(&["fiber", "hub"]);
    assert!(
        missing.ends_with("Run `fiber --help` for usage."),
        "{missing}"
    );
    let command_line = |text: &str| text.lines().any(|line| line.starts_with("  hub "));
    assert!(
        !command_line(&menu()),
        "the menu names no hub command:\n{}",
        menu()
    );
    assert!(
        !command_line(&super::render_help::<&str>(&[]).unwrap()),
        "top-level help names no hub command"
    );
    assert!(
        visible().iter().all(|name| name != "hub"),
        "hub stays hidden: {:?}",
        visible()
    );
}

#[test]
fn the_hidden_refresh_child_takes_provider_names_only() {
    let Invocation::Run(Some(Commands::RefreshModelLists { providers })) =
        parse_from(["fiber", "refresh-model-lists", "openai", "anthropic"])
    else {
        panic!("refresh-model-lists with two providers");
    };
    assert_eq!(providers, ["openai", "anthropic"]);
    let Invocation::Run(Some(Commands::RefreshModelLists { providers })) =
        parse_from(["fiber", "refresh-model-lists"])
    else {
        panic!("bare refresh-model-lists");
    };
    assert!(providers.is_empty());
    assert!(
        !command()
            .get_subcommands()
            .any(|sub| sub.get_name() == "refresh-model-lists" && !sub.is_hide_set()),
        "the refresh child stays out of the menu"
    );
}
