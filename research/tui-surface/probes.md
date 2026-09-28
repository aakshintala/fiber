# TUI toolkit probes: Go/Bubble Tea v2, Python/Textual, TS-Bun/OpenTUI, Rust/ratatui (heavy)

All numbers below are from this machine: **macOS arm64 (Darwin 25.6.0, Apple Silicon)**. Do not
generalise timings to Linux (see prior survey's own caveat); sizes and counts generalise, timings
do not. Companion capability survey: `/tmp/fiber-tui-toolkits.md` (read first, not repeated here).

Probe app in every toolkit: full-screen/alt-screen, mouse capture on, a scrollable list of 2,000
deterministically generated text items (1–8 wrapped lines each, formula: item `i` has
`1 + i % 8` lines, each line built from a fixed 30-word list indexed by `(i*31 + line*7 + word)
% 30`), a fixed 32-column side panel, and a one-line input at the bottom. Toolkit's own scroll
container used in every case (ratatui: manual offset over pre-wrapped lines, as instructed).

## Summary table

| Toolkit | Version | Artifact for distribution | Peak memory footprint (idle 10s) | Idle IDLEW (3 samples, 3s apart) | Idle CPU | Threads | Rough time-to-first-pty-byte |
|---|---|---|---|---|---|---|---|
| Rust / ratatui (heavy) | ratatui 0.30.2, crossterm 0.29.0 | 505,056 B (493 KiB) release, `opt-level=z,lto,strip` | 1,999,160 B ≈ **1.9 MiB** | 0 / 0 / 0 | 0.0% | 1 | 19 ms |
| Go / Bubble Tea v2 | bubbletea v2.0.10, bubbles/v2 v2.2.1, lipgloss/v2 v2.0.6 | 4,094,434 B (3.9 MiB), `-ldflags="-s -w"` | 11,665,936 B ≈ **11.1 MiB** | 50 / 54 / 62 (climbing) | 0.0–0.5% | 9–10 | 15 ms |
| TS/Bun / OpenTUI | @opentui/core 0.5.12, Bun 1.4.0 | 74,428,658 B (71.0 MiB), `bun build --compile` standalone | 92,226,448 B ≈ **87.9 MiB** | 7 / 9 / 9 (slow climb) | 0.0–5.4% | 10 | 18 ms |
| Python / Textual | textual 8.2.8, venv | 7,564,624 B (7.2 MiB) PyInstaller `--onefile` (unofficial); venv itself 20 MiB | 93,225,320 B ≈ **88.9 MiB** | 1 / 2 / 4 (slow climb) | 0.0–0.1% | 3 | 18 ms |
| *(reference)* Rust hello, prior pass | ratatui 0.30 / crossterm 0.29 | 432 KiB | 1.0 MiB | 0/0 | 0.0% | — | — |

The "rough time-to-first-pty-byte" column is **not a clean signal** — see "Time to first frame"
below; all four land in a suspiciously narrow 15–19 ms band because the method mostly measures
`script`'s own pty-attach latency, not each runtime's real startup cost. Treat the size, peak
footprint and idle-wakeup columns as the trustworthy ones.

## How each was measured

- Real pty: `script -q /dev/null <cmd>` (as the prior pass did), so raw mode/mouse-capture setup
  succeeds.
- Peak memory footprint: `/usr/bin/time -l <cmd>` under the same pty, left idle 10 s, then sent
  `SIGTERM`, reading `peak memory footprint` (not "maximum resident set size", a different, larger
  number `time -l` also prints) from its stderr, which `script` merges into the captured pty
  stream.
- Idle wakeups/CPU/threads: `top -l 3 -s 3 -pid <pid> -stats pid,cpu,idlew,th`, three samples 3 s
  apart, while the app sat idle (no keys/mouse after launch).
- Artifact size: `ls -la` on the built binary (Rust/Go/OpenTUI-compiled/PyInstaller), `du -sh` on
  the Python venv.

## Rust / ratatui — heavier workload

- Probe: `/tmp/tui-probe2/rust/heavy/src/main.rs`, `/tmp/tui-probe2/rust/heavy/Cargo.toml`
  (`ratatui = "0.30"`, `crossterm = "0.29"`, release profile `opt-level="z", lto=true, strip=true`,
  matching the prior "hello" probe's profile for a like-for-like comparison).
- Idle loop: `event::poll(Duration::from_secs(3600))` then `event::read()` — a genuinely blocking
  read between frames, same as the prior pass's finding; confirmed again empirically (IDLEW = 0
  across all three samples, 0.0% CPU, 1 thread, 5 involuntary context switches over the whole
  10 s run per `time -l`).
- No workaround needed. No blinking cursor by default (ratatui/crossterm don't draw one unless
  you explicitly position/show it, which this probe never does).
- Peak footprint nearly doubled versus the "hello" probe (1.9 MiB vs 1.0 MiB) — expected, since
  this probe builds and holds ~10,000 pre-wrapped `String` lines plus a `Vec<Line>` per frame,
  versus the hello app's near-empty state.

## Go / Bubble Tea v2

- Probe: `/tmp/tui-probe2/go-app/main.go`, `go.mod` (module `tuiprobe`, Go 1.27.1 toolchain
  downloaded to `/tmp/tui-probe2/go-toolchain` from the official darwin-arm64 tarball
  `go1.27.1.darwin-arm64.tar.gz`, per go.dev/VERSION at probe time). Deps:
  `charm.land/bubbletea/v2 v2.0.10`, `charm.land/bubbles/v2 v2.2.1`,
  `charm.land/lipgloss/v2 v2.0.6`. `GOPATH`/`GOCACHE`/`GOMODCACHE` all under `/tmp/tui-probe2`.
  Build: `go build -ldflags="-s -w" -o probe .`.
- Used `bubbles/v2` `viewport.Model` for the scroll container, `textinput.Model` for the input
  line, `lipgloss/v2.JoinHorizontal` for the side-panel split. Mouse/alt-screen are set **per
  frame on the returned `tea.View`** in v2 (`v.AltScreen = true; v.MouseMode =
  tea.MouseModeCellMotion`), not via a `tea.NewProgram` option — this is a real v2 API change
  from v1's `tea.WithAltScreen()`/`tea.WithMouseCellMotion()` program options, confirmed by
  reading `tea.go` (no `WithAltScreen`/mouse-mode program options exist in v2.0.10; `View.AltScreen
  bool` and `View.MouseMode MouseMode` live on the `View` struct instead, `tea.go:149-177`).
- **Blinking cursor pitfall (found and fixed in this probe, worth flagging for Fiber):**
  `bubbles/v2/textinput`'s `Model.SetVirtualCursor(false)` only changes *rendering*
  (`updateVirtualCursorStyle`, `textinput.go:937-955`), but `Model.Focus()`
  (`textinput.go:268-271`) unconditionally calls the underlying `cursor.Model.Focus()`
  (`cursor/cursor.go:214-222`), which starts a self-perpetuating `Blink()` command chain
  (`cursor/cursor.go:183-198`) if the cursor's *mode* is still `CursorBlink` at the time `Focus()`
  runs. Calling `SetVirtualCursor(false)` *before* `Focus()` sets the mode to `CursorHide` first
  (`cursor.go:169-179` — `SetMode` only returns the blink-restart `Cmd` when `mode ==
  CursorBlink`), so `Focus()` never starts the ticker. Call order matters and is easy to get
  backwards (I did, then fixed it — see the comment in `main.go` at the `newModel()` call site).
- **Idle wakeups are real, not measurement noise:** even after fixing the cursor-blink ticker,
  IDLEW climbed every sample (50 → 54 → 62) with 9–10 threads and 32,904 involuntary context
  switches over the run (vs. Rust's 5). Root cause, confirmed by reading source:
  `Program.startRenderer()` (`tea.go:1417-1421`) starts an unconditional
  `time.NewTicker(time.Second / p.fps)`, and `defaultFPS = 60` (`renderer.go:13`) — Bubble Tea v2
  redraws on a **fixed 60 fps ticker for the life of the program**, whether or not anything
  changed, only skippable via the undocumented-in-README `tea.WithFPS(n)` program option
  (`options.go:149-158`). This is a materially different idle model from ratatui's blocking read.
- Distribution: 3.9 MiB static binary, no runtime — Go's baseline, unaffected by these libraries.
- Peak footprint 11.1 MiB — an order of magnude above ratatui's, consistent with Go's runtime
  (GC, goroutine scheduler, 9-thread pool) rather than anything Bubble Tea-specific.

## TS/Bun / OpenTUI

- Probe: `/tmp/tui-probe2/opentui/index.ts`, `/tmp/tui-probe2/opentui/package.json`
  (`@opentui/core@0.5.12` via `bun add`). Used `ScrollBoxRenderable` (content added via its
  inherited `.add()`, which is documented as routing to `.content`, a `BoxRenderable`),
  `TextRenderable` per item, `BoxRenderable` for the side panel, `InputRenderable` for the input
  line, all composed with the flexbox layout (`BoxRenderable` + `flexDirection`/`flexGrow`/fixed
  `width: 32`), per the README's `createCliRenderer` + `renderer.root.add()` pattern
  (`node_modules/@opentui/core/README.md`).
- `bun build --compile index.ts --outfile probe-compiled` **works**: produces a 71.0 MiB
  Mach-O arm64 executable that I copied to a separate directory (`/tmp/tui-probe2/elsewhere`) and
  ran successfully with no `node_modules` present — confirms the native Zig renderer (loaded via
  FFI) is embedded in the compiled output, not loaded from a sibling file at runtime. Uncompiled
  `node_modules` alone is 63 MiB, so the compile step doesn't save space, it just makes the app
  self-contained (bundles the whole Bun runtime, same tradeoff the prior pass flagged for
  Ink/Bun without a local number — now measured).
- No blinking cursor by default; didn't need to turn anything off.
- Idle behaviour, read from source (`node_modules/@opentui/core/index.node.js`, the shipped
  bundle — not the GitHub source tree, flagged as such): rendering is **request-driven, not a
  continuous ticker**. `ScrollBoxRenderable`'s `startAutoScroll`/`stopAutoScroll`
  (`index.node.js:~14607-14634`) and `Timeline`'s `updateLiveState()` (`index.node.js:~1878-1890`)
  both call `renderer.requestLive()`/`renderer.dropLive()` around the specific periods an
  animation or a mouse-drag-triggered auto-scroll is active; `hasOtherLiveReasons()` returns
  `false` otherwise. No `setInterval`/`setTimeout` frame ticker was found anywhere in the bundle
  (only two unrelated `setInterval` calls, both scoped to a transient "arrow mouse" input-mode
  timeout, `index.node.js:13949,13964`). `CliRendererConfig.targetFps`/`maxFps`
  (`renderer.d.ts:37,434-435`) exist as config, but the actual frame-scheduling loop lives inside
  the compiled Zig binary reached over FFI — **not visible from the TypeScript bundle**, so I
  can't cite a file/line for it the way I can for ratatui/Bubble Tea. Empirically: IDLEW crept up
  slowly (7 → 9 → 9) with 0–5.4% CPU while idle — some low-rate wakeup exists that I could not
  trace to a JS-level source; flagged as unconfirmed rather than guessed.
- Peak footprint 87.9 MiB, dominated by the bundled Bun runtime + native FFI layer, not the app.

## Python / Textual

- Probe: `/tmp/tui-probe2/py/app.py`, venv at `/tmp/tui-probe2/py/venv`
  (`pip install textual` → 8.2.8, `python3 -m venv`). Used `VerticalScroll` containers (one for
  the 2,000-item list of `Static` widgets, one for the side panel) and `Input` for the bottom
  line, laid out with Textual's CSS (`layout: horizontal`, fixed `width: 32` for the side panel,
  `dock: bottom` for the input).
- Turned off the input's blinking cursor explicitly: `Input.cursor_blink = False`, set in
  `on_mount()` (Textual's `Input` widget defaults `cursor_blink` to `True`, confirmed in the prior
  pass from Textual's own docs) — this is a one-line, documented reactive attribute, no
  workaround needed.
- Idle wakeups: IDLEW crept up slowly (1 → 2 → 4) with 3 threads and only 0.0–0.1% CPU — an order
  of magnitude quieter than Bubble Tea's ticker, but not perfectly silent either. I did not
  re-confirm the asyncio-timer source claim from the prior pass against Textual's own source in
  this pass (time budget) — that finding stands as search-sourced, not primary-verified, same
  flag as before, now with a concrete empirical number attached (≤4 wakeups per 3 s window while
  idle with the input's cursor_blink off).
- Distribution: no first-party single-binary story confirmed again. Venv alone is 20 MiB.
  `pip install pyinstaller` was fast (under 3 s) and `pyinstaller --onefile app.py` worked on the
  first try: **7.2 MiB** standalone executable, copied to a separate directory and run
  successfully with the venv absent — this is a real, working (if unofficial/third-party) single-
  binary path for Textual that the prior pass hadn't measured.
- Peak footprint 88.9 MiB — same order of magnitude as OpenTUI/Bun, both paying for an embedded
  interpreter/runtime; Go and Rust (compiled, no runtime) are 8–90x smaller.

## Also checked, from primary sources (per the "Also check" list)

- **OpenTUI search / cross-scroll selection / streaming markdown:** no in-app search feature
  found anywhere in the npm package or the `opentui.com/docs/core-concepts/interaction/` page
  (confirms the prior pass's "not found"). Mouse-drag selection: the interaction docs say
  dragging "extends the selection across selectable descendants in the active container" but
  never explicitly says this reaches content scrolled out of the ScrollBox's viewport; the local
  source's `Selection` class (`lib/selection.d.ts`) tracks `_selectedRenderables`/
  `_touchedRenderables` as renderable-tree objects rather than screen coordinates, which is
  structurally consistent with selection surviving scroll, but this is my inference from reading
  the type surface, not a documented guarantee — flagged as such, an upgrade from the prior
  pass's flat "not found" but still not a confirmed "yes". **Streaming markdown: correction to
  the prior pass.** The prior survey said "not found in the docs pages scanned" for OpenTUI
  markdown streaming; this pass found it directly in the installed package:
  `renderables/markdown-parser.d.ts` exports `parseMarkdownIncremental(newContent, prevState,
  trailingUnstable)`, documented as "Incrementally parse markdown, reusing unchanged tokens from
  previous parse. Compares `token.raw` at each offset - matching tokens keep same object
  reference," backing a full `MarkdownRenderable` component (`renderables/Markdown.d.ts`). This is
  a first-class, shipped incremental-markdown primitive, not a gap.
- **OpenTUI version/cadence:** latest is 0.5.12 (2026-09-22, per `github.com/anomalyco/opentui`
  releases), with roughly 1–7 day gaps between releases from mid-August through September 2026 —
  an actively, rapidly released pre-1.0 project.
- **Bubble Tea v2 stable-or-RC:** correction to the prior pass, which called v2 "release-candidate
  stage." The GitHub releases list for `charmbracelet/bubbletea` shows a `v2.0.x` line
  (`v2.0.1` through `v2.0.10`, latest 2026-09-24) with **no `-alpha`/`-rc` suffix** on any of
  these tags — i.e., `v2.0.0` itself was already a plain, non-prerelease semver tag before this
  pass. No release page or README sentence explicitly says the word "stable," but the versioning
  scheme itself (bare `2.0.x`, ten patch releases deep) is the signal: this is not an RC anymore,
  it's the released major version. No Charm/Bubbles component for text selection or clipboard
  copy was found in the README (confirms prior pass).
- **Textual selection/copy/search:** no page fetched this pass (or the prior pass) confirms
  built-in text selection-and-copy across a scrolled container as a Textual feature; the prior
  pass's `App.copy_to_clipboard()` (OSC 52, app-level, not tied to a mouse-drag selection) still
  stands as the only clipboard primitive found. `Markdown.get_stream()` is confirmed to exist
  ("Get a MarkdownStream instance to stream Markdown in the background," batching updates so
  frequent appends don't outrun rendering) but I could not pull exact method signatures from the
  fetched page — the prior pass's more detailed v4 "Streaming Release" citation
  (simonwillison.net) remains the best source for how it batches. No built-in in-content search
  widget was found; Textual's own docs site has a search box, but that's the documentation
  site's search, not an app-facing Textual feature.

## What broke / needed a workaround

- Go: `charm.land/bubbletea/v2`'s alt-screen/mouse-mode API moved from `tea.NewProgram` options
  (v1) to per-frame fields on the `tea.View` returned by `Model.View()` — not a break, but a
  real API-shape difference from v1 that would trip up anyone porting v1 code or an LLM trained
  on v1 examples.
- Go: the textinput blink-ticker ordering footgun above — genuinely broke the idle-wakeup
  measurement on the first run (IDLEW climbing much faster, 50/54/62 even after my attempted fix
  was in the wrong order) until I read the source and reordered `SetVirtualCursor(false)` before
  `Focus()`.
- Rust/Go/OpenTUI/Textual probes all rendered correctly under `script -q /dev/null <cmd>` on the
  first or second try; no other crashes or missing-dependency issues.
- The "time to first frame" methodology (poll a `script`-captured log file for its first non-empty
  read) needed `-t 0` (macOS `script`'s immediate-flush flag) — without it, `script`'s default
  30 s flush interval meant the log stayed empty for the first 30 s regardless of the app,
  producing a hung measurement, not a wrong number. Even fixed, the four results (15–19 ms) are
  too close together and too fast to reflect real differences (a `python3 -c "import
  textual.app"` alone measured 105 ms; `bun -e "1"` measured 8 ms) — the log's first byte is
  written by `script`/the terminal-negotiation escape sequences before the app's own heavier
  startup work runs, so this metric is reported but flagged as unreliable rather than trusted.

## Probe sources (all under `/tmp/tui-probe2/`)

- `rust/heavy/` — Cargo project (`src/main.rs`, `Cargo.toml`), release binary at
  `rust/heavy/target/release/heavy`.
- `go-app/` — Go module (`main.go`, `go.mod`), binary at `go-app/probe`. Toolchain at
  `go-toolchain/`, module/build caches at `gopath/`, `gocache/`, `gomodcache/`.
- `opentui/` — Bun project (`index.ts`, `package.json`, `node_modules/`), compiled binary at
  `opentui/probe-compiled`.
- `py/` — `app.py`, venv at `py/venv/`, PyInstaller output at `py/dist/probe`.
- `measurements/` — per-toolkit `stdout.log`/`time.txt` captures and the `ttfb.sh` helper used
  for the (flagged-unreliable) first-frame timing.
