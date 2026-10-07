use std::sync::LazyLock;

use clap::{Arg, ArgAction, Command, ValueHint};

use super::{PARSER, Shell, script, visible};

/// One argument as a generator reads it.
#[derive(Debug, PartialEq, Eq)]
struct ArgShape {
    id: String,
    long: Option<String>,
    short: Option<char>,
    aliases: Vec<String>,
    short_aliases: Vec<char>,
    help: Option<String>,
    takes_values: bool,
    index: Option<usize>,
}

/// One command path as a generator reads it.
#[derive(Debug, PartialEq, Eq)]
struct CommandShape {
    path: String,
    about: Option<String>,
    long_about: Option<String>,
    version: Option<String>,
    aliases: Vec<String>,
    args: Vec<ArgShape>,
    subcommands: Vec<String>,
}

fn arg_shape(arg: &Arg) -> ArgShape {
    ArgShape {
        id: arg.get_id().to_string(),
        long: arg.get_long().map(str::to_owned),
        short: arg.get_short(),
        aliases: arg
            .get_visible_aliases()
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        short_aliases: arg.get_visible_short_aliases().unwrap_or_default(),
        help: arg.get_help().map(ToString::to_string),
        takes_values: arg.get_action().takes_values(),
        index: arg.get_index(),
    }
}

/// Every visible path of a built command, with its visible arguments, in
/// the order clap holds them.
fn shapes(cmd: &Command, path: &str, out: &mut Vec<CommandShape>) {
    let subs: Vec<&Command> = cmd.get_subcommands().filter(|s| !s.is_hide_set()).collect();
    out.push(CommandShape {
        path: path.to_owned(),
        about: cmd.get_about().map(ToString::to_string),
        long_about: cmd.get_long_about().map(ToString::to_string),
        version: cmd.get_version().map(str::to_owned),
        aliases: cmd.get_visible_aliases().map(str::to_owned).collect(),
        args: cmd
            .get_arguments()
            .filter(|a| !a.is_hide_set())
            .map(arg_shape)
            .collect(),
        subcommands: subs.iter().map(|s| s.get_name().to_owned()).collect(),
    });
    for sub in subs {
        shapes(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

fn built_shapes(mut cmd: Command) -> Vec<CommandShape> {
    cmd.build();
    let mut out = Vec::new();
    shapes(&cmd, "fiber", &mut out);
    out
}

/// Every argument of every command, at every depth, with its command's name.
fn every_arg(cmd: &Command, out: &mut Vec<(String, Arg)>) {
    for arg in cmd.get_arguments() {
        out.push((cmd.get_name().to_owned(), arg.clone()));
    }
    for sub in cmd.get_subcommands() {
        every_arg(sub, out);
    }
}

#[test]
fn the_copy_has_the_parsers_visible_grammar_in_both_directions() {
    let parser = built_shapes(crate::cli::command());
    let copy = built_shapes(visible(&PARSER));
    assert_eq!(copy, parser);

    let paths: Vec<&str> = copy.iter().map(|shape| shape.path.as_str()).collect();
    assert!(paths.contains(&"fiber extension install"), "{paths:?}");
    assert!(paths.contains(&"fiber completion"), "{paths:?}");
    for shape in &copy {
        for hidden in [
            "grep",
            "find",
            "image",
            "session",
            "refresh-model-lists",
            "hub",
        ] {
            assert!(
                !shape.path.split(' ').any(|word| word == hidden),
                "{} names {hidden}",
                shape.path
            );
            assert!(
                !shape.subcommands.iter().any(|name| name == hidden),
                "{} offers {hidden}",
                shape.path
            );
        }
    }
    let root = copy.first().unwrap();
    let flags: Vec<(Option<char>, Option<&str>)> = root
        .args
        .iter()
        .map(|arg| (arg.short, arg.long.as_deref()))
        .collect();
    assert_eq!(
        flags,
        [(Some('v'), Some("version")), (Some('h'), Some("help"))]
    );
}

static HAND_BUILT: LazyLock<Command> = LazyLock::new(|| {
    Command::new("tool")
        .arg(Arg::new("shown").long("shown").action(ArgAction::SetTrue))
        .arg(
            Arg::new("secret")
                .long("secret")
                .action(ArgAction::SetTrue)
                .hide(true),
        )
        .subcommand(Command::new("open").arg(Arg::new("inner").long("inner").hide(true)))
        .subcommand(Command::new("internal").hide(true))
});

#[test]
fn hidden_arguments_and_subcommands_leave_the_copy_and_visible_ones_stay() {
    let mut copy = visible(&HAND_BUILT);
    copy.build();
    let longs: Vec<&str> = copy.get_arguments().filter_map(Arg::get_long).collect();
    assert_eq!(longs, ["shown", "help"]);
    let subs: Vec<&str> = copy.get_subcommands().map(Command::get_name).collect();
    assert_eq!(subs, ["open"]);
    let open = copy.find_subcommand("open").unwrap();
    let longs: Vec<&str> = open.get_arguments().filter_map(Arg::get_long).collect();
    assert_eq!(longs, ["help"]);
}

#[test]
fn value_arguments_offer_no_values_and_flags_keep_their_action() {
    let mut copy = visible(&PARSER);
    copy.build();
    let mut args = Vec::new();
    every_arg(&copy, &mut args);
    let (values, flags): (Vec<_>, Vec<_>) = args
        .iter()
        .partition(|(_, arg)| arg.get_action().takes_values());
    assert!(
        values
            .iter()
            .any(|(cmd, arg)| cmd == "ask" && arg.get_long() == Some("model")),
        "--model is missing"
    );
    assert!(
        values
            .iter()
            .any(|(cmd, arg)| cmd == "completion" && arg.get_id() == "shell"),
        "<shell> is missing"
    );
    for (cmd, arg) in values {
        assert_eq!(arg.get_value_hint(), ValueHint::Other, "{cmd} {arg}");
        assert!(arg.get_possible_values().is_empty(), "{cmd} {arg}");
    }
    assert!(
        flags
            .iter()
            .any(|(cmd, arg)| cmd == "models" && arg.get_long() == Some("json")),
        "--json is missing"
    );
    let mut parser = crate::cli::command();
    parser.build();
    let mut parser_args = Vec::new();
    every_arg(&parser, &mut parser_args);
    for (cmd, arg) in flags {
        assert_eq!(arg.get_value_hint(), ValueHint::Unknown, "{cmd} {arg}");
        let action = format!("{:?}", arg.get_action());
        assert!(
            parser_args.iter().any(|(name, original)| name == cmd
                && original.get_id() == arg.get_id()
                && format!("{:?}", original.get_action()) == action),
            "{cmd} {arg} changed its action to {action}"
        );
    }
}

#[test]
fn the_bash_script_completes_without_falling_back_to_file_names() {
    let text = script(Shell::Bash);
    assert!(text.starts_with("_fiber() {"), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.contains(&"    complete -F _fiber -o nosort fiber"),
        "{text}"
    );
    assert!(lines.contains(&"    complete -F _fiber fiber"), "{text}");
    assert!(!text.contains("default"), "{text}");
    assert!(!text.contains("compgen -f"), "{text}");
}

#[test]
fn the_zsh_script_completes_no_values_or_file_names() {
    let text = script(Shell::Zsh);
    assert!(text.starts_with("#compdef fiber\n"), "{text}");
    assert!(
        text.contains("'--model=[The model for this run, as a person types it]:model:' \\\n"),
        "{text}"
    );
    for absent in [
        "_default",
        "_files",
        "(bash zsh fish)",
        "grep",
        "refresh-model-lists",
    ] {
        assert!(!text.contains(absent), "{absent}\n{text}");
    }
}

#[test]
fn the_fish_script_offers_commands_and_ends_without_file_names() {
    let text = script(Shell::Fish);
    assert!(text.ends_with("\ncomplete -c fiber -f\n"), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.contains(
            &"complete -c fiber -n \"__fish_fiber_needs_command\" -f -a \"extension\" -d 'Manage extensions'"
        ),
        "{text}"
    );
    assert!(
        lines.contains(
            &"complete -c fiber -n \"__fish_fiber_using_subcommand extension; and not __fish_seen_subcommand_from install update remove list\" -f -a \"install\" -d 'Install an extension and its dependencies'"
        ),
        "{text}"
    );
    for absent in ["\"grep\"", "\"hub\"", "\"session\"", "bash zsh fish"] {
        assert!(!text.contains(absent), "{absent}\n{text}");
    }
}

#[test]
fn the_same_shell_gives_the_same_bytes() {
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        assert_eq!(script(shell), script(shell), "{shell:?}");
    }
}
