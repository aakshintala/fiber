# How TUI coding agents test their terminal end to end

Research for [#1679](https://github.com/aakshintala/fiber/issues/1679). Question: how do open-source coding agents with a terminal UI test that UI with the real binary in a terminal and no model in the loop, and what should Fiber do?

Method. Each project was cloned at the commit named in its citations (`repo@<12-char commit>:path:line`) and read: tests, fixtures, CI configs and, for churn, `git log` or the GitHub commits API over the six months from 2026-04-10. Counts carry the command that produced them. "Not found" means the paths named were searched and nothing turned up. CI wall times are not published in any repository examined, so none is given. Claude Code has no public source: only its shipped commands and binary were probed.

Fiber today, for reference (`origin/main@554a1e904a94`): `crates/main/tests/terminal.rs` has 24 `#[test]`s (1194 lines). Eight test the phrase matcher itself (`phrase_end_*`, lines 527-570); the rest drive the real binary through `rustix::pty` against `fakes::ProviderServer` and wait for a phrase in the raw bytes, skipping SGR and cursor-move gaps (`phrase_end`, `frame_gap_end`, `cursor_move_end`, `sgr_end`, lines 419-525). `crates/tui` has 196 committed `.snap` files from `insta` (`find crates -name '*.snap' | wc -l`), the in-process layer `docs/testing.md` ("Screens") describes.

## Findings in brief

- Every project that tests its real terminal does it without a model. Three harness shapes exist. (a) A real binary in a PTY whose output feeds a terminal emulator that rebuilds the screen grid: codex (`PtyCodex` plus `vt100`), television (`phantom-test`), Go's `vttest`. (b) A real binary in a PTY asserted on stripped output: gemini-cli (`stripAnsi` substring), the same blind spot as Fiber's phrase matching (layout, cursor placement, overlap). (c) An in-process emulator or test backend with no binary: pi, opencode, crush, gitui, and codex's `VT100Backend` widget tests. Only (a) checks the grid of the real binary.
- Snapshot files come in three kinds, and the counts are not comparable. In-process render snapshots (no binary, no PTY): codex widget tests 1382 `.snap` under `src/`, crush 363 ANSI goldens, gemini-cli 112 (ink frames), opencode 1, gitui 3. Emulator-grid snapshots of a whole run: none found at scale; `vttest` supports them but the one adopter read asserts text only. Real-binary PTY snapshots: codex 5 `.snap` files (`tests/suite/snapshots/`) and nothing else found. Whole-binary journeys are asserted as screen text (television, codex, `omni`), not as stored snapshots.
- Nobody runs a model or a fuzzer against its own TUI. Model-in-the-loop work exists (gemini-cli `evals/`, terminal-bench, aider's benchmark, `claude plugin eval`) but grades agent behaviour, never rendering. No project property-tests TUI input sequences either.
- Flake control that recurs: pinned size, pinned `TERM`/colour, predicate polling with a deadline rather than sleeps, serial or capped parallelism for PTY tests, and CI-scaled budgets. The flaky cases the projects admit to are timing (gemini-cli `retry: 2` and a deflake workflow; television's fast-exit race; bubbletea's skipped teatest tests) and ambient size and colour (teatest).

## opencode, v2 branch (anomalyco/opencode@v2)

HEAD `55dde8810d8a` (`55dde88 feat(sdk): seed host plugins…`, 2026-10-09).
Package `@opencode/tui` 2.0.26 (`opencode-v2@55dde8810d8a:packages/tui/package.json:3-5`).

1. Harness. In-process test backend, no pty/tmux/real binary. `createTestRenderer`
from `@opentui/core/testing` with explicit dims (default 100×30, `useThread: false`)
(`opencode-v2@55dde8810d8a:packages/tui/test/fixture/app.ts:1,22-24`); input via
`setup.mockInput.typeText/pressKey/pressEnter`
(`opencode-v2@55dde8810d8a:packages/tui/test/new-session-id.test.tsx:71-72,77-79`).
The full-app fixture boots the real `run()` against a `Bun.serve` mock HTTP server plus
`terminalHandoff` of the test renderer
(`opencode-v2@55dde8810d8a:packages/tui/test/fixture/app.ts:32,39`).
Component tests use `testRender` from `@opentui/solid`
(`opencode-v2@55dde8810d8a:packages/tui/test/interactivity.test.tsx:1-2`).
No test imports `node-pty` (repo-wide grep for `from "node-pty"` in
`packages/*/test,packages/*/src` returns nothing); pty packages are runtime-only deps
for terminal panes (`opencode-v2@55dde8810d8a:packages/cli/package.json:61-66`).
2. What is asserted. Rebuilt char grid, not raw bytes: `captureCharFrame()` +
`toContain` (`opencode-v2@55dde8810d8a:packages/tui/test/app-lifecycle.test.tsx:50-51`),
`waitForFrame(predicate)` polling (`…:46`). Exactly one committed snapshot file,
`packages/tui/test/cli/tui/__snapshots__/inline-tool-wrap-snapshot.test.tsx.snap`
(`find packages/tui -name "*.snap"` → 1 file), written by bun's `toMatchSnapshot`
(`opencode-v2@55dde8810d8a:packages/tui/test/cli/tui/inline-tool-wrap-snapshot.test.tsx:250,254`);
reviewed/updated with the standard `bun test --update-snapshots` flow (no custom
snapshot tooling found). Churn: 9 commits touching
`packages/tui/test/cli/tui/__snapshots__` on `v2` since 2026-04-10 (command:
`gh api "repos/anomalyco/opencode/commits?path=packages/tui/test/cli/tui/__snapshots__&since=2026-04-10T00:00:00Z&per_page=100&sha=v2" --paginate`).
No rendered-image assertions in TUI tests (`fixture/diff-image.ts` is a PNG byte
fixture for the image-renderable unit test, not screenshots).
3. The model. Scripted fake server: `createFetch` returns canned JSON per route with
per-test overrides, `createEventStream().emit()` pushes SSE session events
(`opencode-v2@55dde8810d8a:packages/tui/test/fixture/tui-client.ts:42-64`
`createEventStream`, `…:66-` `createFetch`). Streaming repeatability comes from
driving Solid signals directly (e.g. `batch(() => { setContent(…);
setStreaming(false) })` in
`opencode-v2@55dde8810d8a:packages/tui/test/markdown-streaming.test.tsx:20-24`)
and deterministic promise resolvers for fetch (`…/app-lifecycle.test.tsx:13-16`
`Promise.withResolvers`). Separately, `@opencode/simulation` provides a scripted
OpenAI-compatible provider backend streaming `ProviderResponseEvent | finish`
(`opencode-v2@55dde8810d8a:packages/simulation/src/backend/simulated-provider.ts:30-40`).
4. Scope. Narrow per-feature suites: `find packages/tui/test -name "*.test.*" | wc -l`
→ 169 files (dialog/session-tabs/prompt/context/mini/…); 6 test files use the
full-app `createAppFixture` (`grep -rln createAppFixture packages/tui/test | wc -l`
→ 7 incl. the fixture itself). Per-package `bun test --timeout 30000 --only-failures`
(`opencode-v2@55dde8810d8a:packages/tui/package.json:9`). CI: unit job
`timeout-minutes: 20`, web-app Playwright e2e `timeout-minutes: 30`
(`opencode-v2@55dde8810d8a:.github/workflows/test.yml:107,240`); actual CI wall
time not found (no timing data in repo).
5. Exploration. Yes, infrastructure for it, v2-only: `OPENCODE_DRIVE` swaps the real
renderer for a headless `SimulationRenderer` plus a websocket UI control server
(`ui.capture/ui.state/ui.snapshot/ui.type/ui.press/…`)
(`opencode-v2@55dde8810d8a:packages/tui/src/app.tsx:254-257`,
`opencode-v2@55dde8810d8a:packages/simulation/src/frontend/server.ts:18-34`),
with `Timeline` output recordings
(`opencode-v2@55dde8810d8a:packages/simulation/src/recording.ts:18-36`) and the
simulated provider backend above — i.e. a real binary drivable by an external
agent over JSON-RPC. No scheduled agent-vs-TUI runs, fuzzers, or findings/cost
data found (looked in `packages/simulation/test` — unit tests for
protocol/control-server/manifest/recording only — and `.github/workflows`).
6. Flakiness. Predicate polling (`waitForFrame`) + explicit `renderOnce` instead of
sleeps; explicit width/height per fixture (`fixture/app.ts:22-24`); `animations:
false` in app-fixture configs
(`opencode-v2@55dde8810d8a:packages/tui/test/new-session-id.test.tsx:27`).
Skip markers found are platform gates (`test.skipIf(process.platform === "win32")`,
`opencode-v2@55dde8810d8a:packages/tui/test/app-lifecycle.test.tsx:1347`,
`…/mini/scrollback.surface.test.ts:697`) plus two `test.skip` in
`mini/footer.view.test.tsx:1540,1564`. `retry` hits in TUI tests are domain
vocabulary (retry-provider UI), not a retry harness; no TERM/COLUMNS/LINES
pinning found (grep over `packages/tui/test`, `bunfig.toml`, `test.yml` empty);
no test-level retry config found.

Copy / avoid for Fiber. Copy: (a) the full-app fixture shape — real `run()` +
mock server fetch + `waitForFrame`-style predicate polling — as the upgrade path
from raw-byte matching to frame-content assertions; (b) the drive-mode pattern
(headless renderer + JSON-RPC control + recordings) if Fiber ever wants agent
QA of its own TUI. Avoid/pitfall: the mock surface is large (a ~200-line default
fetch router in `tui-client.ts` that throws on unexpected routes) — every new
server endpoint breaks tests until the mock learns it.

## opencode, default branch (`dev`; sst/opencode == anomalyco/opencode)

HEAD `055d95bb7e27` (`055d95b fix(tui): highlight C++ module interface files
(#53852)`, 2026-10-09). Package `@opencode-ai/tui` 1.18.35
(`opencode-dev@055d95bb7e27:packages/tui/package.json:3-5`). No `simulation`,
`ai`, or `session-ui` packages (vs v2); server side lives in `packages/opencode`
instead of `packages/core`+`packages/server`.

1. Harness. Same family (in-process `@opentui/core/testing`), older/less factored:
no `createAppFixture` helper (file does not exist; `fixture/` holds only
`fixture.ts`, `tui-environment.tsx`, `tui-plugin.ts`, `tui-runtime.ts`,
`tui-sdk.ts`). Tests hand-roll setup: `createTestRenderer({width: 80, height: 24,
useThread: false })` + `mock.module("@opentui/core", () => ({ ...core,
createCliRenderer: async () => setup.renderer }))` to inject the test renderer
into the real `run()` (`opencode-dev@055d95bb7e27:packages/tui/test/app-lifecycle.test.tsx:11-13`).
Component tests use `testRender` + `captureCharFrame` exactly as in v2 (the
`inline-tool-wrap-snapshot` suite exists in both, though its fixtures/assertions
differ substantially — v2 tests new session-route helpers such as
`executeCallSummary`, dev tests older ones such as `formatSubagentTitle`).
2. What is asserted. Same: char-frame strings, `toContain`/`toBe`; one `.snap`
file at the same path (`find packages/tui -name "*.snap"` → 1 file).
Churn: 5 commits touching that dir on `dev` since 2026-04-10 (same `gh api`
command with `sha=dev`); the v2 list (9) is a superset — the extra 4 include
`e6f660f feat(tui): add v2 terminal interface` and
`5ae9309 refactor(core): replace bash tool with shell tool`.
3. The model. Same shape, smaller: `createFetch` + `createEventSource`
(SSE-shaped, `emit` throws if nobody subscribed)
(`opencode-dev@055d95bb7e27:packages/tui/test/fixture/tui-sdk.ts:18,64`).
No simulated-provider package on this branch.
4. Scope. Much smaller: `find packages/tui/test -name "*.test.*" | wc -l` → 46
files (vs 169 on v2); e.g. `app-lifecycle.test.tsx` holds 2 tests (SIGHUP
disposal, exit epilogue) instead of v2's session-dialog suites. Same
`bun test --timeout 30000` runner; same CI workflow file layout (both branches
run `test.yml`).
5. Exploration. None found: zero hits for `OPENCODE_DRIVE|simulation|Simulation`
in `packages/tui/src` and `packages/cli/src` on dev.
6. Flakiness. Same techniques (explicit 80×24 dims, `renderOnce` pairs); no
branch-specific flake controls found beyond what v2 has.

How the TUI tests differ (v2 vs dev). v2 keeps the in-process
`createTestRenderer`/`testRender`/`captureCharFrame` style but (a) factors
full-app setup into a shared `createAppFixture` (mock HTTP server + SSE events +
`terminalHandoff`), (b) grows the suite ~3.7× (46 → 169 files) with session,
mini-composer, and plugin suites, (c) adds the `@opencode/simulation` drive
stack (headless renderer, websocket UI control, recordings, fake OpenAI
provider) gated on `OPENCODE_DRIVE`/`OPENCODE_SIMULATE`, and (d) renames the
line from `@opencode-ai/*` (v1 lineage, `packages/opencode` server) to
`@opencode/*` (Effect-based `packages/core`+`packages/server` split).

Copy / avoid for Fiber. Nothing extra beyond the v2 notes; the dev branch is
mostly useful as the "before" picture — its hand-rolled `mock.module`
renderer injection is the pattern v2's `createAppFixture` replaced, so Fiber
should skip straight to the fixture helper and not copy the mock-module style.

## pi (earendil-works/pi; installed package + repo)

Repo HEAD `42a3497d03ad` (`42a3497 feat(durable): read returns images…`,
2026-10-09). Installed `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/`
v1.0.0: ships NO tests — `files` covers only `dist`, `docs`, `examples`
(`package.json` `files` field); `find … -name "*.test.*" -o -name "__tests__" -o
-name "*.snap"` under the install prefix hits only a dependency's own test files
(`node_modules/@stablelib/base64/*.test.*`).

1. Harness. In-process virtual terminal, no pty/tmux/spawned binary.
`packages/tui/test/virtual-terminal.ts` wraps `@xterm/headless` (devDependency
`5.5.0`, `pi@42a3497d03ad:packages/tui/package.json:59-62`) in a class
implementing pi's `Terminal` interface
(`pi@42a3497d03ad:packages/tui/test/virtual-terminal.ts:1-27`), injected into
`createInteractiveTui({…, terminal})` /
`new TuiMainScreen(new VirtualTerminal())`
(`pi@42a3497d03ad:packages/coding-agent/test/interactive-tui.test.ts:1-17`,
`pi@42a3497d03ad:packages/coding-agent/test/bug-report.test.ts:27-32`).
Input via `sendInput(data)` (raw escape sequences, incl. SGR mouse
`\x1b[<…M`), resize via `resize(cols, rows)`; `node --test` runs `packages/tui`
tests, vitest runs `packages/coding-agent` tests. No `node-pty` dependency
found (grep over tui/coding-agent/root `package.json`); `pi-test.sh` just runs
the CLI from source, not under a pty.
2. What is asserted. Real screen-grid rebuild via xterm: `getViewport(): string[]`
reads the active buffer's visible lines
(`pi@42a3497d03ad:packages/tui/test/virtual-terminal.ts:~190-210`), asserted
with `toContain` (e.g. jump-to-bottom hint
`pi@42a3497d03ad:packages/coding-agent/test/interactive-tui.test.ts:101-104`);
a `RecordingTerminal` subclass additionally captures raw writes to assert on
ANSI sequences (e.g. alt-screen `\x1b[?1049h`,
`pi@42a3497d03ad:packages/coding-agent/test/interactive-tui.test.ts:29-64`).
Snapshots: zero `*.snap` files in the repo (`find . -name "*.snap"
-not -path "*/node_modules/*"` empty); 12 `toMatchInlineSnapshot` hits, used
for component render output, e.g. `normalizeRenderedOutput(…)`
(`pi@42a3497d03ad:packages/coding-agent/test/interactive-mode-status.test.ts:790,836`).
3. The model. Scripted `streamFn` on the `Agent`: tests build canned
`AssistantMessage`s and deliver them through
`createAssistantMessageEventStream()` via `queueMicrotask`
(`pi@42a3497d03ad:packages/coding-agent/test/agent-session-retry.test.ts:60-80`),
with shared `createModelRegistry` test utils
(`…/test/model-runtime-test-utils.ts`, used at `…:16`). Timing is made
repeatable with microtask-queued scripted streams plus `Terminal.waitForRender()`
(`nextTick` + 20 ms + xterm `flush`)
(`pi@42a3497d03ad:packages/tui/test/virtual-terminal.ts:214-219`).
4. Scope. `find packages/tui/test -name "*.test.*" | wc -l` → 43 files (unit:
editor/layout/keys/render incl. 1024-line `tui-render.test.ts`, 32 `it/test`
blocks); `ls packages/coding-agent/test/*.test.ts | wc -l` → 210 files, mostly
narrow (single command/selector/viewport behaviors); the closest thing to a TUI
journey test is `interactive-tui.test.ts` (523 lines, TUI chrome: modes,
copy/paste, status indicators — no full agent loop under the TUI; agent-session
suites run headless via `test/suite/harness.ts`). CI is one `ubuntu-latest`
job running `npm test` (per-workspace)
(`pi@42a3497d03ad:.github/workflows/ci.yml:16-45`) with `testTimeout: 30000`
and offline-by-default `PI_OFFLINE=1`
(`pi@42a3497d03ad:packages/coding-agent/vitest.config.ts:8-14`); actual CI wall
time not found.
5. Exploration. No agent/fuzzer-vs-TUI found. `packages/evals` is
model-in-the-loop behavioral evals in Docker requiring `PI_PROVIDER`/`PI_MODEL`
(`pi@42a3497d03ad:packages/evals/README.md:25-30`), not TUI QA; `fuzz` greps
hit only unrelated component names (fuzzy matcher). Stated as not found
(searched `packages/evals/src`, workflow files, `packages/*/test`).
6. Flakiness. Settle helper `waitForRender()` (above); explicit sizes per test
(e.g. `new RecordingTerminal(50, 4)`); real terminal semantics come free from
xterm rather than env pinning — no TERM/COLUMNS pinning in tests (prod fallback
`process.stdout.columns || COLUMNS || 80` stays in
`pi@42a3497d03ad:packages/tui/src/terminal.ts:533`, unused by `VirtualTerminal`
which keeps fixed dims); no `test.skip/it.skip/describe.skip`, no
retry/flaky config in vitest setup (only `retry` hit in the TUI tests is a
status-indicator kind name,
`pi@42a3497d03ad:packages/coding-agent/test/interactive-tui.test.ts:472`).

Copy / avoid for Fiber. Copy: (a) `VirtualTerminal` — a real emulator grid
(`@xterm/headless`) implementing Fiber's terminal interface would turn the 24
raw-byte phrase tests into viewport assertions robust to escape-sequence churn,
while still running the real binary output; (b) the `waitForRender`
settle-helper idiom (yield + small delay + drain) for Fiber's byte-stream
polling. Avoid/pitfall: the fixed 20 ms sleep inside `waitForRender` is a
latent flake source under CI load — prefer Fiber's predicate-with-deadline
polling over fixed sleeps; also note pi never closes the loop (agent + TUI
together), so there is no worked example of asserting streamed agent output
through the grid.


## codex (https://github.com/openai/codex, `codex-rs/tui`)

1. Harness. Three layers. (a) Unit/widget tests render in-process with
`ratatui::backend::TestBackend`, e.g.
`codex@4bad6d78e9b5:codex-rs/tui/src/analytics_tests.rs:33,40`
(`Terminal::new(TestBackend::new(width, height))`). (b) A `VT100Backend`
(`codex@4bad6d78e9b5:codex-rs/tui/src/test_backend.rs:1,135`) wraps
`CrosstermBackend<vt100::Parser>` so drawing goes through a real terminal
emulator in-process; tests assert on `term.backend().vt100().screen()`,
e.g. `codex@4bad6d78e9b5:codex-rs/tui/tests/suite/vt100_history.rs:31,45`.
`force_color_output(true)` is set in the constructor
(`codex@4bad6d78e9b5:codex-rs/tui/src/test_backend.rs:32`).
(c) Real-binary PTY tests: `PtyCodex` in
`codex@4bad6d78e9b5:codex-rs/tui/tests/suite/focus_palette.rs:279,420`
spawns the built `codex-tui`/`codex` binary behind `libc::openpty` with a
pinned `winsize { ws_row: 32, ws_col: 120 }` (`:326,327`),
`TERM=xterm-256color`, `TERM_PROGRAM=kitty`, TMUX vars removed (`:362,368`),
and a `vt100::Parser` on the master side (`:380,381`); it answers the
binary's own capability probes (cursor `ESC[6n`, keyboard `ESC[?u`,
palette `OSC 10/11`, `:440,470`) and polls with `wait_for_screen` /
`wait_for_focus_input` (`:417,438`). A fourth, tmux-based
resize/reflow smoke drives the real binary at 120x40 and splits the pane
(`codex@4bad6d78e9b5:codex-rs/tui/tests/suite/resize_reflow.rs:24,120`).
`codex-rs/exec/tests/suite/` (20 files, 73 `#[test]`/`#[tokio::test]`
markers) and `codex-rs/cli/tests/` are headless `assert_cmd`-style e2e,
not PTY (e.g. `codex@4bad6d78e9b5:codex-rs/exec/tests/suite/ephemeral.rs:1,50`).
2. What is asserted. PTY tests assert substring presence in the rebuilt
`vt100` screen grid (`screen_contains`, `:505,511`), never raw bytes or
images. Widget/unit tests snapshot rendered text with `insta`
(dev-dep `codex@4bad6d78e9b5:codex-rs/tui/Cargo.toml:183`; e.g.
`insta::assert_snapshot!(rendered)` in `history_cell/mcp_tests.rs:48`).
Snapshot volume (`find codex-rs/tui -name '*.snap' | wc -l`): **1387**
total = **1382** under `src/` (per-widget `__snapshots__`-style
`snapshots/` dirs, e.g. `history_cell/snapshots/`) + **5** under
`tests/suite/snapshots/` (PTY warning goldens, e.g.
`all__suite__daemon_compatibility__daemon_feature_mismatch.snap`).
Review/update is standard insta (`cargo insta review/accept`; no repo
docs found mentioning `INSTA_UPDATE` — `grep -rn INSTA_UPDATE|cargo insta`
in `codex-rs` and `.github` found nothing). Churn
(`git -C codex log --oneline --since=2026-04-10 -- <path> | wc -l`,
full-history clone): **1647** commits touch `codex-rs/tui/`; of those
**5** touch `tests/suite/snapshots/` (9 `.snap` changes) —
`7d4c7a0767`, `d5355e95ef`, `d6093d3228`, `1537497e52`, `70e8fe1be3` —
and **52** touch `src/history_cell/snapshots/`. So PTY goldens almost
never churn; widget snapshots churn steadily with UI work.
3. The model. `wiremock` mock servers (dev-dep, `Cargo.toml:190`) via
shared crates `core_test_support` (`Cargo.toml:177`, sources at
`codex-rs/core/tests/common/`) and `app_test_support` (`Cargo.toml:175`,
`codex-rs/app-server/tests/common/`). Scripted SSE bodies are built with
helpers like `responses::sse`, `ev_response_created`,
`ev_assistant_message`, `ev_completed`
(`codex@4bad6d78e9b5:codex-rs/core/tests/common/responses.rs:753,864`)
and served whole with `sse_response` (200 + `text/event-stream`,
`:1083,1087`); sequencing via `SeqResponder` or
`mount_sse_once[_match]` (`:1131,1144`). No chunk pacing/delay on the
SSE path — the body arrives at once; explicit delays exist only for the
models-list endpoint and websocket handshake
(`mount_models_once_with_delay`, `accept_delay`,
`:577,581` and `:1166,1274`). Repeatability comes from
request-body invariant validation on every mock call (`:740,748`) plus
`expect(n)` call counts in `create_mock_responses_server_sequence`
(`codex@4bad6d78e9b5:codex-rs/app-server/tests/common/mock_model_server.rs:14,34`).
4. Scope. Narrow per-feature. `grep -rn '#\[test\]\|#\[tokio::test'`
counts: **5768** markers across `tui/src` + `tui/tests` (of which 3473
plain `#[test]` in `src/`), i.e. thousands of in-process widget tests;
the real-binary suite is **15 tests that start `PtyCodex`** (`grep -c
'#\[test\]\|#\[tokio::test'` in the 10 `tests/suite/` files that use
`PtyCodex`: focus_palette 6, nine files with 1 each) **plus 4 `#[ignore]`d tmux
tests** in `resize_reflow.rs`. `vt100_history.rs` (7 tests) and
`vt100_live_commit.rs` are in-process: they build a `Terminal<VT100Backend>`
and call `insert_history_lines`, with no binary or PTY
(`vt100_history.rs:23-36`). CI time: not found in
repo (no published durations); `rust-ci.yml` sets per-platform
`timeout_minutes: 30`
(`codex@4bad6d78e9b5:.github/workflows/rust-ci.yml:167,184`) and runs
`cargo test` (`:156`); nextest appears in `rust-ci-full-nextest-platform.yml`.
5. Exploration. Not found. No fuzzer/proptest/bolero target
(`git grep fuzz` hits only `codex-utils-fuzzy-match` dependency and
fuzzy-filter UI code), no `evals/` dir at repo root, no agent-vs-own-TUI
harness found in `codex-rs/tui`, `docs/`, or workflows.
6. Flakiness. Controls found: fixed sizes everywhere (in-process
`TestBackend::new(w,h)` per test; PTY pinned 120x32; tmux smoke 120x40);
`TERM`/`TERM_PROGRAM` pinned and capability probes answered by the
harness itself; colour forced on in `VT100Backend`; polling
`wait_for_screen` with 30 s `STARTUP_TIMEOUT` / 5 s
`FOCUS_INPUT_TIMEOUT` (`focus_palette.rs:21,22`) instead of fixed
sleeps (only two `tokio::time::sleep(200ms)` in `directory_trust.rs:195,208`);
`skip_if_no_network!` / `skip_if_sandbox!` macros
(`codex@4bad6d78e9b5:codex-rs/core/tests/common/lib.rs:579,605`);
`#[serial]` (`serial_test` dev-dep, `Cargo.toml:186`) on env-sensitive
unit tests (e.g. `src/app/tests.rs:509`); `#[ignore]`d manual-only
tests (all 4 tmux resize tests, `resize_reflow.rs:18`; Windows path
snapshots, `history_cell/tests.rs:759,1865`); one resize test notes
Rosetta slowness in CI (`focus_palette.rs` STARTUP comment). No
`retry`/`flaky` test-retry mechanism found; a recent fix commit is
`9a59289fbe Fix races in remote environment and session replacement tests`.

Copy / avoid. Copy: (1) `VT100Backend` — CrosstermBackend over a
`vt100::Parser` gives Fiber a screen grid with almost no new code (it
already uses ratatui + raw bytes); assert on grid contents, keep the
byte-phrase tests as-is. (2) PTY harness answering the child's own
capability probes (cursor/keyboard/palette) instead of disabling them —
Fiber's fake-provider tests could do the same for any OSC/kitty probes.
Avoid/pitfall: two snapshot systems (1382 widget snaps + 5 PTY goldens)
with different churn rates; if Fiber adds grid snapshots, keep them
per-widget, not whole-screen PTY goldens, or every layout tweak breaks
everything.

## gemini-cli (https://github.com/google-gemini/gemini-cli)

1. Harness. Real binary in a PTY via `@lydell/node-pty` (a maintained
`node-pty` fork): `TestRig.runInteractive()` spawns the bundled
`bundle/gemini.js` with `pty.spawn(executable, args, { name:
'xterm-color', cols: 80, rows: 80, cwd, env })`
(`gemini@9b6e0265d16b:packages/test-utils/src/test-rig.ts:1616,1665`,
opts at `:1647,1652`). Keystrokes go through `ptyProcess.write`
(`InteractiveRun.type/sendText/sendKeys`, `:271,320`); `type()` waits
per-character for the echo to appear (5 s/char, `:283,299`). Unit level:
components render in-process with ink's `render` plus `@xterm/headless`
`Terminal`, wrapped in `packages/cli/src/test-utils/render.tsx`
(`RenderInstance` exposes `frames`, `lastFrame`, `lastFrameRaw`,
`terminal`, `:133,202,374,406`); `AppRig` (`test-utils/AppRig.tsx`)
mounts the full `AppContainer` with mocked config/services for
interaction tests (key dispatch, `awaitingResponse` tracking).
2. What is asserted. Integration tests assert `stripAnsi(output)`
substrings via `expectText` polling (`test-rig.ts:258,268`) and
telemetry/tool-call logs (`waitForTelemetryEvent`, `waitForToolCall`,
`readToolLogs`, `:1055,1175`); no screen-grid cell asserts, no images
at this level (an SVG serializer `generateSvgForTerminal` exists in
`test-utils/svg.ts` but is a debug aid, wired as `generateSvg` in
`render.tsx:167`). Unit tests use vitest file snapshots of `lastFrame`:
**112** `.snap` files under `packages/cli/src`
(`find packages/cli/src -name '*.snap' | wc -l`), e.g.
`ui/auth/__snapshots__/AuthDialog.test.tsx.snap` (box-drawing frames);
**0** `.snap` in `integration-tests/`. Churn: clone is `--depth 1`
so `git log` is unusable; via API
(`gh api repos/google-gemini/gemini-cli/commits?path=integration-tests&since=2026-04-10... --paginate --jq`):
**19** commits touch `integration-tests/` in ~6 months, mostly
deflake/stabilize fixes (`b0bc3f72 test(integration): deflake
run_shell_command and file-system-interactive`, `test(e2e): stabilize
file-system-interactive test on slow runners`, `test(e2e): default
integration tests to Flash Preview`), i.e. low churn, fixes are
stability not goldens. Snapshot review is standard vitest
(`-u`/`--update`); no repo-specific snapshot policy found.
3. The model. Recorded/replayed model traffic: `*.responses` JSONL
goldens (e.g. `integration-tests/flicker-detector.max-height.responses`
— lines like `{"method":"generateContentStream","response":[...]}`);
`rig.setup(name, { fakeResponsesPath })` copies the golden into the
test dir (`test-rig.ts:401,406`) and passes one of `--fake-responses`
/ `--fake-responses-non-strict` / `--record-responses <path>`
(`:548,554`), so tests replay canned `generateContent` /
`generateContentStream` payloads deterministically, and `--record-`
mode rewrites the golden from live traffic (`:1028,1030`). Streaming
repeatability = whole recorded stream replayed, no pacing controls
found; `sendText` vs per-char `sendKeys` exists to dodge paste
detection, and `type('\r')` sleeps 50 ms to avoid fast-return
conversion (`:273,280`).
4. Scope. **48** interactive test files
(`ls integration-tests/*.test.ts | wc -l`) out of 114 entries; each
file is a short journey (start app → type prompt → await telemetry /
text), e.g. `flicker.test.ts` (prompt → `user_prompt` event +
`ui.flicker.count` metric). CI time: not found (no published
durations); budgets are `testTimeout: 300000` (5 min/test) and
`retry: 2` in `integration-tests/vitest.config.ts`, `timeout-minutes:
60` on the slow Windows shard (`ci.yml:391`), and separate
`test:integration:sandbox:{none,docker,podman}` scripts plus a
`chained_e2e.yml` post-merge E2E workflow (`package.json:53,64`).
5. Exploration. Yes — `evals/` (45 `.ts` files) runs the real agent
against scripted tasks with a live model (`EVAL_MODEL =
process.env['GEMINI_MODEL'] || PREVIEW_GEMINI_FLASH_MODEL`,
`gemini@9b6e0265d16b:evals/test-helper.ts:30`; cases declare
`ALWAYS_PASSES` (run in every CI, `test:always_passing_evals`) vs
`USUALLY_PASSES`/`USUALLY_FAILS` trendline sets (`:33,51`,
`package.json:53,54`); API-error retries with skip-after-3
(`withEvalRetries`, `test-helper.ts:53,90`). Cost control is by policy
tier + `RUN_EVALS=1` gate (`test-helper.ts:381`); no per-run dollar
figure found in repo. This is agent-QA-by-model, though aimed at
agent behaviour, not TUI rendering.
6. Flakiness. Explicit machinery: `retry: 2` in integration vitest
config; `poll()`/`expectText`/`waitFor*` everywhere instead of fixed
sleeps; `type()` echo-wait per char; `globalSetup.ts` deletes
`NO_COLOR` for consistent theme behaviour; terminal pinned to 80x80
`xterm-color`; `getDefaultTimeout()` scales 15 s local / 30 s
container / 60 s CI (`test-rig.ts:37,43`); `RUN_FLAKY_INTEGRATION=1`
opt-in set (`package.json:57`); a dedicated `deflake.yml` workflow
(re-runs E2E N times) and deflake commits; Unicode/width issues not
called out in the files read.

Copy / avoid. Copy: (1) recorded-`*.responses` replay with a
`--record-` rewrite mode — Fiber's fake provider could record once
against the real server and replay deterministically, including
multi-step tool-call sequences. (2) `ALWAYS_PASSES` vs `USUALLY_PASSES`
tiering so model-adjacent tests don't gate merges. Pitfall: 80x80 is
far from real terminals and `stripAnsi`+substring asserts can't catch
layout regressions (cursor placement, reflow, overlapping regions) —
Fiber's byte-stream phrase matching has the same blind spot, which is
exactly what a grid rebuild (codex-style) would cover.

## Claude Code (no public source; probeable surface only)

Nothing about Anthropic's own internal TUI tests is public. Evidence
from this machine (Claude Code 2.1.296, docs repo
`2301018b1f61`):

1. Harness. No first-party TUI harness is probeable. `claude --help`
exposes runtime flags only (`-p/--print`, `--ax-screen-reader`,
`--bare`, etc.), no test flags. What ships for third parties is
plugin testing, not TUI testing: `claude plugin test [dir]` "runs
every `*.test.ts(x)` under dir, each file in a child of this binary,
in an environment like the one the mod's hooks run in ... imports its
kit from `claude-code/testing`" (i.e. unit tests for plugin/hook
code, no terminal involved).
2. What is asserted. For plugins: unit-test assertions the plugin
author writes. For evals: scored grades (see 5). No snapshots, screen
grids, or terminal-size pinning anywhere in the public surface.
3. The model. `claude plugin eval` runs eval cases
(`case.yaml`/`prompt.md` + `graders/*.md` under `evals/`) as full
`claude` child processes on the operator's own credential
(`--concurrency 1-8`, `--model`, `--runs` default 3, `--threshold`,
`--mocks record|off` for MCP servers, `--max-cost-usd`,
`--judge-model` default haiku, HTML+JSON reports). This is a
real-model harness for *plugin* behaviour, not a scripted-provider
TUI test.
4. Scope. Public repo `anthropics/claude-code` contains only
docs/plugins/examples/hooks/settings (`ls` shows `plugins/`,
`examples/{gateway,hooks,mdm,settings}`, `mods/`, no `docs/CI`,
no test dirs); the closest testing guidance is
`plugins/plugin-dev/skills/command-development/references/testing-strategies.md`,
which covers slash-command YAML/syntax validation levels, not
terminal interaction. No counts or CI times exist publicly.
5. Exploration. `claude plugin eval` *is* agent-QA-by-model, but the
agent under test is the plugin's, evaluated by LLM graders — there is
no public equivalent of "run the model against its own TUI". Cost is
explicitly bounded per run (`--max-cost-usd`, `max_turns`,
`timeout_seconds`, `--judge-model`).
6. Flakiness. Nothing public. Binary forensics
(`strings` on `~/.local/share/claude/versions/2.1.296`, Mach-O arm64):
`node-pty` appears 4x — the one context recovered is a runtime memory
diagnostic ("leak may be in native addons (node-pty, etc.)"), i.e. a
shipped dependency, not a test harness; `vt100` 9x matches are TERM
regexes in bundled supports-color/hyperlink detection
(`/^screen|^xterm|^vt100|.../`); `__snapshots__` 6x with context not
recovered (a full-`strings`+`grep` pass exceeded 170 s on the 240 MB
binary and was abandoned); `insta` hits are substring noise
(instant/install). No `ink-testing-library`, `TestBackend`, or
snapshot-review tooling identified.

Copy / avoid. Copy: (1) the `ALWAYS`/`USUALLY` idea taken further —
`plugin eval`'s per-case `runs: 3`, numeric threshold, and
`--max-cost-usd` budget ceiling are a good template if Fiber ever
runs model-in-the-loop QA. (2) `claude-code/testing` kit pattern: ship
the test kit *with* the binary so third-party tests run in a faithful
environment. Avoid: there is nothing to copy for headless scripted
TUI tests — Claude's public tooling assumes a live model and a live
terminal, which is precisely the setup Fiber's fake-provider rig is
built to avoid.


## aider (Aider-AI/aider)

Aider has no full-screen TUI; its interface is a prompt_toolkit REPL plus non-interactive `--exit` runs.
There is no PTY/tmux/recorded-session harness for its own UI.

1. Harness. Unit/in-process only. CLI entry points are called directly with
   `prompt_toolkit.input.DummyInput` / `DummyOutput`, e.g.
   `main(["--no-git", "--exit", "--yes"], input=DummyInput(), output=DummyOutput())`
   (aider@5dc9490bb35f:tests/basic/test_main.py:49; import at :11).
   `pexpect` appears in exactly one production file, not in tests:
   `aider/run_cmd.py` uses `pexpect.spawn(shell, args=["-i", "-c", command], …)`
   (aider@5dc9490bb35f:aider/run_cmd.py:116) then `child.interact(…)` (:124) so a
   *user command aider runs* keeps a TTY; it never drives aider itself.
   (`grep -rl pexpect` over the repo returns only `aider/run_cmd.py`, HISTORY.md and a chat fixture.)
2. What is asserted. Return values, created files, command-dispatch side effects
   (e.g. `test_cmd_add` creates foo.txt/bar.txt and asserts existence,
   tests/basic/test_commands.py `test_cmd_add`). No snapshots, no screen grid.
   Snapshot-dir churn: n/a (no snapshot dir).
3. The model. Fully mocked at the HTTP boundary: tests patch the completion call,
   e.g. `mock_completion.side_effect = […]` then assert on the parsed reply
   (aider@5dc9490bb35f:tests/basic/test_sendchat.py:24-55). Streaming/timing never
   enter the picture — repeatability comes from never calling a model.
   Quality-with-a-model lives outside the test suite in `benchmark/benchmark.py`:
   threaded (`--threads`, :201) SWE-bench/polyglot exercises run against real models,
   with a "run in a docker container" warning because it executes unvetted model code (:253).
4. Scope. 32 files under `tests/basic/` (`ls tests/basic/test_*.py | wc -l` → 32),
   all narrow per-command/per-module unit tests. CI (`ubuntu-tests.yml`) is bare
   `pytest` on a 5-Python matrix (aider@5dc9490bb35f:.github/workflows/ubuntu-tests.yml:56);
   CI wall time not found (no timeouts recorded in config).
5. Exploration. None against its own UI. Model-in-the-loop QA = the benchmark
   harness above (real models, docker, task pass/fail) — it finds regressions in
   model+prompt behaviour, at the cost of real API calls per exercise; no cost
   figures in-repo.
6. Flakiness. Not found as a category: no `flaky`/`rerun`/`retry` markers in
   tests or workflows (`grep -rn "flaky\|rerun" tests/basic/*.py .github/workflows/*.yml`
   returns only unrelated "retrying…" tool-error strings); no TERM/COLUMNS pinning,
   no sleep/wait helpers — consistent with no timing-dependent UI tests.

Copy / avoid. Copy: the `DummyInput`/`--exit` pattern is the cheapest rung for
anything Fiber can test without a terminal (pure logic). Avoid: pexpect is a red
herring here — aider uses it in production to give *child commands* a TTY, not to
test itself; do not cite aider as "pexpect-based TUI testing".

## goose (block/goose)

Goose's interactive surface is the CLI `session` loop plus an Electron desktop app.
Neither is driven in a PTY; the CLI is tested at the protocol level, the desktop
app in Playwright with a live backend.

1. Harness. (a) CLI/agent crates: Rust integration tests drive ACP/session APIs
   against stub agents and fixture MCP servers — e.g. `roam_acp_client.rs` stands up
   "a stub ACP *agent* … over the real iroh transport" with no LLM
   (goose@3bd852002903:crates/goose-cli/tests/roam_acp_client.rs:1-11), and
   `crates/goose-test-support` provides `McpFixture`/`McpFixtureServer` plus a
   `capture` binary with `record`/`playback` subcommands for stdio MCP traffic
   (goose@3bd852002903:crates/goose-test/src/bin/capture.rs:13-42).
   `grep -rln "pty\|expectrl\|rexpect\|portable-pty\|vt100\|insta\|snapbox" crates/`
   returns only false positives (e.g. "empty", "instance_id"). (b) Desktop app:
   Playwright launches a fresh Electron binary per test
   ("launches a fresh Electron app for EACH test", …/ui/desktop/tests/e2e/fixtures.ts:13)
   and selects on `[data-testid="chat-input"]` (:25). 5 spec files
   (`ls ui/desktop/tests/e2e/*.spec.ts | wc -l` → 5).
2. What is asserted. Protocol payloads (echoed prompts, session ids), MCP
   record/replay parity (`mcp_replays/` fixtures + `.results.json`), and DOM
   selectors/screenshots-on-failure in Playwright. No terminal grid anywhere.
   Churn (6 mo): `crates/goose/tests/mcp_replays` 8 commits, `ui/desktop/tests/e2e` 2.
3. The model. Canned SSE fixtures served over HTTP: "the canned SSE fixtures (which
   return Chat Completions format)" with a pinned `TEST_MODEL = "gpt-4.1"` because
   newer model routes need a different mock format
   (goose@3bd852002903:crates/goose-test-support/src/session.rs:4-8). Repeatability
   = fixed fixture bytes, not timing control. The desktop e2e instead uses ambient
   user config ("uses the user's existing Goose configuration (providers, models…)"),
   i.e. it can hit real providers.
4. Scope. Dozens of narrow protocol/scheduler/permission tests in `crates/goose/tests/`
   (~30 files) plus 5 Playwright journeys (chat, context-management, loading,
   performance). Playwright runs serially: `workers: 1`, `fullyParallel: false`,
   60 s test timeout (goose@3bd852002903:ui/desktop/playwright.config.ts:5-10).
   CI wall time not found in config (no timeout values for the Rust jobs either).
5. Exploration. None against its own TUI (no fuzzer/agent-QA found;
   `evals/harbor/` is a model-in-loop eval runner — `runner.py`, recipes,
   remote-run notes mentioning screen/tmux for *hosting* the run, not asserting UI).
6. Flakiness. Playwright-side: serial workers, 30 s expect/action timeouts,
   trace+video retained on failure (`playwright.config.ts:5-21`); no `retries:` key
   (so Playwright default 0). Rust-side: session-id enforcement fixtures and
   fixed SSE bytes; no TERM/size pinning found (nothing terminal-dependent exists).

Copy / avoid. Copy: the `record`/`playback` MCP-traffic pattern maps directly onto
Fiber's fake-provider server (record once, replay deterministically). Avoid: ambient-
config e2e (tests that depend on the developer's own provider keys) — hermetic
fixtures only.

## crush (charmbracelet/crush)

Bubble Tea (v2) agent. No PTY, no teatest, no VHS in the test suite — TUI testing
is in-process render/golden tests plus model-logic unit tests.

1. Harness. Direct construction of UI items/models + calling render methods, e.g.
   `NewUserMessageItem(…).RawRender(width)` then line-structure assertions
   (crush@8b1824282ecc:internal/ui/chat/user_render_test.go:13-40).
   `grep -rl "teatest\|golden\|vhs"` over `*.go` hits only golden-file render tests
   (`internal/ui/diffview/*_test.go`) — teatest itself is never imported.
2. What is asserted. Escaped-ANSI golden files via `charmbracelet/x/exp/golden`:
   `golden.RequireEqual(t, []byte(output))` with `-update` to refresh
   (crush@8b1824282ecc:internal/ui/diffview/diffview_test.go:150; mechanism at
   x@faa4adf95555:exp/golden/golden.go:17,27), plus structural asserts
   (`assertLineWidth(t, 40/120, output)`, :154-156) and ANSI-stripped line
   comparisons (`renderedLines` strips styling before comparing). 363 `.golden`
   files (`find . -path "*testdata*" -name "*.golden" | wc -l` → 363).
   Churn (6 mo): 2 commits touching `internal/ui/diffview/testdata`, 2 touching
   `*.golden` — low. Review/update: `go test ./… -update` writes files; diffs
   reviewed as normal code diffs (golden files escape control codes per line so
   they diff cleanly).
3. The model. Absent from UI tests — items are built from static
   `message.Message` values, so streaming/timing never arise. (Agent-loop tests
   live under `internal/agent`, separate from UI tests; `Taskfile.yaml` runs them
   with `-timeout=1h`.)
4. Scope. Narrow per-widget regression tests (user/assistant/question/edit-error/
   banner/dialog/overlay/textarea…), each a handful of cases; full suite runs
   `go test -race -failfast ./...` in CI
   (crush@8b1824282ecc:Taskfile.yaml:85, .github/workflows/build.yml:30).
   CI wall time not found.
5. Exploration. Not found (`grep -rln "proptest\|fuzz"` hits only an unrelated
   version-bump test and a vendored js file; no agent-driven QA).
6. Flakiness. Controlled by construction: everything is synchronous render
   (`t.Parallel()` everywhere, no `time.Sleep` in `internal/ui` tests —
   `grep -rn "time.Sleep\|t.Skip" internal/ui --include="*_test.go"` is empty).
   Terminal size is pinned per-case (`Width(40)`/`Width(120)`, `SetWidth(40)`)
   rather than read from the environment; no TERM/COLUMNS/LINES references in UI
   tests. (The one "flaky" string in UI tests is fixture text: "Fix the flaky test".)

Copy / avoid. Copy: golden files with escaped control codes + `-update` flag, and
ANSI-stripped structural asserts alongside goldens (goldens catch everything,
structural asserts say what matters). Avoid: goldens alone for chat surfaces —
crush pairs them with line-structure asserts so a styling change doesn't read as
a content change.

## bubbletea teatest + VHS + vttest (Go reference)

teatest is dead; vttest is its successor. VHS is a demo renderer, not an assertion
tool. Verified at the module proxy: `x/teatest@latest` → "no matching versions",
while `x/vttest` = `v0.0.0-20261008172826-faa4adf95555` (2026-10-08),
`x/xpty` = `v0.1.4` (2026-07-30), bubbletea `v1.3.10`, vhs `v0.12.1`.

1. Harness. teatest (v1 era): in-process — `teatest.NewTestModel(t, m,
   teatest.WithInitialTermSize(70, 30))`, `tm.Type`/`tm.Send`, `teatest.WaitFor`,
   `teatest.RequireEqualOutput`. The only in-repo example,
   `examples/simple/main_test.go`, is entirely commented out. vttest (current):
   real PTY + full terminal emulator — `vttest.NewTerminal(t, 80, 24)`
   (x@faa4adf95555:vttest/vttest.go:53), `tt.Start(cmd)` (:154) running the real
   binary (in the reference example, the test binary itself via
   `GO_TEST_HELPER_PROCESS`), `tt.SendText` (:202), `tt.Wait` (:162),
   `tt.Snapshot()` (:245). VHS: `.tape` scripts (`Type`, `Sleep`, `Set Width/…`)
   rendered to GIF/MP4 (vhs@24fa2254a980:examples/demo.tape:1-40); no assertion
   step exists in the tool.
2. What is asserted. teatest: raw output bytes vs golden. vttest: `Snapshot()`
   structs (cells, cursor, modes, colours, JSON/YAML-marshalable) compared with
   `snapshot.TestdataEqualf`, as in `examples/btvttest/bt_test.go:takeSnapshot`.
   VHS asserts nothing — its own tests are parser unit tests
   (`TestCommand` pins 31 command types, vhs@24fa2254a980:command_test.go:12-13).
   No repo found using `.tape` files as pass/fail tests
   (`grep -rln "\.tape"` across aider/goose/crush/zellij/television/vhs-Makefile: empty).
   Snapshot churn: bubbletea `testdata`+`examples/simple` 5 commits/6 mo.
3. The model. n/a (framework-level; no provider concept). Timing in the btvttest
   example is `time.Sleep(1s)` between snapshots — i.e. the reference example
   itself is timing-fragile, not a model of repeatability.
4. Scope. One reference example each (btvttest; VHS `browser_e2e_test.go` is
   opt-in behind `VHS_TEST_BROWSER=1`, vhs@24fa2254a980:browser_e2e_test.go:17-19).
   Real-world vttest adopter found via code search: `lkshrk/omni`
   (`omni@0cb8248b100d:integration_tests/tui_integration_test.go`, build tag
   `integration`, line 1) builds the real binary (`buildOmniBinary`, :577),
   starts it in `vttest.NewTerminal` (:82), polls `waitForRequiredScreen`
   (:857; budgets 3 s to 6 s, e.g. :41,47) for text predicates, sends keys via
   `writeTUIKeys` (:835), and asserts screen *text* contains/excludes strings
   (:42,53,57). That is
   the closest Go-world analogue to Fiber's current phrase-matching.
5. Exploration. Not found — no agent/fuzz QA in bubbletea, VHS, or x.
6. Flakiness. The teatest example documents the failure modes inline: disabled
   because "the output is colored … but the test runs against a buffer output and
   not a terminal, tty, or pty" (`t.Skip("this test is currently disabled")`,
   bubbletea@f9df43c4f2c0:examples/simple/main_test.go:11) and a second test
   skipped as "flaky … We need a more concrete way to set the initial terminal
   size" (:47-48); both use `time.Sleep(time.Second + …)` waits (:24). vttest is
   the explicit fix for both: real PTY (colour negotiates for real) and explicit
   terminal size. Colour lesson: size/colour must be pinned or negotiated, never
   ambient.

Copy / avoid. Copy: vttest's layering (xpty PTY + vt emulator + marshalable
Snapshot + testdata goldens) is the blueprint for Fiber's missing screen-grid
rebuild; omni's `waitForRequiredScreen` predicate-polling is the direct upgrade
of Fiber's byte-phrase matching. Avoid: VHS tapes for assertions (no assert
step; sleep-based scripts); teatest itself (unpublished, examples disabled).

## ratatui apps: television + gitui (why these two)

Picked for opposite, mature ends of the spectrum: television drives the **real
binary in a real PTY** (138 tests) plus headless App tests; gitui is the textbook
**TestBackend + insta** approach. atuin was examined and rejected: its
`atuin-pty-proxy` is a production shell-integration feature, not a test harness,
and `grep -rln "insta\|snapbox\|TestBackend"` over its Rust sources finds no TUI
test usage.

### television (alexpasmantier/television)

1. Harness. Two tiers. (a) `tests/pty/`: real `tv` binary in a PTY via the
   `phantom-test` crate (`phantom-test = { version = "0.3", … features = ["alacritty"] }`,
   television@d4a7ab397209:Cargo.toml:79-81): `Phantom::new()` per test, `pt.run(TV_BIN_PATH).size(120, 30)`
   (tests/pty/common.rs:68-78), `s.send().key(…)`, `s.wait().text(…).until()`,
   `s.wait().exit_code(0)`. Typical test: wait for `● files`, send ctrl-c,
   expect exit 0 (tests/pty/channels.rs:7-16). (b) `tests/headless/`: in-process
   `App` driven by sending `Action`s over its channel, 10 `#[tokio::test(flavor =
   "multi_thread", worker_threads = 3)]` tests (`grep -c "async fn test"` → 10).
2. What is asserted. Live-screen **text presence** read from `phantom-test`'s
   emulator grid (`s.screenshot().text()`, `tests/pty/common.rs:124-125,140-143`;
   `wait().text()` polls it every 50 ms), exit
   codes, and post-exit stdout (`exit_and_output` — with a comment explaining
   that polling can miss a ms-lived alt-to-primary transition under contention,
   common.rs `exit_and_output`). No snapshot files and no images; the grid rebuild is inside `phantom-test`.
   Churn (6 mo): `tests/pty` 5 commits, `tests/headless` 3 — active but stable.
3. The model. n/a (fuzzy finder; no provider). Repeatability via hermetic local
   config/cable dirs passed as CLI flags (`--cable-dir ./cable/unix --config-file
   ./.config/config.toml`) and per-test temp dirs.
4. Scope. 138 PTY tests (`cat tests/pty/*.rs | grep -c "#\[test\]"` → 137 in
   tests/pty + 0 in headless = 138 total per combined count) across 13 files
   (channels/config/layout/modes/preview/remote-control/search/selection/…) plus
   10 headless tests — broad cross-feature journeys, not just widgets. CI runs
   everything serially-ish: `TV_CI=1 cargo test --locked --all-features --workspace
   -- --nocapture --test-threads=4` (television@d4a7ab397209:.github/workflows/ci.yml:31).
   CI wall time not found.
5. Exploration. Not found (no agent/fuzz QA in-repo).
6. Flakiness. Explicit, documented controls: `TV_CI` env raises all budgets
   (wait 5 s→15 s in `wait_timeout_ms`, common.rs:42; input delay 100 ms→300 ms),
   `--test-threads=4` caps PTY parallelism, `TV_TEST_WAIT_MS`/`TV_TEST_STABLE_MS`
   env overrides, and helpers distinguish "wait for text" from "assert absence
   after stabilization". The one candid comment admits the remaining race
   (fast-exit output missed by the 50 ms poller).

Copy / avoid. Copy: phantom-test's API shape (`run→send→wait.text→wait.exit_code`)
and television's two-tier split (broad PTY journeys + fast headless logic tests);
CI-aware timeout scaling via env. Note: television already asserts on a rebuilt grid (through
`phantom-test`), as text only; its remaining race is polling a screen that
changes faster than the poll.

### gitui (extrawurst/gitui)

1. Harness. Fully in-process: a `#[cfg(test)]`-only API on the `Gitui` struct —
   `input_event(KeyCode, KeyModifiers)` injects crossterm key events,
   `update_async`/`update` pump the loops, `draw(&mut Terminal<TestBackend>)`
   renders (gitui@d7214eccb3ca:src/gitui.rs:163-202). Component tests construct
   widgets against `TestBackend::new(100, 100)` directly
   (src/components/status_tree.rs:621,661). Real repos via `git2-testing` temp
   dirs; async git notifications awaited with `recv_timeout(100ms)` loops.
2. What is asserted. insta snapshots of the TestBackend buffer
   (`assert_snapshot!("app_loading", terminal.backend())`, src/gitui.rs:261,269,285;
   `insta = { version = "1.41.0", features = ["filters"] }` in dev-deps) with
   committed `.snap` files (only 3: `ls src/snapshots/ | wc -l` → 3) plus
   `assert_eq!` on widget state (scroll position etc.). Fixed 90×12 / 100×100
   backends pin size. Churn (6 mo): 1 commit touching `src/snapshots` — very stable.
3. The model. n/a (git UI). Async git work is real (temp repos) but awaited
   deterministically via notification matching, not sleeps.
4. Scope. 79 `#[test]`s in `src/` (`grep -rn "#\[test\]" --include="*.rs" src/ | wc -l`
   → 79), mostly narrow widget tests + one whole-app journey (`gitui_starts`:
   draw → snapshot → keypress → snapshot). CI wall time not found.
5. Exploration. Not found.
6. Flakiness. Path/commit-hash filters bound in a macro (`apply_common_filters!`:
   temp-dir paths → `[TEMP_FILE]`, hashes → `[AAAAA]`, src/gitui.rs:214-231) so
   snapshots are hermetic across OSes; no sleeps in test code (100 ms
   `recv_timeout` pumps instead).

Copy / avoid. Copy: insta snapshot filters for temp paths/hashes (Fiber's byte
streams will contain home-dir and timing artefacts — same trick applies), and the
`#[cfg(test)]` input-injection seam. Avoid: TestBackend-only coverage — gitui
never tests the real binary in a terminal, which is exactly Fiber's gap.

## Rust crates Fiber could adopt (crates.io, 2026-10-10)

Versions/licences from `crates.io/api/v1/crates/<name>[/<version>]`; "async"
= non-optional normal dependencies on a runtime/IO-reactor (Fiber forbids async
runtimes, ADR 0004). `wezterm-term` does not exist on crates.io
(`{"detail":"crate wezterm-term does not exist"}`) — the wezterm author's
published terminal library is `termwiz`.

| crate (newest, updated, licence, downloads) | needs async runtime? | role for Fiber |
|---|---|---|
| vt100 0.16.2, 2025-07-12, MIT, 13.6M | none (deps: itoa, unicode-width, vte) | screen-grid rebuild from Fiber's byte stream; sync; lowest-risk pick |
| termwiz 0.23.3, 2025-03-20, MIT, 26.0M | none required (optional: cassowary, fnv, image, serde — no tokio) | full surface/CellAttributes grid; heavier API than vt100 |
| portable-pty 0.9.0, 2025-02-11, MIT, 19.8M | none required (optional: serde only) | PTY master if Fiber ever outgrows rustix::pty; sync API |
| expectrl 0.9.0, 2026-05-11, MIT, 0.9M | none (conpty, nix, ptyprocess, regex) | expect-style `wait for text` over PTY; sync |
| rexpect 0.7.1, 2026-05-14, MIT OR Apache-2.0, 2.1M | none (comma, nix, regex, tempfile, thiserror) | same niche as expectrl, smaller; maintained (May 2026) |
| insta 1.49.0, 2026-10-03, Apache-2.0, 109M | none | snapshot review (`cargo insta review`, filters); the default pick |
| alacritty_terminal 0.26.0, 2026-04-06, Apache-2.0, 1.9M | caution: requires `polling`+`piper` (async-io reactor libs, not a full runtime, but I/O machinery beyond std) | most faithful grid (it *is* a real terminal); heaviest integration |
| avt 0.18.0, 2026-05-05, Apache-2.0, 0.3M | none (rgb, unicode-width) | tiny sync VT parser/grid; less battle-tested |
| tui-term 0.3.4, 2026-04-07, MIT, 1.4M | none (ratatui-core, ratatui-widgets) | feeds a vt100 parser into a ratatui widget — for *embedding* a terminal, not asserting on one; wrong direction for Fiber |
| phantom-test 0.3.0, 2026-08-31, MIT, 1.7k | `mio` + out-of-process `phantom-daemon` (no tokio, but a PTY-daemon architecture) | television's driver; newest/least-proven (1.7k downloads); watch, don't adopt yet |

## Exploration: LLM agents / fuzzers run against their own TUIs

Searched via `gh search code` (authed) for `proptest ratatui` (→ `[]`, nothing),
`"tui fuzz"` (→ only noise: docs and unrelated repos), `tmux send-keys test agent
cli` (→ `[]`), `vttest.NewTerminal` adopters (→ charmbracelet/x example,
`lkshrk/omni` integration tests, pomerium ssh test, one crush-modules testutil),
and `phantom-test` (→ television only). In-repo `grep -rln "proptest\|fuzz"` over
goose/zellij/television/crush/aider found no TUI fuzzing (only vendored JS,
matcher unit tests, and an unrelated version-bump test).

1. Harness. The nearest real thing is not self-QA but benchmarks that put a model
   in a terminal: terminal-bench (laude-institute/terminal-bench, 2.6k stars,
   pushed Jul 2026, "a benchmark for LLMs on complicated tasks in the terminal")
   runs agents in docker containers with tmux and checks task outcomes with
   pytest — task-level, not pixel-level. Aider's `benchmark/` (real models,
   threaded, docker-recommended) and goose's `evals/harbor/` (runner + recipes)
   are the same shape: model-in-the-loop, task pass/fail.
2. What is asserted. Task outcomes (files changed, tests passing, SWE-bench
   resolution) — never screen content.
3. The model. The model *is* the test subject, so nothing is mocked; cost =
   real API calls × tasks × threads, plus container time. No per-run cost figures
   found in any repo examined; terminal-bench's cost is bounded by task count and
   documented externally, not in-repo.
4. Scope. Long cross-feature journeys by construction (whole tasks), minutes per
   task; no per-merge CI use of them was found in aider or goose.
5. What it finds. Per aider HISTORY/benchmark docs: prompt/model regressions and
   unvetted-code hazards — behaviour bugs, not rendering bugs. Nothing found
   suggests agent-QA catches TUI rendering issues; that remains an open gap.
6. Flakiness. These harnesses embrace nondeterminism (pass@k, retries) rather
   than controlling it — the opposite of TUI-test practice above.

Copy / avoid. Copy: nothing for Fiber's TUI tests; keep model-eval (harbor-style)
and TUI-e2e as separate lanes. Avoid: "let the model click around the TUI and
see" — no examined project does this, costs are unbounded, and failures are
unauditable. Fuzzing note: property/fuzz testing of TUI *state machines*
(proptest on input-event sequences against a grid model) is absent everywhere
checked — a genuine gap Fiber could pioneer, cheaply, on top of a vt100 grid.


## Comparison

| Project | Harness | Asserts | Model | Scope | Exploration by model | Flake control |
|---|---|---|---|---|---|---|
| opencode v2 | in-process `@opentui/core/testing` renderer, full-app fixture | char-frame `toContain`; 1 bun `.snap` (9 commits/6 mo) | scripted fetch/SSE; `@opencode/simulation` fake provider | 169 narrow test files; 6 full-app | `OPENCODE_DRIVE` headless renderer plus websocket control: infrastructure only, no runs found | `waitForFrame` polling, fixed size, `animations:false` |
| opencode dev | same, hand-rolled `mock.module` injection | char-frame; same 1 `.snap` (5/6 mo) | scripted fetch/event source | 46 files | none | as v2 |
| pi | in-process `VirtualTerminal` on `@xterm/headless` | emulator viewport strings, raw-write capture, 12 inline snapshots, 0 `.snap` | scripted `streamFn` | 43 + 210 narrow files; no agent-plus-TUI journey | none (`packages/evals` is model-in-loop, not TUI) | `waitForRender` (fixed 20 ms), fixed sizes |
| codex | `TestBackend`; `VT100Backend` (`vt100::Parser`); real-binary PTY and tmux | substring in `vt100` grid; insta, 1387 `.snap` (PTY goldens: 5, 9 changes) | wiremock scripted SSE, whole-body replay | thousands of widget tests; 15 PTY tests plus 4 ignored tmux tests; 7+ in-process `VT100Backend` tests | none found | pinned 120x32, `TERM`, answers the binary's capability probes, poll with deadline, `#[ignore]` on tmux tests |
| gemini-cli | real bundle in `@lydell/node-pty` 80x80; ink plus `@xterm/headless` for units | `stripAnsi` substring plus telemetry; 112 vitest `.snap` (units only) | recorded `*.responses` replay with a record mode | 48 short journeys | `evals/` live-model agent evals, tiered | `retry: 2`, deflake workflow, scaled timeouts, per-char echo wait |
| Claude Code | none public | n/a | n/a | n/a | `claude plugin eval` (plugin behaviour, cost-capped) | nothing public |
| aider | in-process, `DummyInput` | return values, files | mocked completion | 32 unit files | benchmark harness (real models) | none needed |
| goose | stub ACP agents; Playwright for desktop; no PTY | protocol payloads, replay parity, DOM | canned SSE; MCP record/playback | about 30 Rust files plus 5 serial Playwright journeys | `evals/harbor` (model-eval) | `workers:1`, trace and video on failure |
| crush | in-process render | escaped-ANSI goldens (363) plus stripped structural asserts | none in UI tests | narrow per-widget | none | synchronous render, no sleeps, per-case width |
| vttest (Go) | real PTY plus emulator plus `Snapshot` | marshalable cell snapshots; text predicates in real adopter | n/a | one example; adopter `lkshrk/omni` | none | explicit size; real colour negotiation |
| television | real binary via `phantom-test` 0.3; headless `App` tier | polled text of `phantom-test`'s emulator grid, exit code | none (no provider) | 138 PTY tests plus 10 headless | none | `TV_CI` scaled budgets, `--test-threads=4`, stabilize-before-absence |
| gitui | `TestBackend` plus key-injection seam | insta, 3 `.snap`; widget state | none | 79 tests, 1 whole-app journey | none | insta filters for paths and hashes |

## Can terminal QA be scripted so no model has to run it?

Yes, and it is the norm. A scripted provider, a real binary in a PTY, a terminal emulator that rebuilds the grid, and deadline-bounded waiting on that grid cover raw mode, resize, layout and cross-feature journeys with no model. What no project does is replace judgement about how a screen looks: a snapshot says "this changed", a person says whether the change is right (`cargo insta review`). Model-driven exploration of a TUI has no precedent in the sources read: the nearest, gemini-cli `evals/` and terminal-bench, grade task outcomes, cost real API calls per run, and test agent behaviour, not rendering. gemini-cli runs its `ALWAYS_PASSES` evals in every CI and gates the `USUALLY_PASSES` and `USUALLY_FAILS` trendline sets behind `RUN_EVALS=1`. Property tests of input sequences against a grid model were searched for and not found anywhere.

## Recommendation for Fiber

These are evidence-backed proposals, not rulings; surveyor turns them into rulings and tickets.

1. **Harness: keep the real binary in `rustix::pty`; rebuild the screen from its bytes with `vt100`.** Feed the bytes the harness already reads into a `vt100::Parser` (as codex does, `codex@4bad6d78e9b5:codex-rs/tui/src/test_backend.rs:22-42`; codex pins `vt100 = "0.16.2"`). Then replace `phrase_end` and its gap helpers with predicates over `Screen::contents()` and cursor position. Do not adopt `phantom-test`, `expectrl` or `rexpect`: Fiber's PTY, process-group kill and watchdog code already exists in `terminal.rs`, and `phantom-test` runs a PTY daemon and has 1.7k downloads. Do not adopt `alacritty_terminal` (pulls `polling` and `piper`), `termwiz` or `avt` (more API or less proven than `vt100` for the same job).
2. **What to assert.** Wait on a grid predicate with one named deadline (`docs/testing.md`, "Waits and timeouts"), never a sleep; the television comment on missing a millisecond-lived alt-to-primary switch is a reason to also assert exit status and final output. Snapshot the settled grid, as plain text rows plus cursor position, at a few named moments per journey with `insta` and its filters for temp paths, ids and timestamps (gitui's `apply_common_filters!`, `gitui@d7214eccb3ca:src/gitui.rs:214-231`). Pin the size (codex: 120x32) and `TERM`/colour in the harness, and answer the binary's own capability probes as codex does. Colour and style belong in the existing in-process snapshots; keep PTY snapshots to text and cursor so a theme change does not break every journey.
3. **Scope: few long journeys, many in-process snapshots.** codex keeps 1382 widget snapshots against 5 PTY goldens, and the PTY ones almost never change (5 commits in six months against 52 for one widget directory), while gemini-cli's journey suite has 19 commits in six months touching `integration-tests/`, several of them deflake fixes. A journey costs flake risk each time it is added. The evidence supports three to six journeys that cross features (prompt, tool approval, resize, resume, image paste) after the Look group (#1604), snapshotted at key moments, run on every merge in the existing Linux and macOS legs.
4. **The model: the existing scripted `fakes::ProviderServer`.** Scripted replies are what codex (wiremock SSE), gemini-cli (`*.responses` replay) and opencode (fake provider) all use. gemini-cli's `--record-responses` mode and goose's record/playback for MCP are worth copying only if recorded real-provider replies are wanted; Fiber's probe keys already allow that for a ticket that asks. Release streamed chunks on a signal the test waits for, not on a timer.
5. **Deleting narrow tests: the evidence supports a narrow yes and no more.** No surveyed project replaced narrow PTY tests with journeys; television has 138 narrow PTY tests and codex 15 (plus 4 manual tmux tests). What the evidence does support: the eight `phrase_end_*` tests and the roughly 110 lines of gap helpers in `terminal.rs` (409-525) go when the grid replaces them, and a narrow PTY test may go when a journey snapshot asserts everything it asserts, shown by `cargo-mutants` on CI. Do not delete in-process screen tests: they are the layer that carries layout regressions in every project that has one.
6. **Exploration.** Do not build model-driven TUI QA now: no precedent, unbounded cost, unauditable failures. If wanted later, `claude plugin eval`'s per-case runs, threshold and `--max-cost-usd` ceiling are the template, kept off the merge path. A `proptest` run over key sequences against a `vt100` grid is a cheap gap nobody has filled; `proptest` is already listed in `docs/dependencies.md`.
7. **Dependencies against `docs/dependencies.md`.** One new dev-dependency: `vt100` 0.16.2 (MIT, `crates.io` 2025-07-12, 13.6M downloads, MSRV 1.70). Its normal dependencies are `itoa`, `unicode-width` (both already in `Cargo.lock`) and `vte`; it has no async runtime. The "Tests and development tools" table needs one row. `insta` and `proptest` are already listed. A dev-dependency has no memory row: `docs/dependencies.md` ("Measuring memory") says dev-dependencies "never reach the shipped binary". The licence, advisory and source checks still apply, so `cargo deny check` runs on the new tree in the ticket's PR.
8. **First build step: a spike.** Not probed here (no cargo builds in this ticket): how `vt100` renders what Fiber emits (synchronized output, kitty keyboard queries, OSC 9 notifications, alternate screen). Two journeys on the new harness will answer it before any narrow test is deleted.

## Limits of this survey

- CI wall times were not found for any project, so the cost of a journey in CI is unmeasured. Measure it on Fiber's runners with the first journeys.
- Churn is a six-month commit count on a path, not a measure of how often reviewers accept changed snapshots.
- gemini-cli was cloned at depth 1; its churn comes from the GitHub commits API.
- Claude Code's internal tests are not public. `strings` on its binary found `node-pty` only in a memory-diagnostic message and no test harness.
