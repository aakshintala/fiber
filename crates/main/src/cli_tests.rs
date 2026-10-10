use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use clap::error::ContextValue;

use crate::completion::Shell;

use super::{
    ApproveArgs, AskArgs, Commands, ConfigCommands, ExtensionCommands, HubCommands, Invocation,
    LoginArgs, LogoutArgs, MENU, SessionArgs, SessionsArgs, SessionsCommands, command, parse_from,
    usage_sentence, version_line,
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

/// The visible subcommands the menu does not name: a command line is a
/// line under a group heading that is indented by two spaces, and its
/// first whitespace-separated token is the command it documents. A
/// substring match is not enough: the `sessions delete` description holds
/// "the sessions that continue it", which contains "continue".
fn missing_from_menu(menu: &str, names: &[String]) -> Vec<String> {
    let tokens: Vec<&str> = menu
        .lines()
        .filter(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    names
        .iter()
        .filter(|name| !tokens.iter().any(|token| token == name))
        .cloned()
        .collect()
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
    let names = visible();
    assert_eq!(
        missing_from_menu(&long, &names),
        Vec::<String>::new(),
        "{long}"
    );
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
fn the_menu_check_fails_when_a_command_line_is_gone() {
    let names = visible();
    let without_continue: String = menu()
        .lines()
        .filter(|line| line.split_whitespace().next() != Some("continue"))
        .collect::<Vec<&str>>()
        .join("\n");
    assert_eq!(
        missing_from_menu(&without_continue, &names),
        ["continue".to_owned()]
    );
    // `hub` stays while any of its lines stays: removing `hub install`
    // leaves `hub status`, so `hub` is still found.
    let without_install: String = menu()
        .lines()
        .filter(|line| !line.contains("hub install"))
        .collect::<Vec<&str>>()
        .join("\n");
    assert!(
        !missing_from_menu(&without_install, &names).contains(&"hub".to_owned()),
        "hub is still named by its other lines"
    );
}

/// Every successful parse the per-flag tests pinned, in one table: each
/// row is the argv and the `Invocation` it parses to. The usage halves of
/// those tests stay where they were, as one-sentence tests.
#[test]
fn parses_argv_into_its_invocation() {
    let cases: Vec<(&[&str], Invocation)> = vec![
        (
            &["fiber", "extension", "update"],
            Invocation::Run(Some(Commands::Extension(ExtensionCommands::Update {
                name: None,
            }))),
        ),
        (
            &["fiber", "extension", "update", "x"],
            Invocation::Run(Some(Commands::Extension(ExtensionCommands::Update {
                name: Some("x".to_owned()),
            }))),
        ),
        (
            &["fiber", "extension", "test"],
            Invocation::Run(Some(Commands::Extension(ExtensionCommands::Test {
                path: None,
            }))),
        ),
        (
            &["fiber", "extension", "test", "./package"],
            Invocation::Run(Some(Commands::Extension(ExtensionCommands::Test {
                path: Some(PathBuf::from("./package")),
            }))),
        ),
        (
            &["fiber", "approve"],
            Invocation::Run(Some(Commands::Approve(ApproveArgs { yes: false }))),
        ),
        (
            &["fiber", "approve", "--yes"],
            Invocation::Run(Some(Commands::Approve(ApproveArgs { yes: true }))),
        ),
        (
            &["fiber", "login"],
            Invocation::Run(Some(Commands::Login(LoginArgs {
                name: None,
                label: None,
                device: false,
            }))),
        ),
        (
            &["fiber", "login", "openrouter"],
            Invocation::Run(Some(Commands::Login(LoginArgs {
                name: Some("openrouter".to_owned()),
                label: None,
                device: false,
            }))),
        ),
        (
            &["fiber", "login", "acme", "--as", "work"],
            Invocation::Run(Some(Commands::Login(LoginArgs {
                name: Some("acme".to_owned()),
                label: Some("work".to_owned()),
                device: false,
            }))),
        ),
        (
            &["fiber", "login", "codex", "--device"],
            Invocation::Run(Some(Commands::Login(LoginArgs {
                name: Some("codex".to_owned()),
                label: None,
                device: true,
            }))),
        ),
        (
            &["fiber", "logout"],
            Invocation::Run(Some(Commands::Logout(LogoutArgs {
                provider: None,
                label: None,
                all: false,
            }))),
        ),
        (
            &["fiber", "logout", "opencode-zen"],
            Invocation::Run(Some(Commands::Logout(LogoutArgs {
                provider: Some("opencode-zen".to_owned()),
                label: None,
                all: false,
            }))),
        ),
        (
            &["fiber", "logout", "acme", "--as", "work"],
            Invocation::Run(Some(Commands::Logout(LogoutArgs {
                provider: Some("acme".to_owned()),
                label: Some("work".to_owned()),
                all: false,
            }))),
        ),
        (
            &["fiber", "logout", "acme", "--all"],
            Invocation::Run(Some(Commands::Logout(LogoutArgs {
                provider: Some("acme".to_owned()),
                label: None,
                all: true,
            }))),
        ),
        (
            &["fiber", "sessions", "export", "s_abc"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Export {
                    id: "s_abc".to_owned(),
                    path: None,
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "export", "s_abc", "out"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Export {
                    id: "s_abc".to_owned(),
                    path: Some(PathBuf::from("out")),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "delete", "s_abc"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Delete {
                    cascade: false,
                    yes: false,
                    id: "s_abc".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "delete", "--cascade", "--yes", "s_abc"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Delete {
                    cascade: true,
                    yes: true,
                    id: "s_abc".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "prune"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Prune {
                    older_than: None,
                    cascade: false,
                    dry_run: false,
                    yes: false,
                    force: false,
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &[
                "fiber",
                "sessions",
                "prune",
                "--older-than",
                "30d",
                "--cascade",
                "--dry-run",
                "--yes",
                "--force",
            ],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Prune {
                    older_than: Some("30d".to_owned()),
                    cascade: true,
                    dry_run: true,
                    yes: true,
                    force: true,
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: None,
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "--all"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: None,
                all: true,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "--json"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: None,
                all: false,
                json: true,
            }))),
        ),
        (
            &["fiber", "sessions", "--json", "--all"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: None,
                all: true,
                json: true,
            }))),
        ),
        (
            &["fiber", "sessions", "search", "x"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Search {
                    all: false,
                    json: false,
                    text: "x".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "search", "--all", "x"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Search {
                    all: true,
                    json: false,
                    text: "x".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "search", "--json", "x"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Search {
                    all: false,
                    json: true,
                    text: "x".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "search", "--all", "--json", "x"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Search {
                    all: true,
                    json: true,
                    text: "x".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "sessions", "search", "--", "-n"],
            Invocation::Run(Some(Commands::Sessions(SessionsArgs {
                command: Some(SessionsCommands::Search {
                    all: false,
                    json: false,
                    text: "-n".to_owned(),
                }),
                all: false,
                json: false,
            }))),
        ),
        (
            &["fiber", "completion", "bash"],
            Invocation::Run(Some(Commands::Completion { shell: Shell::Bash })),
        ),
        (
            &["fiber", "completion", "zsh"],
            Invocation::Run(Some(Commands::Completion { shell: Shell::Zsh })),
        ),
        (
            &["fiber", "completion", "fish"],
            Invocation::Run(Some(Commands::Completion { shell: Shell::Fish })),
        ),
        (
            &["fiber", "ask", "--resume", "s_abc", "hi"],
            Invocation::Run(Some(Commands::Ask(AskArgs {
                model: None,
                overrides: Vec::new(),
                resume: Some("s_abc".to_owned()),
                credential: None,
                worktree: false,
                prompt: vec!["hi".to_owned()],
            }))),
        ),
        (
            &["fiber", "ask", "--resume", "s_abc"],
            Invocation::Run(Some(Commands::Ask(AskArgs {
                model: None,
                overrides: Vec::new(),
                resume: Some("s_abc".to_owned()),
                credential: None,
                worktree: false,
                prompt: Vec::new(),
            }))),
        ),
        (
            &["fiber", "ask", "hi"],
            Invocation::Run(Some(Commands::Ask(AskArgs {
                model: None,
                overrides: Vec::new(),
                resume: None,
                credential: None,
                worktree: false,
                prompt: vec!["hi".to_owned()],
            }))),
        ),
        (
            &["fiber", "ask", "--worktree", "hi"],
            Invocation::Run(Some(Commands::Ask(AskArgs {
                model: None,
                overrides: Vec::new(),
                resume: None,
                credential: None,
                worktree: true,
                prompt: vec!["hi".to_owned()],
            }))),
        ),
        (
            &[
                "fiber",
                "ask",
                "--resume",
                "s_abc",
                "--credential",
                "home",
                "hi",
            ],
            Invocation::Run(Some(Commands::Ask(AskArgs {
                model: None,
                overrides: Vec::new(),
                resume: Some("s_abc".to_owned()),
                credential: Some("home".to_owned()),
                worktree: false,
                prompt: vec!["hi".to_owned()],
            }))),
        ),
        (
            &[
                "fiber",
                "session",
                "--id",
                "s_0123456789abcdef",
                "--workspace",
                "/home/u/proj",
                "--worktree",
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: None,
                overrides: Vec::new(),
                prompt: None,
                resume: false,
                worktree: true,
                rewound_from: None,
                parent: None,
                delegate_id: None,
            }))),
        ),
        (
            &[
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
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: Some("fake/m".to_owned()),
                overrides: Vec::new(),
                prompt: Some("hi".to_owned()),
                resume: false,
                worktree: false,
                rewound_from: None,
                parent: None,
                delegate_id: None,
            }))),
        ),
        (
            &[
                "fiber",
                "session",
                "--id",
                "s_0123456789abcdef",
                "--workspace",
                "/home/u/proj",
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: None,
                overrides: Vec::new(),
                prompt: None,
                resume: false,
                worktree: false,
                rewound_from: None,
                parent: None,
                delegate_id: None,
            }))),
        ),
        (
            &[
                "fiber",
                "session",
                "--id",
                "s_0123456789abcdef",
                "--workspace",
                "/home/u/proj",
                "--resume",
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: None,
                overrides: Vec::new(),
                prompt: None,
                resume: true,
                worktree: false,
                rewound_from: None,
                parent: None,
                delegate_id: None,
            }))),
        ),
        (
            &[
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
                "--parent",
                "s_aaaaaaaaaaaaaaaa",
                "--delegate-id",
                "j_bbbbbbbbbbbbbbbb",
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: Some("fake/m".to_owned()),
                overrides: Vec::new(),
                prompt: Some("hi".to_owned()),
                resume: false,
                worktree: false,
                rewound_from: None,
                parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
                delegate_id: Some("j_bbbbbbbbbbbbbbbb".to_owned()),
            }))),
        ),
        (
            &[
                "fiber",
                "session",
                "--id",
                "s_0123456789abcdef",
                "--workspace",
                "/home/u/proj",
                "--rewound-from",
                "s_aaaaaaaaaaaaaaaa",
            ],
            Invocation::Run(Some(Commands::Session(SessionArgs {
                id: "s_0123456789abcdef".to_owned(),
                workspace: PathBuf::from("/home/u/proj"),
                model: None,
                overrides: Vec::new(),
                prompt: None,
                resume: false,
                worktree: false,
                rewound_from: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
                parent: None,
                delegate_id: None,
            }))),
        ),
        (
            &["fiber", "config", "get", "model"],
            Invocation::Run(Some(Commands::Config(ConfigCommands::Get {
                key: "model".to_owned(),
            }))),
        ),
        (
            &["fiber", "config", "set", "model", "a/b"],
            Invocation::Run(Some(Commands::Config(ConfigCommands::Set {
                project: false,
                repo: false,
                key: "model".to_owned(),
                value: "a/b".to_owned(),
            }))),
        ),
        (
            &[
                "fiber",
                "config",
                "set",
                "--project",
                "handoff.tokens",
                "200000",
            ],
            Invocation::Run(Some(Commands::Config(ConfigCommands::Set {
                project: true,
                repo: false,
                key: "handoff.tokens".to_owned(),
                value: "200000".to_owned(),
            }))),
        ),
        (
            &["fiber", "config", "set", "--repo", "model", "a/b"],
            Invocation::Run(Some(Commands::Config(ConfigCommands::Set {
                project: false,
                repo: true,
                key: "model".to_owned(),
                value: "a/b".to_owned(),
            }))),
        ),
        (
            &["fiber", "hub", "serve"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Serve { installed: false }))),
        ),
        (
            &["fiber", "hub", "serve", "--installed"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Serve { installed: true }))),
        ),
        (
            &["fiber", "hub", "install"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Install { port: None }))),
        ),
        (
            &["fiber", "hub", "install", "--port", "4040"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Install {
                port: Some(4040),
            }))),
        ),
        (
            &["fiber", "hub", "install", "--port", "1"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Install { port: Some(1) }))),
        ),
        (
            &["fiber", "hub", "install", "--port", "65535"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Install {
                port: Some(65535),
            }))),
        ),
        (
            &["fiber", "hub", "uninstall"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Uninstall))),
        ),
        (
            &["fiber", "hub", "status"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Status { json: false }))),
        ),
        (
            &["fiber", "hub", "status", "--json"],
            Invocation::Run(Some(Commands::Hub(HubCommands::Status { json: true }))),
        ),
    ];
    for (argv, expected) in &cases {
        let parsed = parse_from(argv.iter().copied());
        assert_eq!(format!("{parsed:?}"), format!("{expected:?}"), "{argv:?}");
    }
}

#[test]
fn extension_case_is_a_hidden_internal_command() {
    let parsed = parse_from(["fiber", "extension-case", "tests/first.json"]);
    assert!(
        matches!(parsed, Invocation::Run(Some(Commands::ExtensionCase { ref case })) if case.as_os_str() == OsStr::new("tests/first.json")),
        "{parsed:?}"
    );
    assert!(!visible().iter().any(|name| name == "extension-case"));
    assert!(!menu().contains("extension-case"));
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
fn extension_test_rejects_extra_arguments() {
    let error = sentence(&["fiber", "extension", "test", "a", "b"]);
    assert!(error.starts_with("Unexpected argument 'b'"), "{error}");
    assert!(error.ends_with("Run `fiber --help` for usage."), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
}

#[test]
fn approve_rejects_anything_else() {
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
fn login_and_logout_reject_extra_arguments() {
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
fn logout_as_conflicts_with_all() {
    let said = sentence(&["fiber", "logout", "acme", "--as", "w", "--all"]);
    assert!(said.contains("cannot be used with"), "{said}");
    assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");
}

#[test]
fn sessions_export_without_an_id_is_a_usage_sentence() {
    assert!(
        sentence(&["fiber", "sessions", "export"])
            .starts_with("The following required arguments were not provided: <id>"),
        "{}",
        sentence(&["fiber", "sessions", "export"])
    );
}

#[test]
fn sessions_delete_without_an_id_is_a_usage_sentence() {
    assert!(
        sentence(&["fiber", "sessions", "delete"])
            .starts_with("The following required arguments were not provided: <id>"),
        "{}",
        sentence(&["fiber", "sessions", "delete"])
    );
}

#[test]
fn a_list_flag_with_a_subcommand_is_a_usage_sentence() {
    for args in [
        &["fiber", "sessions", "--all", "delete", "s_1"][..],
        &["fiber", "sessions", "--json", "export", "s_1"],
    ] {
        let said = sentence(args);
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn resume_takes_an_optional_id() {
    let Invocation::Run(Some(Commands::Resume { id })) = parse_from(["fiber", "resume"]) else {
        panic!("bare resume");
    };
    assert_eq!(id, None);
    let Invocation::Run(Some(Commands::Resume { id })) = parse_from(["fiber", "resume", "s_12"])
    else {
        panic!("resume with an id");
    };
    assert_eq!(id.as_deref(), Some("s_12"));
    let help = super::render_help(&["resume"]).unwrap();
    assert!(
        help.contains("Open a session in the terminal, or home at the session list"),
        "{help}"
    );
}

#[test]
fn continue_takes_no_arguments() {
    let Invocation::Run(Some(Commands::Continue)) = parse_from(["fiber", "continue"]) else {
        panic!("bare continue");
    };
    let help = super::render_help(&["continue"]).unwrap();
    assert!(
        help.contains("Open the most recent session in this project"),
        "{help}"
    );
}

#[test]
fn resume_and_continue_reject_what_they_do_not_take() {
    for args in [
        &["fiber", "resume", "a", "b"][..],
        &["fiber", "resume", ""][..],
        &["fiber", "continue", "x"][..],
    ] {
        let said = sentence(args);
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn sessions_search_without_a_text_is_a_usage_sentence() {
    let missing = sentence(&["fiber", "sessions", "search"]);
    assert!(missing.contains("<text>"), "{missing}");
    assert!(
        missing.ends_with("Run `fiber --help` for usage."),
        "{missing}"
    );
    assert_eq!(missing.lines().count(), 1, "{missing}");
    let empty = sentence(&["fiber", "sessions", "search", ""]);
    assert!(empty.ends_with("Run `fiber --help` for usage."), "{empty}");
    assert_eq!(empty.lines().count(), 1, "{empty}");
}

#[test]
fn a_search_flag_with_a_list_flag_is_a_usage_sentence() {
    for args in [
        &["fiber", "sessions", "--all", "search", "x"][..],
        &["fiber", "sessions", "--json", "search", "x"],
    ] {
        let said = sentence(args);
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
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
    let help = super::render_help(&["models"]).unwrap();
    assert!(
        help.contains("List the models the installed providers serve"),
        "{help}"
    );
}

#[test]
fn an_unknown_shell_names_claps_suggestion() {
    for (word, suggested) in [("bsh", "bash"), ("f", "fish"), ("Bash", "bash")] {
        assert_eq!(
            usage(&["fiber", "completion", word]),
            (
                false,
                format!(
                    "Invalid value '{word}' for '<shell>' [possible values: bash, zsh, fish]; did you mean '{suggested}'? Run `fiber --help` for usage."
                )
            ),
            "{word}"
        );
    }
    for word in ["zhs", "powershell"] {
        assert_eq!(
            usage(&["fiber", "completion", word]),
            (
                false,
                format!(
                    "Invalid value '{word}' for '<shell>' [possible values: bash, zsh, fish]. Run `fiber --help` for usage."
                )
            ),
            "{word}"
        );
    }
}

#[test]
fn completion_without_one_shell_is_a_usage_error() {
    assert_eq!(
        usage(&["fiber", "completion"]),
        (
            false,
            "The following required arguments were not provided: <shell>. Run `fiber --help` for usage."
                .to_owned()
        )
    );
    assert_eq!(
        usage(&["fiber", "completion", "bash", "extra"]),
        (
            false,
            "Unexpected argument 'extra' found. Run `fiber --help` for usage.".to_owned()
        )
    );
    let parsed = parse_from([
        OsString::from("fiber"),
        OsString::from("completion"),
        OsString::from_vec(vec![0xff, 0xfe]),
    ]);
    let Invocation::Usage { ask, sentence } = parsed else {
        panic!("{parsed:?}");
    };
    // clap reads the shell as a possible value, so a byte that is not
    // UTF-8 is an invalid value, shown lossily, like any other.
    assert!(!ask);
    assert_eq!(
        sentence,
        "Invalid value '\u{fffd}\u{fffd}' for '<shell>' [possible values: bash, zsh, fish]. Run `fiber --help` for usage."
    );
}

#[test]
fn extension_alone_is_a_one_line_usage_sentence() {
    assert_eq!(
        sentence(&["fiber", "extension"]),
        "'fiber extension' requires a subcommand but one was not provided [subcommands: install, update, remove, list, test]. Run `fiber --help` for usage."
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
fn ask_worktree_with_resume_is_a_usage_error() {
    let (ask, said) = usage(&["fiber", "ask", "--resume", "s_abc", "--worktree", "hi"]);
    assert!(ask);
    assert!(said.contains("--worktree"), "{said}");
    assert!(said.contains("--resume"), "{said}");
}

#[test]
fn session_worktree_conflicts_with_resume() {
    let (ask, said) = usage(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--resume",
        "--worktree",
    ]);
    assert!(ask);
    assert!(said.contains("--resume"), "{said}");
    assert!(said.contains("--worktree"), "{said}");
}

#[test]
fn the_menu_and_ask_help_show_worktree() {
    assert!(
        menu().contains("[--worktree]"),
        "the menu shows --worktree:\n{}",
        menu()
    );
    let rendered = super::render_help(&["ask"]).unwrap();
    assert!(
        rendered.contains("--worktree"),
        "ask's help shows --worktree:\n{rendered}"
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
            "resume",
            "continue",
            "sessions",
            "models",
            "extension",
            "approve",
            "config",
            "login",
            "logout",
            "completion",
            "version",
            "help",
            "hub"
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
            "resume",
            "continue",
            "sessions",
            "models",
            "extension",
            "approve",
            "config",
            "login",
            "logout",
            "completion",
            "version",
            "help",
            "hub"
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
fn session_resume_conflicts_with_prompt() {
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
fn session_parent_without_its_delegate_id_or_prompt_is_a_usage_error() {
    let missing_id = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--prompt",
        "hi",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
    ]);
    assert!(missing_id.contains("--delegate-id"), "{missing_id}");
    let missing_prompt = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
    ]);
    assert!(missing_prompt.contains("--prompt"), "{missing_prompt}");
    let missing_parent = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--prompt",
        "hi",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
    ]);
    assert!(missing_parent.contains("--parent"), "{missing_parent}");
}

#[test]
fn session_parent_conflicts_with_resume_and_worktree() {
    let resumed = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--prompt",
        "hi",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
        "--resume",
    ]);
    // `--prompt`'s own `--resume` conflict fires first; either way the
    // combination is a usage error naming `--resume`.
    assert!(resumed.contains("--resume"), "{resumed}");
    let isolated = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--prompt",
        "hi",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
        "--worktree",
    ]);
    assert!(isolated.contains("--parent"), "{isolated}");
    assert!(isolated.contains("--worktree"), "{isolated}");
}

#[test]
fn session_rejects_ids_that_are_not_minted_parent_or_job_ids() {
    for parent in ["s_ABCDEF0123456789", "s_short", "j_bbbbbbbbbbbbbbbb"] {
        let said = sentence(&[
            "fiber",
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--prompt",
            "hi",
            "--parent",
            parent,
            "--delegate-id",
            "j_bbbbbbbbbbbbbbbb",
        ]);
        assert!(said.contains("session id"), "{parent}: {said}");
    }
    for job in ["j_ABCDEF0123456789", "j_short", "s_0123456789abcdef"] {
        let said = sentence(&[
            "fiber",
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--prompt",
            "hi",
            "--parent",
            "s_aaaaaaaaaaaaaaaa",
            "--delegate-id",
            job,
        ]);
        assert!(said.contains("job id"), "{job}: {said}");
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
fn hub_serve_stays_hidden() {
    // `fiber hub` with no subcommand is a usage error, not the hub.
    let (_, missing) = usage(&["fiber", "hub"]);
    assert!(
        missing.ends_with("Run `fiber --help` for usage."),
        "{missing}"
    );
    for text in [menu(), super::render_help::<&str>(&[]).unwrap()] {
        assert!(
            !text.contains("hub serve"),
            "the menu names no hub serve:\n{text}"
        );
    }
    let hub_help = super::render_help(&["hub"]).unwrap();
    assert!(
        hub_help
            .lines()
            .all(|line| !line.trim_start().starts_with("serve")),
        "{hub_help}"
    );
    assert!(hub_help.contains("install"), "{hub_help}");
    let serve_help = super::render_help(&["hub", "serve"]).unwrap();
    assert!(!serve_help.contains("--installed"), "{serve_help}");
}

#[test]
fn a_hub_port_outside_1_to_65535_or_an_extra_argument_is_one_usage_sentence() {
    for args in [
        &["fiber", "hub", "install", "--port", "0"][..],
        &["fiber", "hub", "install", "--port", "65536"],
        &["fiber", "hub", "install", "--port", "x"],
        &["fiber", "hub", "status", "extra"],
    ] {
        let (ask, sentence) = usage(args);
        assert!(!ask, "{args:?}");
        assert_eq!(sentence.lines().count(), 1, "{args:?}: {sentence}");
        assert!(
            sentence.ends_with("Run `fiber --help` for usage."),
            "{args:?}: {sentence}"
        );
    }
    assert!(
        sentence(&["fiber", "hub", "install", "--port", "0"]).contains("--port"),
        "{}",
        sentence(&["fiber", "hub", "install", "--port", "0"])
    );
}

#[test]
fn help_hub_install_prints_that_commands_help() {
    let help = super::render_help(&["hub", "install"]).unwrap();
    assert!(
        help.contains("Register the hub as a login service"),
        "{help}"
    );
    assert!(help.contains("--port <port>"), "{help}");
    let parsed = parse_from(["fiber", "help", "hub", "install"]);
    assert!(
        matches!(
            parsed,
            Invocation::Run(Some(Commands::Help { ref command })) if command == &["hub", "install"]
        ),
        "{parsed:?}"
    );
}

#[test]
fn the_hidden_release_install_takes_a_version_and_a_base_url() {
    let Invocation::Run(Some(Commands::ReleaseInstall { version, base_url })) =
        parse_from(["fiber", "release-install", "0.3.0"])
    else {
        panic!("release-install with a version");
    };
    assert_eq!(version, "0.3.0");
    assert_eq!(base_url, None);
    let Invocation::Run(Some(Commands::ReleaseInstall { version, base_url })) = parse_from([
        "fiber",
        "release-install",
        "0.3.0",
        "--base-url",
        "file:///tmp/r",
    ]) else {
        panic!("release-install with a base URL");
    };
    assert_eq!(version, "0.3.0");
    assert_eq!(base_url.as_deref(), Some("file:///tmp/r"));
    assert!(
        command()
            .try_get_matches_from(["fiber", "release-install"])
            .is_err(),
        "the version is required"
    );
    assert!(
        !command()
            .get_subcommands()
            .any(|sub| sub.get_name() == "release-install" && !sub.is_hide_set()),
        "the release install step stays out of the menu"
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

#[test]
fn session_rewound_from_conflicts_with_a_fresh_start() {
    for extra in [
        vec!["--resume"],
        vec!["--prompt", "hi"],
        vec!["--model", "fake/m"],
        vec!["-c", "retry.attempts=2"],
        vec!["--worktree"],
        vec![
            "--prompt",
            "hi",
            "--parent",
            "s_bbbbbbbbbbbbbbbb",
            "--delegate-id",
            "j_bbbbbbbbbbbbbbbb",
        ],
    ] {
        let mut argv = vec![
            "fiber",
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--rewound-from",
            "s_aaaaaaaaaaaaaaaa",
        ];
        argv.extend(extra);
        let (_, said) = usage(&argv);
        assert!(said.contains("--rewound-from"), "{said}");
    }
}

#[test]
fn ask_credential_without_resume_is_a_usage_error() {
    let message = sentence(&["fiber", "ask", "--credential", "home", "hi"]);
    assert!(
        message.contains("--resume"),
        "the error names `--resume`: {message}"
    );
}

#[test]
fn ask_help_shows_credential() {
    let rendered = super::render_help(&["ask"]).unwrap();
    assert!(rendered.contains("--credential <label>"), "{rendered}");
}
