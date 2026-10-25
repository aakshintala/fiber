//! Loader and picker boundary tests (`docs/tui.md`, extension loading).
use super::*;
use fakes::clock::FakeClock;

fn fixture() -> (fakes::TempDir, Loader) {
    let dir = fakes::TempDir::new("fiber-tui-loader");
    let sources = config::Sources {
        home: dir.path().join("home"),
        workspace: dir.path().join("workspace"),
        project: config::ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    };
    std::fs::create_dir_all(&sources.home).unwrap();
    std::fs::create_dir_all(&sources.workspace).unwrap();
    (dir, Loader::new(sources, FakeClock::new()))
}
fn package(
    loader: &Loader,
    name: &str,
    range: Option<Value>,
    source: Option<&str>,
) -> std::path::PathBuf {
    let dir = config::extensions_dir(&loader.sources.home).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = json!({"name":name,"version":"v3.0.0","fiber":"0.1.0","api":1});
    if let Some(range) = range {
        manifest["tui"] = json!({"session_versions":range});
    }
    std::fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    if let Some(source) = source {
        std::fs::create_dir_all(dir.join("tui")).unwrap();
        std::fs::write(dir.join("tui/init.lua"), source).unwrap();
    }
    dir
}
const SCRIPT: &str = "fiber.slot('ledger_row', {timeout=100, render=function(input,width) return {{{text='custom'}}} end})";
fn ctx(half: Option<Vec<(&str, &str)>>) -> SessionCtx {
    SessionCtx {
        session_id: contract::SessionId("s".into()),
        halves: half.map(|pairs| {
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect()
        }),
    }
}

fn render(renderer: &dyn Renderer, ctx: &SessionCtx) -> Decision {
    let args = json!({});
    let row = tui::ext::LedgerRow {
        name: "shell",
        arguments: &args,
        status: Status::Completed,
        exit: Some(0),
        duration_ms: Some(1),
        lines: 1,
        changes: &[],
        error: None,
        output: "",
        answer: None,
        flat: false,
    };
    renderer.render(
        ctx,
        "ledger_row",
        &Input::LedgerRow(row),
        80,
        &|_, _| vec![],
    )
}
#[test]
fn client_only_receives_each_session_and_reload_rereads_source() {
    let (_dir, loader) = fixture();
    let dir = package(
        &loader,
        "a",
        None,
        Some(
            "fiber.slot('ledger_row',{timeout=100,render=function(input) return {{{text=input.session_id}}} end})",
        ),
    );
    let (renderer, notices) = loader.load();
    assert!(notices.is_empty());
    for session in ["parent", "delegate"] {
        let mut ctx = ctx(Some(vec![]));
        ctx.session_id = contract::SessionId(session.into());
        let Decision::Draw { lines, .. } = render(&*renderer, &ctx) else {
            panic!("client-only draws")
        };
        assert_eq!(lines[0][0].text, session);
    }
    std::fs::write(dir.join("tui/init.lua"), SCRIPT).unwrap();
    let (renderer, _) = loader.load();
    let Decision::Draw { lines, .. } = render(&*renderer, &ctx(None)) else {
        panic!("rebuilt")
    };
    assert_eq!(lines[0][0].text, "custom");
}
#[test]
fn compatible_boundaries_and_zero_contender_version_notices_are_deduplicated() {
    let (_dir, loader) = fixture();
    package(
        &loader,
        "a",
        Some(json!({"min":"v1.2.0","before":"v2.0.0"})),
        Some(SCRIPT),
    );
    let (renderer, _) = loader.load();
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Absent
    );
    assert!(renderer.take_pending().is_empty());
    for version in ["v1.2.0", "v1.9.9"] {
        assert_eq!(
            renderer.fills_for(&ctx(Some(vec![("a", version)])), "ledger_row"),
            SlotSelection::Draw {
                extension: "a".into()
            }
        );
    }
    for version in ["v1.1.9", "v2.0.0", "not-a-tag"] {
        for _ in 0..2 {
            assert_eq!(
                renderer.fills_for(&ctx(Some(vec![("a", version)])), "ledger_row"),
                SlotSelection::Absent
            );
        }
        let notices = renderer.take_pending();
        // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, "extension_incompatible");
        assert!(notices[0].1.contains("v3.0.0"));
        assert!(notices[0].1.contains(version));
    }
    for _ in 0..2 {
        renderer.fills_for(&ctx(Some(vec![])), "ledger_row");
    }
    let notices = renderer.take_pending();
    // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
    assert_eq!(notices.len(), 1);
    assert!(notices[0].1.contains("not loaded"));
    let (renderer, _) = loader.load();
    renderer.fills_for(&ctx(Some(vec![])), "ledger_row");
    // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
    assert_eq!(
        renderer.take_pending().len(),
        1,
        "reload resets version notice cadence"
    );
}
#[test]
fn invalid_winner_single_contender_and_compatible_rival_win_silently() {
    let (_dir, loader) = fixture();
    package(
        &loader,
        "a",
        Some(json!({"min":"v1.0.0","before":"v2.0.0"})),
        Some(SCRIPT),
    );
    package(&loader, "b", None, Some(SCRIPT));
    for winner in ["a", "missing", "disabled"] {
        std::fs::write(
            loader.sources.home.join("config.json"),
            json!({"tui":{"slots":{"ledger_row":winner}}}).to_string(),
        )
        .unwrap();
        let (renderer, _) = loader.load();
        assert_eq!(
            renderer.fills_for(&ctx(Some(vec![("a", "v2.0.0")])), "ledger_row"),
            SlotSelection::Draw {
                extension: "b".into()
            }
        );
        let notices = renderer.take_pending();
        // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, "extension_incompatible");
    }
}
#[test]
fn changed_compatible_sets_and_configured_values_get_new_conflict_notices() {
    let (_dir, loader) = fixture();
    package(&loader, "a", None, Some(SCRIPT));
    package(&loader, "b", None, Some(SCRIPT));
    package(&loader, "c", Some(json!({"min":"v1.0.0"})), Some(SCRIPT));
    let (renderer, _) = loader.load();
    for state in [ctx(None), ctx(Some(vec![("c", "v1.0.0")]))] {
        for _ in 0..2 {
            assert_eq!(
                renderer.fills_for(&state, "ledger_row"),
                SlotSelection::Suppressed
            );
        }
        let notices = renderer.take_pending();
        // docs/tui.md:2757: one command_conflict per slot and set of contenders until /reload.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, "command_conflict");
    }
    // Exercise the key itself on one composite, without resetting the notice sets.
    let mut composite = Composite::default();
    let config = config::Config::load(loader.sources.clone()).unwrap();
    for name in ["a", "b"] {
        let dir = config::extensions_dir(&loader.sources.home).join(name);
        composite.candidates.insert(
            name.into(),
            load_one(&dir, &config, &loader.clock).unwrap().unwrap(),
        );
    }
    for value in ["bad-one", "bad-two"] {
        composite
            .configured
            .insert("ledger_row".into(), value.into());
        assert_eq!(
            composite.fills_for(&ctx(None), "ledger_row"),
            SlotSelection::Suppressed
        );
        let notices = composite.take_pending();
        // docs/tui.md:2757: one command_conflict per slot and set of contenders until /reload.
        assert_eq!(notices.len(), 1);
        assert!(notices[0].1.contains(value));
    }
}
#[test]
fn config_reload_error_keeps_existing_error_code_and_empty_renderer() {
    let (_dir, loader) = fixture();
    package(&loader, "a", None, Some(SCRIPT));
    std::fs::write(loader.sources.home.join("config.json"), "{").unwrap();
    let (renderer, notices) = loader.load();
    // docs/tui.md:1233: an event notice reports its failure once.
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "config_invalid");
    assert!(!renderer.fills("ledger_row"));
}
#[test]
fn runtime_and_wrong_shape_callbacks_keep_typed_hook_failure_identity() {
    for source in [
        "fiber.slot('ledger_row',{timeout=100,render=function() error('fixture') end})",
        "fiber.slot('ledger_row',{timeout=100,render=function() return false end})",
    ] {
        let (_dir, loader) = fixture();
        package(&loader, "a", None, Some(source));
        let (renderer, _) = loader.load();
        let Decision::Fail(failure) = render(&*renderer, &ctx(None)) else {
            panic!("callback fails")
        };
        assert_eq!(failure.kind, FailureKind::HookFailed);
        assert_eq!(failure.extension, "a");
        assert!(
            renderer.fills("ledger_row"),
            "non-timeout errors do not disable the VM"
        );
    }
}

#[test]
fn render_deadline_reports_typed_extension_failure_and_switches_off() {
    // The binary cases keep the screen fallback; this one forces the
    // renderer deadline on the injected clock, with no wall-clock waits.
    let dir = fakes::TempDir::new("fiber-tui-render-timeout");
    let sources = config::Sources {
        home: dir.path().join("home"),
        workspace: dir.path().join("workspace"),
        project: config::ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    };
    std::fs::create_dir_all(&sources.home).unwrap();
    std::fs::create_dir_all(&sources.workspace).unwrap();
    let clock = FakeClock::new();
    let loader = Loader::new(sources, clock.clone());
    package(
        &loader,
        "slow",
        None,
        Some(
            "fiber.slot('ledger_row',{timeout=100,render=function() builtin() while true do end end})",
        ),
    );
    let (renderer, notices) = loader.load();
    assert!(notices.is_empty());
    let args = json!({});
    let row = tui::ext::LedgerRow {
        name: "shell",
        arguments: &args,
        status: Status::Completed,
        exit: Some(0),
        duration_ms: Some(1),
        lines: 1,
        changes: &[],
        error: None,
        output: "",
        answer: None,
        flat: false,
    };
    let input = Input::LedgerRow(row);
    // The clock moves only inside the render callback, once the renderer
    // runs: past its declared timeout the render is a typed failure.
    let over = |_: &Input<'_>, _: u16| {
        clock.advance(std::time::Duration::from_millis(100));
        Vec::new()
    };
    let Decision::Fail(failure) = renderer.render(&ctx(None), "ledger_row", &input, 80, &over)
    else {
        panic!("overdue render fails")
    };
    assert_eq!(failure.kind, FailureKind::ExtensionFailed);
    assert_eq!(failure.extension, "slow");
    // Switched off until rebuild: selection drops the timed-out VM with no
    // new notice, and a later render never reaches it, so neither a hook
    // failure nor a second typed failure can follow, without moving the
    // clock again.
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Absent
    );
    assert!(renderer.take_pending().is_empty());
    assert!(!renderer.fills("ledger_row"));
    let Decision::NotForSession { .. } =
        renderer.render(&ctx(None), "ledger_row", &input, 80, &|_, _| vec![])
    else {
        panic!("off renderer stays inert")
    };
}

#[test]
fn compatible_first_selection_and_notice_cadence() {
    let (_dir, loader) = fixture();
    package(
        &loader,
        "a",
        Some(json!({"min":"v1.0.0","before":"v2.0.0"})),
        Some(SCRIPT),
    );
    package(&loader, "b", None, Some(SCRIPT));
    let (renderer, notices) = loader.load();
    assert!(notices.is_empty());
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Draw {
            extension: "b".into()
        }
    );
    assert!(renderer.take_pending().is_empty());
    for _ in 0..2 {
        assert_eq!(
            renderer.fills_for(&ctx(Some(vec![])), "ledger_row"),
            SlotSelection::Draw {
                extension: "b".into()
            }
        );
    }
    let notices = renderer.take_pending();
    // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "extension_incompatible");
    assert!(notices[0].1.contains("v3.0.0"));
    assert!(notices[0].1.contains("not loaded"));
    let both = ctx(Some(vec![("a", "v1.0.0")]));
    assert_eq!(
        renderer.fills_for(&both, "ledger_row"),
        SlotSelection::Suppressed
    );
    assert_eq!(
        renderer.fills_for(&both, "ledger_row"),
        SlotSelection::Suppressed
    );
    let notices = renderer.take_pending();
    // docs/tui.md:2757: one command_conflict per slot and set of contenders until /reload.
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "command_conflict");
    assert!(notices[0].1.contains("a, b"));
    assert_eq!(
        renderer.fills_for(&ctx(Some(vec![("a", "v2.0.0")])), "ledger_row"),
        SlotSelection::Draw {
            extension: "b".into()
        }
    );
    // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
    assert_eq!(renderer.take_pending().len(), 1);
    assert_eq!(
        renderer.fills_for(
            &ctx(Some(vec![("a", "v1.0.0-nightly.20260101")])),
            "ledger_row"
        ),
        SlotSelection::Draw {
            extension: "b".into()
        }
    );
}

#[test]
fn reload_rereads_winner_and_enabled_files_and_empty_composite() {
    let (_dir, loader) = fixture();
    package(&loader, "a", None, Some(SCRIPT));
    package(&loader, "b", None, Some(SCRIPT));
    let write = |value: Value| {
        std::fs::write(loader.sources.home.join("config.json"), value.to_string()).unwrap()
    };
    write(json!({"tui":{"slots":{"ledger_row":"a"}}}));
    let (renderer, notices) = loader.load();
    assert!(notices.is_empty());
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Draw {
            extension: "a".into()
        }
    );
    write(json!({"tui":{"slots":{"ledger_row":"b"}}}));
    let (renderer, _) = loader.load();
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Draw {
            extension: "b".into()
        }
    );
    write(json!({"tui":{"slots":{"ledger_row":"missing"}}}));
    let (renderer, _) = loader.load();
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Suppressed
    );
    let notices = renderer.take_pending();
    assert!(notices[0].1.contains("tui.slots.\"ledger_row\""));
    assert!(notices[0].1.contains("`missing`"));
    write(json!({"extensions":{"a":{"enabled":false},"b":{"enabled":false}}}));
    let (renderer, notices) = loader.load();
    assert!(notices.is_empty());
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Absent
    );
}

#[test]
fn declared_half_load_failures_and_absent_optional_half() {
    for failure in [
        "absent",
        "missing",
        "unreadable",
        "manifest",
        "compile",
        "runtime",
    ] {
        let (_dir, loader) = fixture();
        let dir = package(&loader, "a", None, None);
        if failure != "absent" {
            std::fs::create_dir_all(dir.join("tui")).unwrap();
        }
        match failure {
            "unreadable" => std::fs::create_dir(dir.join("tui/init.lua")).unwrap(),
            "manifest" => std::fs::write(dir.join("extension.json"), "{").unwrap(),
            "compile" => std::fs::write(dir.join("tui/init.lua"), "this is not lua").unwrap(),
            "runtime" => std::fs::write(dir.join("tui/init.lua"), "error('init')").unwrap(),
            _ => {}
        }
        let (renderer, notices) = loader.load();
        assert_eq!(
            renderer.fills_for(&ctx(None), "ledger_row"),
            SlotSelection::Absent
        );
        // docs/tui.md:1233: an event notice reports its failure once.
        assert_eq!(notices.len(), usize::from(failure != "absent"), "{failure}");
        if failure != "absent" {
            assert_eq!(notices[0].0, "extension_failed");
        }
    }
}

#[cfg(unix)]
#[test]
fn entrypoint_symlink_cannot_execute_outside_package() {
    let (dir, loader) = fixture();
    let package = package(&loader, "a", None, None);
    std::fs::create_dir(package.join("tui")).unwrap();
    let outside = dir.path().join("outside.lua");
    std::fs::write(&outside, SCRIPT).unwrap();
    std::os::unix::fs::symlink(outside, package.join("tui/init.lua")).unwrap();
    let (renderer, notices) = loader.load();
    // docs/tui.md:1233: an event notice reports its failure once.
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "extension_failed");
    assert_eq!(
        renderer.fills_for(&ctx(None), "ledger_row"),
        SlotSelection::Absent
    );
}
