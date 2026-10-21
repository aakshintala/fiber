# Fiber product page prototype

A static, dark product page for #2926. Open `index.html` in a browser.
There is no build step. The page makes no third-party requests. Fonts come
from the system. Links to the docs and source open GitHub when followed.

The page stays readable with JavaScript off. JavaScript only copies the
install command. If clipboard access fails, it selects the command for
manual copying. The full-size screenshot link needs no JavaScript.

## Files

- `index.html`: the pitch, install command, screenshot, screen guide and sourced figures
- `style.css`: the dark layout, system serif heading and responsive labels
- `site.js`: copy button and clipboard fallback
- `fiber-tui.png`: the owner's original Ghostty screenshot, 3140 × 2144 pixels
- `README.md`: opening instructions, screenshot provenance and content sources

The screenshot is about 916 KiB, so this prototype serves the original
without a second image. It displays at up to 1280 px wide, wider than the
860 px text column. Clicking it opens the original. The 4 labels use
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

- Pitch: `GLOSSARY.md`, “Session” and “Delegate”; `docs/invocation.md`,
  “The hub”; `docs/model-routing.md`, “Choosing the model”. Fiber is a
  terminal coding agent with persistent sessions and model choice.
- Install command and supported systems: `docs/releasing.md`, “Installing”
  and “What a release publishes”. The command is reproduced from the doc. This task did
  not verify a published release or execute the installer.
- RSS: `docs/performance.md`, “Budgets”. Idle headless session CI medians
  are 12,416 to 12,480 KiB peak RSS on Linux x86_64. The ceiling is 13 MiB.
  “What a budget covers” excludes child processes. This is not the
  terminal's memory use or a busy session's memory use.
- Binary size: `docs/ci.md`, “The backstop on main”. CI records each
  commit's stripped binary size in bytes. Neither `docs/performance.md`
  nor `docs/model-routing.md` reports a total binary size. The page says
  so instead of substituting a component measurement or inventing a number.
- Providers: `docs/model-routing.md`, “Protocols and providers”. The doc
  calls these eleven first-party providers, grouping OpenCode's two
  packages in one row. “Keys, tokens and OAuth” names ChatGPT/codex as
  the shipped subscription login.

## Scope

This is the owner's single approved direction, kept on `site/vision`.
It uses the supplied Ghostty screenshot, not recreated HTML terminal cells.
The only missing requested figure is total binary size: the branch docs
provide no figure to quote. Nothing has been published.

## Verification

Local Chrome checks passed at 1440 px and 390 px, and at 1440 px with
JavaScript disabled. The image displays at 1278 px inside its border on
desktop. There was no horizontal overflow, broken image, script error or
third-party request. Automated WCAG A and AA checks reported no violations
on the desktop and phone layouts. Copying and the manual selection fallback
both preserved the complete install command. Desktop and phone screenshots
were inspected. The repository docs check passed.
