use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use clap::error::ContextValue;

use crate::completion::Shell;

use super::{
    ApproveArgs, AskArgs, ChannelArg, Commands, ConfigCommands, ExtensionCommands, HubCommands,
    ImportSource, Invocation, LoginArgs, LogoutArgs, MENU, McpAddArgs, McpCommands, McpRemoveArgs,
    SessionArgs, SessionsArgs, SessionsCommands, SetupArgs, TokenCommands, TopLevel, command,
    parse_from, usage_sentence, version_line,
};

#[test]
fn hub_list_commands_and_client_flags_parse() {
    for args in [
        vec!["fiber", "hub", "add", "box", "ws://box"],
        vec!["fiber", "hub", "add", "--default", "box", "unix:/tmp/hub"],
        vec!["fiber", "hub", "remove", "box"],
        vec!["fiber", "sessions", "--hub", "box", "--json"],
        vec!["fiber", "hub", "refresh", "--hub", "box"],
        vec!["fiber", "hub", "pair", "laptop", "--hub", "box"],
        vec!["fiber", "hub", "token", "list", "--hub", "box"],
        vec!["fiber", "hub", "token", "revoke", "laptop", "--hub", "box"],
    ] {
        let invocation = parse_from(args.clone());
        assert!(
            matches!(invocation, Invocation::Run(Some(_), _)),
            "{args:?}: {invocation:?}"
        );
    }
    for args in [
        vec!["fiber", "sessions", "delete", "s_id", "--hub", "box"],
        vec!["fiber", "hub", "status", "--hub", "box"],
    ] {
        assert!(matches!(parse_from(args), Invocation::Usage { .. }));
    }
}

#[test]
fn hub_list_parser_keeps_names_addresses_defaults_and_selected_hubs() {
    for default in [false, true] {
        let mut args = vec!["fiber", "hub", "add", "box", "ws://box/hub"];
        if default {
            args.push("--default");
        }
        let Invocation::Run(
            Some(Commands::Hub(HubCommands::Add {
                name,
                address,
                default: actual,
            })),
            _,
        ) = parse_from(args)
        else {
            panic!("hub add did not parse")
        };
        assert_eq!(name, "box");
        assert_eq!(address, "ws://box/hub");
        assert_eq!(actual, default);
    }
    let Invocation::Run(Some(Commands::Hub(HubCommands::Remove { name })), _) =
        parse_from(["fiber", "hub", "remove", "box"])
    else {
        panic!("hub remove did not parse")
    };
    assert_eq!(name, "box");
    for args in [
        vec!["fiber", "sessions", "--hub", "box"],
        vec!["fiber", "hub", "refresh", "--hub", "box"],
        vec!["fiber", "hub", "pair", "laptop", "--hub", "box"],
        vec!["fiber", "hub", "token", "list", "--hub", "box"],
        vec!["fiber", "hub", "token", "revoke", "laptop", "--hub", "box"],
    ] {
        let selected = match parse_from(args) {
            Invocation::Run(Some(Commands::Sessions(SessionsArgs { hub, .. })), _) => hub,
            Invocation::Run(
                Some(Commands::Hub(
                    HubCommands::Refresh { hub }
                    | HubCommands::Pair { hub, .. }
                    | HubCommands::Token(
                        TokenCommands::List { hub } | TokenCommands::Revoke { hub, .. },
                    ),
                )),
                _,
            ) => hub,
            invocation
            @ (Invocation::Print(_) | Invocation::Run(..) | Invocation::Usage { .. }) => {
                panic!("unexpected invocation: {invocation:?}")
            }
        };
        assert_eq!(selected.as_deref(), Some("box"));
    }
}

#[test]
fn extension_install_accepts_project_scope_and_requires_exactly_one_name() {
    for (args, expected) in [
        (vec!["fiber", "extension", "install", "./acme"], false),
        (
            vec!["fiber", "extension", "install", "--project", "./acme"],
            true,
        ),
        (
            vec!["fiber", "extension", "install", "./acme", "--project"],
            true,
        ),
        (
            vec!["fiber", "extension", "install", "--project", "acme"],
            true,
        ),
    ] {
        let parsed = parse_from(args);
        let Invocation::Run(
            Some(Commands::Extension(ExtensionCommands::Install {
                project,
                name_or_path,
            })),
            _,
        ) = parsed
        else {
            panic!("expected install, got {parsed:?}")
        };
        assert_eq!(project, expected);
        assert!(name_or_path.ends_with("acme"));
    }
    for args in [
        vec!["fiber", "extension", "install", "--project"],
        vec!["fiber", "extension", "install", "acme", "extra"],
    ] {
        assert!(matches!(parse_from(args), Invocation::Usage { .. }));
    }
    let Invocation::Print(help) = parse_from(["fiber", "extension", "install", "--help"]) else {
        panic!("expected help")
    };
    assert!(help.to_string().contains("--project"));
    assert!(MENU.contains("extension install [--project] <name or path>"));
}

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
            Invocation::Run(Some(Commands::Help { ref command }), _) if command.as_slice().is_empty()
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
            Invocation::Run(
                Some(Commands::Extension(ExtensionCommands::Update {
                    name: None,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "extension", "update", "x"],
            Invocation::Run(
                Some(Commands::Extension(ExtensionCommands::Update {
                    name: Some("x".to_owned()),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "extension", "reinstall", "acme"],
            Invocation::Run(
                Some(Commands::Extension(ExtensionCommands::Reinstall {
                    name: "acme".to_owned(),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "extension", "test"],
            Invocation::Run(
                Some(Commands::Extension(ExtensionCommands::Test { path: None })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "extension", "test", "./package"],
            Invocation::Run(
                Some(Commands::Extension(ExtensionCommands::Test {
                    path: Some(PathBuf::from("./package")),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "approve"],
            Invocation::Run(
                Some(Commands::Approve(ApproveArgs { yes: false })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "approve", "--yes"],
            Invocation::Run(
                Some(Commands::Approve(ApproveArgs { yes: true })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "login"],
            Invocation::Run(
                Some(Commands::Login(LoginArgs {
                    name: None,
                    label: None,
                    device: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "login", "openrouter"],
            Invocation::Run(
                Some(Commands::Login(LoginArgs {
                    name: Some("openrouter".to_owned()),
                    label: None,
                    device: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "login", "acme", "--as", "work"],
            Invocation::Run(
                Some(Commands::Login(LoginArgs {
                    name: Some("acme".to_owned()),
                    label: Some("work".to_owned()),
                    device: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "login", "codex", "--device"],
            Invocation::Run(
                Some(Commands::Login(LoginArgs {
                    name: Some("codex".to_owned()),
                    label: None,
                    device: true,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "logout"],
            Invocation::Run(
                Some(Commands::Logout(LogoutArgs {
                    provider: None,
                    label: None,
                    all: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "logout", "opencode-zen"],
            Invocation::Run(
                Some(Commands::Logout(LogoutArgs {
                    provider: Some("opencode-zen".to_owned()),
                    label: None,
                    all: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "doctor"],
            Invocation::Run(Some(Commands::Doctor), TopLevel::default()),
        ),
        (
            &["fiber", "logout", "acme", "--as", "work"],
            Invocation::Run(
                Some(Commands::Logout(LogoutArgs {
                    provider: Some("acme".to_owned()),
                    label: Some("work".to_owned()),
                    all: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "logout", "acme", "--all"],
            Invocation::Run(
                Some(Commands::Logout(LogoutArgs {
                    provider: Some("acme".to_owned()),
                    label: None,
                    all: true,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "export", "s_abc"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Export {
                        id: "s_abc".to_owned(),
                        path: None,
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "export", "s_abc", "out"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Export {
                        id: "s_abc".to_owned(),
                        path: Some(PathBuf::from("out")),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "delete", "s_abc"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Delete {
                        cascade: false,
                        yes: false,
                        id: "s_abc".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "delete", "--cascade", "--yes", "s_abc"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Delete {
                        cascade: true,
                        yes: true,
                        id: "s_abc".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "prune"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Prune {
                        older_than: None,
                        cascade: false,
                        dry_run: false,
                        yes: false,
                        force: false,
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Prune {
                        older_than: Some("30d".to_owned()),
                        cascade: true,
                        dry_run: true,
                        yes: true,
                        force: true,
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: None,
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "--all"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: None,
                    all: true,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "--json"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: None,
                    all: false,
                    json: true,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "--json", "--all"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: None,
                    all: true,
                    json: true,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "x"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: false,
                        json: false,
                        text: "x".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "--all", "x"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: true,
                        json: false,
                        text: "x".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "--json", "x"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: false,
                        json: true,
                        text: "x".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "--all", "--json", "x"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: true,
                        json: true,
                        text: "x".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "--json", "--all", "x"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: true,
                        json: true,
                        text: "x".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "sessions", "search", "--", "-n"],
            Invocation::Run(
                Some(Commands::Sessions(SessionsArgs {
                    hub: None,
                    command: Some(SessionsCommands::Search {
                        all: false,
                        json: false,
                        text: "-n".to_owned(),
                    }),
                    all: false,
                    json: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "completion", "bash"],
            Invocation::Run(
                Some(Commands::Completion { shell: Shell::Bash }),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "completion", "zsh"],
            Invocation::Run(
                Some(Commands::Completion { shell: Shell::Zsh }),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "completion", "fish"],
            Invocation::Run(
                Some(Commands::Completion { shell: Shell::Fish }),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "ask", "--resume", "s_abc", "hi"],
            Invocation::Run(
                Some(Commands::Ask(AskArgs {
                    model: None,
                    overrides: Vec::new(),
                    resume: Some("s_abc".to_owned()),
                    credential: None,
                    worktree: false,
                    prompt: vec!["hi".to_owned()],
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "ask", "--resume", "s_abc"],
            Invocation::Run(
                Some(Commands::Ask(AskArgs {
                    model: None,
                    overrides: Vec::new(),
                    resume: Some("s_abc".to_owned()),
                    credential: None,
                    worktree: false,
                    prompt: Vec::new(),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "ask", "hi"],
            Invocation::Run(
                Some(Commands::Ask(AskArgs {
                    model: None,
                    overrides: Vec::new(),
                    resume: None,
                    credential: None,
                    worktree: false,
                    prompt: vec!["hi".to_owned()],
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "ask", "--worktree", "hi"],
            Invocation::Run(
                Some(Commands::Ask(AskArgs {
                    model: None,
                    overrides: Vec::new(),
                    resume: None,
                    credential: None,
                    worktree: true,
                    prompt: vec!["hi".to_owned()],
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Ask(AskArgs {
                    model: None,
                    overrides: Vec::new(),
                    resume: Some("s_abc".to_owned()),
                    credential: Some("home".to_owned()),
                    worktree: false,
                    prompt: vec!["hi".to_owned()],
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: None,
                    resume: false,
                    worktree: true,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: Some("fake/m".to_owned()),
                    overrides: Vec::new(),
                    prompt: Some("hi".to_owned()),
                    resume: false,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: None,
                    resume: false,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: None,
                    resume: true,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
                "--prompt=- also check Y",
                "--parent",
                "s_aaaaaaaaaaaaaaaa",
                "--delegate-id",
                "j_0123456789abcdef",
            ],
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: Some("- also check Y".to_owned()),
                    resume: true,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
                    delegate_id: Some("j_0123456789abcdef".to_owned()),
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: Some("fake/m".to_owned()),
                    overrides: Vec::new(),
                    prompt: Some("hi".to_owned()),
                    resume: false,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
                    delegate_id: Some("j_bbbbbbbbbbbbbbbb".to_owned()),
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: None,
                    resume: false,
                    worktree: false,
                    rewound_from: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: false,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &[
                "fiber",
                "session",
                "--id",
                "s_0123456789abcdef",
                "--workspace",
                "/home/u/proj",
                "--login-shell",
            ],
            Invocation::Run(
                Some(Commands::Session(SessionArgs {
                    id: "s_0123456789abcdef".to_owned(),
                    workspace: PathBuf::from("/home/u/proj"),
                    model: None,
                    overrides: Vec::new(),
                    prompt: None,
                    resume: false,
                    worktree: false,
                    rewound_from: None,
                    continues: None,
                    forked_from: None,
                    forked_seq: None,
                    parent: None,
                    delegate_id: None,
                    depth: None,
                    login_shell: true,
                    credential: None,
                    supervised: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "config", "get", "model"],
            Invocation::Run(
                Some(Commands::Config(ConfigCommands::Get {
                    key: "model".to_owned(),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "config", "set", "model", "a/b"],
            Invocation::Run(
                Some(Commands::Config(ConfigCommands::Set {
                    project: false,
                    repo: false,
                    key: "model".to_owned(),
                    value: "a/b".to_owned(),
                })),
                TopLevel::default(),
            ),
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
            Invocation::Run(
                Some(Commands::Config(ConfigCommands::Set {
                    project: true,
                    repo: false,
                    key: "handoff.tokens".to_owned(),
                    value: "200000".to_owned(),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "config", "set", "--repo", "model", "a/b"],
            Invocation::Run(
                Some(Commands::Config(ConfigCommands::Set {
                    project: false,
                    repo: true,
                    key: "model".to_owned(),
                    value: "a/b".to_owned(),
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "import", "claude-code"],
            Invocation::Run(
                Some(Commands::Import {
                    source: ImportSource::ClaudeCode,
                }),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "import", "codex"],
            Invocation::Run(
                Some(Commands::Import {
                    source: ImportSource::Codex,
                }),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "serve"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Serve { installed: false })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "serve", "--installed"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Serve { installed: true })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "install"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Install { port: None })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "install", "--port", "4040"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Install { port: Some(4040) })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "install", "--port", "1"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Install { port: Some(1) })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "install", "--port", "65535"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Install { port: Some(65535) })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "uninstall"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Uninstall)),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "status"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Status { json: false })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "status", "--json"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Status { json: true })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "refresh"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Refresh { hub: None })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "pair", "phone"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Pair {
                    device: "phone".to_owned(),
                    address: None,
                    hub: None,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &[
                "fiber",
                "hub",
                "pair",
                "phone",
                "--address",
                "wss://mybox.tailnet.ts.net",
            ],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Pair {
                    device: "phone".to_owned(),
                    address: Some(::cli::Address::parse("wss://mybox.tailnet.ts.net").unwrap()),
                    hub: None,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "token", "list"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Token(TokenCommands::List {
                    hub: None,
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "hub", "token", "revoke", "phone"],
            Invocation::Run(
                Some(Commands::Hub(HubCommands::Token(TokenCommands::Revoke {
                    hub: None,
                    device: "phone".to_owned(),
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "setup"],
            Invocation::Run(
                Some(Commands::Setup(SetupArgs {
                    provider: Vec::new(),
                    model: None,
                    thinking: None,
                    import: Vec::new(),
                    completions: None,
                    no_completions: false,
                    hub: false,
                    no_hub: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &[
                "fiber",
                "setup",
                "--provider",
                "openrouter",
                "--provider",
                "codex",
                "--model",
                "fake/m",
                "--thinking",
                "high",
                "--import",
                "claude-code",
                "--completions",
                "zsh",
                "--hub",
            ],
            Invocation::Run(
                Some(Commands::Setup(SetupArgs {
                    provider: vec!["openrouter".to_owned(), "codex".to_owned()],
                    model: Some("fake/m".to_owned()),
                    thinking: Some("high".to_owned()),
                    import: vec![ImportSource::ClaudeCode],
                    completions: Some(Shell::Zsh),
                    no_completions: false,
                    hub: true,
                    no_hub: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "setup", "--import", "codex", "--no-completions"],
            Invocation::Run(
                Some(Commands::Setup(SetupArgs {
                    provider: Vec::new(),
                    model: None,
                    thinking: None,
                    import: vec![ImportSource::Codex],
                    completions: None,
                    no_completions: true,
                    hub: false,
                    no_hub: false,
                })),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "setup", "--no-hub"],
            Invocation::Run(
                Some(Commands::Setup(SetupArgs {
                    provider: Vec::new(),
                    model: None,
                    thinking: None,
                    import: Vec::new(),
                    completions: None,
                    no_completions: false,
                    hub: false,
                    no_hub: true,
                })),
                TopLevel::default(),
            ),
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
        matches!(parsed, Invocation::Run(Some(Commands::ExtensionCase { ref case }), _) if case.as_os_str() == OsStr::new("tests/first.json")),
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
        matches!(parsed, Invocation::Run(Some(Commands::Version), _)),
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
fn doctor_takes_no_arguments() {
    let error = sentence(&["fiber", "doctor", "extra"]);
    assert!(error.starts_with("Unexpected argument 'extra'"), "{error}");
    assert!(error.ends_with("Run `fiber --help` for usage."), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
    assert!(
        MENU.contains(
            "doctor                                    Says whether a session can start, and how to fix it when it cannot"
        ),
        "{MENU}"
    );
    assert!(MENU.contains("feedback [<prompt>] [-]"), "{MENU}");
}

#[test]
fn update_takes_an_optional_channel_and_upgrade_runs_the_same_command() {
    let Invocation::Run(
        Some(Commands::Update {
            channel,
            test_base_url,
        }),
        _,
    ) = parse_from(["fiber", "update"])
    else {
        panic!("bare update did not parse");
    };
    assert_eq!(channel, None);
    assert_eq!(test_base_url.base_url, None);
    for (args, expected) in [
        (
            vec!["fiber", "update", "--channel", "stable"],
            Some(ChannelArg::Stable),
        ),
        (
            vec!["fiber", "update", "--channel", "nightly"],
            Some(ChannelArg::Nightly),
        ),
        (vec!["fiber", "upgrade"], None),
        (
            vec!["fiber", "upgrade", "--channel", "nightly"],
            Some(ChannelArg::Nightly),
        ),
    ] {
        let Invocation::Run(
            Some(Commands::Update {
                channel,
                test_base_url,
            }),
            _,
        ) = parse_from(args.clone())
        else {
            panic!("{args:?} did not parse");
        };
        assert_eq!(channel, expected, "{args:?}");
        assert_eq!(test_base_url.base_url, None, "{args:?}");
    }
    let Invocation::Run(
        Some(Commands::Update {
            channel,
            test_base_url,
        }),
        _,
    ) = parse_from(["fiber", "update", "--base-url", "http://127.0.0.1:1"])
    else {
        panic!("update with --base-url did not parse");
    };
    assert_eq!(channel, None);
    assert_eq!(
        test_base_url.base_url.as_deref(),
        Some("http://127.0.0.1:1")
    );
}

#[test]
fn update_and_release_install_share_one_hidden_base_url() {
    let cmd = command();
    for name in ["update", "release-install"] {
        let sub = cmd
            .get_subcommands()
            .find(|sub| sub.get_name() == name)
            .unwrap_or_else(|| panic!("{name} is a subcommand"));
        let arg = sub
            .get_arguments()
            .find(|arg| arg.get_long() == Some("base-url"))
            .unwrap_or_else(|| panic!("{name} takes --base-url"));
        assert!(arg.is_hide_set(), "{name} hides --base-url");
    }
}

#[test]
fn update_rejects_a_positional_argument_and_an_unknown_channel() {
    let error = sentence(&["fiber", "update", "extra"]);
    assert!(error.starts_with("Unexpected argument 'extra'"), "{error}");
    assert!(error.ends_with("Run `fiber --help` for usage."), "{error}");
    let error = sentence(&["fiber", "update", "--channel", "bogus"]);
    assert!(
        error.starts_with("Invalid value 'bogus' for '--channel"),
        "{error}"
    );
    assert!(
        error.contains("[possible values: stable, nightly]"),
        "{error}"
    );
    assert!(error.ends_with("Run `fiber --help` for usage."), "{error}");
}

#[test]
fn the_menu_names_update_and_its_alias_and_channel() {
    let line = MENU
        .lines()
        .find(|line| line.starts_with("  update"))
        .unwrap();
    assert!(line.contains("`upgrade`"), "{line}");
    assert!(line.contains("`--channel"), "{line}");
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
fn setup_flags_conflict_in_pairs() {
    for args in [
        &["fiber", "setup", "--completions", "zsh", "--no-completions"][..],
        &["fiber", "setup", "--hub", "--no-hub"],
    ] {
        let said = sentence(args);
        assert!(said.contains("cannot be used with"), "{said}");
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn setup_help_names_every_flag() {
    let help = super::render_help(&["setup"]).unwrap();
    for flag in [
        "--provider",
        "--model",
        "--thinking",
        "--import",
        "--completions",
        "--no-completions",
        "--hub",
        "--no-hub",
    ] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    assert!(help.contains("Usage: fiber setup"), "{help}");
}

#[test]
fn the_menu_names_setup_with_its_synopsis() {
    let itself = menu()
        .split("\n\n")
        .find(|group| group.starts_with("Fiber itself:"))
        .unwrap()
        .to_owned();
    let row = itself
        .lines()
        .find(|line| line.starts_with("  setup "))
        .unwrap()
        .to_owned();
    // The synopsis is the text before the description's gap of two or
    // more spaces, past the two-space indent.
    assert_eq!(
        row[2..].split("  ").next().unwrap_or(""),
        "setup [--provider <name>]... [--model <model>] [--thinking <level>] [--import <claude-code | codex>]... [--completions <shell> | --no-completions] [--hub | --no-hub]",
        "{row}"
    );
}

#[test]
fn the_menu_names_stop_with_its_synopsis() {
    let itself = menu()
        .split("\n\n")
        .find(|group| group.starts_with("Fiber itself:"))
        .unwrap()
        .to_owned();
    let row = itself
        .lines()
        .find(|line| line.starts_with("  stop "))
        .unwrap()
        .to_owned();
    // The synopsis is the text before the description's gap of two or
    // more spaces, past the two-space indent.
    assert_eq!(
        row[2..].split("  ").next().unwrap_or(""),
        "stop [--hub <name>]",
        "{row}"
    );
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
fn bare_fiber_takes_model_and_repeatable_dash_c_as_the_terminal_layer() {
    for (argv, model, overrides) in [
        (vec!["fiber"], None, Vec::new()),
        (
            vec!["fiber", "--model", "fake/m"],
            Some("fake/m"),
            Vec::new(),
        ),
        (
            vec!["fiber", "-c", "retry.attempts=2"],
            None,
            vec!["retry.attempts=2".to_owned()],
        ),
        (
            vec!["fiber", "--model", "fake/m", "-c", "a=1", "-c", "b=2"],
            Some("fake/m"),
            vec!["a=1".to_owned(), "b=2".to_owned()],
        ),
        (
            vec!["fiber", "-c", "a=1", "--model", "fake/m"],
            Some("fake/m"),
            vec!["a=1".to_owned()],
        ),
    ] {
        let Invocation::Run(None, top) = parse_from(argv.clone()) else {
            panic!("bare fiber with its flags: {argv:?}");
        };
        assert_eq!(top.model.as_deref(), model, "{argv:?}");
        assert_eq!(top.overrides, overrides, "{argv:?}");
    }
}

#[test]
fn top_level_flags_with_a_subcommand_are_a_usage_sentence() {
    // `ask` keeps its own `--model` and `-c`; the top-level pair with
    // any subcommand is a usage error, as before the flags existed.
    for args in [
        &["fiber", "--model", "fake/m", "sessions"][..],
        &["fiber", "-c", "retry.attempts=2", "sessions"][..],
        &["fiber", "--model", "fake/m", "ask", "hi"][..],
        &["fiber", "-c", "retry.attempts=2", "ask", "hi"][..],
        &["fiber", "--model", "fake/m", "resume", "s_12"][..],
        &["fiber", "-c", "a=1", "--model", "fake/m", "continue"][..],
    ] {
        let (ask, said) = usage(args);
        assert!(!ask, "{args:?}");
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn a_dash_c_without_an_equals_is_a_usage_sentence() {
    for (args, ask) in [
        (&["fiber", "-c", "retry.attempts"][..], false),
        (&["fiber", "ask", "-c", "retry.attempts", "hi"][..], true),
    ] {
        let used = usage(args);
        assert_eq!(used.0, ask, "{args:?}");
        assert!(
            used.1.ends_with("Run `fiber --help` for usage."),
            "{}",
            used.1
        );
        assert_eq!(used.1.lines().count(), 1, "{}", used.1);
    }
}

#[test]
fn resume_takes_an_optional_id() {
    let Invocation::Run(
        Some(Commands::Resume {
            id,
            credential,
            hub,
        }),
        _,
    ) = parse_from(["fiber", "resume"])
    else {
        panic!("bare resume");
    };
    assert_eq!(id, None);
    assert_eq!(credential, None);
    assert_eq!(hub, None);
    let Invocation::Run(
        Some(Commands::Resume {
            id,
            credential,
            hub,
        }),
        _,
    ) = parse_from(["fiber", "resume", "s_12"])
    else {
        panic!("resume with an id");
    };
    assert_eq!(id.as_deref(), Some("s_12"));
    assert_eq!(credential, None);
    assert_eq!(hub, None);
    let help = super::render_help(&["resume"]).unwrap();
    assert!(
        help.contains("Open a session in the terminal, or home at the session list"),
        "{help}"
    );
    assert!(help.contains("--credential <label>"), "{help}");
}

#[test]
fn resume_credential_rides_with_an_id_in_either_order() {
    for argv in [
        vec!["fiber", "resume", "s_12", "--credential", "work"],
        vec!["fiber", "resume", "--credential", "work", "s_12"],
        vec!["fiber", "resume", "s_12", "--credential=work"],
    ] {
        let Invocation::Run(
            Some(Commands::Resume {
                id,
                credential,
                hub,
            }),
            _,
        ) = parse_from(argv)
        else {
            panic!("resume with a credential");
        };
        assert_eq!(id.as_deref(), Some("s_12"));
        assert_eq!(credential.as_deref(), Some("work"));
        assert_eq!(hub, None);
    }
}

#[test]
fn resume_credential_without_an_id_is_a_usage_error() {
    let message = sentence(&["fiber", "resume", "--credential", "work"]);
    assert!(
        message.contains("<id>"),
        "the error names the missing id: {message}"
    );
}

#[test]
fn resume_help_names_the_credential_flag() {
    let help = super::render_help(&["resume"]).unwrap();
    assert!(help.contains("--credential <label>"), "{help}");
}

#[test]
fn hub_names_the_listed_hub_on_bare_fiber_and_resume() {
    let Invocation::Run(None, top) = parse_from(["fiber", "--hub", "box"]) else {
        panic!("bare fiber with --hub");
    };
    assert_eq!(top.hub.as_deref(), Some("box"));
    assert_eq!(top.model, None);
    assert!(top.overrides.is_empty());
    let Invocation::Run(None, top) =
        parse_from(["fiber", "--hub=box", "-c", "a=1", "--model", "m"])
    else {
        panic!("bare fiber with --hub, -c and --model");
    };
    assert_eq!(top.hub.as_deref(), Some("box"));
    assert_eq!(top.model.as_deref(), Some("m"));
    assert_eq!(top.overrides, ["a=1"]);
    let Invocation::Run(None, top) = parse_from(["fiber"]) else {
        panic!("bare fiber");
    };
    assert_eq!(top.hub, None);
    for (argv, id) in [
        (
            vec!["fiber", "resume", "--hub", "box", "s_12"],
            Some("s_12"),
        ),
        (vec!["fiber", "resume", "--hub", "box"], None),
        (
            vec![
                "fiber",
                "resume",
                "s_12",
                "--credential",
                "work",
                "--hub",
                "box",
            ],
            Some("s_12"),
        ),
    ] {
        let Invocation::Run(
            Some(Commands::Resume {
                id: actual,
                credential,
                hub,
            }),
            _,
        ) = parse_from(argv.clone())
        else {
            panic!("resume with --hub: {argv:?}");
        };
        assert_eq!(actual.as_deref(), id, "{argv:?}");
        assert_eq!(hub.as_deref(), Some("box"), "{argv:?}");
        if id.is_some() && argv.contains(&"--credential") {
            assert_eq!(credential.as_deref(), Some("work"), "{argv:?}");
        } else {
            assert_eq!(credential, None, "{argv:?}");
        }
    }
    let help = super::render_help(&["resume"]).unwrap();
    assert!(help.contains("--hub <name>"), "{help}");
}

#[test]
fn stop_takes_an_optional_listed_hub() {
    for (argv, hub) in [
        (vec!["fiber", "stop"], None),
        (vec!["fiber", "stop", "--hub", "box"], Some("box")),
    ] {
        let Invocation::Run(Some(Commands::Stop { hub: actual }), _) = parse_from(argv.clone())
        else {
            panic!("stop with --hub: {argv:?}");
        };
        assert_eq!(actual.as_deref(), hub, "{argv:?}");
    }
    let help = super::render_help(&["stop"]).unwrap();
    assert!(help.contains("--hub <name>"), "{help}");
}

#[test]
fn hub_stop_stays_a_usage_error() {
    let said = sentence(&["fiber", "hub", "stop"]);
    assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");
}

#[test]
fn hub_with_a_subcommand_or_without_an_id_is_a_usage_sentence() {
    for args in [
        &["fiber", "--hub", "box", "sessions"][..],
        &["fiber", "--hub", "box", "resume", "s_12"],
        &["fiber", "resume", "--hub", "box", "--credential", "work"],
    ] {
        let said = sentence(args);
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn continue_takes_no_arguments() {
    let Invocation::Run(Some(Commands::Continue), _) = parse_from(["fiber", "continue"]) else {
        panic!("bare continue");
    };
    let help = super::render_help(&["continue"]).unwrap();
    assert!(
        help.contains("Open the most recent session in this project"),
        "{help}"
    );
}

#[test]
fn continue_rejects_the_credential_flag() {
    let message = sentence(&["fiber", "continue", "--credential", "work"]);
    assert!(
        message.contains("--credential"),
        "the error names `--credential`: {message}"
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
    let Invocation::Run(Some(Commands::Models(models)), _) = parse_from(["fiber", "models"]) else {
        panic!("bare models");
    };
    assert_eq!(models.search, None);
    assert!(!models.json);
    let Invocation::Run(Some(Commands::Models(models)), _) =
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
        "'fiber extension' requires a subcommand but one was not provided [subcommands: install, update, remove, reinstall, list, test]. Run `fiber --help` for usage."
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
        let Invocation::Run(Some(Commands::Help { command }), _) = parsed else {
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
    let Invocation::Run(Some(Commands::Ask(args)), _) = parsed else {
        panic!("{parsed:?}");
    };
    assert_eq!(args.model, None);
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), true)
    );

    let Invocation::Run(Some(Commands::Ask(args)), _) = parse_from(["fiber", "ask", "-"]) else {
        panic!("dash");
    };
    assert_eq!(super::ask_parts(&args.prompt).unwrap(), (None, true));

    let Invocation::Run(Some(Commands::Ask(args)), _) = parse_from(["fiber", "ask", "hi"]) else {
        panic!("prompt");
    };
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), false)
    );

    assert!(matches!(parse_from(["fiber"]), Invocation::Run(None, _)));

    let shape = "`fiber ask` takes one prompt, then an optional `-`; quote the prompt. Run `fiber --help` for usage.";
    for args in [
        &["fiber", "ask", "a", "b"][..],
        &["fiber", "ask", "-", "a"],
        &["fiber", "ask", "-", "-"],
    ] {
        let Invocation::Run(Some(Commands::Ask(ask)), _) = parse_from(args.iter().copied()) else {
            panic!("{args:?}");
        };
        assert_eq!(
            super::ask_parts(&ask.prompt).unwrap_err(),
            shape,
            "{args:?}"
        );
    }
}

/// `--session` takes one exact session id, before or after the
/// description, and its absence leaves the choice to the project.
#[test]
fn feedback_takes_an_optional_exact_session_id() {
    for args in [
        &["fiber", "feedback", "--session", "s_0123456789abcdef", "hi"][..],
        &["fiber", "feedback", "hi", "--session", "s_0123456789abcdef"],
    ] {
        let Invocation::Run(Some(Commands::Feedback(feedback)), _) =
            parse_from(args.iter().copied())
        else {
            panic!("{args:?}");
        };
        assert_eq!(feedback.session.as_deref(), Some("s_0123456789abcdef"));
        assert_eq!(
            super::ask_parts(&feedback.prompt).unwrap(),
            (Some("hi".to_owned()), false)
        );
    }
    let Invocation::Run(Some(Commands::Feedback(feedback)), _) =
        parse_from(["fiber", "feedback", "hi"])
    else {
        panic!("no session");
    };
    assert_eq!(feedback.session, None);
}

/// A session id that is not `s_` and 16 lowercase hex digits is a usage
/// error before anything is read.
#[test]
fn feedback_rejects_a_malformed_session_id() {
    for id in [
        "s_abc",
        "s_0123456789ABCDEF",
        "0123456789abcdef",
        "s_0123456789abcdef0",
        "../x",
    ] {
        let error = sentence(&["fiber", "feedback", "--session", id, "hi"]);
        assert!(
            error.contains("a session id is `s_` followed by 16 lowercase hex digits"),
            "{id}: {error}"
        );
    }
    let error = sentence(&["fiber", "feedback", "--session"]);
    assert!(error.contains("--session"), "{error}");
}

/// `feedback` resolves its description through the same `ask_parts`
/// seam as `ask`: one description, then an optional `-`.
#[test]
fn feedback_takes_one_description_then_an_optional_dash() {
    let parsed = parse_from(["fiber", "feedback", "hi", "-"]);
    let Invocation::Run(Some(Commands::Feedback(args)), _) = parsed else {
        panic!("{parsed:?}");
    };
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), true)
    );

    let Invocation::Run(Some(Commands::Feedback(args)), _) = parse_from(["fiber", "feedback", "-"])
    else {
        panic!("dash");
    };
    assert_eq!(super::ask_parts(&args.prompt).unwrap(), (None, true));

    let Invocation::Run(Some(Commands::Feedback(args)), _) =
        parse_from(["fiber", "feedback", "hi"])
    else {
        panic!("description");
    };
    assert_eq!(
        super::ask_parts(&args.prompt).unwrap(),
        (Some("hi".to_owned()), false)
    );

    for args in [
        &["fiber", "feedback", "a", "b"][..],
        &["fiber", "feedback", "-", "a"],
        &["fiber", "feedback", "-", "-"],
    ] {
        let Invocation::Run(Some(Commands::Feedback(feedback)), _) =
            parse_from(args.iter().copied())
        else {
            panic!("{args:?}");
        };
        assert_eq!(
            super::ask_parts(&feedback.prompt).unwrap_err(),
            super::ASK_SHAPE,
            "{args:?}"
        );
    }

    assert_eq!(
        super::FEEDBACK_SHAPE,
        "`fiber feedback` takes one description, then an optional `-`; quote the description. Run `fiber --help` for usage."
    );
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
        menu().contains("[--resume <id> [--credential <label>]]"),
        "the menu nests --credential under --resume:\n{}",
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
            "update",
            "ask",
            "resume",
            "continue",
            "sessions",
            "models",
            "extension",
            "mcp",
            "approve",
            "config",
            "import",
            "login",
            "setup",
            "logout",
            "doctor",
            "stop",
            "feedback",
            "completion",
            "version",
            "help",
            "hub"
        ]
    );
    let Invocation::Run(Some(Commands::Grep { args }), _) =
        parse_from(["fiber", "grep", "needle", "a.txt"])
    else {
        panic!("grep with a pattern and a path");
    };
    assert_eq!(args, [OsString::from("needle"), OsString::from("a.txt")]);
    // Clap handles no help or version flag for them: all is operands.
    let Invocation::Run(Some(Commands::Grep { args }), _) =
        parse_from(["fiber", "grep", "--help", "-n"])
    else {
        panic!("grep with flags");
    };
    assert_eq!(args, [OsString::from("--help"), OsString::from("-n")]);
    let Invocation::Run(Some(Commands::Grep { args }), _) = parse_from(["fiber", "grep"]) else {
        panic!("bare grep");
    };
    assert!(args.is_empty());
    let Invocation::Run(Some(Commands::Grep { args }), _) =
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
    let Invocation::Run(Some(Commands::Find { args }), _) =
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
            "update",
            "ask",
            "resume",
            "continue",
            "sessions",
            "models",
            "extension",
            "mcp",
            "approve",
            "config",
            "import",
            "login",
            "setup",
            "logout",
            "doctor",
            "stop",
            "feedback",
            "completion",
            "version",
            "help",
            "hub"
        ]
    );
    let Invocation::Run(Some(Commands::Image { args }), _) =
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
    let Invocation::Run(Some(Commands::Image { args }), _) =
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
    let Invocation::Run(Some(Commands::Find { args }), _) =
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
    assert!(said.contains("--parent"), "{said}");
}

#[test]
fn session_resume_with_a_prompt_and_a_parent_parses_as_a_delegate_resume() {
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--resume",
        "--prompt",
        "hi",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
    ]) else {
        panic!("a delegate resume parses");
    };
    assert!(args.resume);
    assert_eq!(args.prompt.as_deref(), Some("hi"));
    assert_eq!(args.parent.as_deref(), Some("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(args.delegate_id.as_deref(), Some("j_bbbbbbbbbbbbbbbb"));
}

#[test]
fn session_resume_with_a_credential_parses_the_label() {
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--resume",
        "--credential",
        "work",
    ]) else {
        panic!("a resume with a credential parses");
    };
    assert!(args.resume);
    assert_eq!(args.credential.as_deref(), Some("work"));
}

#[test]
fn session_resume_with_an_equals_credential_parses_the_label() {
    // The hub spawns the resumed session with `--credential=<label>` as
    // one argument, so an empty or leading-hyphen label is one value.
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--resume",
        "--credential=--resume",
    ]) else {
        panic!("a resume with an equals credential parses");
    };
    assert!(args.resume);
    assert_eq!(args.credential.as_deref(), Some("--resume"));
}

#[test]
fn session_credential_without_resume_is_a_usage_error() {
    let message = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--credential",
        "work",
    ]);
    assert!(
        message.contains("--resume"),
        "the error names `--resume`: {message}"
    );
}

#[test]
fn session_credential_with_a_rewind_is_a_usage_error() {
    let message = sentence(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--rewound-from",
        "s_aaaaaaaaaaaaaaaa",
        "--credential",
        "work",
    ]);
    assert!(
        message.contains("--resume"),
        "the error names `--resume`: {message}"
    );
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
fn session_depth_is_internal_unsigned_and_requires_a_parent() {
    let base = [
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
    ];
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from(base) else {
        panic!("session parses");
    };
    assert_eq!(args.depth, None);
    for depth in ["0", "1", "2", "3"] {
        let mut argv = base.to_vec();
        argv.extend(["--depth", depth]);
        let Invocation::Run(Some(Commands::Session(args)), _) = parse_from(argv) else {
            panic!("depth parses");
        };
        assert_eq!(args.depth, Some(depth.parse().unwrap()));
    }
    for depth in ["x", "-1"] {
        let mut argv = base.to_vec();
        argv.extend(["--depth", depth]);
        assert!(!sentence(&argv).is_empty());
    }
    let mut argv = base[..8].to_vec();
    argv.extend(["--depth", "2"]);
    assert!(sentence(&argv).contains("--parent"));
}

#[test]
fn session_parent_conflicts_with_resume_and_worktree() {
    // `--resume` with a prompt and a parent is a delegate's resume: it
    // parses, and the resume runs the prompt as one turn.
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
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
    ]) else {
        panic!("a delegate resume with --resume parses");
    };
    assert!(args.resume);
    assert_eq!(args.parent.as_deref(), Some("s_aaaaaaaaaaaaaaaa"));
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
fn import_rejects_anything_but_the_two_sources() {
    let refused = sentence(&["fiber", "import", "vim"]);
    assert!(
        refused.contains("claude-code") && refused.contains("codex"),
        "{refused}"
    );
    let missing = sentence(&["fiber", "import"]);
    assert!(
        missing.starts_with("The following required arguments were not provided: <source>"),
        "{missing}"
    );
}

#[test]
fn pairing_usage_errors_are_one_sentence() {
    let suffix = " Run `fiber --help` for usage.";
    for (argv, expected) in [
        (
            &["fiber", "hub", "pair"][..],
            "The following required arguments were not provided: <device>.",
        ),
        (
            &["fiber", "hub", "pair", "phone", "--address", "http://x"],
            "Invalid value 'http://x' for '--address <url>': The address must be a ws:// or wss:// URL.",
        ),
        (
            &["fiber", "hub", "pair", "phone", "--address"],
            "A value is required for '--address <url>' but none was supplied.",
        ),
        (
            &["fiber", "hub", "token"],
            "'fiber hub token' requires a subcommand but one was not provided [subcommands: list, revoke].",
        ),
        (
            &["fiber", "hub", "token", "revoke"],
            "The following required arguments were not provided: <device>.",
        ),
    ] {
        assert_eq!(sentence(argv), format!("{expected}{suffix}"), "{argv:?}");
    }
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
            Invocation::Run(Some(Commands::Help { ref command }), _) if command == &["hub", "install"]
        ),
        "{parsed:?}"
    );
}

#[test]
fn the_hidden_release_install_takes_a_version_and_a_base_url() {
    let Invocation::Run(
        Some(Commands::ReleaseInstall {
            version,
            test_base_url,
        }),
        _,
    ) = parse_from(["fiber", "release-install", "0.3.0"])
    else {
        panic!("release-install with a version");
    };
    assert_eq!(version, "0.3.0");
    assert_eq!(test_base_url.base_url, None);
    let Invocation::Run(
        Some(Commands::ReleaseInstall {
            version,
            test_base_url,
        }),
        _,
    ) = parse_from([
        "fiber",
        "release-install",
        "0.3.0",
        "--base-url",
        "file:///tmp/r",
    ])
    else {
        panic!("release-install with a base URL");
    };
    assert_eq!(version, "0.3.0");
    assert_eq!(test_base_url.base_url.as_deref(), Some("file:///tmp/r"));
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
    let Invocation::Run(Some(Commands::RefreshModelLists { providers }), _) =
        parse_from(["fiber", "refresh-model-lists", "openai", "anthropic"])
    else {
        panic!("refresh-model-lists with two providers");
    };
    assert_eq!(providers, ["openai", "anthropic"]);
    let Invocation::Run(Some(Commands::RefreshModelLists { providers }), _) =
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
fn session_forked_from_conflicts_with_a_fresh_start() {
    for extra in [
        vec!["--resume"],
        vec!["--model", "fake/m"],
        vec!["-c", "retry.attempts=2"],
        vec!["--worktree"],
        vec!["--rewound-from", "s_bbbbbbbbbbbbbbbb"],
    ] {
        let mut argv = vec![
            "fiber",
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--forked-from",
            "s_aaaaaaaaaaaaaaaa",
            "--forked-seq",
            "41",
            "--prompt",
            "hi",
            "--parent",
            "s_aaaaaaaaaaaaaaaa",
            "--delegate-id",
            "j_bbbbbbbbbbbbbbbb",
        ];
        argv.extend(extra);
        let (_, said) = usage(&argv);
        assert!(
            said.contains("--forked-from") || said.contains("--forked-seq"),
            "{said}"
        );
    }
    // The conflict reads both ways: `--rewound-from` with `--forked-seq`
    // fails whichever flag the parser meets first.
    let (_, said) = usage(&[
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--rewound-from",
        "s_aaaaaaaaaaaaaaaa",
        "--forked-seq",
        "41",
    ]);
    assert!(
        said.contains("--rewound-from") || said.contains("--forked-seq"),
        "{said}"
    );
}

#[test]
fn session_forked_flags_require_each_other_parent_delegate_id_and_prompt() {
    // One missing requirement per case, so a dropped `requires` on any
    // one of them fails exactly its case.
    let full = [
        ("--forked-from", "s_aaaaaaaaaaaaaaaa"),
        ("--forked-seq", "41"),
        ("--prompt", "hi"),
        ("--parent", "s_aaaaaaaaaaaaaaaa"),
        ("--delegate-id", "j_bbbbbbbbbbbbbbbb"),
    ];
    for skip in [
        "--forked-from",
        "--forked-seq",
        "--prompt",
        "--parent",
        "--delegate-id",
    ] {
        let mut argv = vec![
            "fiber",
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
        ];
        for (flag, value) in &full {
            if *flag != skip {
                argv.push(flag);
                argv.push(value);
            }
        }
        let (_, said) = usage(&argv);
        assert!(said.contains(skip), "missing {skip}: {said}");
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

#[test]
fn the_menu_fiber_and_ask_rows_show_c() {
    let menu = menu();
    for name in ["fiber", "ask"] {
        let row = menu
            .lines()
            .find(|l| l.starts_with(&format!("  {name} ")))
            .unwrap_or_else(|| panic!("menu has a `{name}` row"));
        assert!(row.contains("[-c <key>=<value>]..."), "menu row: {row}");
    }
}

/// `fiber mcp add|remove|list` parses into its invocation: each row is the
/// argv and the `Invocation` it parses to. A shape the parser cannot say —
/// both a url and a command, `-e` with a url — parses, and fails at run
/// time through the binary tests.
#[test]
fn mcp_parses_into_its_invocation() {
    let cases: Vec<(&[&str], Invocation)> = vec![
        (
            &["fiber", "mcp", "add", "docs", "https://example.test/mcp"],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Add(McpAddArgs {
                    project: false,
                    repo: false,
                    name: "docs".to_owned(),
                    url: Some("https://example.test/mcp".to_owned()),
                    env: Vec::new(),
                    secret: Vec::new(),
                    command: Vec::new(),
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &[
                "fiber",
                "mcp",
                "add",
                "--repo",
                "docs",
                "https://example.test/mcp",
            ],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Add(McpAddArgs {
                    project: false,
                    repo: true,
                    name: "docs".to_owned(),
                    url: Some("https://example.test/mcp".to_owned()),
                    env: Vec::new(),
                    secret: Vec::new(),
                    command: Vec::new(),
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &[
                "fiber",
                "mcp",
                "add",
                "--project",
                "x",
                "-e",
                "A=1",
                "--secret",
                "TOKEN",
                "--",
                "npx",
                "-y",
                "pkg",
            ],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Add(McpAddArgs {
                    project: true,
                    repo: false,
                    name: "x".to_owned(),
                    url: None,
                    env: vec!["A=1".to_owned()],
                    secret: vec!["TOKEN".to_owned()],
                    command: vec!["npx".to_owned(), "-y".to_owned(), "pkg".to_owned()],
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "mcp", "remove", "docs"],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Remove(McpRemoveArgs {
                    project: false,
                    repo: false,
                    name: "docs".to_owned(),
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "mcp", "remove", "--repo", "docs"],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Remove(McpRemoveArgs {
                    project: false,
                    repo: true,
                    name: "docs".to_owned(),
                }))),
                TopLevel::default(),
            ),
        ),
        (
            &["fiber", "mcp", "list"],
            Invocation::Run(Some(Commands::Mcp(McpCommands::List)), TopLevel::default()),
        ),
        // Both a url and a command parses; running it is the usage error
        // the binary tests pin.
        (
            &[
                "fiber",
                "mcp",
                "add",
                "x",
                "https://example.test/mcp",
                "--",
                "npx",
            ],
            Invocation::Run(
                Some(Commands::Mcp(McpCommands::Add(McpAddArgs {
                    project: false,
                    repo: false,
                    name: "x".to_owned(),
                    url: Some("https://example.test/mcp".to_owned()),
                    env: Vec::new(),
                    secret: Vec::new(),
                    command: vec!["npx".to_owned()],
                }))),
                TopLevel::default(),
            ),
        ),
    ];
    for (argv, expected) in &cases {
        let parsed = parse_from(argv.iter().copied());
        // Compared field by field, never through `McpAddArgs`'s redacted
        // `Debug`: its `Debug` prints names and the layer only, so a
        // `Debug` comparison would not prove values parsed.
        match (parsed, expected) {
            (
                Invocation::Run(Some(Commands::Mcp(actual)), _),
                Invocation::Run(Some(Commands::Mcp(wanted)), _),
            ) => match (actual, wanted) {
                (McpCommands::Add(actual), McpCommands::Add(wanted)) => {
                    assert_eq!(actual.project, wanted.project, "{argv:?}");
                    assert_eq!(actual.repo, wanted.repo, "{argv:?}");
                    assert_eq!(actual.name, wanted.name, "{argv:?}");
                    assert_eq!(actual.url, wanted.url, "{argv:?}");
                    assert_eq!(actual.env, wanted.env, "{argv:?}");
                    assert_eq!(actual.secret, wanted.secret, "{argv:?}");
                    assert_eq!(actual.command, wanted.command, "{argv:?}");
                }
                (McpCommands::Remove(actual), McpCommands::Remove(wanted)) => {
                    assert_eq!(actual.project, wanted.project, "{argv:?}");
                    assert_eq!(actual.repo, wanted.repo, "{argv:?}");
                    assert_eq!(actual.name, wanted.name, "{argv:?}");
                }
                (McpCommands::List, McpCommands::List) => {}
                _ => panic!("mismatched MCP variant for {argv:?}"),
            },
            _ => panic!("not an MCP invocation for {argv:?}"),
        }
    }
}

#[test]
fn mcp_add_args_debug_prints_names_never_values() {
    let args = McpAddArgs {
        project: false,
        repo: true,
        name: "docs".to_owned(),
        url: Some("https://user:pass@example.test/mcp?token=zzz".to_owned()),
        env: vec!["TOKEN=zzz".to_owned()],
        secret: vec!["KEY".to_owned()],
        command: vec!["npx".to_owned(), "--token=zzz".to_owned()],
    };
    let printed = format!("{args:?}");
    assert!(printed.contains("docs"), "{printed}");
    assert!(printed.contains("repository"), "{printed}");
    assert!(printed.contains("TOKEN"), "{printed}");
    assert!(printed.contains("KEY"), "{printed}");
    assert!(!printed.contains("zzz"), "{printed}");
    assert!(!printed.contains("user"), "{printed}");
    assert!(!printed.contains("pass"), "{printed}");
    assert!(!printed.contains("npx"), "{printed}");
}

#[test]
fn mcp_rejects_a_missing_name_and_conflicting_scopes() {
    let missing = sentence(&["fiber", "mcp", "add"]);
    assert!(
        missing.contains("<name>"),
        "the error names the missing name: {missing}"
    );
    assert!(
        missing.ends_with("Run `fiber --help` for usage."),
        "{missing}"
    );
    assert_eq!(missing.lines().count(), 1, "{missing}");
    for args in [
        &["fiber", "mcp", "add", "--project", "--repo", "x", "u"][..],
        &["fiber", "mcp", "remove", "--project", "--repo", "x"],
    ] {
        let said = sentence(args);
        assert!(said.contains("cannot be used with"), "{said}");
        assert!(said.ends_with("Run `fiber --help` for usage."), "{said}");
        assert_eq!(said.lines().count(), 1, "{said}");
    }
}

#[test]
fn the_menu_names_the_mcp_commands() {
    let group = menu()
        .split("\n\n")
        .find(|group| group.starts_with("MCP servers:"))
        .unwrap()
        .to_owned();
    for line in [
        "  mcp add [--project | --repo] <name> <url>",
        "  mcp add [--project | --repo] <name> [-e KEY=value]... [--secret KEY]... -- <command> [args]...",
        "  mcp remove [--project | --repo] <name>",
        "  mcp list",
        "  mcp login <server>",
        "  mcp logout <server>",
    ] {
        assert!(
            group.lines().any(|l| l.starts_with(line)),
            "{line}\n{group}"
        );
    }
}

/// `fiber mcp login|logout` parses into its invocation: each row is the
/// argv and the server it parses to. An empty or missing server is a usage
/// error, as for every command taking a name.
#[test]
fn mcp_login_parses_into_its_invocation() {
    let cases: Vec<(&[&str], &str, bool)> = vec![
        (&["fiber", "mcp", "login", "docs"], "docs", true),
        (&["fiber", "mcp", "logout", "docs"], "docs", false),
    ];
    for (argv, server, login) in &cases {
        match parse_from(argv.iter().copied()) {
            Invocation::Run(Some(Commands::Mcp(McpCommands::Login { server: name })), _)
                if *login =>
            {
                assert_eq!(name, *server, "{argv:?}");
            }
            Invocation::Run(Some(Commands::Mcp(McpCommands::Logout { server: name })), _)
                if !login =>
            {
                assert_eq!(name, *server, "{argv:?}");
            }
            Invocation::Run(_, _) | Invocation::Print(_) | Invocation::Usage { .. } => {
                panic!("not an MCP login invocation for {argv:?}")
            }
        }
    }
    for argv in [
        &["fiber", "mcp", "login"][..],
        &["fiber", "mcp", "logout"][..],
    ] {
        assert!(
            matches!(
                parse_from(argv.iter().copied()),
                Invocation::Usage { ask: false, .. }
            ),
            "{argv:?} names no server"
        );
    }
}

/// The command reference the help menu must match word for word
/// (`docs/invocation.md`, "Commands and flags").
const INVOCATION: &str = include_str!("../../../docs/invocation.md");

/// One Markdown table row, split at `|` outside backticks: an escaped
/// `\|` inside a synopsis stays in its cell.
fn split_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        if ch == '`' {
            quoted = !quoted;
            current.push(ch);
        } else if ch == '|' && !quoted {
            cells.push(current);
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    cells.push(current);
    cells
}

/// The `Command`-column synopsis of every table in `section`, backticks
/// stripped and `\|` unescaped. Only tables headed `Command` count: any
/// other table is a different contract.
fn synopses_in(section: &str) -> Vec<String> {
    let mut synopses = Vec::new();
    let mut in_table = false;
    for line in section.lines() {
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        let cells = split_cells(line);
        let first = cells.get(1).map(|cell| cell.trim()).unwrap_or("");
        if first == "Command" {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if !first.starts_with('`') {
            continue;
        }
        synopses.push(first.trim_matches('`').replace("\\|", "|"));
    }
    synopses
}

/// The doc's "Commands and flags" section: later sections hold driver
/// and hub commands, which are not `fiber` synopses.
fn commands_section() -> &'static str {
    let after = INVOCATION.split("## Commands and flags\n").nth(1).unwrap();
    after.split("\n## ").next().unwrap()
}

/// The `Command`-column synopsis of every built command's row in the
/// doc's "Commands and flags" section.
fn doc_synopses() -> Vec<String> {
    synopses_in(commands_section())
}

/// The words of a synopsis before its first argument: `fiber` names the
/// terminal itself, so its row leaves none.
fn command_path(synopsis: &str) -> Vec<&str> {
    let mut words: Vec<&str> = synopsis
        .split_whitespace()
        .take_while(|word| {
            !word.starts_with('[') && !word.starts_with('<') && !word.starts_with('-')
        })
        .collect();
    if words.first() == Some(&"fiber") {
        words.remove(0);
    }
    words
}

/// Whether the parser builds the command the path names: every word
/// names a visible subcommand of the last. An empty path is the
/// terminal, which is always built.
fn is_built(path: &[&str]) -> bool {
    let mut cmd = command();
    cmd.build();
    let mut sub = &mut cmd;
    for word in path {
        let Some(next) = sub.find_subcommand_mut(word) else {
            return false;
        };
        if next.is_hide_set() {
            return false;
        }
        sub = next;
    }
    true
}

/// The synopsis each indented menu line shows: the text before the gap
/// of two or more spaces that starts its description.
fn menu_synopses(menu: &str) -> Vec<&str> {
    menu.lines()
        .filter(|line| line.starts_with("  "))
        .map(|line| line[2..].split("  ").next().unwrap_or(""))
        .collect()
}

#[test]
fn split_cells_keeps_pipes_inside_backticks_in_their_cell() {
    let cells = split_cells("| `logout <provider> [--as <label> \\| --all]` | Deletes. |");
    assert_eq!(
        cells.get(1).map(|cell| cell.trim()),
        Some("`logout <provider> [--as <label> \\| --all]`")
    );
}

#[test]
fn synopses_come_from_command_tables_only() {
    let section = [
        "| Command | What it does |",
        "|---|---|",
        "| `ask [<prompt>]` | Runs one turn. |",
        "",
        "| Key | Type |",
        "|---|---|",
        "| `id` | string |",
        "",
        "A `quoted` word in prose is not a row.",
    ]
    .join("\n");
    assert_eq!(synopses_in(&section), ["ask [<prompt>]"]);
}

#[test]
fn doc_synopses_are_the_commands_section() {
    let synopses = doc_synopses();
    assert_eq!(synopses.len(), 46);
    assert_eq!(synopses[0], "fiber [--model <model>] [-c <key>=<value>]...");
    assert_eq!(
        synopses[1],
        "ask [--model <model>] [-c <key>=<value>]... [--resume <id> [--credential <label>]] [--worktree] [<prompt>] [-]"
    );
    assert!(
        synopses.contains(&"logout <provider> [--as <label> | --all]".to_owned()),
        "an escaped pipe reads as one: {synopses:?}"
    );
    assert!(
        synopses
            .contains(&"mcp add [--project | --repo] <name> [-e KEY=value]... [--secret KEY]... -- <command> [args]...".to_owned()),
        "{synopses:?}"
    );
    // The driver commands live outside the section: they are not synopses.
    assert!(
        !synopses.iter().any(|synopsis| synopsis == "subscribe"),
        "{synopses:?}"
    );
}

#[test]
fn command_path_takes_the_words_before_the_first_argument() {
    assert_eq!(command_path("continue"), ["continue"]);
    assert_eq!(
        command_path("sessions search [--all] <text>"),
        ["sessions", "search"]
    );
    assert_eq!(
        command_path("fiber [--model <model>] [-c <key>=<value>]..."),
        Vec::<&str>::new()
    );
    assert_eq!(
        command_path("mcp add [--project | --repo] <name> <url>"),
        ["mcp", "add"]
    );
    assert_eq!(
        command_path("extension install [--project] <name or path>"),
        ["extension", "install"]
    );
}

#[test]
fn is_built_follows_the_parser() {
    assert!(is_built(&["ask"]));
    assert!(is_built(&["hub", "token", "list"]));
    assert!(is_built(&["hub", "add"]));
    assert!(is_built(&[]));
    assert!(is_built(&["update"]));
    assert!(is_built(&["doctor"]));
    assert!(!is_built(&["mcp", "serve"]));
    assert!(is_built(&["hub", "remove"]));
}

#[test]
fn menu_synopses_take_the_text_before_the_description_gap() {
    let fields = menu_synopses(
        "Fiber, a coding agent.\n\nSessions:\n  continue   Open the most recent session\n  sessions [--all] [--json]   List sessions\n",
    );
    assert_eq!(fields, ["continue", "sessions [--all] [--json]"]);
    // A shorter doc synopsis is not a match: the menu field must equal it.
    assert!(!fields.contains(&"sessions [--all]"));
}

/// The `SessionArgs` a round-trip returns: `parse_from` of
/// `fiber` plus the argv, which must be the session command with equal
/// fields.
#[track_caller]
fn round_tripped(args: &SessionArgs) -> SessionArgs {
    let mut argv = vec![OsString::from("fiber")];
    argv.extend(contract::session_argv::SessionArgv::from(args).to_argv());
    let Invocation::Run(Some(Commands::Session(got)), _) = parse_from(argv) else {
        panic!("to_argv does not parse back to a session: {args:?}");
    };
    got
}

/// Every field of two sessions is equal: the round-trip keeps all 17.
#[track_caller]
fn assert_same_session(got: &SessionArgs, want: &SessionArgs) {
    assert_eq!(got.id, want.id, "id");
    assert_eq!(got.workspace, want.workspace, "workspace");
    assert_eq!(got.model, want.model, "model");
    assert_eq!(got.overrides, want.overrides, "overrides");
    assert_eq!(got.prompt, want.prompt, "prompt");
    assert_eq!(got.resume, want.resume, "resume");
    assert_eq!(got.credential, want.credential, "credential");
    assert_eq!(got.worktree, want.worktree, "worktree");
    assert_eq!(got.rewound_from, want.rewound_from, "rewound_from");
    assert_eq!(got.continues, want.continues, "continues");
    assert_eq!(got.forked_from, want.forked_from, "forked_from");
    assert_eq!(got.forked_seq, want.forked_seq, "forked_seq");
    assert_eq!(got.parent, want.parent, "parent");
    assert_eq!(got.delegate_id, want.delegate_id, "delegate_id");
    assert_eq!(got.depth, want.depth, "depth");
    assert_eq!(got.login_shell, want.login_shell, "login_shell");
    assert_eq!(got.supervised, want.supervised, "supervised");
}

fn minimal_session() -> SessionArgs {
    SessionArgs {
        id: "s_0123456789abcdef".to_owned(),
        workspace: PathBuf::from("/w"),
        model: None,
        overrides: Vec::new(),
        prompt: None,
        resume: false,
        credential: None,
        worktree: false,
        rewound_from: None,
        continues: None,
        forked_from: None,
        forked_seq: None,
        parent: None,
        delegate_id: None,
        depth: None,
        login_shell: false,
        supervised: false,
    }
}

#[test]
fn session_argv_round_trips_a_minimal_session() {
    let args = minimal_session();
    assert_eq!(
        contract::session_argv::SessionArgv::from(&args).to_argv(),
        ["session", "--id=s_0123456789abcdef", "--workspace=/w",]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_new_launch_with_overrides() {
    let args = SessionArgs {
        id: "s_0123456789abcdef".to_owned(),
        workspace: PathBuf::from("/w with space"),
        model: Some("-weird/model".to_owned()),
        overrides: vec!["a=1".to_owned(), "-x=1".to_owned()],
        prompt: Some("-go".to_owned()),
        resume: false,
        credential: None,
        worktree: false,
        rewound_from: None,
        continues: None,
        forked_from: None,
        forked_seq: None,
        parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        delegate_id: Some("j_bbbbbbbbbbbbbbbb".to_owned()),
        depth: Some(2),
        login_shell: false,
        supervised: false,
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    // A plain override travels as two arguments; one starting with `-`
    // travels attached, so clap never reads it as a flag.
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        rendered.contains(&"--model=-weird/model".to_owned()),
        "{rendered:?}"
    );
    assert!(
        rendered.contains(&"--prompt=-go".to_owned()),
        "{rendered:?}"
    );
    let at = rendered
        .iter()
        .position(|arg| arg == "-c")
        .expect("-c pair");
    assert_eq!(rendered[at + 1], "a=1");
    assert!(rendered.contains(&"-c-x=1".to_owned()), "{rendered:?}");
    assert!(!rendered.iter().any(|arg| arg == "-x=1"), "{rendered:?}");
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_resume_with_a_credential() {
    let args = SessionArgs {
        resume: true,
        credential: Some("-label".to_owned()),
        prompt: Some("- also check Y".to_owned()),
        parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        delegate_id: Some("j_bbbbbbbbbbbbbbbb".to_owned()),
        depth: Some(1),
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--resume".to_owned()), "{rendered:?}");
    assert!(
        rendered.contains(&"--credential=-label".to_owned()),
        "{rendered:?}"
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_continues_parses_and_converts() {
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--continues",
        "s_aaaaaaaaaaaaaaaa",
    ]) else {
        panic!("--continues parses");
    };
    assert_eq!(args.continues.as_deref(), Some("s_aaaaaaaaaaaaaaaa"));
    let argv = contract::session_argv::SessionArgv::from(&args);
    assert_eq!(
        argv.continues,
        Some(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()))
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_supervised_parses_and_round_trips() {
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--supervised",
    ]) else {
        panic!("--supervised parses");
    };
    assert!(args.supervised);
    assert!(!contract::session_argv::SessionArgv::from(&minimal_session()).supervised);
    let rendered: Vec<String> = contract::session_argv::SessionArgv::from(&args)
        .to_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--supervised".to_owned()));
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_supervised_conflicts_with_resume_and_delegate_flags() {
    for extra in [
        vec!["--resume"],
        vec!["--rewound-from", "s_bbbbbbbbbbbbbbbb"],
        vec![
            "--prompt",
            "hi",
            "--parent",
            "s_aaaaaaaaaaaaaaaa",
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
            "--supervised",
        ];
        argv.extend(extra);
        let (_, said) = usage(&argv);
        assert!(said.contains("--supervised"), "{said}");
    }
    // A `continues` start is a new session: it may stay supervised.
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--supervised",
        "--continues",
        "s_aaaaaaaaaaaaaaaa",
    ]) else {
        panic!("--supervised with --continues parses");
    };
    assert!(args.supervised);
    assert_eq!(args.continues.as_deref(), Some("s_aaaaaaaaaaaaaaaa"));
}

#[test]
fn session_continues_conflicts_with_the_other_continuations() {
    for extra in [
        vec!["--resume"],
        vec!["--rewound-from", "s_bbbbbbbbbbbbbbbb"],
        vec![
            "--prompt",
            "hi",
            "--parent",
            "s_bbbbbbbbbbbbbbbb",
            "--delegate-id",
            "j_bbbbbbbbbbbbbbbb",
        ],
        vec![
            "--forked-from",
            "s_bbbbbbbbbbbbbbbb",
            "--forked-seq",
            "41",
            "--prompt",
            "hi",
            "--parent",
            "s_bbbbbbbbbbbbbbbb",
            "--delegate-id",
            "j_bbbbbbbbbbbbbbbb",
        ],
        vec![
            "--forked-seq",
            "41",
            "--forked-from",
            "s_bbbbbbbbbbbbbbbb",
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
            "--continues",
            "s_aaaaaaaaaaaaaaaa",
        ];
        argv.extend(extra);
        let (_, said) = usage(&argv);
        assert!(said.contains("--continues"), "{said}");
    }
    // The conflict reads both ways: `--forked-from` with `--continues`
    // fails whichever flag the parser meets first.
    let mut argv = vec![
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--forked-from",
        "s_aaaaaaaaaaaaaaaa",
        "--forked-seq",
        "41",
        "--prompt",
        "hi",
        "--parent",
        "s_aaaaaaaaaaaaaaaa",
        "--delegate-id",
        "j_bbbbbbbbbbbbbbbb",
    ];
    argv.extend(["--continues", "s_bbbbbbbbbbbbbbbb"]);
    let (_, said) = usage(&argv);
    assert!(
        said.contains("--forked-from") || said.contains("--continues"),
        "{said}"
    );
}

#[test]
fn session_continues_with_worktree_parses_for_the_hub_to_refuse() {
    let Invocation::Run(Some(Commands::Session(args)), _) = parse_from([
        "fiber",
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        "/w",
        "--continues",
        "s_aaaaaaaaaaaaaaaa",
        "--worktree",
    ]) else {
        panic!("--continues with --worktree parses");
    };
    assert_eq!(args.continues.as_deref(), Some("s_aaaaaaaaaaaaaaaa"));
    assert!(args.worktree);
}

#[test]
fn session_argv_round_trips_a_continues() {
    let args = SessionArgs {
        continues: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        rendered.contains(&"--continues=s_aaaaaaaaaaaaaaaa".to_owned()),
        "{rendered:?}"
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_rewind() {
    let args = SessionArgs {
        rewound_from: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        continues: None,
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        rendered.contains(&"--rewound-from=s_aaaaaaaaaaaaaaaa".to_owned()),
        "{rendered:?}"
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_fork() {
    let args = SessionArgs {
        forked_from: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        forked_seq: Some(41),
        prompt: Some("-go".to_owned()),
        parent: Some("s_aaaaaaaaaaaaaaaa".to_owned()),
        delegate_id: Some("j_bbbbbbbbbbbbbbbb".to_owned()),
        depth: Some(1),
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        rendered.contains(&"--forked-from=s_aaaaaaaaaaaaaaaa".to_owned()),
        "{rendered:?}"
    );
    assert!(
        rendered.contains(&"--forked-seq=41".to_owned()),
        "{rendered:?}"
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_worktree_and_a_login_shell() {
    let args = SessionArgs {
        model: Some("fake/m".to_owned()),
        worktree: true,
        login_shell: true,
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--worktree".to_owned()), "{rendered:?}");
    assert_eq!(rendered.last().map(String::as_str), Some("--login-shell"));
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_workspace_named_like_a_flag() {
    let args = SessionArgs {
        workspace: PathBuf::from("-w"),
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // One argument, so a workspace starting with `-` never reads as a
    // flag.
    assert!(
        rendered.contains(&"--workspace=-w".to_owned()),
        "{rendered:?}"
    );
    assert_same_session(&round_tripped(&args), &args);
}

#[test]
fn session_argv_round_trips_a_workspace_that_is_not_utf8() {
    let args = SessionArgs {
        // Built from bytes, never created on disk: only the argv and
        // its parse ever see these bytes.
        workspace: PathBuf::from(OsString::from_vec(b"/w/\xff".to_vec())),
        ..minimal_session()
    };
    let argv = contract::session_argv::SessionArgv::from(&args).to_argv();
    // The workspace travels byte for byte: a lossy render would fail
    // the round-trip below.
    assert_eq!(argv[2], OsString::from_vec(b"--workspace=/w/\xff".to_vec()),);
    assert_same_session(&round_tripped(&args), &args);
}

/// The Command column of `docs/invocation.md` is the synopsis, so the
/// help menu holds each built command's row word for word. Rows the
/// parser does not build name future commands, not drift.
#[test]
fn every_built_command_synopsis_in_the_doc_matches_the_menu() {
    let rendered = menu();
    let fields = menu_synopses(&rendered);
    let mut missing = Vec::new();
    let mut unbuilt = Vec::new();
    for synopsis in doc_synopses() {
        if !is_built(&command_path(&synopsis)) {
            unbuilt.push(command_path(&synopsis).join(" "));
            continue;
        }
        if !fields.iter().any(|field| *field == synopsis) {
            missing.push(synopsis);
        }
    }
    assert!(
        missing.is_empty(),
        "these doc synopses are not in the menu: {missing:?}"
    );
    unbuilt.sort();
    assert_eq!(unbuilt, ["mcp serve"]);
}
