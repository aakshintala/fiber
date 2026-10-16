//! The hub's configuration operations use its Fiber home.
use std::fs;
use std::path::Path;

use contract::ErrorCode;
use hub::Machine;
use serde_json::{Value, json};

use super::*;
use crate::configure::tests::{Dirs, write};

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
