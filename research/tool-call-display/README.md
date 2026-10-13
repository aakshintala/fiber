# How coding-agent terminals show tool calls

Reference survey for Fiber's expanded-tool-call design. Method: every claim
below was probed, not recalled. Pi claims cite files under
`/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/`. Claude Code
claims cite `strings -n 8` hits from `~/.local/share/claude/versions/2.1.296`
(saved at `/tmp/toolcall-claude/claude.strings`) or `claude --help`. Codex
claims cite files under `/tmp/toolcall-codex/codex-rs/tui/src/` (sparse
checkout of `github.com/openai/codex`, `codex-rs/tui`, cloned 2026-10-13).

## pi

(a) Collapsed call. Each tool call renders inline in the conversation
transcript as a one-line header plus a short result preview. The header is
per-tool: `read <path>[:start-end]` (`dist/core/tools/renderers/read.js`,
`formatReadCall`), `edit <path>` (`dist/core/tools/renderers/edit.js`,
`formatEditCall`), `$ <command>` with an optional `(timeout Ns)` suffix
(`dist/core/tools/renderers/bash.js`, `formatShellCall`), `grep /pattern/ in
<path>` (`dist/core/tools/renderers/grep.js`, `formatGrepCall`), `find
<pattern> in <path>` (`dist/core/tools/renderers/find.js`, `formatFindCall`),
`ls <path>` (`dist/core/tools/renderers/ls.js`, `formatLsCall`), and `write
<path>` followed by the first lines of the written content
(`dist/core/tools/renderers/write.js`, `formatWriteCall`). Tools with no
renderer, including MCP tools, fall back to `toolName key=value ...` with the
argument preview cut at 100 characters (`dist/core/tools/render-utils.js:54`,
`COLLAPSED_ARGS_CHARS`, and `formatToolCallWithArgs` at `:60-78`). Collapsed
result previews show the first N lines: 10 for read and write (`read.js:95`,
`write.js:101`), 15 for grep (`grep.js:34`), 20 for ls and find (`ls.js:26`,
`find.js:32`), and 10 for tools using the generic fallback
(`dist/modes/interactive/components/tool-execution.js:6`,
`FALLBACK_PREVIEW_LINES`). Bash instead keeps the last 5 visual lines
(`renderers/bash.js:14`, `BASH_PREVIEW_LINES`, `keep: "end"`). Any hidden
remainder is announced as `... (N more lines, <expand hint> to expand)` in
each renderer. Read is the exception: when collapsed and not an error it shows
no result at all (`read.js:87`, `if (!options.expanded && !isError) return
""`), and some reads collapse further to `[skill] <name>`, `read docs
<label>`, or `read resource <path>` (`read.js:120`,
`getCompactReadClassification`).

(b) How a call is opened and where it shows. Expansion is always inline in the
conversation transcript; there is no separate pane, overlay, or pager for tool
output. `ctrl+o` toggles every tool between collapsed and expanded
(`dist/modes/interactive/interactive-mode.js:2458`, `onAction("app.tools.expand"
...)`, implemented at `:3734`, `toggleToolOutputExpansion`), and left-clicking
a tool result toggles that one call (`tool-execution.js:112-116`,
`createResultRegion`, via `setExpanded` at `:139`). Pi's fullscreen versus
regular distinction is about transcript scrolling, not tool viewing: fullscreen
scrolls inside the app while regular mode leaves scrolling to the terminal's
own scrollback (`dist/modes/interactive/tui-renderer.js:8`; `docs/tui.md`,
"Handle mouse input" and "Use custom screens and overlays"). The keybinding
table confirms no tool-specific overlay action exists
(`docs/keybindings.md:164`, `app.tools.expand`, "Collapse or expand tool
output").

(c) Per kind. Read: header plus, when expanded, the file content with
tab expansion and per-language syntax highlighting, otherwise plain
`toolOutput`-coloured lines (`read.js`, `formatReadResult`); the header range
renders in the warning colour (`formatReadLineRange`). Search (grep/find):
header plus matching lines in the output colour, with a `[Truncated: ...]`
footer when match, result, or byte limits hit. Edit/write: the edit call is a
tinted box whose background reflects state (pending, success, error)
(`edit.js`, `getEditHeaderBg`); it shows a live preview diff computed from the
arguments before the call settles, and the result adds only a diff that differs
from the preview (`edit.js`, `formatEditResult`). The diff format parses lines
of the form `±linenum content`: removed lines render red as `-N content`,
added lines green as `+N content`, context lines dim as ` N content`, tabs
expanded to three spaces (`dist/modes/interactive/components/diff.js:70-122`,
`renderDiff`). When exactly one removed and one added line are adjacent, the
changed words inside them are inverse-video highlighted using word diffing
(`diff.js:20-54`, `renderIntraLineDiff`, `diffWords`). Write shows the new file
content in the call itself (highlighted, 10 lines collapsed) and renders
nothing on success, only errors (`write.js`, `formatWriteResult`). Shell
(bash/powershell, differing only in the `$` versus `PS>` prompt, `renderers/index.js`,
`createShellRenderers`): dimmed output lines, a saved-full-output and
truncation warning footer, and a duration line (`bash.js`,
`rebuildBashResultRenderComponent`). Other/MCP: the generic fallback header
plus the first 10 result lines (`tool-execution.js`, `createCallFallback` /
`createResultFallback`).

(d) Truncation rule and how to see the rest. Model-facing content is capped at
2000 lines or 50 KiB, whichever is hit first, without splitting lines
(`dist/core/tools/truncate.js:10-11`, `DEFAULT_MAX_LINES`,
`DEFAULT_MAX_BYTES`). Each renderer adds a `[Truncated: ...]` warning naming
the hit limit, and bash records the full output to a file whose path is shown
in the footer (`bash.js`, `fullOutputPath`). Display previews are a separate,
smaller cap (10/15/20/5 lines per above); pressing `ctrl+o` or clicking reveals
all retained lines inline, since expanded renderers slice to `lines.length`.
Content cut at the model layer is not recoverable in the UI and must be
re-read from the file or path named in the warning.

(e) Keys. `ctrl+o` (`app.tools.expand`) collapses or expands tool output
(`docs/keybindings.md:164`; `dist/core/keybindings.js:37`). Left-click on a
result toggles that call (`tool-execution.js:112-116`). Fullscreen-only scroll
and transcript-search bindings (`tui.altScreen.*` in `docs/keybindings.md`)
navigate the transcript but do not expand tools.

## Claude Code

(a) Collapsed call. A collapsed tool result renders inline as a count summary
with an expand hint: a component taking `{count, unit, expandable,
hiddenChars}` pluralises the unit ("line"/"lines"), appends an approximate
kilobyte size when 1000 or more characters are hidden, and appends the
transcript-toggle chord as the expand affordance when the block is expandable
(`claude.strings`, `chunk-fch65n13.js`, `function wu` / `function wy`). A
character-level variant reads `(+N chars not shown; ...)` (`claude.strings`,
`the full output is in the debug log` hit). A dimmed affordance line reads `·
Click to expand collapsed tool results` (`claude.strings`). Compaction
summaries show `(ctrl+o to see full summary)` (`claude.strings`,
`displayText:L(e.options.verbose,R)` hit). Subagent progress lines read `N
messages from @<agent> (<expand> to expand)` (`claude.strings`,
`chunk-pvxfv19b.js`, `function Cee`).

(b) How a call is opened and where it shows. `ctrl+o` is the global
`app:toggleTranscript` action (`claude.strings`, `hS("app:toggleTranscript",
"Global","ctrl+o")` with a `ctrl+o` fallback when the binding is unknown).
Toggling enters a transcript screen: telemetry logs `tengu_toggle_transcript`
with `is_entering`, `show_all`, and `message_count` (`claude.strings`), and
non-transcript screens bail back to it (`T!=="transcript"` hit). Full command
output is explicitly deferred to that screen: `Exit code 0 - stdout shown in
transcript mode (ctrl+o)` appears three times (`claude.strings`). So a call
opens in two ways: mouse click expands the collapsed block inline, and
`ctrl+o` opens the transcript screen showing the full content. No floating
window or side pane evidence was found; the transcript is a toggled screen,
not an overlay pane.

(c) Per kind. Read: file results come back in `cat -n` format with line
numbers starting at 1 (`claude.strings`, `are returned using cat -n format,
with line numbers starting at 1`). Search (grep tool): supports `-A/-B/-C`
context flags, `-n` line numbers, `head_limit`, and `files_with_matches` versus
content output modes (`claude.strings`, `supports -A/-B/-C context, -n line
numbers, head_limit` hit). Edit/write: the Edit tool takes `old_string` /
`new_string` / `replace_all` (plus file path) and errors when `old_string` is
a substring of a `new_string` from a previous edit (`claude.strings`); the
patch engine parses `@@ -a[,b] +c[,d] @@` hunk headers, splits multi-file
input on `diff --git` boundaries, and reports `no @@ hunk header found` and
per-hunk errors (`claude.strings`). The visual style of the edit diff (gutter
signs, line numbers, colours, word-level highlighting) could not be confirmed
from strings alone. Shell: the header reports `Exit code N` (nonzero appended
to the result; `claude.strings`, `if(Me.code!==0)X.append(...)` hit), with
stdout and stderr shown; background-task shells are summarised as `N tool uses
· M tokens` progress lines. Other/MCP: servers render under a display name
with an optional qualifier and a one-line body summary (`claude.strings`,
`chunk-pvxfv19b.js`, `displayName` / `qualifier` hit); permission prompts name
tools as `Bash(npm run build)`, `Edit(docs/**)`, `Read(~/.zshrc)`
(`claude.strings`, tool-specifier format hit).

(d) Truncation rule and how to see the rest. Three truncation markers were
found: `[output truncated at <J> bytes]` (byte cap on a retained file read,
`claude.strings`), `(output truncated: the command printed <size>; only the
start was r...)` for commands whose output exceeds the inline budget
(`claude.strings`), and `… (earlier output truncated)` for tail-kept views
(`claude.strings`). Oversized output is persisted: tool results carry a
`persistedOutputPath` in a tool-results directory "set when output is too
large for inline", and the model is told `(the full output was saved to a file
... that was not sent ...)` (`claude.strings`). A person sees the rest by
pressing `ctrl+o` for the transcript screen or by opening the persisted file;
nothing in the strings names a numeric line cap for display.

(e) Keys. `ctrl+o` toggles the transcript screen and is the documented expand
affordance (`claude.strings`, `app:toggleTranscript` / `Global` / `ctrl+o`).
Mouse click expands a collapsed tool result inline (`· Click to expand
collapsed tool results`). `claude --help` exposes no tool-display flags; the
closest rendering option is `--ax-screen-reader`, which renders flat text with
no decorative borders or animations (`claude --help`).

## Codex

(a) Collapsed call. The owned-transcript compact row is one status line plus a
short tail. Shell calls render `• Ran <first command line>` with a green
bullet, `• Running` while active, `• Failed (exit N)` in red on nonzero exit,
and `• You ran` for commands the user typed themselves
(`exec_cell/compact.rs:88-110`; `exec_cell/render.rs:471-475`). Multi-line
commands get a `…` marker after the first line, and output shows the last 3
lines (`history_cell/activity_preview.rs:7`, `DETAIL_PREVIEW_LINES = 3`),
each prefixed `└` / indented and dimmed, or `└ (no output)` when empty
(`compact.rs`, `compact_command_lines`). Read-only exploration batches group
into `• Explored` / `• Exploring` with `Read a, b, c` lines
(`exec_cell/render.rs:334-404`). Patch (edit/write) cells collapse to a diff
summary: one header line such as `• Edited <path> (+a −r)` (or `Added` /
`Deleted`, or `Edited N files` with totals) followed by the bounded diff
preview (`diff_render.rs:437-483`, `render_changes_block`;
`render_line_count_summary` colours `+a` green and `−r` red). MCP calls render
`• Called <server>.<tool>` (green), `• Failed <server>.<tool>` (red), or `•
Calling ...` while running, plus up to 3 retained tail lines with over-wide
and over-long content clipped (`history_cell/mcp_preview.rs:21-80`). Plain
(non-shell) tool output previews keep at most 5 lines total, taken head plus
tail around a `… +N lines (<transcript hint>)` ellipsis line
(`exec_cell/render.rs:43`, `TOOL_CALL_MAX_LINES = 5`, and `:150-190`,
`output_lines`). A bounded user-shell exception keeps up to 50 output rows
(`render.rs:44`, `USER_SHELL_TOOL_CALL_MAX_LINES = 50`).

(b) How a call is opened and where it shows. Both inline and in a separate
full-screen overlay. Inline, every activity row carries a disclosure control:
`+ N lines (<key> to expand)` when a counted number of output lines is hidden,
`+ Show details` for other hidden detail, and `− Show less` once expanded
(`transcript_view/layout.rs:39-52`, `activity_layout`). Expanding reveals the
cell's retained full content in place; the module docs state that canonical
cells retain all available data and that expansion uses retained source content
(`chatwidget/activity_presentation.rs:1-2`;
`history_cell/activity_preview.rs:1`). Separately, `ctrl+t` opens a
full-screen transcript overlay (`app/input.rs:580-588`,
`open_transcript_overlay`; `pager_overlay/transcript.rs:71-72`,
`Clear.render(area, buf)` paints the whole area). The overlay shares the main
conversation viewport, including scrolling, selection, search, and copy
(`pager_overlay.rs:1-4` docs; `pager_overlay/transcript.rs:1-4` docs and
`:126-255` search/selection/copy handling). `F3` opens the overlay directly
into search (`app/input.rs:589-592`).

(c) Per kind. Read: parsed read commands display as `Read <name>` lines,
grouped for consecutive successful reads; list and search parses display as
`List <path>` and `Search <query> in <path>`, anything else as `Run <cmd>`
(`exec_cell/render.rs:360-404`). The command header uses bash syntax
highlighting with wrapping that preserves logical lines for selection
(`render.rs:454-520`). Shell: the detailed view shows the highlighted command
and dimmed output lines prefixed `└` for the first line; nonzero exits append
`(exit N)`, or `(command exit N)` for compound commands, and a ripgrep-style
search exit of 1 (no matches) is reported dimmed rather than red
(`render.rs:406-443`). Exit-0 failures hide stderr-only output in some summary
paths (`render.rs:129`). Edit/write (patch): full unified diffs with
right-aligned line numbers, gutter signs (`+` / `-` / ` `), per-hunk syntax
highlighting that preserves parser state within but not across hunks, hard
wrapping of long lines with styles preserved, tab expansion to 4 spaces, and
theme-aware add/delete backgrounds (dark-terminal tints versus light-terminal
GitHub pastels, foreground-only in 16-colour mode) (`diff_render.rs:1-28`
docs; `TAB_REPLACEMENT`; hunk highlighting at `:674-696`). File rows sort by
path and show rename arrows (`diff_render.rs`, `collect_rows`,
`render_changes_block`). Web search cells render `Opened/Opening <url>`,
`Searched/Searching for <pattern>`, with full detail in the transcript
(`history_cell/search.rs:27-80`). MCP: wire results are projected once into
text/JSON/summary blocks, keeping full text and exact JSON fallbacks but
dropping bodies representable only as media or resource summaries
(`history_cell/mcp_result.rs:33-60`); the compact preview shows the tail lines
(`mcp_preview.rs`).

(d) Truncation rule and how to see the rest. Display caps are 5 lines for plain
tool output, 50 rows for user-run shell output, 3 tail lines in compact rows,
3 diff preview lines, 80 graphemes for background-command snippets with a
` [...]` suffix, at most 16 background processes listed, and 16 KiB per line
in MCP previews (`exec_cell/render.rs:43-44`; `activity_preview.rs:7`;
`history_cell/exec.rs`, `UnifiedExecProcessesCell`; `mcp_preview.rs`,
`push_tail`, `floor_char_boundary(16 * 1024)`). Underneath, each cell tracks
total versus retained logical lines (`exec_cell/model.rs:38-41`,
`line_counts`), and truncation notices from upstream are preserved rather than
dropped (`history_cell/mod.rs`, `expanded_hyperlink_lines` / disclosure docs;
`exec_cell/compact.rs`, `command_disclosure`). The ellipsis line always names
the way out: `… +N lines (ctrl+t to view transcript)`
(`exec_cell/render.rs:304-306`, `output_ellipsis_text`; `ui_consts.rs:12-15`,
`transcript_hint`). A person sees the rest by expanding the row inline (`−
Show less`) or by opening the transcript overlay, both of which render the
retained text.

(e) Keys. `ctrl+t` opens the transcript overlay and also closes it
(`keymap.rs:1658`, `open_transcript`; `:1897`, `close_transcript`).
`F3` (or `/` in the pager) searches; `F3` from the app opens the transcript
already searching (`keymap.rs`, `find_transcript`; `PagerKeymap.find`
defaults at `:1888-1894`). Pager scrolling is vim-flavoured: `up`/`k`,
`down`/`j`, `PgUp`/`Shift+Space`/`ctrl+b`, `PgDn`/`Space`/`ctrl+f`,
`ctrl+u`/`ctrl+d` half-pages, `Home`/`End` jumps, `q` or `ctrl+c` closes
(`keymap.rs`, `PagerKeymap` defaults at `:1873-1895`). `ctrl+o` copies the
selection or last message (`keymap.rs:1663`, `copy`). Inline disclosure
controls are toggled per activity (`transcript_view/layout.rs:39-52`).

## Comparison

| Item | pi | Claude Code | Codex |
|---|---|---|---|
| Collapsed call | One-line per-tool header (`read`/`edit`/`$`/`grep`/`find`/`ls`/`write`) + first N result lines; generic `name key=value…` (100 chars) fallback for MCP (`render-utils.js:54-78`; `renderers/*.js`) | Inline count summary (`N lines`, `~KB`, `+N chars not shown`) + `(ctrl+o to expand)`; `· Click to expand…` affordance (`chunk-fch65n13.js` `wu`/`wy`; affordance hit) | One status line (`• Ran`/`• Failed (exit N)`/`• Running`/`• You ran`) + last 3 dimmed tail lines; patches `• Edited path (+a −r)`; MCP `• Called server.tool` (`compact.rs`; `diff_render.rs`; `mcp_preview.rs`) |
| How a call is opened / where | Inline only: `ctrl+o` toggles all, click toggles one (`interactive-mode.js:2458,3734`; `tool-execution.js:112-116,139`) | Inline click expands; `ctrl+o` toggles a transcript screen with the full content (`Click to expand…` hit; `app:toggleTranscript`; `tengu_toggle_transcript`; `stdout shown in transcript mode`) | Both: inline disclosure control (`+ N lines`/`+ Show details`/`− Show less`) and full-screen transcript overlay on `ctrl+t` (`layout.rs:39-52`; `app/input.rs:580-588`; `transcript.rs:71-72`) |
| Read | Header + content, syntax-highlighted when expanded, nothing when collapsed (`read.js:87,95`) | `cat -n` output, line numbers from 1 (strings hit) | Grouped `Read <name>` lines under `• Explored` (`render.rs:334-404`) |
| Search | `grep`/`find` headers + match lines, 15/20-line previews (`grep.js:34`; `find.js:32`) | `-A/-B/-C` context, `-n` numbers, `head_limit`, files-vs-content modes (strings hit) | `Search <query> in <path>`; web cells `Opened/Searched…` with detail in transcript (`render.rs`; `search.rs`) |
| Edit/write diff | `±linenum content` lines: red `-N`, green `+N`, dim context; word-level inverse on 1:1 changed pairs; live preview in tinted box (`diff.js:20-122`; `edit.js`) | `old_string`/`new_string` params; `@@ -a,b +c,d @@` hunk parsing; visual style unconfirmed from strings | Unified diff, right-aligned numbers, gutter signs, per-hunk syntax highlight, theme-aware backgrounds, hard-wrapped long lines (`diff_render.rs:1-28,674-696`) |
| Shell (command + output + exit) | `$ cmd` header, dimmed output (last 5 visual lines), duration line, truncation footer with saved-file path (`bash.js`) | `Exit code N` appended; stdout/stderr shown; full stdout deferred to transcript mode (strings hits) | Highlighted command, `└`-prefixed dimmed output, `(exit N)` / `(command exit N)` suffix; exit 1 from search not red (`render.rs:406-475`) |
| Other / MCP | Generic header + first 10 result lines (`tool-execution.js:6,createCallFallback/createResultFallback`) | Display name + qualifier + one-line body; `N messages from @agent` for subagents (`chunk-pvxfv19b.js`) | `• Called/Failed/Calling server.tool` + 3-line tail; wire blocks projected to text/JSON/summary, media bodies dropped (`mcp_preview.rs`; `mcp_result.rs`) |
| Truncation; seeing the rest | Model cap 2000 lines / 50 KiB + `[Truncated: …]`; display caps 5–20 lines; `ctrl+o`/click shows all retained; model-cut content re-read from source (`truncate.js:10-11`; `bash.js` footer) | `[output truncated at J bytes]`; `(output truncated: … only the start…)`; `… (earlier output truncated)`; full output persisted to tool-results file; `ctrl+o` transcript or the file (strings hits) | Display caps 5/50/3 lines, 80-grapheme commands, 16 processes, 16 KiB MCP lines; total-vs-retained counts kept with notices; inline expand or overlay shows retained text (`model.rs:38-41`; `render.rs:304-306`; `ui_consts.rs:12-15`) |
| Keys | `ctrl+o` expand/collapse; click toggles one (`keybindings.md:164`) | `ctrl+o` transcript/expand; click expands inline (`app:toggleTranscript`; affordance hit) | `ctrl+t` open/close overlay; inline disclosure toggle; pager: `k/j`, pages, `ctrl+u/d`, `/`/`F3` find, `q` close; `ctrl+o` copy (`keymap.rs:1658,1897,1873-1895,1663`) |

## Notes

- Only Codex uses a full-screen overlay for reading back tool output: the
transcript overlay clears and repaints the whole area
(`pager_overlay/transcript.rs:71-72`, `Clear.render`) while sharing the
conversation viewport's scrolling, selection, search, and copy
(`pager_overlay.rs:1-4`). Pi expands strictly inline
(`tool-execution.js:112-139`), and Claude Code pairs inline click-expansion
with a toggled transcript screen rather than a floating pane (transcript-mode
and `tengu_toggle_transcript` hits).
- Selection and copy differ by surface. In pi's regular mode the terminal owns
scrollback so ordinary drag-selection works, while fullscreen mode routes mouse
events to components and keeps unhandled drags available for transcript
selection (`docs/tui.md`, "Handle mouse input"). Codex had to build selection,
search, and copy (`ctrl+o`, `copy_on_select`, primary selection) into its
overlay (`pager_overlay/transcript.rs:126-255`; `pager_overlay.rs`,
`new_transcript`). Any floating surface in Fiber inherits that burden; an
inline surface reuses the terminal's own selection.
- Search exists in Codex's overlay (`/` or `F3`, `PagerKeymap.find`) and in
pi's fullscreen transcript (`tui.altScreen.search`, `docs/keybindings.md`),
but pi has no search scoped to one expanded call, and no per-call search was
found in the Claude Code strings.
- The three tools model "hidden content" differently. Codex retains full
logical text per cell and counts hidden lines for its disclosure labels
(`exec_cell/model.rs:38-41`; `exec_cell/compact.rs`, `command_disclosure`;
`transcript_view/layout.rs:39-52`). Pi re-renders from result details with
separate model-layer and display-layer caps (`truncate.js`; `renderers/*.js`).
Claude Code persists oversized output to files outside the transcript
(`persistedOutputPath` in a tool-results directory). The choice decides
whether "show the rest" is always instant (retained), sometimes a re-read
(model-truncated), or a file open (persisted).
- Disclosure wording is explicit everywhere: pi names the key in the hidden-lines
hint (``... (N more lines, ctrl+o to expand)``), Codex labels counts and keys
(``… +N lines (ctrl+t to view transcript)``, `+ Show details`, `− Show
less`), and Claude Code labels counts, sizes, and the toggle (`N lines
(~KB)`, `ctrl+o to expand`). None of the three relies on an unlabelled
chevron or colour change alone.
- Animation and progress occupy the collapsed row while a call runs in all
three (pi's pending box tint in `edit.js` / `tool-execution.js`; Claude Code's
`Thinking…` / subagent progress hits; Codex's `• Running` / `Exploring`
markers in `compact.rs` / `render.rs`), so the collapsed form doubles as the
live form.
