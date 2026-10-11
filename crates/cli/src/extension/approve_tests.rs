//! The extension install and remove prompts: what each shows, what it asks
//! and when it goes ahead, with the input and the output stream in memory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;

use super::*;

fn summary() -> InstallSummary {
    InstallSummary {
        name: "github.com/aakshintala/fiber/providers/opencode".into(),
        source: "/src/opencode".into(),
        version: "v1.2.0".into(),
        changes: None,
        replaces: Vec::new(),
        process: None,
        install_step: None,
        carries: Vec::new(),
        staged: PathBuf::from("/nonexistent-staged"),
        providers: vec![(
            "opencode".into(),
            vec![
                "https://opencode.ai/zen/go/v1".into(),
                "https://opencode.ai/zen/v1".into(),
            ],
        )],
    }
}

#[test]
fn an_install_in_a_terminal_shows_its_summary_and_goes_ahead_only_on_yes() {
    for (answer, approved) in [
        ("y\n", true),
        ("yes\n", true),
        ("n\n", false),
        ("\n", false),
        ("", false),
    ] {
        let mut out = Vec::new();
        let ok = install_approved(&[summary()], true, &mut answer.as_bytes(), &mut out).unwrap();
        assert_eq!(ok, approved, "answer {answer:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Install github.com/aakshintala/fiber/providers/opencode from /src/opencode\n\
             Version v1.2.0\n\
             Provider opencode: https://opencode.ai/zen/go/v1, https://opencode.ai/zen/v1\n\
             Go ahead? [y/N/s to show the full source] "
        );
    }
    let none = InstallSummary {
        providers: Vec::new(),
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[none], true, &mut "y\n".as_bytes(), &mut out).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("It registers no provider.\n")
    );
}

#[test]
fn a_summary_names_each_built_in_the_extension_replaces() {
    let replacing = InstallSummary {
        replaces: vec!["shell".into(), "read".into()],
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[replacing], true, &mut "n\n".as_bytes(), &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("Replaces `shell`\nReplaces `read`\n"),
        "{text}"
    );
    let mut out = Vec::new();
    install_approved(&[summary()], true, &mut "n\n".as_bytes(), &mut out).unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("Replaces"));
}

#[test]
fn an_install_without_a_terminal_goes_ahead_without_asking() {
    let mut out = Vec::new();
    assert!(install_approved(&[summary()], false, &mut "n\n".as_bytes(), &mut out).unwrap());
    assert!(out.is_empty());
}

#[test]
fn a_summary_of_several_extensions_asks_once_and_an_update_shows_its_changes() {
    let update = InstallSummary {
        changes: Some(" b.lua | 1 +\n".into()),
        ..summary()
    };
    let dep = InstallSummary {
        name: "github.com/acme/dep".into(),
        version: "v1.4.0".into(),
        providers: Vec::new(),
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[update, dep], true, &mut "y\n".as_bytes(), &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Update github.com/aakshintala/fiber/providers/opencode from /src/opencode\n\
         Version v1.2.0\n\
         Changes since the installed commit:\n b.lua | 1 +\n\
         Provider opencode: https://opencode.ai/zen/go/v1, https://opencode.ai/zen/v1\n\
         Install github.com/acme/dep from /src/opencode\n\
         Version v1.4.0\n\
         It registers no provider.\n\
         Go ahead? [y/N/s to show the full source] "
    );
}

#[test]
fn a_summary_shows_the_program_the_install_step_and_what_the_package_carries() {
    let full = InstallSummary {
        process: Some("node dist/main.js".into()),
        install_step: Some("npm ci".into()),
        carries: vec!["skills: plan, review".into(), "themes: dark.json".into()],
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[full], true, &mut "n\n".as_bytes(), &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    for line in [
        "Runs the program: node dist/main.js\n",
        "Install step, run now and at every update: npm ci\n",
        "Its dependencies' own install scripts run too.\n",
        "Carries skills: plan, review\n",
        "Carries themes: dark.json\n",
    ] {
        assert!(text.contains(line), "{line:?} in {text}");
    }
    let plain = String::from_utf8({
        let mut out = Vec::new();
        install_approved(&[summary()], true, &mut "n\n".as_bytes(), &mut out).unwrap();
        out
    })
    .unwrap();
    assert!(
        !plain.contains("Install step") && !plain.contains("Carries"),
        "{plain}"
    );
}

#[test]
fn the_s_key_shows_every_staged_file_and_asks_again() {
    let dir = fakes::TempDir::new("fiber-doors-source");
    fs::create_dir_all(dir.path().join("lib")).unwrap();
    fs::write(dir.path().join("extension.json"), "{}").unwrap();
    fs::write(dir.path().join("lib/a.lua"), "return 1\n").unwrap();
    fs::write(dir.path().join("blob"), [0xff, 0xfe, 0xfd]).unwrap();
    let shown = InstallSummary {
        staged: dir.path().to_path_buf(),
        ..summary()
    };
    let mut out = Vec::new();
    let ok = install_approved(&[shown], true, &mut "s\ny\n".as_bytes(), &mut out).unwrap();
    assert!(ok);
    let text = String::from_utf8(out).unwrap();
    let prompt = "Go ahead? [y/N/s to show the full source] ";
    assert_eq!(text.matches(prompt).count(), 2, "{text}");
    let shown_at = text
        .find("=== github.com/aakshintala/fiber/providers/opencode")
        .unwrap();
    let after = &text[shown_at..];
    assert!(after.contains("--- extension.json\n{}\n"), "{after}");
    assert!(after.contains("--- lib/a.lua\nreturn 1\n"), "{after}");
    assert!(after.contains("--- blob (3 bytes, not text)\n"), "{after}");
    assert!(after.find("--- blob").unwrap() < after.find("--- extension.json").unwrap());
    // `s` then no is still no, and nothing else asks again.
    let mut out = Vec::new();
    let shown = InstallSummary {
        staged: dir.path().to_path_buf(),
        ..summary()
    };
    assert!(!install_approved(&[shown], true, &mut "s\nn\n".as_bytes(), &mut out).unwrap());
}

#[test]
fn show_source_lists_nested_directories_in_path_order() {
    let dir = fakes::TempDir::new("fiber-approve-source");
    fs::create_dir_all(dir.path().join("a")).unwrap();
    fs::create_dir_all(dir.path().join("b/inner")).unwrap();
    fs::write(dir.path().join("a/z.txt"), "z\n").unwrap();
    fs::write(dir.path().join("b/a.txt"), "a\n").unwrap();
    fs::write(dir.path().join("b/inner/y.txt"), "y\n").unwrap();
    fs::write(dir.path().join("m.txt"), "m").unwrap();
    let shown = InstallSummary {
        staged: dir.path().to_path_buf(),
        ..summary()
    };
    let mut out = Vec::new();
    let ok = install_approved(&[shown], true, &mut "s\nn\n".as_bytes(), &mut out).unwrap();
    assert!(!ok);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Install github.com/aakshintala/fiber/providers/opencode from /src/opencode\n\
         Version v1.2.0\n\
         Provider opencode: https://opencode.ai/zen/go/v1, https://opencode.ai/zen/v1\n\
         Go ahead? [y/N/s to show the full source] \
         === github.com/aakshintala/fiber/providers/opencode\n\
         --- a/z.txt\n\
         z\n\
         --- b/a.txt\n\
         a\n\
         --- b/inner/y.txt\n\
         y\n\
         --- m.txt\n\
         m\n\
         Go ahead? [y/N/s to show the full source] "
    );
}

#[test]
fn a_remove_in_a_terminal_lists_what_it_deletes_and_goes_ahead_only_on_yes() {
    let names = vec![
        "github.com/acme/x".to_owned(),
        "github.com/acme/dep".to_owned(),
    ];
    let data = vec![PathBuf::from("/h/data/github.com-acme-x")];
    for (answer, approved) in [("y\n", true), ("yes\n", true), ("n\n", false), ("", false)] {
        let mut out = Vec::new();
        let ok = remove_approved(&names, &data, true, &mut answer.as_bytes(), &mut out).unwrap();
        assert_eq!(ok, approved, "{answer:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Remove github.com/acme/x\nRemove github.com/acme/dep\n\
             Delete /h/data/github.com-acme-x\nGo ahead? [y/N] "
        );
    }
    let mut out = Vec::new();
    assert!(remove_approved(&names, &data, false, &mut "n\n".as_bytes(), &mut out).unwrap());
    assert!(out.is_empty());
}
