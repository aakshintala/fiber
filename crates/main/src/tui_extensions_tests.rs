//! Loader and picker boundary tests (`docs/tui.md`, extension loading).
use super::*;
use fakes::clock::FakeClock;
use tui::ext::{AnswerDisposition, LedgerResult, TurnCardState, TurnGroupState};

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
        session_id: Some(contract::SessionId("s".into())),
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
        state: CallState::Completed(LedgerResult {
            exit: Some(0),
            duration_ms: Some(1),
            lines: 1,
            changes: &[],
            output: "",
        }),
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
        ctx.session_id = Some(contract::SessionId(session.into()));
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
        state: CallState::Completed(LedgerResult {
            exit: Some(0),
            duration_ms: Some(1),
            lines: 1,
            changes: &[],
            output: "",
        }),
        answer: None,
        flat: false,
    };
    let input = Input::LedgerRow(row);
    // The clock moves only inside the render callback, once the renderer
    // runs: past its declared timeout the render is a typed failure.
    let over = |_: &Input<'_>, _: u16| {
        #[allow(
            clippy::disallowed_methods,
            reason = "no code under test waits on the clock (ruling 6179406086)"
        )]
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

fn render_slot(
    renderer: &dyn Renderer,
    ctx: &SessionCtx,
    slot: &str,
    input: &Input<'_>,
) -> Decision {
    renderer.render(ctx, slot, input, 80, &|_, _| vec![])
}

fn prompt_input<'a>(text: &'a str, images: Vec<tui::ext::PromptImage<'a>>) -> Input<'a> {
    Input::Prompt(tui::ext::Prompt {
        text,
        images,
        flat: false,
    })
}

/// The 4a tables reach Lua with the doc's keys: a prompt renderer reads
/// the text, the first image's path and the flat flag.
#[test]
fn a_prompt_renderer_reads_the_docs_keys() {
    let (_dir, loader) = fixture();
    package(
        &loader,
        "a",
        None,
        Some(
            "fiber.slot('prompt',{timeout=100,render=function(input) return {{{text=input.text .. '|' .. input.images[1].path .. '|' .. tostring(input.flat)}}} end})",
        ),
    );
    let (renderer, _) = loader.load();
    let input = prompt_input(
        "go",
        vec![tui::ext::PromptImage {
            path: "a.png",
            width: 10,
            height: 8,
        }],
    );
    let Decision::Draw { lines, .. } = render_slot(&*renderer, &ctx(None), "prompt", &input) else {
        panic!("a prompt draws")
    };
    assert_eq!(lines[0][0].text, "go|a.png|false");
}

#[test]
fn the_prompt_table_holds_text_images_and_flat() {
    let input = prompt_input(
        "go",
        vec![tui::ext::PromptImage {
            path: "a.png",
            width: 10,
            height: 8,
        }],
    );
    assert_eq!(
        input_value(&input),
        json!({"text": "go", "images": [{"path": "a.png", "width": 10, "height": 8}], "flat": false}),
    );
    // No image carries the log's type: the one-line rows never read it.
    let table = input_value(&input);
    assert!(table.get("mime_type").is_none());
    for image in table["images"].as_array().expect("images") {
        // (`docs/tui.md`, "The slots", `prompt`: each image carries `path`, `width` and `height`.)
        let mut keys = image
            .as_object()
            .expect("object")
            .keys()
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, ["height", "path", "width"]);
        assert!(image.get("mime_type").is_none());
    }
}

#[test]
fn the_reply_table_holds_text_and_flat() {
    let input = Input::Reply(tui::ext::ReplyText {
        text: "ok",
        flat: true,
    });
    assert_eq!(input_value(&input), json!({"text": "ok", "flat": true}));
}

#[test]
fn the_thinking_line_table_holds_when_and_open() {
    for ended in [None, Some(2)] {
        let input = Input::ThinkingLine(tui::ext::ThinkingLine {
            text: "plan",
            started: 1,
            ended,
            open: true,
            flat: false,
        });
        assert_eq!(
            input_value(&input),
            json!({"text": "plan", "started": 1, "ended": ended, "open": true, "flat": false}),
        );
    }
}

/// A card input holding one reply entry and running totals.
fn card() -> tui::ext::TurnCard<'static> {
    tui::ext::TurnCard {
        prompts: vec![tui::ext::CardPrompt {
            text: "go",
            images: vec![tui::ext::PromptImage {
                path: "a.png",
                width: 10,
                height: 8,
            }],
        }],
        entries: vec![tui::ext::TurnEntry::Reply { text: "ok" }],
        started: 1,
        calls: 2,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 3,
                cache_read: 4,
                cache_write: [("ephemeral".to_owned(), 5)].into_iter().collect(),
                output: 6,
            },
            cost: Some(0.5),
            subscription_cost: 1.5,
        },
        state: TurnCardState::Ended(tui::ext::TurnEnded::Failed {
            ended_ts: 7,
            error: Some(tui::ext::TurnError {
                code: contract::ErrorCode::IoFailed,
                message: "boom",
            }),
        }),
        flat: false,
    }
}

#[test]
fn the_turn_card_table_matches_the_doc() {
    let table = input_value(&Input::TurnCard(card()));
    assert_eq!(
        table,
        json!({
            "prompts": [{"text": "go", "images": [{"path": "a.png", "width": 10, "height": 8}]}],
            "entries": [{"kind": "reply", "text": "ok"}],
            "started": 1,
            "calls": 2,
            "usage": {"tokens": {"input": 3, "cache_read": 4, "cache_write": {"ephemeral": 5}, "output": 6}, "cost": 0.5, "subscription_cost": 1.5},
            "ended": {"outcome": "failed", "ended_ts": 7, "error": {"code": "io_failed", "message": "boom"}},
            "running": false,
            "flat": false,
        }),
    );
}

#[test]
fn the_turn_card_entries_groups_and_ending_follow_the_doc() {
    use tui::ext::{TurnAnswers, TurnAside, TurnBand, TurnBandState, TurnEntry};
    let entries = vec![
        TurnEntry::Reply { text: "ok" },
        TurnEntry::Steer {
            text: "wait",
            images: vec![],
            ts: 1,
        },
        TurnEntry::Aside(TurnAside::Warning { message: "down" }),
        TurnEntry::Aside(TurnAside::Orphans {
            jobs: vec![tui::ext::TurnOrphan {
                name: "job",
                message: "left",
            }],
            open: true,
        }),
        TurnEntry::Band(TurnBand {
            trigger: "automatic",
            ts: 2,
            state: TurnBandState::Unchanged { cancelled: true },
        }),
        TurnEntry::Answers(TurnAnswers::Declined),
    ];
    let table = input_value(&Input::TurnCard(tui::ext::TurnCard {
        prompts: vec![],
        entries,
        started: 0,
        calls: 0,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: None,
            subscription_cost: 0.0,
        },
        state: TurnCardState::Running,
        flat: true,
    }));
    let entries = table["entries"].as_array().expect("entries");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["kind"].as_str().expect("kind"))
            .collect::<Vec<_>>(),
        ["reply", "steer", "aside", "aside", "band", "answers"],
    );
    assert_eq!(
        entries[1],
        json!({"kind": "steer", "text": "wait", "images": [], "ts": 1}),
    );
    assert_eq!(
        entries[2],
        json!({"kind": "aside", "aside": {"kind": "warning", "message": "down"}}),
    );
    assert_eq!(
        entries[3],
        json!({"kind": "aside", "aside": {"kind": "orphans", "jobs": [{"name": "job", "message": "left"}], "open": true}}),
    );
    assert_eq!(
        entries[4],
        json!({"kind": "band", "trigger": "automatic", "ts": 2, "state": {"kind": "unchanged", "cancelled": true}}),
    );
    assert_eq!(
        entries[5],
        json!({"kind": "answers", "outcome": "declined", "parts": [], "note": null, "ts": null}),
    );
    assert_eq!(table["ended"], Value::Null);
    assert_eq!(table["running"], true);
}

#[test]
fn the_turn_card_group_entry_lists_sections_streaming_and_failed() {
    use tui::ext::{
        CallState, TurnEntry, TurnFailed, TurnGroup, TurnSection, TurnStreaming, TurnThought,
    };
    let table = input_value(&Input::TurnCard(tui::ext::TurnCard {
        prompts: vec![],
        entries: vec![TurnEntry::Group(TurnGroup {
            first: 1,
            last: 2,
            state: TurnGroupState::Running,
            open: false,
            sections: vec![TurnSection {
                step: 3,
                thoughts: vec![TurnThought {
                    text: "plan",
                    started: 4,
                    ended: None,
                    open: false,
                }],
                calls: vec![tui::ext::TurnCall {
                    name: "read",
                    arguments: &json!({"path": "a.rs"}),
                    state: CallState::Running,
                    answer: None,
                    open: true,
                    asking: false,
                }],
                failed: vec![TurnFailed {
                    code: "rate_limited",
                    attempt: 1,
                }],
            }],
            streaming: vec![TurnStreaming {
                message: "m",
                index: 0,
                name: Some("shell"),
                text: "arg",
            }],
        })],
        started: 0,
        calls: 1,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: None,
            subscription_cost: 0.0,
        },
        state: TurnCardState::Running,
        flat: false,
    }));
    let entries = table["entries"].as_array().expect("entries");
    // (`docs/tui.md`, "The slots", `turn_card`: the card's items in order, each with its `kind`.)
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0],
        json!({
            "kind": "group",
            "first": 1,
            "last": 2,
            "running": true,
            "open": false,
            "cut": false,
            "sections": [{
                "step": 3,
                "thoughts": [{"text": "plan", "started": 4, "ended": null, "open": false}],
                "calls": [{
                    "name": "read",
                    "arguments": {"path": "a.rs"},
                    "status": "running",
                    "exit": null,
                    "duration_ms": null,
                    "lines": 0,
                    "changes": [],
                    "error": null,
                    "output": "done",
                    "headline": "",
                    "content": "",
                    "answer": null,
                    "images": [],
                    "open": true,
                    "asking": false,
                }],
                "failed": [{"code": "rate_limited", "attempt": 1}],
            }],
            "streaming": [{"message": "m", "index": 0, "name": "shell", "text": "arg"}],
        }),
    );
}

/// The 4b tables reach Lua with the doc's keys: a steer renderer reads
/// the text, the first image's path and the landing time, never the
/// log's type.
#[test]
fn the_steer_table_holds_text_images_ts_and_flat() {
    let input = Input::Steer(tui::ext::Steer {
        text: "wait",
        images: vec![tui::ext::PromptImage {
            path: "a.png",
            width: 10,
            height: 8,
        }],
        ts: 1,
        flat: false,
    });
    assert_eq!(
        input_value(&input),
        json!({"text": "wait", "images": [{"path": "a.png", "width": 10, "height": 8}], "ts": 1, "flat": false}),
    );
    let table = input_value(&input);
    assert!(table.get("mime_type").is_none());
    for image in table["images"].as_array().expect("images") {
        assert!(image.get("mime_type").is_none());
    }
}

#[test]
fn the_answers_table_holds_outcome_parts_note_and_ts() {
    let given = Input::Answers(tui::ext::Answers {
        answers: tui::ext::TurnAnswers::Given {
            parts: vec![tui::ext::TurnAnswerPart {
                header: "Base",
                disposition: AnswerDisposition::Answered { text: "main" },
            }],
            note: Some("go"),
            ts: 1,
        },
        flat: false,
    });
    assert_eq!(
        input_value(&given),
        json!({"outcome": "given", "parts": [{"header": "Base", "text": "main", "skipped": false}], "note": "go", "ts": 1, "flat": false}),
    );
    let declined = Input::Answers(tui::ext::Answers {
        answers: tui::ext::TurnAnswers::Declined,
        flat: false,
    });
    assert_eq!(
        input_value(&declined),
        json!({"outcome": "declined", "parts": [], "note": null, "ts": null, "flat": false}),
    );
}

/// A skip projects the literal text the built-in row drew, and an
/// answered text that happens to read "skipped" stays an answer.
#[test]
fn skipped_answers_project_the_literal_text() {
    let skipped = Input::Answers(tui::ext::Answers {
        answers: tui::ext::TurnAnswers::Given {
            parts: vec![tui::ext::TurnAnswerPart {
                header: "Base",
                disposition: AnswerDisposition::Skipped,
            }],
            note: None,
            ts: 1,
        },
        flat: false,
    });
    assert_eq!(
        input_value(&skipped),
        json!({"outcome": "given", "parts": [{"header": "Base", "text": "skipped", "skipped": true}], "note": null, "ts": 1, "flat": false}),
    );
    let answered = Input::Answers(tui::ext::Answers {
        answers: tui::ext::TurnAnswers::Given {
            parts: vec![tui::ext::TurnAnswerPart {
                header: "Base",
                disposition: AnswerDisposition::Answered { text: "skipped" },
            }],
            note: None,
            ts: 1,
        },
        flat: false,
    });
    assert_eq!(
        input_value(&answered),
        json!({"outcome": "given", "parts": [{"header": "Base", "text": "skipped", "skipped": false}], "note": null, "ts": 1, "flat": false}),
    );
}

/// Every call state projects its documented Lua shape: no-payload
/// states carry the current defaults, terminal states carry their
/// result, a failure carries its split code and message, and a denial
/// carries its reason as the headline.
#[test]
fn every_call_state_projects_its_documented_shape() {
    use tui::ext::{CallState, LedgerResult, RowError, TurnCallResult};
    fn ledger(state: CallState<'static, LedgerResult<'static>, ()>) -> serde_json::Value {
        let args = json!({});
        input_value(&Input::LedgerRow(tui::ext::LedgerRow {
            name: "shell",
            arguments: &args,
            state,
            answer: None,
            flat: false,
        }))
    }
    fn terminal() -> LedgerResult<'static> {
        LedgerResult {
            exit: None,
            duration_ms: Some(2),
            lines: 1,
            changes: &[],
            output: "first",
        }
    }
    // Pending, running and unknown carry no result: the projection
    // keeps the current defaults.
    for (state, status) in [
        (CallState::Pending, "pending"),
        (CallState::Running, "running"),
        (CallState::Unknown, "unknown"),
    ] {
        let table = ledger(state);
        assert_eq!(table["status"], status);
        assert_eq!(table["exit"], serde_json::Value::Null);
        assert_eq!(table["duration_ms"], serde_json::Value::Null);
        assert_eq!(table["lines"], 0);
        assert_eq!(table["changes"], json!([]));
        assert_eq!(table["error"], serde_json::Value::Null);
        assert_eq!(table["output"], "done");
    }
    // A terminal call may lack its exit: the absent data stays null.
    let table = ledger(CallState::Completed(terminal()));
    assert_eq!(table["status"], "completed");
    assert_eq!(table["exit"], serde_json::Value::Null);
    assert_eq!(table["duration_ms"], 2);
    assert_eq!(table["output"], "first");
    let table = ledger(CallState::Cancelled(terminal()));
    assert_eq!(table["status"], "cancelled");
    // Both error fields empty gives an empty headline; an empty code
    // gives the message; otherwise it is "code: message".
    for (code, message, headline) in [
        ("", "", ""),
        ("", "bad key", "bad key"),
        ("io_failed", "bad key", "io_failed: bad key"),
    ] {
        let table = ledger(CallState::Failed {
            result: terminal(),
            error: RowError { code, message },
        });
        assert_eq!(table["status"], "failed");
        assert_eq!(
            table["error"],
            json!({"code": code, "message": message}),
            "code {code:?} message {message:?}"
        );
        let call = tui::ext::TurnCall {
            name: "shell",
            arguments: &json!({}),
            state: CallState::Failed {
                result: TurnCallResult {
                    exit: None,
                    duration_ms: Some(2),
                    lines: 1,
                    changes: &[],
                    output: "first",
                    content: "full",
                    images: Vec::new(),
                },
                error: RowError { code, message },
            },
            answer: None,
            open: false,
            asking: false,
        };
        let table = input_value(&Input::TurnCard(tui::ext::TurnCard {
            prompts: Vec::new(),
            entries: vec![tui::ext::TurnEntry::Group(tui::ext::TurnGroup {
                first: 1,
                last: 2,
                state: TurnGroupState::Finished,
                open: false,
                sections: vec![tui::ext::TurnSection {
                    step: 1,
                    thoughts: Vec::new(),
                    calls: vec![call],
                    failed: Vec::new(),
                }],
                streaming: Vec::new(),
            })],
            started: 0,
            calls: 0,
            usage: contract::shapes::Usage {
                tokens: contract::shapes::Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: Default::default(),
                    output: 0,
                },
                cost: None,
                subscription_cost: 0.0,
            },
            state: TurnCardState::Running,
            flat: false,
        }));
        let shown = &table["entries"][0]["sections"][0]["calls"][0];
        assert_eq!(
            shown["headline"], headline,
            "code {code:?} message {message:?}"
        );
        assert_eq!(shown["content"], "full");
    }
    // A denial carries its reason as the headline and no error.
    let table = ledger(CallState::Denied {
        result: terminal(),
        detail: (),
    });
    assert_eq!(table["status"], "denied");
    assert_eq!(table["error"], serde_json::Value::Null);
    let denied = tui::ext::TurnCall {
        name: "shell",
        arguments: &json!({}),
        state: CallState::Denied {
            result: TurnCallResult {
                exit: None,
                duration_ms: None,
                lines: 0,
                changes: &[],
                output: "done",
                content: "",
                images: Vec::new(),
            },
            detail: "not allowed",
        },
        answer: None,
        open: false,
        asking: false,
    };
    assert_eq!(denied.headline(), "not allowed");
}

/// A running card and an ended card project the documented `running`
/// and `ended` keys, and each group lifecycle projects its own pair.
#[test]
fn card_and_group_states_project_their_documented_keys() {
    use tui::ext::{TurnCardState, TurnEnded};
    let ended = |state| {
        input_value(&Input::TurnCard(tui::ext::TurnCard {
            prompts: Vec::new(),
            entries: Vec::new(),
            started: 0,
            calls: 0,
            usage: contract::shapes::Usage {
                tokens: contract::shapes::Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: Default::default(),
                    output: 0,
                },
                cost: None,
                subscription_cost: 0.0,
            },
            state,
            flat: false,
        }))
    };
    let table = ended(TurnCardState::Running);
    assert_eq!(table["running"], true);
    assert_eq!(table["ended"], serde_json::Value::Null);
    let table = ended(TurnCardState::Ended(TurnEnded::Completed { ended_ts: 7 }));
    assert_eq!(table["running"], false);
    assert_eq!(
        table["ended"],
        json!({"outcome": "completed", "ended_ts": 7, "error": null}),
    );
    let card = |state| {
        input_value(&Input::TurnCard(tui::ext::TurnCard {
            prompts: Vec::new(),
            entries: vec![tui::ext::TurnEntry::Group(tui::ext::TurnGroup {
                first: 1,
                last: 2,
                state,
                open: false,
                sections: Vec::new(),
                streaming: Vec::new(),
            })],
            started: 0,
            calls: 0,
            usage: contract::shapes::Usage {
                tokens: contract::shapes::Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: Default::default(),
                    output: 0,
                },
                cost: None,
                subscription_cost: 0.0,
            },
            state: TurnCardState::Running,
            flat: false,
        }))["entries"][0]
            .take()
    };
    let group = |state| card(state);
    assert_eq!(group(TurnGroupState::Running)["running"], true);
    assert_eq!(group(TurnGroupState::Running)["cut"], false);
    assert_eq!(group(TurnGroupState::Finished)["running"], false);
    assert_eq!(group(TurnGroupState::Finished)["cut"], false);
    assert_eq!(group(TurnGroupState::Cut)["running"], false);
    assert_eq!(group(TurnGroupState::Cut)["cut"], true);
}

#[test]
fn the_group_line_table_holds_running_and_aggregates() {
    let input = Input::GroupLine(tui::ext::GroupLine {
        running: true,
        calls: Vec::new(),
        streaming: Vec::new(),
        thoughts: Vec::new(),
        failed: Vec::new(),
        first: 1,
        last: 2,
        open: false,
        flat: false,
    });
    assert_eq!(
        input_value(&input),
        json!({"running": true, "calls": [], "streaming": [], "thoughts": [], "failed": [], "first": 1, "last": 2, "open": false, "flat": false}),
    );
}

#[test]
fn the_handoff_band_table_holds_trigger_ts_state_and_sizes() {
    use tui::ext::{HandoffBand, TurnBandState, TurnSizes};
    let band = |state: TurnBandState| {
        Input::HandoffBand(HandoffBand {
            trigger: "the model handed off",
            ts: 1,
            state,
            flat: false,
        })
    };
    assert_eq!(
        input_value(&band(TurnBandState::Writing)),
        json!({"trigger": "the model handed off", "ts": 1, "state": {"kind": "writing"}, "flat": false}),
    );
    assert_eq!(
        input_value(&band(TurnBandState::Done(TurnSizes {
            before: 1,
            after: Some(2),
        }))),
        json!({"trigger": "the model handed off", "ts": 1, "state": {"kind": "done", "before": 1, "after": 2}, "flat": false}),
    );
    assert_eq!(
        input_value(&band(TurnBandState::Unchanged { cancelled: true })),
        json!({"trigger": "the model handed off", "ts": 1, "state": {"kind": "unchanged", "cancelled": true}, "flat": false}),
    );
    assert_eq!(
        input_value(&band(TurnBandState::Done(TurnSizes {
            before: 1,
            after: None,
        }))),
        json!({"trigger": "the model handed off", "ts": 1, "state": {"kind": "done", "before": 1, "after": null}, "flat": false}),
    );
    // The band row reads no note and no open flag.
    let table = input_value(&band(TurnBandState::Writing));
    assert!(table.get("open").is_none());
    assert!(table.get("note").is_none());
}

#[test]
fn the_error_line_table_holds_code_message_and_provider() {
    let table = {
        let provider = contract::shapes::ProviderFailure {
            name: "fake".to_owned(),
            status: Some(401),
            message: "bad key".to_owned(),
        };
        let failed = Input::ErrorLine(tui::ext::ErrorLine {
            code: contract::ErrorCode::AuthenticationFailed,
            message: "sign in",
            provider: Some(&provider),
            flat: false,
        });
        input_value(&failed)
    };
    assert_eq!(
        table,
        json!({"code": "authentication_failed", "message": "sign in", "provider": {"name": "fake", "status": 401, "message": "bad key"}, "flat": false}),
    );
    let plain = Input::ErrorLine(tui::ext::ErrorLine {
        code: contract::ErrorCode::IoFailed,
        message: "read failed",
        provider: None,
        flat: false,
    });
    assert_eq!(
        input_value(&plain),
        json!({"code": "io_failed", "message": "read failed", "provider": null, "flat": false}),
    );
}

#[test]
fn the_notice_table_holds_code_and_text() {
    let input = Input::Notice(tui::ext::Notice {
        code: "io_failed",
        text: "read failed",
        flat: false,
    });
    assert_eq!(
        input_value(&input),
        json!({"code": "io_failed", "text": "read failed", "flat": false}),
    );
}

/// A notice input for the sessionless adapter tests.
fn notice_input() -> Input<'static> {
    Input::Notice(tui::ext::Notice {
        code: "io_failed",
        text: "read failed",
        flat: false,
    })
}

#[test]
fn a_sessionless_adapter_sends_no_session_id_key() {
    let (_dir, loader) = fixture();
    package(
        &loader,
        "a",
        None,
        Some(
            "fiber.slot('notice',{timeout=100,render=function(input) if input.session_id == nil then return {{{text='none'}}} else return {{{text=input.session_id}}} end end})",
        ),
    );
    let (renderer, _) = loader.load();
    // Home binds no session: the client-only renderer draws and Lua
    // reads nil, never a reused, empty or synthetic id.
    let ctx = SessionCtx {
        session_id: None,
        halves: None,
    };
    let Decision::Draw { lines, .. } = render_slot(&*renderer, &ctx, "notice", &notice_input())
    else {
        panic!("a client-only renderer draws sessionless")
    };
    assert_eq!(lines[0][0].text, "none");
}

#[test]
fn a_sessionless_range_bound_renderer_stays_inert_without_notice() {
    let (_dir, loader) = fixture();
    package(&loader, "a", Some(json!({"min": "v1.0.0"})), Some(SCRIPT));
    let (renderer, _) = loader.load();
    // No session half has been seen on home: the contender stays inert
    // without an incompatibility notice.
    let ctx = SessionCtx {
        session_id: None,
        halves: None,
    };
    assert_eq!(
        renderer.fills_for(&ctx, "ledger_row"),
        SlotSelection::Absent
    );
    // (`docs/tui.md`, "How a TUI extension runs": a range-bound client half stays inert without notice until the session's first `extensions_loaded`.)
    assert!(renderer.take_pending().is_empty());
}
