//! The offer: what is shown for each item, and the diff of a changed one.

use std::fs;

use config::ProjectKey;
use contract::events::{OfferedItem, OfferedKind};
use serde_json::json;

use super::declared_tests::Repo;
use super::{Index, Store, hash, pending};

fn store(repo: &Repo) -> Store {
    Store::new(&repo.home(), &ProjectKey::new("-p").unwrap())
}

fn offered(repo: &Repo) -> Vec<OfferedItem> {
    pending(&store(repo), &mut Index::scratch(), repo.items())
        .unwrap()
        .into_iter()
        .map(|p| p.offered)
        .collect()
}

fn approve_all(repo: &Repo) {
    let store = store(repo);
    for item in repo.items() {
        let hash = hash(&mut Index::scratch(), &item).unwrap();
        store.approve(&item, &hash).unwrap();
    }
}

fn lines(item: &OfferedItem) -> Vec<&str> {
    item.summary.lines().collect()
}

#[test]
fn an_extension_summary_shows_what_an_install_shows_and_where_it_is() {
    let repo = Repo::new();
    repo.package(
        "tools/acme",
        "github.com/acme/fiber-acme",
        &json!({
            "replaces": ["shell", "web_search"],
            "install": ["npm", "ci"],
            "process": {"program": "node", "args": ["server.js"]},
            "prompt": "prompt.md",
        }),
    );
    repo.write("tools/acme/skills/deploy/SKILL.md", "s");
    repo.write("tools/acme/themes/dark.json", "{}");
    repo.write(
        "tools/acme/providers/acme.json",
        &json!({"name": "acme", "models": [
            {"id": "m1", "protocol": "openai-completions", "base_url": "https://api.acme.dev/v1"},
            {"id": "m2", "protocol": "openai-completions", "base_url": "https://api.acme.dev/v1"},
            {"id": "m3", "protocol": "openai-completions", "base_url": "https://eu.acme.dev/v1"},
        ]})
        .to_string(),
    );
    repo.config(&json!({"repository_extensions": [{"path": "tools/acme", "required": true}]}));
    let [item] = offered(&repo).try_into().unwrap();
    assert_eq!(item.kind, OfferedKind::Extension);
    assert_eq!(item.name, "github.com/acme/fiber-acme");
    assert_eq!(item.version.as_deref(), Some("v1.0.0"));
    assert!(item.required);
    assert!(item.diff.is_none());
    let lines = lines(&item);
    for expected in [
        "name: github.com/acme/fiber-acme",
        "from: this repository",
        "version: v1.0.0",
        "path in the repository: tools/acme",
        "loads in this project only",
        "replaces `shell`",
        "replaces `web_search`",
        "registers provider `acme` at `https://api.acme.dev/v1`, `https://eu.acme.dev/v1`",
        "runs the program: node server.js",
        "install step: npm ci, which runs its dependencies' own install scripts too",
        "skills: deploy",
        "themes: dark.json",
        "system prompt text: prompt.md",
        "the repository marks it required",
    ] {
        assert!(lines.contains(&expected), "{expected}\n{}", item.summary);
    }
}

#[test]
fn a_lua_extension_shows_its_memory_cap_only_above_one_mib() {
    let repo = Repo::new();
    repo.package("a", "fiber.test/a", &json!({"memory_mib": 8}));
    repo.package("b", "fiber.test/b", &json!({"memory_mib": 1}));
    repo.config(&json!({"repository_extensions": [{"path": "a"}, {"path": "b"}]}));
    let items = offered(&repo);
    assert!(lines(&items[0]).contains(&"memory cap: 8 MiB"));
    assert!(!items[1].summary.contains("memory cap"));
    assert!(!items[1].summary.contains("replaces"));
    assert!(!items[1].summary.contains("install step"));
    assert!(!items[1].summary.contains("provider"));
}

#[test]
fn a_hook_and_a_server_summary_name_what_runs_and_what_is_pinned() {
    let repo = Repo::new();
    repo.write("scripts/warn.sh", "x");
    repo.hooks(&json!({"warn": {"point": "session_start", "tools": ["edit"], "command": "scripts/warn.sh", "args": ["now"]}}));
    repo.config(&json!({"mcp": {"servers": {
        "db": {"command": "node", "args": ["srv.js", "--ro"], "required": true},
        "web": {"url": "https://mcp.example/x"},
    }}}));
    repo.write("srv.js", "x");
    let items = offered(&repo);
    let names: Vec<_> = items.iter().map(|i| (i.kind, i.name.as_str())).collect();
    assert_eq!(
        names,
        [
            (OfferedKind::Hook, "warn"),
            (OfferedKind::McpServer, "db"),
            (OfferedKind::McpServer, "web")
        ]
    );
    assert_eq!(
        lines(&items[0]),
        [
            "hook: warn",
            "declared in: .fiber/config/hooks.json",
            "point: session_start",
            "tools: edit",
            "runs: scripts/warn.sh now",
            "pins: scripts/warn.sh"
        ]
    );
    assert_eq!(
        lines(&items[1]),
        [
            "MCP server: db",
            "declared in: .fiber/config.json",
            "runs: node srv.js --ro",
            "pins: srv.js",
            "the repository marks it required"
        ]
    );
    assert_eq!(
        lines(&items[2]),
        [
            "MCP server: web",
            "declared in: .fiber/config.json",
            "url: https://mcp.example/x",
            "pins no file of the repository"
        ]
    );
    assert!(items.iter().all(|i| i.version.is_none()));
}

#[test]
fn an_item_that_resolved_outside_the_repository_is_named_as_not_pinned() {
    let repo = Repo::new();
    let outside = repo.elsewhere("elsewhere/x.sh", "x");
    repo.link("run.sh", &outside);
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "run.sh", "args": []}}}}));
    repo.write("other.sh", "o");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "./run.sh", "args": ["other.sh"]}}}}),
    );
    let [item] = offered(&repo).try_into().unwrap();
    assert!(
        lines(&item).contains(&"not pinned: ./run.sh (outside the repository)"),
        "{}",
        item.summary
    );
    assert!(lines(&item).contains(&"pins: other.sh"));
}

#[test]
fn an_approved_item_is_not_offered_and_a_never_is() {
    let repo = Repo::new();
    repo.write("scripts/a.sh", "a");
    repo.hooks(&json!({"a": {"point": "x", "command": "scripts/a.sh"}, "b": {"point": "y", "command": "scripts/a.sh"}}));
    let store = store(&repo);
    let a = repo.item(OfferedKind::Hook, "a");
    let b = repo.item(OfferedKind::Hook, "b");
    let (hash_a, hash_b) = (
        hash(&mut Index::scratch(), &a).unwrap(),
        hash(&mut Index::scratch(), &b).unwrap(),
    );
    assert_eq!(offered(&repo).len(), 2);
    store.approve(&a, &hash_a).unwrap();
    store.never(&b, &hash_b).unwrap();
    let left = offered(&repo);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "b");
    assert_eq!(left[0].hash, hash_b);
}

#[test]
fn a_changed_hook_carries_the_declaration_diff_and_the_file_diff() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "line one\nline two\nline three\n");
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 100}}),
    );
    approve_all(&repo);
    assert!(offered(&repo).is_empty());

    repo.write("scripts/fmt.sh", "line one\nline 2\nline three\n");
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 200}}),
    );
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(
        diff.starts_with("--- a/declaration\n+++ b/declaration\n"),
        "{diff}"
    );
    assert!(
        diff.contains("-    \"timeout\": 100\n+    \"timeout\": 200\n"),
        "{diff}"
    );
    assert!(
        diff.contains("--- a/scripts/fmt.sh\n+++ b/scripts/fmt.sh\n"),
        "{diff}"
    );
    assert!(diff.contains("-line two\n+line 2\n"), "{diff}");
    // The old approval and its copy are still there.
    let copies = fs::read_dir(repo.home().join("pinned"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .count();
    assert_eq!(copies, 1);
}

#[test]
fn files_added_and_removed_show_whole() {
    let repo = Repo::new();
    repo.write("a.sh", "old a\n");
    repo.write("b.sh", "kept b\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh", "args": ["b.sh"]}}}}));
    approve_all(&repo);
    fs::remove_file(repo.root().join("a.sh")).unwrap();
    repo.write("c.sh", "new c\n");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "./a.sh", "args": ["b.sh", "c.sh"]}}}}),
    );
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(
        diff.contains("--- a/a.sh\n+++ /dev/null\n@@ -1 +0,0 @@\n-old a\n"),
        "{diff}"
    );
    assert!(
        diff.contains("--- /dev/null\n+++ b/c.sh\n@@ -0,0 +1 @@\n+new c\n"),
        "{diff}"
    );
    assert!(!diff.contains("a/b.sh"), "{diff}");
}

#[test]
fn a_file_that_is_not_text_shows_that_it_changed() {
    let repo = Repo::new();
    fs::write(repo.root().join("blob"), [0xff_u8, 0xfe, 0x00]).unwrap();
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./blob"}}}}));
    approve_all(&repo);
    fs::write(repo.root().join("blob"), [0xff_u8, 0xfe, 0x01]).unwrap();
    let [item] = offered(&repo).try_into().unwrap();
    assert_eq!(item.diff.as_deref(), Some("blob: binary file changed\n"));
}

#[test]
fn a_large_text_file_shows_its_whole_diff() {
    let repo = Repo::new();
    let big = "x\n".repeat(700_000);
    repo.write("big.txt", &big);
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./big.txt"}}}}));
    approve_all(&repo);
    repo.write("big.txt", &big.replacen("x\n", "z\n", 1));
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(
        diff.contains("-x\n+z\n"),
        "{}",
        &diff[..diff.len().min(300)]
    );
}

#[test]
fn an_empty_file_added_or_removed_is_named() {
    let repo = Repo::new();
    repo.write("a.sh", "a\n");
    repo.write("empty", "");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh"}}}}));
    approve_all(&repo);
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh", "args": ["empty"]}}}}));
    let [item] = offered(&repo).try_into().unwrap();
    assert!(item.diff.unwrap().contains("empty: empty file added\n"));
    approve_all(&repo);
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh"}}}}));
    // Back to the first version, approved: switch the other way instead.
    repo.write("a.sh", "b\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh", "args": ["empty"]}}}}));
    approve_all(&repo);
    repo.write("a.sh", "c\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./a.sh"}}}}));
    let [item] = offered(&repo).try_into().unwrap();
    assert!(item.diff.unwrap().contains("empty: empty file removed\n"));
}

#[test]
fn a_changed_execute_bit_alone_shows_in_the_diff() {
    let repo = Repo::new();
    repo.write("run.sh", "x\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./run.sh"}}}}));
    approve_all(&repo);
    repo.executable("run.sh");
    let [item] = offered(&repo).try_into().unwrap();
    assert_eq!(item.diff.as_deref(), Some("run.sh: execute bit set\n"));
    // Back to the version approved first: nothing to offer.
    fs::set_permissions(
        repo.root().join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    assert!(offered(&repo).is_empty());
}

#[test]
fn a_cleared_execute_bit_shows_in_the_diff() {
    let repo = Repo::new();
    repo.write("run.sh", "x\n");
    repo.executable("run.sh");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./run.sh"}}}}));
    approve_all(&repo);
    fs::set_permissions(
        repo.root().join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let [item] = offered(&repo).try_into().unwrap();
    assert_eq!(item.diff.as_deref(), Some("run.sh: execute bit cleared\n"));
}

#[test]
fn a_changed_extension_shows_added_changed_and_removed_package_files() {
    let repo = Repo::new();
    repo.package(
        "pkg",
        "fiber.test/p",
        &json!({"install": ["sh", "-c", "mkdir -p node_modules && echo dep > node_modules/dep"]}),
    );
    repo.write("pkg/gone.lua", "gone\n");
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    approve_all(&repo);
    assert!(offered(&repo).is_empty());

    fs::remove_file(repo.root().join("pkg/gone.lua")).unwrap();
    repo.write("pkg/init.lua", "-- entry v2\n");
    repo.write("pkg/new.lua", "fresh\n");
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(diff.contains("--- a/gone.lua\n+++ /dev/null\n"), "{diff}");
    assert!(
        diff.contains("-- a/init.lua") || diff.contains("--- a/init.lua\n+++ b/init.lua\n"),
        "{diff}"
    );
    assert!(diff.contains("+-- entry v2\n"), "{diff}");
    assert!(diff.contains("--- /dev/null\n+++ b/new.lua\n"), "{diff}");
    // What the install step built is not a package file.
    assert!(!diff.contains("node_modules"), "{diff}");
}

#[test]
fn a_new_item_with_no_earlier_version_has_no_diff() {
    let repo = Repo::new();
    repo.write("run.sh", "x\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./run.sh"}}}}));
    let [item] = offered(&repo).try_into().unwrap();
    assert!(item.diff.is_none());
}

#[test]
fn binary_files_added_removed_or_changed_only_in_mode_say_so() {
    let repo = Repo::new();
    fs::write(repo.root().join("kept"), [0xff_u8, 0x00]).unwrap();
    fs::write(repo.root().join("gone"), [0xfe_u8, 0x00]).unwrap();
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./kept", "args": ["gone"]}}}}));
    approve_all(&repo);
    fs::remove_file(repo.root().join("gone")).unwrap();
    fs::write(repo.root().join("new"), [0xfd_u8, 0x00]).unwrap();
    repo.executable("kept");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "./kept", "args": ["gone", "new"]}}}}),
    );
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(diff.contains("gone: binary file removed\n"), "{diff}");
    assert!(diff.contains("new: binary file added\n"), "{diff}");
    assert!(diff.contains("kept: execute bit set\n"), "{diff}");
    assert!(!diff.contains("kept: binary file"), "{diff}");
}

#[test]
fn a_package_file_named_like_something_the_install_step_built_is_added() {
    let repo = Repo::new();
    repo.package(
        "pkg",
        "fiber.test/p",
        &json!({"install": ["sh", "-c", "echo built > generated.txt"]}),
    );
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    approve_all(&repo);
    repo.write("pkg/generated.txt", "committed\n");
    let [item] = offered(&repo).try_into().unwrap();
    let diff = item.diff.unwrap();
    assert!(
        diff.contains("--- /dev/null\n+++ b/generated.txt\n"),
        "{diff}"
    );
    assert!(!diff.contains("-built"), "{diff}");
}

#[test]
fn a_watcher_and_a_phase_show_in_a_hook_summary() {
    let repo = Repo::new();
    repo.hooks(&json!({"done": {"watch": ["turn_completed", "tool_call_completed"], "phase": "early", "command": "notify"}}));
    let [item] = offered(&repo).try_into().unwrap();
    let lines = lines(&item);
    assert!(
        lines.contains(&"watch: turn_completed, tool_call_completed"),
        "{}",
        item.summary
    );
    assert!(lines.contains(&"phase: early"), "{}", item.summary);
    assert!(lines.contains(&"runs: notify"), "{}", item.summary);
}
