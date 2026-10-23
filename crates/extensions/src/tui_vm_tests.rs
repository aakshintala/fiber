#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::tui_vm::{SLOT_NAMES, SLOT_PREFIXES, Span, TuiVm};
use crate::{Error, ErrorCode};

const MS: Duration = Duration::from_millis(1);

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
    vm.call("notice", "s1", input, 80, &none)
}

/// Calls `notice`, moving the clock by `by` from inside `builtin`: the
/// render has started, and the clock moves only then.
fn call_advancing(
    vm: &mut TuiVm,
    clock: &FakeClock,
    by: Duration,
) -> Result<Vec<Vec<Span>>, Error> {
    let advance = |_: &Value, _: u16| {
        clock.advance(by);
        Vec::new()
    };
    vm.call("notice", "s1", &json!({}), 80, &advance)
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
    let missing = vm.call("home", "s1", &json!([1]), 80, &none);
    assert!(matches!(missing, Err(Error::TuiNoSlot(name)) if name == "home"));
    // A registered slot with a non-object input: shape.
    let shape = vm.call("notice", "s1", &json!([1]), 80, &none);
    assert!(matches!(shape, Err(Error::TuiShape(_))), "{shape:?}");
    let timed_out = call_advancing(&mut vm, &clock, 100 * MS);
    assert!(matches!(timed_out, Err(Error::TuiTimeout)));
    // Off now answers first, for an absent slot and a non-object input.
    let off = vm.call("home", "s1", &json!([1]), 80, &none);
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
            "s1",
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
    let lines = vm.call("notice", "s1", &json!({}), 37, &none).unwrap();
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
    let lines = vm.call("notice", "s1", &json!({}), 80, &builtin).unwrap();
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
    vm.call("notice", "s1", &input, 80, &builtin).unwrap();
    // The table the render passed is ignored, and so is the injected
    // `session_id`; the width it passed is used.
    assert_eq!(*seen.borrow(), vec![(input.clone(), 74)]);
    let (_, mut plain) = notice("return builtin()");
    plain.call("notice", "s1", &input, 80, &builtin).unwrap();
    assert_eq!(seen.borrow().last(), Some(&(input, 80)));
}

#[test]
fn builtin_expires_when_the_call_ends() {
    let (_, mut vm) = vm(
        "fiber.slot('notice', { timeout = 100, render = function() kept = builtin return {} end })\n\
         fiber.slot('home', { timeout = 100, render = function() \
         return { { { text = tostring(pcall(kept)) } } } end })",
    );
    vm.call("notice", "s1", &json!({}), 80, &none).unwrap();
    let lines = vm.call("home", "s1", &json!({}), 80, &none).unwrap();
    assert_eq!(text(&lines), "false");
}

#[test]
fn an_unregistered_slot_has_no_slot() {
    let (_, mut vm) = notice("return {}");
    let result = vm.call("reply", "s1", &json!({}), 80, &none);
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
    for row in table.lines().filter(|line| line.starts_with("| `")) {
        let name = row[3..].split('`').next().expect("a backticked slot name");
        match name.split_once('<') {
            Some((prefix, _)) => prefixed.insert(prefix.to_owned()),
            None => fixed.insert(name.to_owned()),
        };
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
        vm.call("home", "s1", &json!({}), 80, &none).unwrap_err(),
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
