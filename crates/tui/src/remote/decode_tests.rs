use super::*;
use serde_json::json;

#[test]
fn rules_preserve_physical_rows_and_reject_malformed_answers() {
    let input = json!({"global":{"file":"/hub/rules","rows":[{"line":7,"text":"deny shell rm","rule":{"decision":"deny","tool":"shell","prefix":"rm"}}]},"project":{"file":"/hub/project/rules","rows":[]}});
    let parsed = rules(&input).unwrap();
    let row = &parsed.0.rows.unwrap()[0];
    assert_eq!(row.line, 7);
    assert_eq!(row.text, "deny shell rm");
    assert_eq!(row.rule.decision, contract::RuleDecision::Deny);
    assert_eq!(row.rule.tool, "shell");
    assert_eq!(row.rule.prefix, "rm");
    assert_eq!(parsed.1.rows, Ok(vec![]));
    for field in ["line", "text", "rule"] {
        let mut broken = input.clone();
        broken["global"]["rows"][0][field] = json!(null);
        assert!(rules(&broken).is_none(), "malformed rule {field}");
    }
    let value = json!({"groups":[{"group":{"mcp":"server","extension":"ext"},"project":{"disabled":[]},"everywhere":{"disabled":[]}}]});
    assert!(tools(&value).is_none());
    assert!(reviewer(&json!({"models":[],"notices":[],"selected":7})).is_none());
    assert!(tools(&json!({"groups":[{"group":{"mcp":"server"},"project":{"enabled":7,"disabled":[]},"everywhere":{"disabled":[]}}]})).is_none());
    assert!(settings(&json!({"rows":[{"key":"x","layer":"default","file":7,"shown":"unset","scope":"global_only"}]})).is_none());
    assert!(settings(&json!({"rows":[{"key":"x","layer":"global","shown":"union","names":[],"own":[["invalid","[]"]],"scope":"global_only"}]})).is_none());
}

#[test]
fn answer_validation_preserves_rejections_and_names_malformed_answers() {
    let line = |kind: &str, payload: Value| {
        serde_json::from_value::<HubLine>(
            json!({"kind":kind,"ts":1,"schema_version":1,"payload":payload}),
        )
        .unwrap()
    };
    assert_eq!(
        result(&line("command_accepted", json!({})), "unit").unwrap(),
        Value::Null
    );
    assert_eq!(
        result(
            &line("command_accepted", json!({"result":{"text":"ok"}})),
            "skill_text"
        )
        .unwrap(),
        json!({"text":"ok"})
    );
    assert_eq!(
        result(
            &line(
                "command_rejected",
                json!({"code":"invalid_arguments","message":"refused"})
            ),
            "set"
        )
        .unwrap_err(),
        ConfigureError {
            code: ErrorCode::InvalidArguments,
            message: "refused".into()
        }
    );
    assert_eq!(
        result(
            &line(
                "command_rejected",
                json!({"code":"future_code","message":"new"})
            ),
            "set"
        )
        .unwrap_err()
        .code,
        ErrorCode::Other("future_code".into())
    );
    for malformed in [
        line("command_rejected", json!({"code":"io_failed"})),
        line("unrelated", json!({"result":{}})),
    ] {
        assert_eq!(
            result(&malformed, "settings").unwrap_err(),
            io_error("settings")
        );
    }
}

#[test]
fn setting_rows_preserve_all_shown_and_scope_forms() {
    let input = json!({"rows":[
        {"key":"a","layer":"default","shown":"unset","scope":"global_only"},
        {"key":"b","layer":"global","file":"/hub/config.json","shown":"value","value":"true","scope":"any","repo":true},
        {"key":"c","layer":"repository","shown":"redacted","value":"hidden","scope":"repo_only"},
        {"key":"d","layer":"project","shown":"union","names":[["one","global"]],"own":[["global","[]"],["project","[1]"],["repository","[2]"]],"scope":"person_files"},
        {"key":"e","layer":"default","shown":"unset","scope":"any","repo":false}
    ]});
    let rows = settings(&input).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].value, Shown::Unset);
    assert_eq!(rows[0].scope, WriteScope::GlobalOnly);
    assert_eq!(rows[0].file, None);
    assert_eq!(rows[1].value, Shown::Value("true".into()));
    assert_eq!(rows[1].scope, WriteScope::Any { repo: true });
    assert_eq!(rows[1].file, Some("/hub/config.json".into()));
    assert_eq!(rows[2].value, Shown::Redacted("hidden".into()));
    assert_eq!(rows[2].scope, WriteScope::RepoOnly);
    assert_eq!(rows[3].scope, WriteScope::PersonFiles);
    assert_eq!(
        rows[3].value,
        Shown::Union {
            names: vec![("one".into(), "global".into())],
            own: vec![
                (Layer::Global, "[]".into()),
                (Layer::Project, "[1]".into()),
                (Layer::Repository, "[2]".into())
            ]
        }
    );
    assert_eq!(rows[4].scope, WriteScope::Any { repo: false });
    for (field, bad) in [
        ("shown", json!("other")),
        ("scope", json!("other")),
        ("repo", json!(null)),
        ("key", json!(7)),
        ("layer", json!(null)),
    ] {
        let mut broken = input.clone();
        broken["rows"][1][field] = bad;
        assert!(settings(&broken).is_none(), "malformed {field}");
    }
}

#[test]
fn rules_tools_targets_stores_and_reviewer_answers() {
    let parsed = rules(&json!({"global":{"file":"/hub/rules","rows":[]},"project":{"file":"/hub/project/rules","error":"bad rules"}})).unwrap();
    assert_eq!(parsed.0.rows, Ok(vec![]));
    assert_eq!(parsed.1.rows, Err("bad rules".into()));
    assert_eq!(parsed.1.file, PathBuf::from("/hub/project/rules"));
    let groups=tools(&json!({"groups":[{"group":{"mcp":"server"},"project":{"enabled":[],"disabled":["x"]},"everywhere":{"disabled":[]}},{"group":{"extension":"acme/ext"},"project":{"disabled":[]},"everywhere":{"enabled":["y"],"disabled":[]}}]})).unwrap();
    assert_eq!(groups[0].group, ToolGroup::Mcp("server".into()));
    assert_eq!(groups[0].project.enabled, Some(vec![]));
    assert_eq!(groups[0].project.disabled, ["x"]);
    assert_eq!(groups[0].everywhere.enabled, None);
    assert_eq!(groups[1].group, ToolGroup::Extension("acme/ext".into()));
    assert_eq!(groups[1].everywhere.enabled, Some(vec!["y".into()]));
    let login_targets=targets(&json!({"targets":[{"name":"a","kind":"key"},{"name":"b","kind":"browser"},{"name":"c","kind":"secret"}]})).unwrap();
    assert_eq!(
        login_targets.iter().map(|t| t.kind).collect::<Vec<_>>(),
        [LoginKind::Key, LoginKind::Browser, LoginKind::Secret]
    );
    assert_eq!(login_targets[0].name, "a");
    assert_eq!(
        stored(&json!({"path":"credentials/a/default","replaced":true})).unwrap(),
        Stored {
            path: "credentials/a/default".into(),
            replaced: true
        }
    );
    for selected in [None, Some("m1")] {
        let mut value = json!({"models":["m1"],"notices":["notice"]});
        if let Some(selected) = selected {
            value["selected"] = json!(selected);
        }
        assert_eq!(
            reviewer(&value).unwrap(),
            ReviewerModels {
                models: vec!["m1".into()],
                selected: selected.map(str::to_owned),
                notices: vec!["notice".into()]
            }
        );
    }
    assert_eq!(
        revoked(&json!({"revoked":"removed"})),
        Some(Revoked::Removed)
    );
    assert_eq!(revoked(&json!({"revoked":"stale"})), Some(Revoked::Stale));
    assert!(revoked(&json!({"revoked":"other"})).is_none());
    assert_eq!(
        disabled(&json!({"project":["a"],"everywhere":["b"]})).unwrap(),
        SkillsDisabled {
            project: vec!["a".into()],
            everywhere: vec!["b".into()]
        }
    );
    assert!(settings(&json!({})).is_none());
    assert!(tools(&json!({"groups":[{"group":{},"project":{},"everywhere":{}}]})).is_none());
    assert!(targets(&json!({"targets":[{"name":"a","kind":"unknown"}]})).is_none());
}
