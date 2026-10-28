#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::tui_vm::{
    KeyBinding, KeyForm, KeyRejection, RenderOut, SLOT_NAMES, SLOT_PREFIXES, Span, TuiVm,
};
use crate::{Error, ErrorCode};

const MS: Duration = Duration::from_millis(1);

#[test]
fn loading_deadline_starts_before_source_read_and_precedes_read_error() {
    // Owner ruling 6397275683: read, compile and init share LOAD_TIMEOUT.
    for read_error in [false, true] {
        let clock = FakeClock::new();
        let result = TuiVm::load_with_source(
            "test",
            || {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "no code under test waits on the clock (ruling 6179406086)"
                )]
                clock.advance(crate::lua::LOAD_TIMEOUT + MS);
                if read_error {
                    Err(Error::TuiLua("read failed".into()))
                } else {
                    Ok(
                        "fiber.slot('notice', {timeout=100, render=function() return {} end})"
                            .into(),
                    )
                }
            },
            clock.clone(),
        );
        assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
    }
}

/// A VM loaded from `source` on a clock that moves only when a test says.
fn vm(source: &str) -> (Arc<FakeClock>, TuiVm) {
    let clock = FakeClock::new();
    let vm = TuiVm::load("test", source, clock.clone()).expect("the script loads");
    (clock, vm)
}

/// A VM with the one slot `notice`, timeout 100 ms, drawn by `render`.
fn notice(render: &str) -> (Arc<FakeClock>, TuiVm) {
    vm(&format!(
        "fiber.slot('notice', {{ timeout = 100, render = function(input, width) {render} end }})"
    ))
}

fn none(_: &Value, _: u16) -> Vec<Vec<Span>> {
    Vec::new()
}

fn call(vm: &mut TuiVm, input: &Value) -> Result<Vec<Vec<Span>>, Error> {
    vm.call("notice", Some("s1"), input, 80, &none)
}

/// Moves the clock by `by`, from inside a callback the VM is running:
/// the callback has started, and the clock moves only then. The suite's
/// one raw clock move; no code under test waits on the clock
/// (ruling 6179406086).
fn tick(clock: &FakeClock, by: Duration) {
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(by);
}

/// Calls `notice`, moving the clock by `by` from inside `builtin`: the
/// render has started, and the clock moves only then.
fn call_advancing(
    vm: &mut TuiVm,
    clock: &FakeClock,
    by: Duration,
) -> Result<Vec<Vec<Span>>, Error> {
    let advance = |_: &Value, _: u16| {
        tick(clock, by);
        Vec::new()
    };
    vm.call("notice", Some("s1"), &json!({}), 80, &advance)
}

/// Runs the `raw` binding, moving the clock by `by` from inside `ticked`:
/// the handler has started, and the clock moves only then.
fn run_advancing(
    vm: &mut TuiVm,
    clock: Arc<FakeClock>,
    raw: &str,
    by: Duration,
) -> Result<bool, Error> {
    vm.lua()
        .globals()
        .set(
            "ticked",
            vm.lua()
                .create_function(move |_, ()| {
                    tick(&clock, by);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    vm.run_key(raw, "s1")
}

fn text(lines: &[Vec<Span>]) -> String {
    lines
        .iter()
        .flatten()
        .map(|span| span.text.as_str())
        .collect()
}

fn span(text: &str) -> Span {
    Span {
        text: text.to_owned(),
        ..Span::default()
    }
}

#[test]
fn a_renderer_returns_spans_with_every_field() {
    let (_, mut vm) = notice(
        "return { { { text = ' exit 0 ', fg = 'success', bg = 'surface', bold = true, \
         dim = true, italic = true, underline = true, link = 'https://x', click = 'badge' }, \
         { text = 'b' } }, {} }",
    );
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(
        lines,
        vec![
            vec![
                Span {
                    text: " exit 0 ".to_owned(),
                    fg: Some("success".to_owned()),
                    bg: Some("surface".to_owned()),
                    bold: true,
                    dim: true,
                    italic: true,
                    underline: true,
                    link: Some("https://x".to_owned()),
                    click: Some("badge".to_owned()),
                },
                span("b"),
            ],
            vec![],
        ]
    );
}

#[test]
fn a_false_flag_and_an_absent_one_both_read_as_false() {
    let (_, mut vm) = notice("return { { { text = 'a', bold = false }, { text = 'b' } } }");
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(lines, vec![vec![span("a"), span("b")]]);
}

#[test]
fn the_slots_are_listed_with_their_timeouts() {
    let (_, vm) = vm(
        "fiber.slot('notice', { timeout = 100, render = function() end })\n\
                      fiber.slot('card:jobs', { timeout = 250, render = function() end })",
    );
    let slots = vm.slots();
    assert_eq!(slots.get("card:jobs"), Some(&(250 * MS)));
    assert_eq!(slots.get("notice"), Some(&(100 * MS)));
}

#[test]
fn a_loop_past_the_timeout_is_cut_off_and_switches_the_vm_off() {
    let (clock, mut vm) = notice("builtin() while true do end");
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
    let later = call(&mut vm, &json!({}));
    assert!(matches!(later, Err(Error::TuiOff)), "{later:?}");
}

#[test]
fn a_pcall_cannot_swallow_the_timeout() {
    let (clock, mut vm) =
        notice("builtin() pcall(function() while true do end end) while true do end");
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_wrapped_coroutine_is_cut_off() {
    let (clock, mut vm) = notice("builtin() coroutine.wrap(function() while true do end end)()");
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_pcall_cannot_swallow_a_wrapped_timeout() {
    let (clock, mut vm) = notice(
        "builtin() pcall(coroutine.wrap(function() while true do end end)) while true do end",
    );
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_created_coroutine_is_cut_off() {
    let (clock, mut vm) = notice(
        "builtin() local co = coroutine.create(function() while true do end end) \
         coroutine.resume(co) while true do end",
    );
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_render_that_finishes_after_its_deadline_is_a_timeout() {
    let (clock, mut vm) = notice("builtin() return { { { text = 'late' } } }");
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
    let later = call(&mut vm, &json!({}));
    assert!(matches!(later, Err(Error::TuiOff)), "{later:?}");
}

#[test]
fn the_deadline_is_exactly_the_declared_timeout() {
    let (clock, mut vm) = notice("builtin() return { { { text = 'ok' } } }");
    let before = call_advancing(&mut vm, &clock, 99 * MS);
    assert_eq!(text(&before.unwrap()), "ok");
    // The clock stands 99 ms in; the next call's deadline is 100 ms from there.
    let at = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(at, Err(Error::TuiTimeout)), "{at:?}");
}

#[test]
fn one_millisecond_past_the_deadline_is_a_timeout() {
    let (clock, mut vm) = notice("builtin() return { { { text = 'ok' } } }");
    let result = call_advancing(&mut vm, &clock, 101 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_render_that_yields_after_its_deadline_is_a_timeout() {
    let (clock, mut vm) = notice("builtin() coroutine.yield({})");
    let result = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
}

#[test]
fn a_render_that_yields_is_a_lua_error() {
    let (_, mut vm) = notice("coroutine.yield({ { { text = 'yielded' } } })");
    let result = call(&mut vm, &json!({}));
    let Err(Error::TuiLua(message)) = result else {
        panic!("expected a Lua error, got {result:?}");
    };
    assert!(message.contains("no host call"), "{message}");
    // A yield is an error, not a timeout: the VM stays on.
    let again = call(&mut vm, &json!({}));
    assert!(matches!(again, Err(Error::TuiLua(_))), "{again:?}");
}

#[test]
fn a_lua_error_carries_its_message_and_leaves_the_vm_on() {
    let (_, mut vm) =
        notice("if input.fail then error('x marks') end return { { { text = 'ok' } } }");
    let result = call(&mut vm, &json!({ "fail": true }));
    let Err(Error::TuiLua(message)) = result else {
        panic!("expected a Lua error, got {result:?}");
    };
    assert!(message.contains("x marks"), "{message}");
    assert_eq!(text(&call(&mut vm, &json!({})).unwrap()), "ok");
}

#[test]
fn a_return_table_with_a_looping_index_is_read_without_firing_it() {
    let (_, mut vm) = notice(
        "return setmetatable({}, { __index = function() while true do end end, \
         __len = function() while true do end end })",
    );
    let lines = call(&mut vm, &json!({})).unwrap();
    assert!(lines.is_empty());
}

#[test]
fn a_span_with_a_looping_index_is_read_without_firing_it() {
    let (_, mut vm) = notice(
        "local span = setmetatable({ text = 'a' }, { __index = function() while true do end end }) \
         return { { span } }",
    );
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(lines, vec![vec![span("a")]]);
}

#[test]
fn the_base_library_has_no_io_and_no_require() {
    let (_, mut vm) = notice(
        "local present = {} \
         for _, name in ipairs({ 'print', 'warn', 'dofile', 'loadfile', 'require', 'io', 'os', 'package' }) do \
           if rawget(_G, name) ~= nil then present[#present + 1] = name end \
         end \
         return { { { text = table.concat(present, ',') } } }",
    );
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(lines, vec![vec![span("")]]);
}

#[test]
fn require_is_a_lua_error() {
    let (_, mut vm) = notice("require('x')");
    assert!(matches!(call(&mut vm, &json!({})), Err(Error::TuiLua(_))));
}

#[test]
fn off_wins_over_no_slot_and_no_slot_over_shape() {
    let (clock, mut vm) = notice("builtin() while true do end");
    // An absent slot with a non-object input: no slot comes before shape.
    let missing = vm.call("home", Some("s1"), &json!([1]), 80, &none);
    assert!(matches!(missing, Err(Error::TuiNoSlot(name)) if name == "home"));
    // A registered slot with a non-object input: shape.
    let shape = vm.call("notice", Some("s1"), &json!([1]), 80, &none);
    assert!(matches!(shape, Err(Error::TuiShape(_))), "{shape:?}");
    let timed_out = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(timed_out, Err(Error::TuiTimeout)));
    // Off now answers first, for an absent slot and a non-object input.
    let off = vm.call("home", Some("s1"), &json!([1]), 80, &none);
    assert!(matches!(off, Err(Error::TuiOff)), "{off:?}");
}

#[test]
fn a_huge_timeout_registers_and_is_never_cut_off() {
    let (clock, mut vm) = vm("fiber.slot('notice', { timeout = math.maxinteger, \
         render = function() builtin() return { { { text = 'ok' } } } end })");
    let lines = call_advancing(&mut vm, &clock, Duration::from_secs(3600)).unwrap();
    assert_eq!(text(&lines), "ok");
}

#[test]
fn two_vms_do_not_share_globals() {
    let (_, mut first) = vm("seen = 'one' fiber.slot('notice', { timeout = 100, \
                             render = function() return { { { text = seen } } } end })");
    let (_, mut second) = vm("fiber.slot('notice', { timeout = 100, \
                              render = function() return { { { text = tostring(seen) } } } end })");
    assert_eq!(text(&call(&mut first, &json!({})).unwrap()), "one");
    assert_eq!(text(&call(&mut second, &json!({})).unwrap()), "nil");
}

fn refused(source: &str) -> String {
    let clock = FakeClock::new();
    match TuiVm::load("test", source, clock) {
        Err(Error::TuiRegistration(message)) => message,
        other => panic!("expected a registration refusal, got {other:?}"),
    }
}

#[test]
fn a_slot_without_a_timeout_is_refused_by_name() {
    let message = refused("fiber.slot('notice', { render = function() end })");
    assert!(
        message.contains("notice") && message.contains("timeout"),
        "{message}"
    );
}

#[test]
fn a_timeout_must_be_a_positive_whole_number() {
    for bad in ["0", "-1", "1.5", "'100'"] {
        let message = refused(&format!(
            "fiber.slot('notice', {{ timeout = {bad}, render = function() end }})"
        ));
        assert!(message.contains("timeout"), "{bad}: {message}");
    }
    let (_, vm) = vm("fiber.slot('notice', { timeout = 1, render = function() end })");
    assert_eq!(vm.slots().get("notice"), Some(&MS));
}

#[test]
fn a_render_must_be_a_function() {
    let message = refused("fiber.slot('notice', { timeout = 100, render = 'x' })");
    assert!(message.contains("render"), "{message}");
}

#[test]
fn an_unknown_slot_is_refused_by_name() {
    let message = refused("fiber.slot('banner', { timeout = 100, render = function() end })");
    assert!(message.contains("banner"), "{message}");
}

#[test]
fn a_prefixed_slot_needs_a_suffix() {
    for prefix in SLOT_PREFIXES {
        let bare = refused(&format!(
            "fiber.slot('{prefix}', {{ timeout = 100, render = function() end }})"
        ));
        assert!(bare.contains(prefix), "{bare}");
        let (_, vm) = vm(&format!(
            "fiber.slot('{prefix}x', {{ timeout = 100, render = function() end }})"
        ));
        assert!(vm.slots().contains_key(&format!("{prefix}x")));
    }
}

#[test]
fn a_duplicate_slot_is_refused_by_name() {
    let message = refused(
        "fiber.slot('notice', { timeout = 100, render = function() end })\n\
         fiber.slot('notice', { timeout = 100, render = function() end })",
    );
    assert!(message.contains("notice"), "{message}");
}

#[test]
fn fiber_slot_after_load_is_a_lua_error() {
    let (_, mut vm) = notice("fiber.slot('home', { timeout = 100, render = function() end })");
    let result = call(&mut vm, &json!({}));
    assert!(matches!(result, Err(Error::TuiLua(_))), "{result:?}");
    assert!(!vm.slots().contains_key("home"));
}

#[test]
fn a_refusal_a_pcall_swallowed_still_fails_the_load() {
    let message = refused(
        "pcall(fiber.slot, 'banner', { timeout = 100, render = function() end })\n\
         fiber.slot('notice', { timeout = 100, render = function() end })",
    );
    assert!(message.contains("banner"), "{message}");
}

#[test]
fn animate_is_accepted() {
    let (_, vm) = vm(
        "fiber.slot('notice', { timeout = 100, animate = { interval_ms = 80 }, \
                      render = function() end })",
    );
    assert!(vm.slots().contains_key("notice"));
}

#[test]
fn a_script_error_is_a_lua_error_naming_the_cause() {
    let clock = FakeClock::new();
    let result = TuiVm::load("test", "error('broken script')", clock);
    let Err(Error::Lua { extension, message }) = result else {
        panic!("expected a Lua error, got {result:?}");
    };
    assert_eq!(extension, "test");
    assert!(message.contains("broken script"), "{message}");
}

#[test]
fn a_syntax_error_is_a_lua_error() {
    let clock = FakeClock::new();
    let result = TuiVm::load("test", "fiber.slot(", clock);
    assert!(matches!(result, Err(Error::Lua { .. })), "{result:?}");
}

#[test]
fn a_script_that_yields_is_a_lua_error() {
    let clock = FakeClock::new();
    let result = TuiVm::load("test", "coroutine.yield()", clock);
    assert!(matches!(result, Err(Error::Lua { .. })), "{result:?}");
}

#[test]
fn a_json_null_reads_as_nil() {
    let (_, mut vm) =
        notice("return { { { text = tostring(input.error) .. tostring(input.code) } } }");
    let lines = call(&mut vm, &json!({ "error": null, "code": 7 })).unwrap();
    assert_eq!(text(&lines), "nil7");
}

#[test]
fn a_non_object_input_is_a_shape_and_runs_no_render() {
    let (_, mut vm) = notice("error('ran')");
    for input in [json!([1]), json!("x"), json!(null), json!(3)] {
        let result = call(&mut vm, &input);
        let Err(Error::TuiShape(message)) = result else {
            panic!("expected a shape error for {input}, got {result:?}");
        };
        assert!(message.contains("object"), "{message}");
    }
}

#[test]
fn the_session_id_reaches_the_render_and_overrides_the_input() {
    let (_, mut vm) = notice("return { { { text = input.session_id .. ':' .. input.n } } }");
    let lines = vm
        .call(
            "notice",
            Some("s1"),
            &json!({ "n": 2, "session_id": "mine" }),
            80,
            &none,
        )
        .unwrap();
    assert_eq!(text(&lines), "s1:2");
}

#[test]
fn the_render_receives_the_width() {
    let (_, mut vm) = notice("return { { { text = tostring(width) } } }");
    let lines = vm
        .call("notice", Some("s1"), &json!({}), 37, &none)
        .unwrap();
    assert_eq!(text(&lines), "37");
}

#[test]
fn builtin_returns_the_supplied_lines() {
    let builtin = |_: &Value, _: u16| {
        vec![vec![
            Span {
                text: "built".to_owned(),
                fg: Some("muted".to_owned()),
                bold: true,
                ..Span::default()
            },
            span("in"),
        ]]
    };
    let (_, mut vm) = notice("return builtin(input, width)");
    let lines = vm
        .call("notice", Some("s1"), &json!({}), 80, &builtin)
        .unwrap();
    assert_eq!(
        lines,
        vec![vec![
            Span {
                text: "built".to_owned(),
                fg: Some("muted".to_owned()),
                bold: true,
                ..Span::default()
            },
            span("in"),
        ]]
    );
}

#[test]
fn builtin_gets_the_original_input_and_the_width_the_render_passes() {
    let seen = RefCell::new(Vec::new());
    let builtin = |input: &Value, width: u16| {
        seen.borrow_mut().push((input.clone(), width));
        Vec::new()
    };
    let (_, mut vm) = notice("return builtin({ changed = true }, width - 6)");
    let input = json!({ "name": "shell" });
    vm.call("notice", Some("s1"), &input, 80, &builtin).unwrap();
    // The table the render passed is ignored, and so is the injected
    // `session_id`; the width it passed is used.
    assert_eq!(*seen.borrow(), vec![(input.clone(), 74)]);
    let (_, mut plain) = notice("return builtin()");
    plain
        .call("notice", Some("s1"), &input, 80, &builtin)
        .unwrap();
    assert_eq!(seen.borrow().last(), Some(&(input, 80)));
}

#[test]
fn builtin_expires_when_the_call_ends() {
    let (_, mut vm) = vm(
        "fiber.slot('notice', { timeout = 100, render = function() kept = builtin return {} end })\n\
         fiber.slot('home', { timeout = 100, render = function() \
         return { { { text = tostring(pcall(kept)) } } } end })",
    );
    vm.call("notice", Some("s1"), &json!({}), 80, &none)
        .unwrap();
    let lines = vm.call("home", Some("s1"), &json!({}), 80, &none).unwrap();
    assert_eq!(text(&lines), "false");
}

#[test]
fn an_unregistered_slot_has_no_slot() {
    let (_, mut vm) = notice("return {}");
    let result = vm.call("reply", Some("s1"), &json!({}), 80, &none);
    assert!(matches!(result, Err(Error::TuiNoSlot(name)) if name == "reply"));
}

#[test]
fn a_wrong_shape_is_named() {
    let cases = [
        ("return 'text'", "lines"),
        ("return nil", "lines"),
        ("return { 'line' }", "line"),
        ("return { { 'span' } }", "span"),
        ("return { { { bold = true } } }", "text"),
        ("return { { { text = 5 } } }", "text"),
        ("return { { { text = 'a', fg = 5 } } }", "fg"),
        ("return { { { text = 'a', bg = true } } }", "bg"),
        ("return { { { text = 'a', link = {} } } }", "link"),
        ("return { { { text = 'a', click = 1 } } }", "click"),
        ("return { { { text = 'a', bold = 'yes' } } }", "bold"),
        ("return { { { text = 'a', dim = 1 } } }", "dim"),
        ("return { { { text = 'a', italic = 'no' } } }", "italic"),
        ("return { { { text = 'a', underline = {} } } }", "underline"),
    ];
    for (body, field) in cases {
        let (_, mut vm) = notice(body);
        let result = call(&mut vm, &json!({}));
        let Err(Error::TuiShape(message)) = result else {
            panic!("`{body}`: expected a shape error, got {result:?}");
        };
        assert!(message.contains(field), "`{body}`: {message}");
        // A wrong shape leaves the VM on.
        let again = call(&mut vm, &json!({}));
        assert!(matches!(again, Err(Error::TuiShape(_))), "{again:?}");
    }
}

#[test]
fn a_span_with_no_text_is_a_shape() {
    let (_, mut vm) = notice("return { { { fg = 'success' } } }");
    assert!(matches!(call(&mut vm, &json!({})), Err(Error::TuiShape(_))));
}

#[test]
fn the_slot_table_in_the_doc_is_the_slot_set_in_the_code() {
    let doc = include_str!("../../../docs/tui.md");
    let table = doc
        .split("### The slots")
        .nth(1)
        .and_then(|rest| rest.split("\n###").next())
        .expect("docs/tui.md has a `The slots` section");
    let mut fixed = BTreeSet::new();
    let mut prefixed = BTreeSet::new();
    // Only the slot table: the per-slot field tables below it share the
    // row shape, so stop at the first line past the slot table.
    let mut past_slots = false;
    for line in table.lines() {
        if let Some(row) = line.strip_prefix("| `") {
            past_slots = true;
            let name = row.split('`').next().expect("a backticked slot name");
            match name.split_once('<') {
                Some((prefix, _)) => prefixed.insert(prefix.to_owned()),
                None => fixed.insert(name.to_owned()),
            };
        } else if past_slots {
            break;
        }
    }
    let own_fixed: BTreeSet<String> = SLOT_NAMES.iter().map(|name| (*name).to_owned()).collect();
    let own_prefixed: BTreeSet<String> = SLOT_PREFIXES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(fixed, own_fixed);
    assert_eq!(prefixed, own_prefixed);
}

#[test]
fn a_renderer_failure_is_hook_failed_and_the_rest_are_extension_failed() {
    let (clock, mut vm) = notice("builtin({}) return {}");
    let failed = [
        TuiVm::load("test", "error('x')", FakeClock::new()).unwrap_err(),
        TuiVm::load("test", "fiber.slot('banner', {})", FakeClock::new()).unwrap_err(),
        vm.call("home", Some("s1"), &json!({}), 80, &none)
            .unwrap_err(),
        call_advancing(&mut vm, &clock, 100 * MS).unwrap_err(),
        call(&mut vm, &json!({})).unwrap_err(),
    ];
    for error in failed {
        assert_eq!(error.code(), ErrorCode::ExtensionFailed, "{error:?}");
    }
    let (_, mut vm) = notice("if input.fail then error('x') end return {}");
    let errored = call(&mut vm, &json!({ "fail": true })).unwrap_err();
    let shaped = call(&mut vm, &json!(1)).unwrap_err();
    assert!(matches!(errored, Error::TuiLua(_)), "{errored:?}");
    assert!(matches!(shaped, Error::TuiShape(_)), "{shaped:?}");
    for error in [errored, shaped] {
        assert_eq!(error.code(), ErrorCode::HookFailed, "{error:?}");
    }
}

// Kills the MISSED CI mutant at tui_vm.rs:161:9 (Debug for TuiVm::fmt).
#[test]
fn debug_names_the_extension_and_its_slots() {
    let vm = TuiVm::load(
        "acme-badge",
        "fiber.slot('notice', { timeout = 100, render = function() end })",
        FakeClock::new(),
    )
    .expect("the script loads");
    let shown = format!("{vm:?}");
    assert!(shown.contains("acme-badge"), "{shown}");
    assert!(shown.contains("notice"), "{shown}");
}

/// A VM with the one animated slot `notice`, timeout 100 ms and the raw
/// interval 500 ms, drawn by `render`.
fn animated(render: &str) -> (Arc<FakeClock>, TuiVm) {
    vm(&format!(
        "fiber.slot('notice', {{ timeout = 100, animate = {{ interval_ms = 500 }}, render = function(input, width) {render} end }})"
    ))
}

/// Calls `notice` as frame `frame`, the seam's animated path.
fn call_frame(vm: &mut TuiVm, input: &Value, frame: u64) -> Result<RenderOut, Error> {
    vm.call_animated("notice", Some("s1"), input, 80, frame, &none)
}

/// Calls `notice` as frame `frame`, moving the clock by `by` from inside
/// `builtin`: the render has started, and the clock moves only then.
fn call_frame_advancing(
    vm: &mut TuiVm,
    clock: &FakeClock,
    by: Duration,
    frame: u64,
) -> Result<RenderOut, Error> {
    let advance = |_: &Value, _: u16| {
        #[allow(
            clippy::disallowed_methods,
            reason = "no code under test waits on the clock (ruling 6179406086)"
        )]
        clock.advance(by);
        Vec::new()
    };
    vm.call_animated("notice", Some("s1"), &json!({}), 80, frame, &advance)
}

fn out_text(out: &RenderOut) -> String {
    text(&out.lines)
}

#[test]
fn animate_is_refused_unless_a_table_with_a_positive_whole_interval() {
    for bad in [
        "80",
        "{ interval_ms = 0 }",
        "{ interval_ms = -1 }",
        "{ interval_ms = 1.5 }",
        "{ interval_ms = '100' }",
        "{ }",
    ] {
        let message = refused(&format!(
            "fiber.slot('notice', {{ timeout = 100, animate = {bad}, render = function() end }})"
        ));
        assert!(
            message.contains("notice") && message.contains("interval_ms"),
            "{bad}: {message}"
        );
    }
    // At the edge one millisecond registers.
    let (_, vm) = vm(
        "fiber.slot('notice', { timeout = 100, animate = { interval_ms = 1 }, \
         render = function() end })",
    );
    assert_eq!(vm.animate("notice"), Some(MS));
}

#[test]
fn animate_with_a_key_beside_interval_ms_is_refused() {
    // docs/extensions.md:1507-1509: any other animate shape fails
    // registration, naming the slot.
    for bad in [
        "{ interval_ms = 500, extra = 1 }",
        "{ interval_ms = 500, [1] = 2 }",
    ] {
        let message = refused(&format!(
            "fiber.slot('notice', {{ timeout = 100, animate = {bad}, render = function() end }})"
        ));
        assert!(
            message.contains("notice") && message.contains("interval_ms"),
            "{bad}: {message}"
        );
    }
}

#[test]
fn an_interval_below_the_tick_floor_stores_the_raw_value() {
    // No error here: the adapter normalizes to the 120 ms floor at
    // registration, and the seam clamps its dues and asks.
    let (_, vm) = vm(
        "fiber.slot('notice', { timeout = 100, animate = { interval_ms = 80 }, \
         render = function() end })",
    );
    assert_eq!(vm.animate("notice"), Some(80 * MS));
}

#[test]
fn a_five_hundred_ms_interval_stores() {
    let (_, vm) = animated("return {}");
    assert_eq!(vm.animate("notice"), Some(500 * MS));
}

#[test]
fn a_slot_without_animate_stores_none() {
    let (_, vm) = notice("return {}");
    assert_eq!(vm.animate("notice"), None);
}

#[test]
fn the_frame_reaches_the_render_with_session_and_width() {
    let (_, mut vm) = animated(
        "return { { { text = input.frame .. ':' .. input.session_id .. ':' .. width } } }",
    );
    let out = call_frame(&mut vm, &json!({}), 3).unwrap();
    assert_eq!(out_text(&out), "3:s1:80");
    assert!(out.continues);
}

#[test]
fn a_plain_call_accepts_keyed_renderer_results() {
    let (_, mut vm) = notice("return { lines = { { { text = 'still' } } } }");
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(text(&lines), "still");

    let (_, mut vm) = notice("return { lines = { { { text = 'still' } } }, animate = false }");
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(text(&lines), "still");
}

#[test]
fn a_plain_call_sends_no_frame_key() {
    let (_, mut vm) = notice("return { { { text = tostring(input.frame) } } }");
    let lines = call(&mut vm, &json!({})).unwrap();
    assert_eq!(text(&lines), "nil");
}

#[test]
fn a_plain_animated_list_continues() {
    let (_, mut vm) = animated("return { { { text = 'tick' } } }");
    let out = call_frame(&mut vm, &json!({}), 0).unwrap();
    assert_eq!(out_text(&out), "tick");
    assert!(out.continues);
}

#[test]
fn a_lines_table_continues() {
    let (_, mut vm) = animated("return { lines = { { { text = 'tick' } } } }");
    let out = call_frame(&mut vm, &json!({}), 7).unwrap();
    assert_eq!(out_text(&out), "tick");
    assert!(out.continues);
}

#[test]
fn a_lines_table_with_animate_false_draws_then_stops() {
    let (_, mut vm) = animated("return { lines = { { { text = 'still' } } }, animate = false }");
    let out = call_frame(&mut vm, &json!({}), 7).unwrap();
    assert_eq!(out_text(&out), "still");
    assert!(!out.continues);
}

#[test]
fn an_animate_value_other_than_false_is_a_wrong_shape() {
    for animate in ["true", "0", "'no'", "{ }"] {
        let (_, mut vm) = animated(&format!(
            "return {{ lines = {{ {{ {{ text = 'tick' }} }} }}, animate = {animate} }}"
        ));
        let result = call_frame(&mut vm, &json!({}), 0);
        let Err(Error::TuiShape(message)) = result else {
            panic!("animate = {animate}: expected a shape error, got {result:?}");
        };
        assert!(message.contains("animate"), "{message}");
        // A wrong shape leaves the VM on.
        let again = call_frame(&mut vm, &json!({}), 0);
        assert!(matches!(again, Err(Error::TuiShape(_))), "{again:?}");
    }
}

#[test]
fn a_keyed_return_without_lines_is_a_wrong_shape() {
    let (_, mut vm) = animated("return { animate = false }");
    assert!(matches!(
        call_frame(&mut vm, &json!({}), 0),
        Err(Error::TuiShape(_))
    ));
}

#[test]
fn a_keyed_return_with_non_list_lines_is_a_wrong_shape() {
    let (_, mut vm) = animated("return { lines = 'tick' }");
    assert!(matches!(
        call_frame(&mut vm, &json!({}), 0),
        Err(Error::TuiShape(_))
    ));
}

#[test]
fn a_keyed_return_with_an_extra_key_is_a_wrong_shape() {
    for body in [
        "return { lines = { { { text = 'tick' } } }, extra = 1 }",
        "return { lines = { { { text = 'tick' } } }, animate = false, extra = 1 }",
    ] {
        let (_, mut vm) = animated(body);
        let result = call_frame(&mut vm, &json!({}), 0);
        assert!(
            matches!(result, Err(Error::TuiShape(_))),
            "{body}: {result:?}"
        );
    }
}

#[test]
fn empty_lines_are_valid_and_empty() {
    let (_, mut vm) = animated("return { lines = { } }");
    let out = call_frame(&mut vm, &json!({}), 0).unwrap();
    assert!(out.lines.is_empty());
    assert!(out.continues);
    let (_, mut vm) = animated("return { }");
    let out = call_frame(&mut vm, &json!({}), 0).unwrap();
    assert!(out.lines.is_empty());
    assert!(out.continues);
}

#[test]
fn a_non_table_animated_return_is_a_wrong_shape() {
    let (_, mut vm) = animated("return 'text'");
    assert!(matches!(
        call_frame(&mut vm, &json!({}), 0),
        Err(Error::TuiShape(_))
    ));
}

#[test]
fn an_animated_tick_past_its_deadline_switches_the_vm_off() {
    let (clock, mut vm) = animated("builtin() while true do end");
    let result = call_frame_advancing(&mut vm, &clock, 100 * MS, 0);
    assert!(matches!(result, Err(Error::TuiTimeout)), "{result:?}");
    let later = call_frame(&mut vm, &json!({}), 1);
    assert!(matches!(later, Err(Error::TuiOff)), "{later:?}");
}

#[test]
fn a_sessionless_call_sends_no_session_id_key() {
    let (_, mut vm) = notice(
        "if input.session_id == nil then return {{{text='none'}}} else return {{{text=input.session_id}}} end",
    );
    let lines = vm.call("notice", None, &json!({}), 80, &none).unwrap();
    assert_eq!(text(&lines), "none");
    let lines = vm
        .call("notice", Some("s1"), &json!({}), 80, &none)
        .unwrap();
    assert_eq!(text(&lines), "s1");
}

/// The key rejections `source` records: the load succeeds, holding no
/// binding for the bad registration.
fn rejections(source: &str) -> (Vec<KeyBinding>, Vec<KeyRejection>) {
    let (_, vm) = vm(source);
    (vm.key_bindings(), vm.key_rejections())
}

/// Runs the `raw` binding for session `s1`.
fn run(vm: &mut TuiVm, raw: &str) -> Result<bool, Error> {
    vm.run_key(raw, "s1")
}

#[test]
fn fiber_key_registers_a_run_binding() {
    let (bindings, rejected) =
        rejections("fiber.key('ctrl+o', { timeout = 100, run = function() return true end })");
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        bindings,
        vec![KeyBinding {
            raw: "ctrl+o".to_owned(),
            form: KeyForm::Run { timeout: 100 * MS },
        }]
    );
}

#[test]
fn fiber_key_registers_action_bindings_with_and_without_a_variant() {
    let (bindings, rejected) = rejections(
        "fiber.key('alt+h', { action = 'move_word', variant = 'left' })\n\
         fiber.key('ctrl+n', { action = 'new_session' })",
    );
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        bindings,
        vec![
            KeyBinding {
                raw: "alt+h".to_owned(),
                form: KeyForm::Action {
                    id: "move_word".to_owned(),
                    variant: Some("left".to_owned()),
                },
            },
            KeyBinding {
                raw: "ctrl+n".to_owned(),
                form: KeyForm::Action {
                    id: "new_session".to_owned(),
                    variant: None,
                },
            },
        ]
    );
}

#[test]
fn fiber_key_with_a_non_string_key_records_no_raw() {
    for name in ["5", "nil"] {
        let (bindings, rejected) = rejections(&format!(
            "fiber.key({name}, {{ timeout = 100, run = function() end }})"
        ));
        assert!(bindings.is_empty(), "{name}: {bindings:?}");
        assert_eq!(rejected.len(), 1, "{name}: {rejected:?}");
        assert_eq!(rejected[0].raw, None, "{name}: {rejected:?}");
        assert!(rejected[0].reason.contains("key"), "{name}: {rejected:?}");
    }
}

#[test]
fn fiber_key_with_a_non_table_spec_is_rejected() {
    for spec in ["5", "nil", "'run'"] {
        let (bindings, rejected) = rejections(&format!("fiber.key('ctrl+o', {spec})"));
        assert!(bindings.is_empty(), "{spec}: {bindings:?}");
        assert_eq!(rejected.len(), 1, "{spec}: {rejected:?}");
        assert_eq!(rejected[0].raw.as_deref(), Some("ctrl+o"));
    }
}

#[test]
fn fiber_key_with_neither_run_nor_action_is_rejected() {
    let (bindings, rejected) = rejections("fiber.key('ctrl+o', {})");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("run") && rejected[0].reason.contains("action"));
}

#[test]
fn fiber_key_with_both_run_and_action_is_rejected() {
    let (bindings, rejected) =
        rejections("fiber.key('ctrl+o', { timeout = 100, run = function() end, action = 'send' })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("run") && rejected[0].reason.contains("action"));
}

#[test]
fn fiber_key_with_a_missing_or_invalid_timeout_names_timeout() {
    for timeout in ["nil", "0", "-1", "1.5", "'100'"] {
        let (bindings, rejected) = rejections(&format!(
            "fiber.key('ctrl+o', {{ timeout = {timeout}, run = function() end }})"
        ));
        assert!(bindings.is_empty(), "{timeout}: {bindings:?}");
        assert_eq!(rejected.len(), 1, "{timeout}: {rejected:?}");
        assert!(
            rejected[0].reason.contains("timeout"),
            "{timeout}: {rejected:?}"
        );
    }
    let (bindings, rejected) =
        rejections("fiber.key('ctrl+o', { run = function() return true end })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("timeout"), "{rejected:?}");
}

#[test]
fn fiber_key_with_a_non_function_run_is_rejected() {
    let (bindings, rejected) = rejections("fiber.key('ctrl+o', { timeout = 100, run = 'yes' })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("run"), "{rejected:?}");
}

#[test]
fn fiber_key_with_an_action_and_a_timeout_names_timeout() {
    let (bindings, rejected) =
        rejections("fiber.key('alt+h', { action = 'move_word', variant = 'left', timeout = 100 })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("timeout"), "{rejected:?}");
}

#[test]
fn fiber_key_with_a_run_and_a_variant_names_variant() {
    let (bindings, rejected) = rejections(
        "fiber.key('ctrl+o', { timeout = 100, run = function() end, variant = 'left' })",
    );
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("variant"), "{rejected:?}");
}

#[test]
fn fiber_key_with_a_non_string_action_or_variant_is_rejected() {
    let (bindings, rejected) = rejections("fiber.key('ctrl+o', { action = 5 })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("action"), "{rejected:?}");
    let (bindings, rejected) =
        rejections("fiber.key('alt+h', { action = 'move_word', variant = 5 })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].reason.contains("variant"), "{rejected:?}");
}

#[test]
fn fiber_key_with_clear_then_quit_is_rejected() {
    let (bindings, rejected) = rejections("fiber.key('ctrl+q', { action = 'clear_then_quit' })");
    assert!(bindings.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].reason.contains("clear_then_quit"),
        "{rejected:?}"
    );
}

#[test]
fn fiber_key_with_a_duplicate_raw_name_keeps_the_first() {
    let (bindings, rejected) = rejections(
        "fiber.key('ctrl+o', { timeout = 100, run = function() return true end })\n\
         fiber.key('ctrl+o', { timeout = 200, run = function() return false end })",
    );
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert_eq!(rejected[0].raw.as_deref(), Some("ctrl+o"));
    assert!(rejected[0].reason.contains("twice"), "{rejected:?}");
    assert_eq!(
        bindings,
        vec![KeyBinding {
            raw: "ctrl+o".to_owned(),
            form: KeyForm::Run { timeout: 100 * MS },
        }]
    );
}

#[test]
fn key_bindings_keep_registration_order_across_case() {
    let (bindings, rejected) = rejections(
        "fiber.key('ctrl+o', { action = 'toggle_ledgers' })\n\
         fiber.key('Ctrl+O', { action = 'send' })",
    );
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.raw.as_str())
            .collect::<Vec<_>>(),
        vec!["ctrl+o", "Ctrl+O"]
    );
}

#[test]
fn fiber_slot_with_a_key_field_refuses_the_load() {
    let message =
        refused("fiber.slot('notice', { timeout = 100, key = 'ctrl+o', render = function() end })");
    assert!(message.contains("key"), "{message}");
}

#[test]
fn fiber_key_after_load_is_a_lua_error_and_registers_nothing() {
    let (_, mut vm) =
        notice("fiber.key('ctrl+o', { timeout = 100, run = function() return true end })");
    let result = call(&mut vm, &json!({}));
    assert!(matches!(result, Err(Error::TuiLua(_))), "{result:?}");
    assert!(vm.key_bindings().is_empty());
    assert!(vm.key_rejections().is_empty());
}

#[test]
fn run_key_returns_true_only_for_true() {
    for (body, handled) in [
        ("return true", true),
        ("return false", false),
        ("return nil", false),
        ("return", false),
    ] {
        let (_, mut vm) = vm(&format!(
            "fiber.key('ctrl+o', {{ timeout = 100, run = function() {body} end }})"
        ));
        assert_eq!(run(&mut vm, "ctrl+o").unwrap(), handled, "{body}");
    }
}

#[test]
fn run_key_on_an_action_only_or_unknown_key_is_not_handled() {
    let (_, mut vm) = vm(
        "fiber.key('alt+h', { action = 'move_word', variant = 'left' })\n\
         fiber.key('ctrl+o', { timeout = 100, run = function() return true end })",
    );
    assert_eq!(run(&mut vm, "alt+h").unwrap(), false);
    assert_eq!(run(&mut vm, "ctrl+z").unwrap(), false);
    // Neither call notifies or switches the VM off.
    assert!(vm.key_rejections().is_empty());
    assert_eq!(run(&mut vm, "ctrl+o").unwrap(), true);
}

#[test]
fn run_key_with_a_wrong_shape_is_a_shape() {
    for body in ["return 5", "return 'handled'", "return {}"] {
        let (_, mut vm) = vm(&format!(
            "fiber.key('ctrl+o', {{ timeout = 100, run = function() {body} end }})"
        ));
        assert!(
            matches!(run(&mut vm, "ctrl+o"), Err(Error::TuiShape(_))),
            "{body}"
        );
        // A wrong shape leaves the VM on.
        assert!(matches!(run(&mut vm, "ctrl+o"), Err(Error::TuiShape(_))));
    }
}

#[test]
fn run_key_with_a_lua_error_or_yield_is_a_lua_error() {
    let (_, mut vm) = vm(
        "fiber.key('ctrl+o', { timeout = 100, run = function() error('boom') end })\n\
         fiber.key('ctrl+p', { timeout = 100, run = function() coroutine.yield() end })",
    );
    let Err(Error::TuiLua(message)) = run(&mut vm, "ctrl+o") else {
        panic!("expected a Lua error");
    };
    assert!(message.contains("boom"), "{message}");
    let Err(Error::TuiLua(message)) = run(&mut vm, "ctrl+p") else {
        panic!("expected a Lua error");
    };
    assert!(message.contains("host call"), "{message}");
    // An error leaves the VM on.
    assert!(matches!(run(&mut vm, "ctrl+o"), Err(Error::TuiLua(_))));
}

#[test]
fn run_key_receives_only_the_session_id() {
    let (_, mut vm) = vm("fiber.key('ctrl+o', { timeout = 100, \
         run = function(input) return input.session_id == 's1' and input.key == nil end })");
    assert_eq!(run(&mut vm, "ctrl+o").unwrap(), true);
}

#[test]
fn a_run_that_finishes_before_its_deadline_is_handled() {
    let (clock, mut vm) =
        vm("fiber.key('ctrl+o', { timeout = 100, run = function() ticked() return true end })");
    assert_eq!(
        run_advancing(&mut vm, clock, "ctrl+o", 99 * MS).unwrap(),
        true
    );
}

#[test]
fn a_run_at_or_past_its_deadline_is_a_timeout() {
    let (clock, mut timed) =
        vm("fiber.key('ctrl+o', { timeout = 100, run = function() ticked() return true end })");
    let timed_out = run_advancing(&mut timed, clock.clone(), "ctrl+o", 100 * MS);
    assert!(matches!(timed_out, Err(Error::TuiTimeout)), "{timed_out:?}");
    let later = run(&mut timed, "ctrl+o");
    assert!(matches!(later, Err(Error::TuiOff)), "{later:?}");
    let (fresh_clock, mut fresh) =
        vm("fiber.key('ctrl+o', { timeout = 100, run = function() ticked() return true end })");
    let past = run_advancing(&mut fresh, fresh_clock, "ctrl+o", 101 * MS);
    assert!(matches!(past, Err(Error::TuiTimeout)), "{past:?}");
}

#[test]
fn a_fresh_load_works_after_a_timeout() {
    let source =
        "fiber.key('ctrl+o', { timeout = 100, run = function() ticked() return true end })";
    let (clock, mut timed) = vm(source);
    let timed_out = run_advancing(&mut timed, clock, "ctrl+o", 100 * MS);
    assert!(matches!(timed_out, Err(Error::TuiTimeout)), "{timed_out:?}");
    let (_, mut again) = vm(source);
    assert_eq!(
        run_advancing(&mut again, FakeClock::new(), "ctrl+o", Duration::ZERO).unwrap(),
        true
    );
}
