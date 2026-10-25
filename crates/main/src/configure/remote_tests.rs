//! The hub's configuration operations use its Fiber home.
use std::fs;
use std::path::Path;

use contract::ErrorCode;
use hub::Machine;
use serde_json::{Value, json};

use super::*;
use crate::configure::tests::{Dirs, write};
use fakes::Deadline;

fn call(seam: &Seam, operation: &str, args: Value) -> Result<Option<Value>, Failure> {
    seam.configure(
        operation,
        args.as_object().unwrap(),
        fakes::clock::FakeClock::new(),
    )
}

fn result(seam: &Seam, operation: &str, args: Value) -> Value {
    call(seam, operation, args).unwrap().unwrap()
}

/// A fake `~/.claude` under `person`: one skill, one instruction, and
/// one credential the import must name but never read.
fn claude_person(person: &Path) {
    for (rel, content) in [
        (
            ".claude/skills/review/SKILL.md",
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        ),
        (".claude/CLAUDE.md", "Be kind.\n"),
        (".claude/auth.json", "{\"token\": \"x\"}\n"),
    ] {
        let path = person.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn import(seam: &Seam, person: &Path, source: Option<&str>) -> Result<Option<Value>, Failure> {
    seam.import(person, source, fakes::clock::FakeClock::new())
}

#[test]
fn seam_import_runs_the_shared_import_into_the_seam_home() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let person = dirs.workspace("person");
    claude_person(&person);
    let seam = Seam::new(home.clone());
    let result = import(&seam, &person, None).unwrap().unwrap();
    assert_eq!(result["imported"]["skills"], json!(["review"]));
    assert_eq!(result["imported"]["instructions"], json!(["CLAUDE.md"]));
    assert!(
        result["left_behind"]
            .as_array()
            .unwrap()
            .iter()
            .any(|left| left["item"] == "auth.json"),
        "{result}"
    );
    assert!(home.join("import-claude-code.json").is_file());
    assert!(home.join("skills/review/SKILL.md").is_file());
    // A named source imports that source only.
    let result = import(&seam, &person, Some("claude-code"))
        .unwrap()
        .unwrap();
    assert_eq!(result["imported"]["skills"], json!(["review"]));
    // An unknown source is refused naming the two sources.
    let error = import(&seam, &person, Some("vim")).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert!(error.message.contains("claude-code") && error.message.contains("codex"));
}

/// The pending operation answers from the seam's own home: on the
/// hub machine that home holds the receipts `import` writes, so a
/// remote terminal's offer follows the hub's state, never the
/// terminal's.
#[test]
fn import_pending_reads_the_seam_homes_receipts() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let seam = Seam::new(home.clone());
    assert_eq!(
        result(&seam, "import_pending", json!({})),
        json!({"pending": true})
    );
    write(&home.join("import-claude-code.json"), "{}");
    assert_eq!(
        result(&seam, "import_pending", json!({})),
        json!({"pending": false})
    );
}

fn install(home: &Path) {
    crate::test_support::install_extension(
        home,
        "extensions/acme",
        json!({"name":"acme","version":"v0.0.0","fiber":"0.0.0","api":1}),
        &[(
            "acme",
            json!({"name":"acme","reviewer_model":"m", "models":[{"id":"m","protocol":"openai-responses","base_url":"http://x/v1","context_window":1000}]}),
        )],
    );
}

#[test]
fn provider_install_rejects_invalid_argument_shapes_and_unknown_providers() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("provider-install");
    let seam = Seam::with_origin(
        home.clone(),
        std::sync::Arc::new(extensions::Origin::new("git", |_| {
            panic!("invalid request must not fetch")
        })),
    );
    for args in [
        json!({"workspace":workspace}),
        json!({"provider":"openai"}),
        json!({"workspace":workspace,"provider":"openai","extra":true}),
        json!({"workspace":"relative","provider":"openai"}),
        json!({"workspace":workspace,"provider":1}),
        json!({"workspace":workspace,"provider":"scripted"}),
    ] {
        let error = call(&seam, "install_provider", args).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArguments);
    }
    assert!(!home.join("extensions").exists());
}

#[test]
fn remote_operations_round_trip_their_wire_shapes() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(home.clone());
    install(&home);
    write(
        &home.join("config.json"),
        r#"{"thinking":"high","skills":{"disabled":["s"]},"mcp":{"servers":{"x":{"tools":{"disabled":["t"]}}}}}"#,
    );
    let settings = result(&seam, "settings", json!({"workspace":workspace}));
    let thinking = settings["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "thinking")
        .unwrap();
    assert_eq!(
        *thinking,
        json!({"key":"thinking","shown":"value","value":"\"high\"","layer":"global","file":home.join("config.json"),"scope":"any","repo":true})
    );
    assert_eq!(
        result(
            &seam,
            "set",
            json!({"workspace":workspace,"layer":"global","key":"thinking","text":"low"})
        ),
        json!({"file":home.join("config.json"),"warnings":[]})
    );
    assert!(
        fs::read_to_string(home.join("config.json"))
            .unwrap()
            .contains("low")
    );
    assert_eq!(
        result(&seam, "global_file", json!({})),
        json!({"file":home.join("config.json")})
    );
    let rules = result(&seam, "rules", json!({"workspace":workspace}));
    assert_eq!(
        rules["global"],
        json!({"file":home.join("rules"),"rows":[]})
    );
    assert_eq!(
        result(
            &seam,
            "revoke",
            json!({"workspace":workspace,"scope":"global","line":1,"text":"absent"})
        ),
        json!({"revoked":"stale"})
    );
    assert_eq!(
        result(&seam, "login_targets", json!({})),
        json!({"targets":[{"name":"acme","kind":"key"}]})
    );
    assert_eq!(
        result(&seam, "store_key", json!({"name":"acme","key":"sk-test"})),
        json!({"path":"credentials/acme/default","replaced":false})
    );
    assert_eq!(
        fs::read_to_string(home.join("credentials/acme/default")).unwrap(),
        "sk-test"
    );
    let error = call(&seam, "store_key", json!({"name":"acme","key":"sk-hidden"})).unwrap_err();
    assert!(!error.message.contains("sk-hidden"));
    assert_eq!(
        result(&seam, "reviewer_models", json!({"name":"acme"})),
        json!({"models":["m"],"selected":"m","notices":[]})
    );
    assert_eq!(
        call(&seam, "set_reviewer", json!({"name":"acme","id":"m"})).unwrap(),
        None
    );
    assert_eq!(
        result(&seam, "tool_switches", json!({"workspace":workspace})),
        json!({"groups":[{"group":{"mcp":"x"},"project":{"disabled":["t"]},"everywhere":{"disabled":["t"]}}]})
    );
    assert_eq!(call(&seam,"switch_tool",json!({"workspace":workspace,"group":{"mcp":"x"},"tool":"a","scope":"project","on":false})).unwrap(),None);
    assert_eq!(
        result(&seam, "tool_switches", json!({"workspace":workspace}))["groups"][0]["project"]["disabled"],
        json!(["t", "a"])
    );
    assert_eq!(
        result(&seam, "skills_disabled", json!({"workspace":workspace})),
        json!({"project":[],"everywhere":["s"]})
    );
    assert_eq!(
        call(
            &seam,
            "switch_skill",
            json!({"workspace":workspace,"name":"s","scope":"everywhere","on":true})
        )
        .unwrap(),
        None
    );
    assert_eq!(
        result(&seam, "skills_disabled", json!({"workspace":workspace}))["everywhere"],
        json!([])
    );
    let skill = workspace.join("SKILL.md");
    write(&skill, "skill body");
    assert_eq!(
        result(&seam, "skill_text", json!({"path":skill})),
        json!({"text":"skill body"})
    );
    let notes = result(&seam, "reviewer_notes", json!({"workspace":workspace}));
    assert_eq!(notes["global"]["file"], json!(home.join("config.json")));
    assert_eq!(notes["global"]["text"], "");
    assert_eq!(notes["project"]["text"], "");
    write(
        &home.join("config.json"),
        r#"{"reviewer":{"context":"Our org is acme."}}"#,
    );
    let notes = result(&seam, "reviewer_notes", json!({"workspace":workspace}));
    assert_eq!(notes["global"]["text"], "Our org is acme.");
    let global = notes["global"]["version"].as_str().unwrap().to_owned();
    let project = notes["project"]["version"].as_str().unwrap().to_owned();
    assert_ne!(global, "absent");
    let workspace_save = workspace.clone();
    let global_save = global.clone();
    let (seam, saved) = fakes::within(
        "round-trip global notes save",
        Deadline::after(std::time::Duration::from_secs(10)),
        move || {
            let saved = result(
                &seam,
                "save_reviewer_notes",
                json!({"workspace":workspace_save,"global":{"version":global_save,"text":"new global"}}),
            );
            (seam, saved)
        },
    );
    assert_eq!(
        saved,
        json!({"saved":"written","files":[{"file":home.join("config.json"),"saved":true}]})
    );
    assert!(
        fs::read_to_string(home.join("config.json"))
            .unwrap()
            .contains("new global")
    );
    let before = fs::read(home.join("config.json")).unwrap();
    let workspace_stale = workspace.clone();
    let global_stale = global.clone();
    let project_stale = project.clone();
    let stale = fakes::within(
        "round-trip stale notes save",
        Deadline::after(std::time::Duration::from_secs(10)),
        move || {
            result(
                &seam,
                "save_reviewer_notes",
                json!({"workspace":workspace_stale,"global":{"version":global_stale,"text":"lost"},"project":{"version":project_stale,"text":"new project"}}),
            )
        },
    );
    assert_eq!(stale, json!({"saved":"stale"}));
    assert_eq!(fs::read(home.join("config.json")).unwrap(), before);
}

#[test]
fn remote_client_keys_are_filtered_and_refused_without_writes() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(home.clone());
    let file = home.join("config.json");
    write(
        &file,
        r#"{"tui":{"theme":"dark","hover":true},"keys":{"app.quit":["ctrl+q"]}}"#,
    );
    let before = fs::read(&file).unwrap();
    let rows = result(&seam, "settings", json!({"workspace":workspace}));
    assert!(
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| !client_key(r["key"].as_str().unwrap()))
    );
    for key in [
        "tui.theme",
        "keys.\"app.quit\"",
        "\"tui\".theme",
        "keys",
        "tui",
    ] {
        assert_eq!(
            call(
                &seam,
                "set",
                json!({"workspace":workspace,"layer":"global","key":key,"text":"null"})
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidArguments
        );
        assert_eq!(fs::read(&file).unwrap(), before);
    }
    for key in ["keys.\"x\"", "tui.theme", "\"tui\".hover"] {
        assert!(client_key(key));
    }
    for key in ["tuix", "keysx", "model"] {
        assert!(!client_key(key));
    }
}

#[test]
fn remote_operations_reject_unknown_extra_missing_null_and_bad_types() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    let workspace = dirs.workspace("one");
    let cases = [
        ("settings", json!({"workspace":workspace})),
        (
            "set",
            json!({"workspace":workspace,"layer":"global","key":"thinking","text":"high"}),
        ),
        ("global_file", json!({})),
        ("rules", json!({"workspace":workspace})),
        (
            "revoke",
            json!({"workspace":workspace,"scope":"global","line":1,"text":"x"}),
        ),
        ("login_targets", json!({})),
        (
            "store_key",
            json!({"name":"acme","key":"secret","label":"work"}),
        ),
        ("reviewer_models", json!({"name":"acme"})),
        ("set_reviewer", json!({"name":"acme","id":"m"})),
        ("gaps", json!({"name":"acme"})),
        (
            "save_host",
            json!({"name":"acme","key":"workspace","value":"h.example"}),
        ),
        ("reviewer_notes", json!({"workspace":workspace})),
        ("save_reviewer_notes", json!({"workspace":workspace})),
        ("tool_switches", json!({"workspace":workspace})),
        (
            "switch_tool",
            json!({"workspace":workspace,"group":{"mcp":"x"},"tool":"t","scope":"project","on":false}),
        ),
        ("skills_disabled", json!({"workspace":workspace})),
        (
            "switch_skill",
            json!({"workspace":workspace,"name":"s","scope":"everywhere","on":true}),
        ),
        ("skill_text", json!({"path":"/x/SKILL.md"})),
    ];
    for (operation, valid) in cases {
        let mut extra = valid.clone();
        extra["extra"] = json!(true);
        assert_eq!(
            call(&seam, operation, extra).unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{operation}: extra"
        );
        for key in valid.as_object().unwrap().keys() {
            for bad in [Value::Null, json!([]), json!({})] {
                let mut args = valid.clone();
                args[key] = bad;
                assert_eq!(
                    call(&seam, operation, args).unwrap_err().code,
                    ErrorCode::InvalidArguments,
                    "{operation}: {key}"
                );
            }
            if key != "label" {
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert_eq!(
                    call(&seam, operation, missing).unwrap_err().code,
                    ErrorCode::InvalidArguments,
                    "{operation}: missing {key}"
                );
            }
        }
        if valid.get("workspace").is_some() {
            let mut args = valid.clone();
            args["workspace"] = json!("relative");
            assert_eq!(
                call(&seam, operation, args).unwrap_err().code,
                ErrorCode::InvalidArguments
            );
        }
    }
    let mut seam = seam;
    for (operation, args) in [
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"version":"v","text":"t"}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"project":{"version":"v","text":"t"}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"version":"v","text":"t"},"project":{"version":"v","text":"t"}}),
        ),
    ] {
        // Each layer is optional, so every subset of them is a save.
        let mut extra = args.clone();
        extra["extra"] = json!(true);
        assert_eq!(
            call(&seam, operation, extra).unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{operation}: extra"
        );
        let (back, outcome) = fakes::within(
            "valid notes layers save",
            Deadline::after(std::time::Duration::from_secs(10)),
            move || {
                let outcome = call(&seam, operation, args);
                (seam, outcome)
            },
        );
        seam = back;
        assert!(outcome.unwrap().is_some());
    }
    for (operation, args) in [
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":null}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":[]}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"text":"t"}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"version":"v"}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"version":null,"text":"t"}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"global":{"version":"v","text":7}}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"project":"t"}),
        ),
        (
            "save_reviewer_notes",
            json!({"workspace":workspace,"project":{"version":"v"}}),
        ),
    ] {
        // An explicit null, a layer of the wrong shape and a layer
        // missing its version are no save at all, and write nothing.
        let before = fs::read(dirs.home().join("config.json")).ok();
        assert_eq!(
            call(&seam, operation, args.clone()).unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{args}"
        );
        assert_eq!(
            fs::read(dirs.home().join("config.json")).ok(),
            before,
            "{args}"
        );
    }
    // A notes layer names exactly its version with its text: an extra
    // key is no save at all, and writes neither file.
    let project = dirs.project_file(&workspace);
    let global = dirs.home().join("config.json");
    write(&global, r#"{"reviewer": {"context": "old global"}}"#);
    write(&project, r#"{"reviewer": {"context": "old project"}}"#);
    let before = (fs::read(&global).unwrap(), fs::read(&project).unwrap());
    for layer in ["global", "project"] {
        let mut args = json!({"workspace": workspace});
        args[layer] = json!({"version": "v", "text": "t", "extra": true});
        assert_eq!(
            call(&seam, "save_reviewer_notes", args).unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{layer}"
        );
        assert_eq!(fs::read(&global).unwrap(), before.0, "{layer}");
        assert_eq!(fs::read(&project).unwrap(), before.1, "{layer}");
    }
    for operation in ["unknown", "themes", "theme", "save_keys", "LoginShow"] {
        assert_eq!(
            call(&seam, operation, json!({})).unwrap_err().code,
            ErrorCode::InvalidArguments
        );
    }
    for (operation, args) in [
        (
            "set",
            json!({"workspace":workspace,"layer":"bad","key":"thinking","text":"high"}),
        ),
        (
            "switch_skill",
            json!({"workspace":workspace,"name":"s","scope":"global","on":false}),
        ),
        (
            "switch_tool",
            json!({"workspace":workspace,"group":{"mcp":"x","extension":"y"},"tool":"t","scope":"project","on":false}),
        ),
        (
            "revoke",
            json!({"workspace":workspace,"scope":"everywhere","line":0,"text":"x"}),
        ),
    ] {
        assert_eq!(
            call(&seam, operation, args).unwrap_err().code,
            ErrorCode::InvalidArguments
        );
    }
}

#[test]
fn remote_config_files_allow_only_the_views_paths() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(home.clone());
    let project = dirs.project_file(&workspace);
    let allowed = [
        home.join("config.json"),
        project.clone(),
        workspace.join(".fiber/config.json"),
        home.join("rules"),
        project.with_file_name("rules"),
        workspace.join("SKILL.md"),
    ];
    for path in allowed {
        write(&path, "body");
        let (text, version) = seam.read_config(&workspace, &path).unwrap();
        assert_eq!(text, "body");
        assert_eq!(version.len(), 16);
        assert!(
            version
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        seam.write_config(&workspace, &path, "changed", &version)
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "changed");
    }
    for path in [
        home.join("credentials/x"),
        workspace.join(".fiber/other.json"),
        "config.json".into(),
        workspace.join("SKILL.md.bak"),
        home.join("other/../config.json"),
    ] {
        assert_eq!(
            seam.read_config(&workspace, &path).unwrap_err().code,
            ErrorCode::InvalidArguments
        );
        assert_eq!(
            seam.write_config(&workspace, &path, "x", "missing")
                .unwrap_err()
                .code,
            ErrorCode::InvalidArguments
        );
    }
    assert_eq!(
        seam.read_config(Path::new("relative"), &home.join("config.json"))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(
        seam.write_config(
            Path::new("relative"),
            &home.join("config.json"),
            "x",
            "missing"
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArguments
    );
    assert!(!workspace.join("SKILL.md.lock").exists());
}

#[test]
fn remote_config_versions_cover_missing_stale_and_byte_changes() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let path = dirs.home().join("config.json");
    let seam = Seam::new(dirs.home());
    assert_eq!(
        seam.read_config(&workspace, &path).unwrap(),
        (String::new(), "missing".into())
    );
    seam.write_config(&workspace, &path, "{}", "missing")
        .unwrap();
    assert_eq!(
        seam.write_config(&workspace, &path, "lost", "missing")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    let (_, version) = seam.read_config(&workspace, &path).unwrap();
    // Equal-length edits still invalidate a content version.
    fs::write(&path, "[]").unwrap();
    let error = seam
        .write_config(&workspace, &path, "lost", &version)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleRequest);
    assert_eq!(
        error.message,
        format!("{} changed since it was read.", path.display())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
    let (_, version) = seam.read_config(&workspace, &path).unwrap();
    seam.write_config(&workspace, &path, "{", &version).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "{");
    let rules = dirs.home().join("rules");
    seam.write_config(&workspace, &rules, "this is not a rule\n", "missing")
        .unwrap();
    assert_eq!(fs::read_to_string(rules).unwrap(), "this is not a rule\n");
}

#[test]
fn remote_config_caps_count_bytes_at_both_edges_and_never_cut() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let path = workspace.join("SKILL.md");
    for length in [65_535, 65_536, 65_537] {
        let text = "x".repeat(length);
        write(&path, &text);
        let read = seam.read_config(&workspace, &path);
        if length == 65_537 {
            assert_eq!(read.unwrap_err().code, ErrorCode::TooLarge);
        } else {
            let (body, _) = read.unwrap();
            assert_eq!(body.len(), length);
            assert!(body == text);
        }
        fs::remove_file(&path).unwrap();
        let written = seam.write_config(&workspace, &path, &text, "missing");
        if length == 65_537 {
            assert_eq!(written.unwrap_err().code, ErrorCode::TooLarge);
            assert!(!path.exists());
        } else {
            written.unwrap();
            let body = fs::read(&path).unwrap();
            assert_eq!(body.len(), length);
            assert!(body == text.as_bytes());
        }
    }
    let unicode = "é".repeat(32_768);
    seam.write_config(&workspace, &path, &unicode, "missing")
        .unwrap();
    assert_eq!(seam.read_config(&workspace, &path).unwrap().0.len(), 65_536);
    let (_, version) = seam.read_config(&workspace, &path).unwrap();
    assert_eq!(
        seam.write_config(&workspace, &path, &format!("{unicode}a"), &version)
            .unwrap_err()
            .code,
        ErrorCode::TooLarge
    );
    fs::write(&path, [0xff]).unwrap();
    let error = seam.read_config(&workspace, &path).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(error.message.contains(path.to_str().unwrap()));
}

#[test]
fn remote_config_write_takes_the_lock_before_comparing_the_version() {
    use fakes::Deadline;
    use std::sync::mpsc;
    use std::time::Duration;
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let path = dirs.home().join("config.json");
    let seam = Seam::new(dirs.home());
    write(&path, "old");
    let (_, version) = seam.read_config(&workspace, &path).unwrap();
    let lock = fs::File::create(path.with_file_name("config.json.lock")).unwrap();
    lock.lock().unwrap();
    let (tx, rx) = mpsc::channel();
    let (started, ready) = mpsc::channel();
    let file = path.clone();
    std::thread::spawn(move || {
        started.send(()).unwrap();
        tx.send(seam.write_config(&workspace, &file, "new", &version))
            .unwrap_or(());
    });
    let deadline = Deadline::after(Duration::from_secs(10));
    deadline.recv_or_fail(&ready, "writer started");
    assert!(matches!(
        Deadline::after(Duration::from_millis(300)).recv(&rx),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fs::write(&path, "other").unwrap();
    drop(lock);
    assert_eq!(
        deadline
            .recv_or_fail(&rx, "writer finished")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "other");
}

#[test]
fn remote_setting_shapes_preserve_values_layers_and_scope() {
    for (shown, scope, expected) in [
        (
            Shown::Unset,
            WriteScope::GlobalOnly,
            json!({"shown":"unset","scope":"global_only"}),
        ),
        (
            Shown::Redacted("names secret".into()),
            WriteScope::RepoOnly,
            json!({"shown":"redacted","value":"names secret","scope":"repo_only"}),
        ),
        (
            Shown::Value("false".into()),
            WriteScope::Any { repo: false },
            json!({"shown":"value","value":"false","scope":"any","repo":false}),
        ),
        (
            Shown::Union {
                names: vec![("s".into(), "global".into())],
                own: vec![
                    (Layer::Global, "[]".into()),
                    (Layer::Project, "[1]".into()),
                    (Layer::Repository, "[2]".into()),
                ],
            },
            WriteScope::PersonFiles,
            json!({"shown":"union","names":[["s","global"]],"own":[["global","[]"],["project","[1]"],["repository","[2]"]],"scope":"person_files"}),
        ),
    ] {
        let found = setting(tui::SettingRow {
            key: "test".into(),
            layer: "default".into(),
            file: None,
            value: shown,
            scope,
        });
        let mut expected = expected;
        expected["key"] = json!("test");
        expected["layer"] = json!("default");
        assert_eq!(found, expected);
    }
    assert_eq!(
        lists(ToolLists {
            enabled: Some(vec![]),
            disabled: vec!["t".into()]
        }),
        json!({"enabled":[],"disabled":["t"]})
    );
}

#[test]
fn remote_rules_report_rows_removals_and_parse_errors() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let text = r#"{"decision":"allow","tool":"shell","prefix":"git status"}"#;
    write(&dirs.home().join("rules"), text);
    let rules = result(&seam, "rules", json!({"workspace":workspace}));
    assert_eq!(
        rules["global"]["rows"][0],
        json!({"line":1,"text":text,"rule":{"decision":"allow","tool":"shell","prefix":"git status"}})
    );
    assert_eq!(
        result(
            &seam,
            "revoke",
            json!({"workspace":workspace,"scope":"global","line":1,"text":text})
        ),
        json!({"revoked":"removed"})
    );
    write(
        &dirs.project_file(&workspace).with_file_name("rules"),
        "invalid rule",
    );
    let rules = result(&seam, "rules", json!({"workspace":workspace}));
    assert!(
        rules["project"]["error"]
            .as_str()
            .unwrap()
            .contains("line 1")
    );
    assert!(rules["project"].get("rows").is_none());
}

#[test]
fn remote_operations_preserve_other_layers_and_switch_forms() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    install(&dirs.home());
    for (layer, file) in [
        ("project", dirs.project_file(&workspace)),
        ("repository", workspace.join(".fiber/config.json")),
    ] {
        assert_eq!(
            result(
                &seam,
                "set",
                json!({"workspace":workspace,"layer":layer,"key":"thinking","text":"high"})
            )["file"],
            json!(file)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap()["thinking"],
            "high"
        );
    }
    assert_eq!(
        result(
            &seam,
            "store_key",
            json!({"name":"acme","label":"work","key":"sk-work"})
        ),
        json!({"path":"credentials/acme/work","replaced":false})
    );
    assert_eq!(
        call(&seam, "reviewer_models", json!({"name":"unknown"}))
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    crate::test_support::install_extension(
        &dirs.home(),
        "extensions/plain",
        json!({"name":"plain","version":"v0.0.0","fiber":"0.0.0","api":1}),
        &[(
            "plain",
            json!({"name":"plain","models":[{"id":"p","protocol":"openai-responses","base_url":"http://x/v1","context_window":1000}]}),
        )],
    );
    assert_eq!(
        result(&seam, "reviewer_models", json!({"name":"plain"})),
        json!({"models":["p"],"notices":[]})
    );
    call(&seam,"switch_tool",json!({"workspace":workspace,"group":{"extension":"acme"},"tool":"t","scope":"everywhere","on":false})).unwrap();
    let tools = result(&seam, "tool_switches", json!({"workspace":workspace}));
    assert_eq!(tools["groups"][0]["group"], json!({"extension":"acme"}));
    call(&seam,"switch_tool",json!({"workspace":workspace,"group":{"extension":"acme"},"tool":"t","scope":"everywhere","on":true})).unwrap();
    assert_eq!(
        result(&seam, "tool_switches", json!({"workspace":workspace}))["groups"][0]["everywhere"]["disabled"],
        json!([])
    );
    call(
        &seam,
        "switch_skill",
        json!({"workspace":workspace,"name":"s","scope":"project","on":false}),
    )
    .unwrap();
    assert_eq!(
        result(&seam, "skills_disabled", json!({"workspace":workspace}))["project"],
        json!(["s"])
    );
    for group in [
        json!({}),
        json!({"mcp":null}),
        json!({"extension":null}),
        json!({"unknown":"x"}),
    ] {
        assert_eq!(
            call(
                &seam,
                "switch_tool",
                json!({"workspace":workspace,"group":group,"tool":"t","scope":"project","on":false})
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidArguments
        );
    }
}

#[test]
fn remote_config_io_failures_name_the_file_and_skill_symlinks_are_allowed() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let directory = workspace.join("SKILL.md");
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        seam.read_config(&workspace, &directory).unwrap_err().code,
        ErrorCode::IoFailed
    );
    fs::remove_dir(&directory).unwrap();
    let target = workspace.join("body");
    write(&target, "linked");
    std::os::unix::fs::symlink(&target, &directory).unwrap();
    assert_eq!(
        seam.read_config(&workspace, &directory).unwrap().0,
        "linked"
    );
    let config = dirs.home().join("config.json");
    fs::create_dir(&config).unwrap();
    assert_eq!(
        seam.read_config(&workspace, &config).unwrap_err().code,
        ErrorCode::IoFailed
    );
    assert_eq!(
        seam.write_config(&workspace, &config, "x", "missing")
            .unwrap_err()
            .code,
        ErrorCode::IoFailed
    );
}

#[test]
fn remote_revoke_removes_a_project_rule_through_configure() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let text = r#"{"decision":"allow","tool":"shell","prefix":"git status"}"#;
    let file = dirs.project_file(&workspace).with_file_name("rules");
    write(&file, &format!("{text}\n"));
    assert_eq!(
        result(
            &seam,
            "revoke",
            json!({"workspace":workspace,"scope":"project","line":1,"text":text})
        ),
        json!({"revoked":"removed"})
    );
    assert!(fs::read(&file).unwrap().is_empty());
}

#[test]
fn remote_config_open_failures_other_than_missing_are_io_failures() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    // `SKILL.md` is editable at any absolute path, so a file standing
    // where a directory must be makes `File::open` fail with
    // `NotADirectory` (never `NotFound`). Treating every open failure as
    // missing would return `("", "missing")` here instead of failing.
    let blocker = workspace.join("blocker");
    write(&blocker, "not a directory");
    let path = blocker.join("SKILL.md");
    assert!(std::fs::File::open(&path).unwrap_err().kind() != std::io::ErrorKind::NotFound);
    assert_eq!(
        seam.read_config(&workspace, &path).unwrap_err().code,
        ErrorCode::IoFailed
    );
}

fn remote_git(workspace: &Path, args: &[&str]) {
    use std::os::unix::process::CommandExt;
    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .process_group(0)
        .envs(fakes::check_run())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let output = fakes::finished(
        "remote search git",
        child,
        &[],
        fakes::Deadline::after(std::time::Duration::from_secs(10)),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn remote_search_uses_the_workspaces_listing_and_the_given_home() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("search");
    let seam = Seam::new(home.clone());
    write(&workspace.join("alpha.rs"), "tracked");
    write(&workspace.join("untracked.rs"), "not tracked");
    write(&home.join("from-home"), "home");
    remote_git(&workspace, &["init", "-q"]);
    remote_git(&workspace, &["add", "alpha.rs"]);
    let mut search = seam.file_search(&workspace, Some(&home));
    let result = fakes::within(
        "hub workspace search",
        Deadline::after(std::time::Duration::from_secs(10)),
        move || {
            assert_eq!(
                search.search("alpha", &|| false),
                Some(Ok(vec!["alpha.rs".into()]))
            );
            assert_eq!(search.search("untracked", &|| false), Some(Ok(vec![])));
            assert_eq!(
                search.search("~/from-", &|| false),
                Some(Ok(vec!["~/from-home".into()]))
            );
            search
        },
    );
    let mut search = result;
    write(&workspace.parent().unwrap().join("sibling.txt"), "sibling");
    assert_eq!(
        search.search("../sibling", &|| false),
        Some(Ok(vec!["../sibling.txt".into()]))
    );
    assert_eq!(search.search("alpha", &|| true), None);
    assert_eq!(search.search("~/from-", &|| true), None);
}

#[test]
fn remote_search_reports_the_first_git_error_line_and_caps_both_modes() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let workspace = dirs.workspace("not-git");
    fs::create_dir_all(&workspace).unwrap();
    let seam = Seam::new(home.clone());
    let mut search = seam.file_search(&workspace, Some(&home));
    let result = fakes::within(
        "git error",
        Deadline::after(std::time::Duration::from_secs(10)),
        move || search.search("", &|| false),
    );
    let error = result.unwrap().unwrap_err();
    assert!(error.contains("not a git repository"), "{error}");
    assert!(!error.contains('\n'));
    for (count, kept) in [(49, 49), (50, 50), (51, 50)] {
        let workspace = dirs.workspace(&format!("count{count}"));
        for n in 0..count {
            write(&workspace.join(format!("match{n:03}")), "x");
        }
        remote_git(&workspace, &["init", "-q"]);
        remote_git(&workspace, &["add", "."]);
        let mut search = seam.file_search(&workspace, Some(&workspace));
        fakes::within(
            "bounded remote searches",
            Deadline::after(std::time::Duration::from_secs(10)),
            move || {
                for query in ["match", "~/match"] {
                    let rows = search.search(query, &|| false).unwrap().unwrap();
                    assert_eq!(rows.len(), kept, "{count}: {query}");
                    assert!(rows.iter().all(|row| row.contains("match")));
                }
            },
        );
    }
}

fn remote_install_codex(home: &Path, server: &fakes::OauthServer) {
    crate::test_support::install_extension(
        home,
        "extensions/codex",
        json!({"name":"codex","version":"v0.0.0","fiber":"0.0.0","api":1}),
        &[(
            "codex",
            json!({"name":"codex","login":"browser","models":[{"id":"m","protocol":"openai-responses","base_url":"http://x/v1","context_window":1000}]}),
        )],
    );
    // This provider exercises the hub adapter, not a vendor's OAuth protocol.
    write(
        &home.join("extensions/codex/providers/codex.lua"),
        &format!(
            r#"
        fiber.provider("codex", {{ credential = {{ timeout = 60000, run = function(arg)
            assert(arg.login == "device", "the hub must ask for the device method")
            local email
            local stored = host.oauth.refresh(function()
                local reply = host.http({{ url = "{base}/device", method = "POST" }})
                local started = json.decode(reply.body)
                host.oauth.show("{base}/codex/device", started.user_code)
                local token = host.oauth.poll({{ url = "{base}/token", pending = {{403}}, interval = started.interval }})
                email = token.email
                return {{token = token.token, refresh_token = token.refresh_token, expires_at = 4102444800}}
            end)
            return {{token = stored.token, expires_at = stored.expires_at, email = email}}
        end }} }})
    "#,
            base = server.url()
        ),
    );
}

type LoginOutcome = Result<String, Failure>;
struct RemoteLoginRun {
    login: Arc<dyn hub::DeviceLogin>,
    result: std::sync::mpsc::Receiver<LoginOutcome>,
}
impl RemoteLoginRun {
    fn start(login: Arc<dyn hub::DeviceLogin>) -> Self {
        let (done, result) = std::sync::mpsc::channel();
        let running = login.clone();
        std::thread::spawn(move || {
            drop(done.send(running.run()));
        });
        Self { login, result }
    }
    fn finish(&self) -> LoginOutcome {
        fakes::Deadline::after(std::time::Duration::from_secs(10))
            .recv_or_fail(&self.result, "remote device login ended")
    }
}
impl Drop for RemoteLoginRun {
    fn drop(&mut self) {
        self.login.cancel();
    }
}

struct PollClock {
    clock: Arc<fakes::clock::FakeClock>,
    target: std::time::Instant,
    parked: std::sync::mpsc::Sender<std::thread::ThreadId>,
}
impl Clock for PollClock {
    fn now(&self) -> std::time::Instant {
        self.clock.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.clock.wall()
    }
    fn sleep(&self, duration: std::time::Duration) {
        self.clock.sleep(duration);
    }
    fn subscribe(&self, wake: std::sync::Weak<dyn contract::clock::Wake>) {
        self.clock.subscribe(wake);
    }
    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<std::time::Duration>),
    ) {
        self.clock.wait_until(until, &mut |bound| {
            if until == Some(self.target) {
                self.parked.send(std::thread::current().id()).unwrap_or(());
            }
            wait(bound);
        });
    }
}

#[test]
fn remote_device_login_shows_the_code_and_stores_or_cancels_during_the_poll() {
    use fakes::{Deadline, OauthReply, OauthServer};
    use std::time::Duration;
    for cancel in [false, true] {
        let dirs = Dirs::new();
        let home = dirs.home();
        let access = "device-access-token";
        let server = OauthServer::start(vec![
            OauthReply::raw(
                200,
                &json!({"device_auth_id":"da_1","user_code":"ABCD-1234","interval":1}).to_string(),
            ),
            OauthReply::raw(403, "{}"),
            OauthReply::raw(
                200,
                &json!({"token":access,"email":"alice@example.com","refresh_token":"rt_1"})
                    .to_string(),
            ),
        ]);
        remote_install_codex(&home, &server);
        let clock = fakes::clock::FakeClock::new();
        let target = clock.now() + Duration::from_secs(1);
        let (parked, parks) = std::sync::mpsc::channel();
        let timed: Arc<dyn Clock> = Arc::new(PollClock {
            clock: clock.clone(),
            target,
            parked,
        });
        let (shown, codes) = std::sync::mpsc::channel();
        let login = Seam::new(home.clone()).device_login(
            "codex",
            Box::new(move |url, code| {
                shown.send((url.to_owned(), code.to_owned())).unwrap();
            }),
            timed,
        );
        let run = RemoteLoginRun::start(login);
        let deadline = Deadline::after(Duration::from_secs(10));
        assert_eq!(
            deadline.recv(&codes).unwrap_or_else(|error| panic!(
                "device code: {error}; outcome: {:?}",
                run.result.try_recv()
            )),
            (format!("{}/codex/device", server.url()), "ABCD-1234".into())
        );
        let thread = deadline.recv_or_fail(&parks, "poll waiting past its clock check");
        if cancel {
            run.login.cancel();
        } else {
            clock.advance_when_parked(
                Some(target),
                Some(thread),
                Duration::from_secs(1),
                &deadline,
            );
        }
        let outcome = run.finish();
        if cancel {
            assert_eq!(outcome.unwrap_err().code, ErrorCode::CredentialFailed);
            let credentials = home.join("credentials/codex");
            assert!(!credentials.exists() || fs::read_dir(credentials).unwrap().next().is_none());
        } else {
            assert_eq!(outcome.unwrap(), "credentials/codex/alice@example.com");
            let stored: Value = serde_json::from_slice(
                &fs::read(home.join("credentials/codex/alice@example.com")).unwrap(),
            )
            .unwrap();
            assert_eq!(stored["token"], access);
            assert_eq!(stored["refresh_token"], "rt_1");
        }
    }
}

#[test]
fn remote_device_login_refuses_a_provider_that_opens_a_browser() {
    let dirs = Dirs::new();
    let home = dirs.home();
    install(&home);
    let file = home.join("extensions/acme/providers/acme.json");
    let mut data: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    data["login"] = json!("browser");
    write(&file, &data.to_string());
    let server = fakes::OauthServer::start(vec![]);
    server.hold();
    write(
        &home.join("extensions/acme/providers/acme.lua"),
        &format!(
            r#"fiber.provider("acme", {{ credential = {{ timeout = 60000, run = function()
        host.oauth.open("https://example.invalid/authorize")
        return host.oauth.poll({{ url = "{}/token" }})
    end }} }})"#,
            server.url()
        ),
    );
    let (shown, codes) = std::sync::mpsc::channel();
    let login = Seam::new(home.clone()).device_login(
        "acme",
        Box::new(move |url, code| {
            shown.send((url.to_owned(), code.to_owned())).unwrap();
        }),
        fakes::clock::FakeClock::new(),
    );
    let run = RemoteLoginRun::start(login);
    let error = run.finish().unwrap_err();
    assert_eq!(error.code, ErrorCode::AuthenticationFailed);
    assert_eq!(error.message, "`acme` has no device-code form.");
    assert!(codes.try_recv().is_err());
    assert!(!home.join("credentials/acme/default").exists());
}

#[test]
fn remote_operation_documentation_lists_the_complete_wire_surface() {
    let doc = include_str!("../../../../docs/invocation.md");
    let table = doc
        .split("| Operation | Arguments besides `operation` | Result |\n")
        .nth(1)
        .unwrap()
        .split("\n\n")
        .next()
        .unwrap();
    let names: Vec<_> = table
        .lines()
        .skip(1)
        .map(|row| row.split('`').nth(1).unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "settings",
            "set",
            "global_file",
            "rules",
            "revoke",
            "login_targets",
            "store_key",
            "reviewer_models",
            "models",
            "install_provider",
            "set_reviewer",
            "gaps",
            "save_host",
            "reviewer_notes",
            "save_reviewer_notes",
            "tool_switches",
            "switch_tool",
            "skills_disabled",
            "switch_skill",
            "skill_text"
        ]
    );
}

#[test]
fn logout_operations_answer_their_wire_shapes() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let seam = Seam::new(home.clone());
    install(&home);
    crate::test_support::install_extension(
        &home,
        "extensions/acme-secrets",
        json!({"name":"acme-secrets","version":"v0.0.0","fiber":"0.0.0","api":1,"secrets":["acme.api_key"]}),
        &[],
    );
    assert_eq!(
        result(
            &seam,
            "store_key",
            json!({"name":"acme","key":"sk-test","label":"work"})
        ),
        json!({"path":"credentials/acme/work","replaced":false})
    );
    assert_eq!(
        result(
            &seam,
            "store_key",
            json!({"name":"acme.api_key","key":"sk-secret"})
        ),
        json!({"path":"credentials/acme.api_key","replaced":false})
    );
    // Flipping a variant arm breaks the shape: the provider row carries
    // its label and default mark, the secret row neither.
    assert_eq!(
        result(&seam, "logout_rows", json!({})),
        json!({"rows":[
            {"row":"provider","name":"acme","label":"work","default":true,"deletable":true,"siblings":[]},
            {"row":"secret","name":"acme.api_key","deletable":true},
        ]})
    );
    assert_eq!(
        result(
            &seam,
            "delete_stored",
            json!({"name":"acme","label":"work"})
        ),
        json!({"path":"credentials/acme/work"})
    );
    assert_eq!(
        result(&seam, "logout_rows", json!({})),
        json!({"rows":[
            {"row":"secret","name":"acme.api_key","deletable":true},
        ]})
    );
}

use std::collections::BTreeSet;
use std::path::PathBuf;

use contract::SessionId;
use contract::commands::hub::{PruneExpect, PruneSession};

/// A session dir under `projects/<project>/sessions/<id>`: the hub-wide
/// selection finds it whatever the workspace is; the lock file proves no
/// pass takes it.
fn prune_session(home: &Path, project: &str, id: &str, from: Option<&str>, ts: u64) -> PathBuf {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": "/w"});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "seq": 0, "payload": payload});
    let last = json!({"kind": "x", "ts": ts});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"0123456789").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

fn wall_ms(clock: &std::sync::Arc<fakes::clock::FakeClock>) -> u64 {
    contract::clock::wall_ms(clock.wall())
}

#[test]
fn seam_prune_dry_run_previews_across_projects_without_removing() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let root = prune_session(&home, "one", "s_00000000000000a1", None, 0);
    let fork = prune_session(
        &home,
        "two",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
        0,
    );
    let clock = fakes::clock::FakeClock::new();
    let seam = Seam::new(home.clone());
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    let outcome = seam
        .prune(
            &home,
            Some("30d".to_owned()),
            true,
            false,
            true,
            None,
            clock.clone(),
            &mut delete,
        )
        .unwrap();
    assert!(calls.is_empty(), "a dry run deletes nothing");
    assert_eq!(outcome.items.sessions.len(), 1);
    assert_eq!(outcome.items.sessions[0].id.0, "s_00000000000000a1");
    assert_eq!(
        outcome.items.sessions[0].cascade,
        [
            SessionId("s_00000000000000a1".into()),
            SessionId("s_00000000000000b2".into())
        ]
    );
    assert_eq!(outcome.items.worktrees, Vec::<String>::new());
    let listed = log::session_bytes(&root) + log::session_bytes(&fork);
    assert_eq!(outcome.freed, listed);
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| line.contains("s_00000000000000a1"))
    );
    assert!(root.join("session.lock").is_file());
    assert!(fork.join("session.lock").is_file());
    assert!(root.join("events.jsonl").is_file());
    assert!(fork.join("events.jsonl").is_file());
}

#[test]
fn seam_prune_confirm_removes_previewed_items_through_the_callback() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let root = prune_session(&home, "one", "s_00000000000000a1", None, 0);
    let fork = prune_session(
        &home,
        "two",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
        0,
    );
    fs::create_dir_all(home.join("logs")).unwrap();
    fs::write(home.join("logs").join("old.log"), b"0123456789").unwrap();
    let old_log = home.join("logs").join("old.log").display().to_string();
    let clock = fakes::clock::FakeClock::new();
    fs::File::options()
        .write(true)
        .open(home.join("logs").join("old.log"))
        .unwrap()
        .set_modified(clock.wall() - std::time::Duration::from_secs(31 * 24 * 60 * 60))
        .unwrap();
    let seam = Seam::new(home.clone());
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    let outcome = seam
        .prune(
            &home,
            Some("30d".to_owned()),
            true,
            false,
            false,
            Some(PruneExpect {
                sessions: vec![PruneSession {
                    id: SessionId("s_00000000000000a1".into()),
                    cascade: vec![
                        SessionId("s_00000000000000a1".into()),
                        SessionId("s_00000000000000b2".into()),
                    ],
                }],
                worktrees: Vec::new(),
                pinned: Vec::new(),
                diagnostics: vec![old_log.clone()],
            }),
            clock.clone(),
            &mut delete,
        )
        .unwrap();
    // Each session arrives with its previewed cascade set as `expect`.
    assert_eq!(
        calls,
        [(
            "s_00000000000000a1".to_owned(),
            true,
            Some(
                [
                    SessionId("s_00000000000000a1".into()),
                    SessionId("s_00000000000000b2".into())
                ]
                .into_iter()
                .collect()
            ),
        )]
    );
    let listed = log::session_bytes(&root) + log::session_bytes(&fork);
    assert_eq!(outcome.freed, listed + 10);
    assert_eq!(outcome.total, 2);
    assert!(outcome.failures.is_empty());
    assert!(!home.join("logs").join("old.log").exists());
}

#[test]
fn seam_prune_older_than_boundary_and_invalid_duration() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let clock = fakes::clock::FakeClock::new();
    let now = wall_ms(&clock);
    let thirty_days = 30 * 24 * 60 * 60 * 1000;
    // Just under the duration survives; just over is selected.
    prune_session(
        &home,
        "one",
        "s_00000000000000a1",
        None,
        now - thirty_days + 60_000,
    );
    prune_session(
        &home,
        "one",
        "s_00000000000000b2",
        None,
        now - thirty_days - 60_000,
    );
    let seam = Seam::new(home.clone());
    let mut delete = |_: &SessionId, _: bool, _: Option<&BTreeSet<SessionId>>| Ok(());
    let outcome = seam
        .prune(
            &home,
            Some("30d".to_owned()),
            false,
            false,
            true,
            None,
            clock.clone(),
            &mut delete,
        )
        .unwrap();
    assert_eq!(outcome.items.sessions.len(), 1);
    assert_eq!(outcome.items.sessions[0].id.0, "s_00000000000000b2");
    // An unreadable duration fails before any removal.
    let mut calls = 0;
    let mut counting = |_: &SessionId, _: bool, _: Option<&BTreeSet<SessionId>>| {
        calls += 1;
        Ok(())
    };
    let refused = seam
        .prune(
            &home,
            Some("bogus".to_owned()),
            false,
            false,
            false,
            None,
            clock,
            &mut counting,
        )
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArguments);
    assert_eq!(calls, 0);
    assert!(
        home.join("projects/one/sessions/s_00000000000000a1/events.jsonl")
            .is_file()
    );
    assert!(
        home.join("projects/one/sessions/s_00000000000000b2/events.jsonl")
            .is_file()
    );
}

#[test]
fn seam_prune_running_worktree_survives_and_skips_stale() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let clock = fakes::clock::FakeClock::new();
    // A worktree a running session works in: its lock stays taken through
    // the confirm, so the reselect leaves it out.
    let worktree = home.join("projects/pw/worktrees/w1");
    fs::create_dir_all(&worktree).unwrap();
    let canonical = fs::canonicalize(&worktree).unwrap();
    let running = home.join("projects/pw/sessions/s_00000000000000c3");
    fs::create_dir_all(&running).unwrap();
    fs::write(
        running.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"kind": "session_started", "seq": 0,
                "payload": {"workspace": canonical.to_str().unwrap()}}),
            json!({"kind": "x", "ts": 0}),
        ),
    )
    .unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(running.join("session.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let seam = Seam::new(home.clone());
    let mut delete = |_: &SessionId, _: bool, _: Option<&BTreeSet<SessionId>>| Ok(());
    let key = worktree.display().to_string();
    let outcome = seam
        .prune(
            &home,
            Some("30d".to_owned()),
            false,
            false,
            false,
            Some(PruneExpect {
                sessions: Vec::new(),
                worktrees: vec![key.clone()],
                pinned: Vec::new(),
                diagnostics: Vec::new(),
            }),
            clock,
            &mut delete,
        )
        .unwrap();
    assert_eq!(outcome.total, 1);
    assert_eq!(outcome.freed, 0);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].code, ErrorCode::StaleRequest);
    assert!(outcome.failures[0].item.contains("w1"));
    assert!(worktree.is_dir(), "the running worktree survives");
    drop(lock);
}

#[test]
fn seam_prune_confirm_without_expect_removes_the_current_selection() {
    let dirs = Dirs::new();
    let home = dirs.home();
    let root = prune_session(&home, "one", "s_00000000000000a1", None, 0);
    let fork = prune_session(
        &home,
        "two",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
        0,
    );
    let clock = fakes::clock::FakeClock::new();
    let seam = Seam::new(home.clone());
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    // Without `expect` (`--yes`) the confirm removes the current
    // selection, as the local path does.
    let outcome = seam
        .prune(
            &home,
            Some("30d".to_owned()),
            true,
            false,
            false,
            None,
            clock.clone(),
            &mut delete,
        )
        .unwrap();
    // Each session arrives with its previewed cascade set as `expect`.
    assert_eq!(
        calls,
        [(
            "s_00000000000000a1".to_owned(),
            true,
            Some(
                [
                    SessionId("s_00000000000000a1".into()),
                    SessionId("s_00000000000000b2".into())
                ]
                .into_iter()
                .collect()
            ),
        )]
    );
    let listed = log::session_bytes(&root) + log::session_bytes(&fork);
    assert_eq!(outcome.freed, listed);
    assert_eq!(outcome.total, 1);
    assert!(outcome.failures.is_empty());
}
