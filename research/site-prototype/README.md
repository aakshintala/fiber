# Fiber product page prototype

A static, dark product page for #2926. Open `index.html` in a browser.
There is no build step. The page makes no third-party requests. Fonts come
from the system. The Docs link opens the rendered guide on the site; the other links to the docs and source open GitHub when followed.

The page stays readable with JavaScript off. JavaScript only copies the
install command. If clipboard access fails, it selects the command for
manual copying.

## Files

- `index.html`: the pitch, install command, screenshot, screen guide, budgets and differentiators
- `style.css`: the dark layout, system serif heading and responsive labels
- `site.js`: copy button and clipboard fallback
- `fiber-logo.png`: the TUI's image logo, 640 × 160: `crates/tui/assets/logo-mask.bin` tinted as `research/tui-prototype/src/logo.rs` tints it (the wave in accent, each letter its step of the dark theme's gradient, `docs/tui.md`, "The logo")
- `fiber-tui.png`: the owner's original Ghostty screenshot, 3140 × 2144 pixels
- `README.md`: opening instructions, screenshot provenance and content sources

The screenshot is about 916 KiB, so this prototype serves the original
without a second image. It displays at up to 1280 px wide, wider than the
860 px text column. It is not a link. The 4 labels use
percentage positions over quiet parts of the screenshot. Below 600 px,
they become a short list under the image.

## Screenshot source

The screenshot is a Ghostty capture of `research/tui-prototype` with its
default fixture, `fixtures/session.jsonl`. It shows sessions across fiber,
pi-rig and beacon; a finished turn and summary table; Approval 1 of 2 for
`cargo mutants`; and context, changed files, delegates, jobs and quota.
The displayed paths use tilde form. The screenshot is the prototype,
not a claim that every pictured interaction has shipped in Fiber.

To retake it, run the already built prototype in Ghostty at about 160 × 48
cells. From `research/tui-prototype`, run:

```sh
./target/release/tui-prototype fixtures/session.jsonl --static
```

Use the default dark theme. Wait for the frame, then screenshot the window
and replace `fiber-tui.png`. Match the supplied frame's scroll position and
three-line rail density, so the finished turn, summary table, approval and
all 3 project headings remain visible. Update the HTML image dimensions
and label positions if the framing changes. No converter or rendered cell
capture is part of this page.

## Content sources

Figures and claims were checked against this branch's docs at
`60e174c26086c6836750add05015dab976b29159`. The page links to the
corresponding sections on main for readers.

- Pitch: `docs/invocation.md`, "Two doors" and "The hub"; `docs/delegates.md`,
  "Harnesses"; `docs/extensions.md`, "What an extension is".
- Install command and supported systems: `docs/releasing.md`, “Installing”
  and “What a release publishes”. The command is reproduced from the doc. This task did
  not verify a published release or execute the installer.
- Budgets: `docs/performance.md`, "Budgets". The page quotes four
  ceilings, all gated on Linux x86_64: idle headless session 13,312 KiB
  peak RSS, terminal to its first frame for a new session 50 ms, session
  start 20 ms, and zero context switches in the idle window.
- Hub and clients: `docs/invocation.md`, "The hub" and "Remote clients".
- Extensions: `docs/extensions.md`, "What an extension is" (same seams,
  replacing a built-in by name, Lua at about 120 to 150 KiB, process
  extensions) and "Commands and screens".
- Delegates: `docs/delegates.md`, "What a delegate is" and "Harnesses".
- Reviewer: `docs/permissions.md`, "The reviewer" and "What happens on a
  block".
- Import: `docs/configuration.md`, "Importing".
- Providers: `docs/model-routing.md`, “Protocols and providers”. The doc
  calls these eleven first-party providers, grouping OpenCode's two
  packages in one row. “Keys, tokens and OAuth” names ChatGPT/codex as
  the shipped subscription login.

## Scope

This is the owner's single approved direction, kept on `site/vision`.
It uses the supplied Ghostty screenshot, not recreated HTML terminal cells.
Nothing has been published.

## Verification

Headless Chrome renders at 1440 px and 500 px were inspected: no
horizontal overflow, the labels sit off the TUI text, and the stat grid
and feature list stack on narrow screens. Headless Chrome on macOS will
not size a window below about 500 px, so true phone width needs a
device-emulation check before this ships.
