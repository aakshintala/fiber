use super::*;
use fakes::TempDir;

const BASH: &str = "#!/usr/bin/env bash\n";

/// `docs/ci.md` says which files count, not in what order: compare as sets
/// (a duplicate still fails, since both sides keep their duplicates).
fn assert_same_files(got: Result<Vec<String>, String>, mut want: Vec<String>) {
    let mut got = got.unwrap();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn shebang_lines_that_name_bash() {
    for line in [
        "#!/usr/bin/env bash",
        "#!/bin/bash",
        "#! /bin/bash",
        "#!\t/bin/bash",
        "#!/bin/bash -euo pipefail",
        "#!/usr/bin/env -S bash -eu",
        "#!/usr/bin/env -S LC_ALL=C bash",
        "#!/usr/bin/env FOO=1 bash",
        "#!/usr/bin/env -u NAME bash",
        "#!/usr/bin/env -C /tmp bash",
        "#!/usr/bin/env -uNAME bash",
        "#!/usr/bin/env -iu NAME bash",
        "#!/usr/bin/env -uC bash",
        "#!/usr/bin/env -iS bash",
        "#!/usr/bin/env --unset NAME bash",
        "#!/usr/bin/env --chdir DIR bash",
        "#!/usr/bin/env --unset=NAME bash",
        "#!/usr/bin/env --ignore-environment bash",
        "#!/usr/bin/env -S -u NAME bash",
        "#!/usr/bin/env bash\r",
    ] {
        assert!(is_bash_shebang(line), "{line:?} should be bash");
    }
}

#[test]
fn shebang_lines_that_do_not_name_bash() {
    for line in [
        "#!/usr/bin/env -S 'bash -e'",
        "#!/usr/bin/env -S bash\\ -e",
        "#!/usr/bin/env -u NAME sh",
        "#!/usr/bin/env -u bash",
        "#!/usr/bin/env -u",
        "#!/usr/bin/env -C",
        "#!/usr/bin/env --unset bash",
        "#!/usr/bin/env -S sh # bash",
        "#!/bin/sh",
        "#!/usr/bin/env sh",
        "#!/opt/bash/bin/sh",
        "#!/usr/bin/bash5",
        "#!/usr/bin/env bashful",
        "#!",
        "#!/usr/bin/env",
        "# bash",
        "{",
        "",
        "\u{feff}#!/usr/bin/env bash",
    ] {
        assert!(!is_bash_shebang(line), "{line:?} should not be bash");
    }
}

#[test]
fn walk_lists_bash_files_and_lib_scripts() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("b", "#!/bin/bash\n");
    dir.write("a", BASH);
    dir.write("sub/dir/n", BASH);
    dir.write("lib/x.sh", "# no shebang\n");
    dir.write("c", "#!/bin/sh\n");
    dir.write("d.json", "{\n");
    dir.write("e", "");
    dir.write("f", "# no shebang\n");
    dir.write("lib/y.txt", "# no shebang\n");
    dir.write("sub/lib/z.sh", "# no shebang\n");
    let root = dir.path().display();
    assert_same_files(
        files(dir.path()),
        vec![
            format!("{root}/a"),
            format!("{root}/b"),
            format!("{root}/lib/x.sh"),
            format!("{root}/sub/dir/n"),
        ],
    );
}

#[test]
fn a_lib_script_with_a_bash_shebang_is_listed_once() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("lib/w.sh", BASH);
    assert_same_files(
        files(dir.path()),
        vec![format!("{}/lib/w.sh", dir.path().display())],
    );
}

#[test]
fn a_first_line_that_is_not_utf8_is_not_bash() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("ok", BASH);
    std::fs::write(dir.path().join("bad"), b"#!/usr/bin/env bash\xff\n").unwrap();
    assert_same_files(
        files(dir.path()),
        vec![format!("{}/ok", dir.path().display())],
    );
}

#[test]
fn only_the_install_script_directly_under_the_directory_is_skipped() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("install.sh", BASH);
    dir.write("a", BASH);
    dir.write("sub/install.sh", BASH);
    let root = dir.path().display();
    assert_same_files(
        files(dir.path()),
        vec![format!("{root}/a"), format!("{root}/sub/install.sh")],
    );
}

#[test]
fn a_symlink_is_not_listed() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("a", BASH);
    std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("link")).unwrap();
    assert_same_files(
        files(dir.path()),
        vec![format!("{}/a", dir.path().display())],
    );
}

#[test]
fn a_missing_directory_and_a_regular_file_are_errors() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("a", BASH);
    assert!(files(&dir.path().join("missing")).is_err());
    assert!(files(&dir.path().join("a")).is_err());
}

#[test]
fn a_shebang_line_with_no_trailing_newline_is_bash() {
    let dir = TempDir::new("xtask-shellcheck-files");
    dir.write("a", "#!/bin/bash");
    assert_same_files(
        files(dir.path()),
        vec![format!("{}/a", dir.path().display())],
    );
}
