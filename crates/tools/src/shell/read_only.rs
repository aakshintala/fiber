//! The shell's built-in read-only list (`docs/tools.md`, "Shell", "Effects").
//! `uniq` is not listed: its second operand is an output file.

/// A flag that keeps its command read-only.
pub(super) struct Flag {
    /// The flag as an exact word, such as `-n` or `--stat`.
    pub(super) spelling: &'static str,
    /// Whether the next word, or the text after `=`, is this flag's value.
    pub(super) takes_value: bool,
}

/// One command on the built-in list.
pub(super) struct Command {
    /// `ls`, or `git status` when the command is a git subcommand.
    pub(super) name: &'static str,
    /// Flags that keep it read-only. Any other flag does not.
    pub(super) flags: &'static [Flag],
    /// Whether an operand is a path. `echo` and `pwd` name none.
    pub(super) paths: bool,
    /// Whether the first operand is a search pattern, not a path.
    pub(super) pattern: bool,
    /// Whether `--` ends flags. `find` still reads a primary after `--`.
    pub(super) ends_flags: bool,
}

const fn flag(spelling: &'static str) -> Flag {
    Flag {
        spelling,
        takes_value: false,
    }
}

const fn value(spelling: &'static str) -> Flag {
    Flag {
        spelling,
        takes_value: true,
    }
}

const HEAD_TAIL: &[Flag] = &[flag("-q"), value("-n"), value("-c")];

const GREP: &[Flag] = &[
    flag("-n"),
    flag("-r"),
    flag("-R"),
    flag("-i"),
    flag("-v"),
    flag("-E"),
    flag("-F"),
    flag("-l"),
    flag("-L"),
    flag("-c"),
    flag("-w"),
    flag("-o"),
    flag("-H"),
    flag("-h"),
    flag("-s"),
    flag("-q"),
    flag("-I"),
    value("-A"),
    value("-B"),
    value("-C"),
    value("-m"),
    value("-e"),
    value("--include"),
    value("--exclude"),
    value("--exclude-dir"),
];

const RG: &[Flag] = &[
    flag("-n"),
    flag("-i"),
    flag("-l"),
    flag("-c"),
    flag("-w"),
    flag("-v"),
    flag("-F"),
    flag("-o"),
    flag("-S"),
    flag("-s"),
    flag("-H"),
    flag("-N"),
    flag("-q"),
    value("-A"),
    value("-B"),
    value("-C"),
    value("-m"),
    value("-e"),
    value("-g"),
    value("-t"),
    flag("--hidden"),
    flag("--no-heading"),
    flag("--files"),
    flag("--count"),
    flag("--files-with-matches"),
    flag("--line-number"),
    flag("--ignore-case"),
    flag("--fixed-strings"),
    flag("--word-regexp"),
    flag("--invert-match"),
    flag("--only-matching"),
    value("--glob"),
    value("--type"),
    value("--max-count"),
];

const FIND: &[Flag] = &[
    value("-name"),
    value("-iname"),
    value("-path"),
    value("-type"),
    value("-maxdepth"),
    value("-mindepth"),
    value("-newer"),
    flag("-print"),
];

const SORT: &[Flag] = &[
    flag("-n"),
    flag("-r"),
    flag("-u"),
    flag("-f"),
    flag("-b"),
    flag("-h"),
    flag("-V"),
    value("-k"),
    value("-t"),
];

const GIT_STATUS: &[Flag] = &[
    flag("-s"),
    flag("-b"),
    flag("--short"),
    flag("--branch"),
    flag("--porcelain"),
];

// debt: git diff and git show can run a configured external diff or textconv, drop both entries when the read-only list is no longer trusted
const GIT_DIFF: &[Flag] = &[
    flag("-p"),
    flag("--patch"),
    flag("--stat"),
    flag("--shortstat"),
    flag("--numstat"),
    flag("--name-only"),
    flag("--name-status"),
    flag("--cached"),
    flag("--staged"),
    flag("--no-color"),
    flag("--no-ext-diff"),
    flag("--exit-code"),
    flag("--quiet"),
];

const GIT_LOG: &[Flag] = &[
    flag("-p"),
    flag("--patch"),
    flag("--oneline"),
    flag("--stat"),
    flag("--name-only"),
    flag("--graph"),
    flag("--decorate"),
    flag("--no-color"),
    flag("--no-ext-diff"),
    value("-n"),
];

// debt: git diff and git show can run a configured external diff or textconv, drop both entries when the read-only list is no longer trusted
const GIT_SHOW: &[Flag] = &[
    flag("-p"),
    flag("--patch"),
    flag("--oneline"),
    flag("--stat"),
    flag("--name-only"),
    flag("--no-color"),
    flag("--no-ext-diff"),
];

/// Fiber's own read-only commands. A later change can pass a person's
/// `shell.read_only` entries in beside this slice.
pub(super) const COMMANDS: &[Command] = &[
    Command {
        name: "pwd",
        flags: &[],
        paths: false,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "echo",
        flags: &[flag("-n")],
        paths: false,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "ls",
        flags: &[
            flag("-l"),
            flag("-a"),
            flag("-h"),
            flag("-1"),
            flag("-R"),
            flag("-t"),
            flag("-r"),
            flag("-S"),
            flag("-d"),
            flag("-F"),
        ],
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "cat",
        flags: &[
            flag("-n"),
            flag("-b"),
            flag("-s"),
            flag("-A"),
            flag("-E"),
            flag("-T"),
            flag("-v"),
        ],
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "head",
        flags: HEAD_TAIL,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "tail",
        flags: HEAD_TAIL,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "wc",
        flags: &[flag("-l"), flag("-w"), flag("-c"), flag("-m")],
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "grep",
        flags: GREP,
        paths: true,
        pattern: true,
        ends_flags: true,
    },
    Command {
        name: "rg",
        flags: RG,
        paths: true,
        pattern: true,
        ends_flags: true,
    },
    Command {
        name: "find",
        flags: FIND,
        paths: true,
        pattern: false,
        ends_flags: false,
    },
    Command {
        name: "sort",
        flags: SORT,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "git status",
        flags: GIT_STATUS,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "git diff",
        flags: GIT_DIFF,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "git log",
        flags: GIT_LOG,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
    Command {
        name: "git show",
        flags: GIT_SHOW,
        paths: true,
        pattern: false,
        ends_flags: true,
    },
];
