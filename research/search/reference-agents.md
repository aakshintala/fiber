# Search in reference agents: built-in tools or the shell?

Research for fiber#54 (https://github.com/aakshintala/fiber/issues/54). Primary
sources only. Every claim below is either a file:line citation, a URL, or an
exact binary string; absent evidence is marked "not found" with where I looked.

## 1. pi 0.87 (built-in `grep`/`find`)

Source: `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/core/tools/{grep,find,truncate}.js` (unminified).

**Tools:** `grep` and `find`, plus a separate `ls`. Confirmed in
`docs/cli.md:136` ("`grep` | Search file contents") and `docs/settings.md:42`
("Available built-in tools are `read`, `bash`, `powershell`, `edit`, `write`,
`grep`, `find`, and `ls`").

**`grep`** (`dist/core/tools/grep.js:29-37`):
- Parameters: `pattern` (regex or literal string), `path` (default cwd),
  `glob`, `ignoreCase`, `literal`, `context` (lines before/after), `limit`.
- Description string, verbatim (`grep.js:34`):
  > `` Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars. ``
- Implementation: shells out to a downloaded **ripgrep** binary via
  `ensureTool("rg")` (`grep.js:52`), invoked with
  `["--json", "--line-number", "--color=never", "--hidden", ...]`
  (`grep.js:93`). `--hidden` is always passed; `.gitignore` is respected by rg's
  own defaults (not overridden).
- Caps: `DEFAULT_LIMIT = 100` matches (`grep.js:24`), `DEFAULT_MAX_BYTES = 50 *
  1024` (`truncate.js:11`), `GREP_MAX_LINE_LENGTH = 500` chars/line
  (`truncate.js:12`). Truncation is byte- or match-count-first, whichever hits
  first, and appends an actionable notice (`grep.js:220-233`), e.g. `` `100 matches limit reached. Use limit=200 for more, or refine pattern` ``.
- Sort order: whatever order `rg --json` streams matches in (file-system /
  directory-walk order); no explicit re-sort.

**`find`** (`dist/core/tools/find.js:17-39`):
- Parameters: `pattern` (glob), `path`, `limit` (default 1000).
- Description, verbatim (`find.js:39`):
  > `` Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first). ``
- Implementation: shells out to a downloaded **fd** binary via
  `ensureTool("fd")` (`find.js:119`), with `--glob --color=never --hidden`, plus
  `--no-require-git` only when the search root is *not* inside a git repo
  (comment cites `https://github.com/earendil-works/pi/issues/5960`,
  `find.js:129-145`) — so `.gitignore` handling is git-boundary-aware, matching
  fd's own semantics.

**Download/bundling logic** (`dist/utils/tools-manager.js`):
- Neither `rg` nor `fd` is compiled in or vendored in the npm package. On first
  use, pi looks for a system binary (`getToolPath`, `tools-manager.js:75-92`,
  checking `fd`/`fdfind` and `rg` on `PATH`) and, if absent, **downloads** a
  release binary straight from GitHub Releases
  (`https://github.com/BurntSushi/ripgrep/releases/...`,
  `https://github.com/sharkdp/fd/releases/...`, `tools-manager.js:41-61,
  243`), extracts it with `tar`/`unzip`/PowerShell, and caches it in `getBinDir()`.
  `PI_OFFLINE=1` skips the download and grep/find fail explicitly
  (`tools-manager.js:12-17, 308-311`). fd is version-pinned to `10.3.0` on
  darwin/x64 only; ripgrep always resolves "latest" via a redirect trick that
  avoids GitHub's anonymous API rate limit (comment, `tools-manager.js:93-98`).

**Permission classification:** not found. Grepped `dist/` for
`readOnlyTools`/`READ_ONLY_TOOLS`/`isReadOnlyTool` — no hits. Neither tool
calls any permission/approval gate in its `execute()`.

**Steering text toward the tool vs. shell grep:** none found in
`grepToolSystemPromptContribution`/`findToolSystemPromptContribution`
(`grep.js:20-23`, `find.js:24-27`) beyond the one-line snippets `"Search file
contents for patterns (respects .gitignore)"` and `"Find files by glob pattern
(respects .gitignore)"`; no explicit "don't use bash grep" language, unlike
Claude Code's.

## 2. The owner's pi extension: FFF-backed `grep`/`find` (pi-rig)

Sources: `gh issue view 1,35,60 -R aakshintala/pi-extensions`; local repo
`/Users/aakshintala/work/pi-extensions/extensions/search/{index.ts,README.md}`;
installed package `@ff-labs/fff-node@0.11.0` under
`node_modules/@ff-labs/fff-node` and `@ff-labs/fff-bin-darwin-arm64`.

**Spec (issue #35, "Replace pi-fff"):** the owner replaces a third-party
extension `@ff-labs/pi-fff` (`ffgrep`/`fffind`, ~990 prompt tokens, mostly
ignored — "over 30 days there were 156 `ffgrep` and 24 `fffind` calls, but at
least 757 `bash` calls running `rg`, `grep`, `find` or `fd`") with a thin
in-repo extension that overrides pi's *own* `grep`/`find` names, built on
`@ff-labs/fff-node` directly.

**Built tool** (`extensions/search/index.ts`, shipped, not just spec):
- Registers `grep`/`find` under the same names/parameters as pi's built-ins
  (`GREP_PARAMETERS`/`FIND_PARAMETERS`, `index.ts:55-77`), so the model needs
  no new habits.
- `grep` calls `FileFinder.grep(...)` with `mode: "regex"`; `find` calls
  `FileFinder.glob(...)` when the pattern has glob metacharacters
  (`* ? [ {`), else `FileFinder.fileSearch(...)` (fuzzy name search), per
  README: "a pattern with `*`, `?`, `[` or `{` is a glob... other text is a
  fuzzy name search" (`extensions/search/README.md:15-19`).
- Index lifecycle: one `FileFinder` per realpath'd working directory *per
  process*, shared across sessions on the same root (e.g. subagents),
  scanned in the background from `session_start`, destroyed when the last
  session on that root closes (`index.ts:106-146, 195-204`). A search waits
  up to `SCAN_WAIT_MS = 5_000` ms for the first scan before falling back for
  that call only (`index.ts:46, 150-170`).
- Fallback to pi's stock `createGrepTool`/`createFindTool` when: FFF's native
  library isn't available, the frecency DB fails to open (ranking-only
  degrade, `index.ts:131-134`), the cwd is `$HOME` or `/`
  (`index.ts:126`), the 5 s wait expires, or the path argument escapes the
  workspace or contains glob/space characters FFF can't express safely
  (`index.ts:183, 217-218`). A one-time warning notice fires on fallback
  (`fallback()`, `index.ts:187-193`).
- Caps: `GREP_LIMIT = 100`, `FIND_LIMIT = 1000` (`index.ts:47-48`) — identical
  to pi's built-in defaults — plus a `RERANK_MULTIPLIER = 10` ponytail-marked
  cap on how many glob matches get re-ranked (`index.ts:49-53`).
- Verbatim source line on the pattern-escaping trick (`index.ts:81-85`):
  > `` const hex = (c: string) => `\\x{${c.codePointAt(0)!.toString(16)}}`; ``
  — every non-word character in a literal/whitespace pattern is hex-escaped
  before being sent to FFF, because FFF's query parser reads bare spaces and
  `* / ! {` as file/constraint syntax, not pattern text.
- No `ffgrep`/`fffind`/`multi_grep` tools, no `/fff-*` commands (removed per
  spec #35's "Out of Scope" and "Removed" lists).

**What FFF is** (`@ff-labs/fff-node` README, `node_modules/@ff-labs/fff-node/README.md`):
- Upstream: `https://github.com/dmtrKovalenko/fff`. Verbatim (README line 1-5):
  > `` # fff - Fast File Finder / High-performance fuzzy file finder for Node.js, powered by Rust. ``
- Implementation language: **Rust**, compiled to a native shared library and
  called through FFI (`ffi-rs` dependency, confirmed by `npm view
  @ff-labs/fff-node`: `dependencies: ffi-rs: ^1.0.0`) — not a spawned CLI
  process. Building from source is `cargo build --release -p fff-c` producing
  `libfff_c.{so,dylib,dll}` (README, "Building from Source" section).
- Licence: **MIT** (`npm view` `license: MIT`; README "License" section).
- Index/memory behaviour as documented: each `FileFinder` instance "owns an
  independent native index" built by an initial scan plus a live file
  watcher that keeps it current (README "Quick Start" and "Watching files"
  sections). The index itself is in-process/in-memory — the README documents
  no on-disk persistence for it. The one thing that *is* persisted to disk is
  frecency (access-ranking) data, via an explicit `frecencyDbPath` the pi-rig
  extension points at `<agent dir>/fff/frecency`
  (`index.ts:129`, confirmed present on disk at `~/.pi/agent/fff/frecency/`).
  Exact RAM footprint or index size limits: **not found** — the README
  documents no numeric memory bound; I looked in the README, the `.d.ts`
  files under `dist/`, and found nothing beyond "battle-tested and stable,
  and written in a memory-safe language" (README, API Reference section).

**Permission classification:** the extension has no explicit read-only
declaration of its own (`pi.registerTool` in pi-rig doesn't carry a
`readOnly` flag in this version); it simply overrides pi's own `grep`/`find`,
which (per §1) carry none either.

## 3. Claude Code (`Grep`, `Glob`)

Source: `strings` dump of Claude Code 2.1.283, pre-extracted at
`/private/tmp/claude-501/.../scratchpad/cc.txt` (54 MB). Grepped, not fully
read.

**`Glob`:** description string, verbatim (`cc.txt:269786-269791`, and the
lean-prompt variant at `cc.txt:377934` word-for-word identical):
> `` Fast file pattern matching. Supports glob patterns like "**/*.js" or "src/**/*.ts". Returns matching file paths sorted by modification time. ``
The longer, non-lean variant adds (`cc.txt:269786-269790`):
> `` - Fast file pattern matching tool that works with any codebase size / - Supports glob patterns like "**/*.js" or "src/**/*.ts" / - Returns matching file paths sorted by modification time / - Use this tool when you need to find files by name patterns / - When you are doing an open ended search that may require multiple rounds of globbing and grepping, use the [Task/Agent] tool instead (if available) ``
Sort order is explicit and unique among the five agents surveyed:
**modification time**, not path or match order. No numeric result cap string
was found near the Glob description (looked at `cc.txt:269700-269820`);
Glob's own object literal (parallel to Grep's `L0`) was not isolated in the
dump — cap value for Glob specifically: **not found**.

**`Grep`:** internal name constant `$r`, `userFacingName()` returns `"Search"`
(`cc.txt:380972`). Parameters, all confirmed in the Zod-like schema
(`cc.txt:380972`): `pattern`, `path`, `glob`, `output_mode` (enum
`content`/`files_with_matches`/`count`, default `files_with_matches`), `-B`,
`-A`, `-C`/`context`, `-n` (line numbers, default true), `-i`, `-o`
(only-matching), `type`, `head_limit` (default 250, `0` = unlimited, "use
sparingly — large result sets waste context"), `offset`, `multiline`.
Object metadata (`cc.txt:380972`): `maxResultSizeChars:20000`,
`backgrounding:"never"`, `remoteExecution:{supported:!0}`,
**`isReadOnly(){return!0}`**, `isConcurrencySafe(){return!0}`,
`searchHint:"search file contents with regex (ripgrep)"`.

Prompt-steering text (full/non-lean variant, verbatim,
`cc.txt:377934-377939`):
> `` A powerful search tool built on ripgrep\n  Usage:\n  - ALWAYS use Grep for search tasks. NEVER invoke `grep` or `rg` as a Bash command. The Grep tool has been optimized for correct permissions and access.\n  - Supports full regex syntax (e.g., "log.*Error", "function\s+\w+")\n  - Filter files with glob parameter (e.g., "*.js", "**/*.tsx") or type parameter (e.g., "js", "py", "rust")\n  - Output modes: "content" shows matching lines, "files_with_matches" shows only file paths (default), "count" shows match counts\n  - Use [Task] tool (if available) for open-ended searches requiring multiple rounds\n  - Pattern syntax: Uses ripgrep (not grep) - literal braces need escaping (use `interface\{\}` to find `interface{}` in Go code)\n  - Multiline matching: By default patterns match within single lines only. For cross-line patterns like `struct \{[\s\S]*?field`, use `multiline: true` ``
The lean-prompt variant compresses this to (`cc.txt:377933-377934`):
> `` Content search built on ripgrep. Prefer this over `grep`/`rg` via Bash — results integrate with the permission UI and file links. ``
Both variants explicitly steer the model away from Bash `grep`/`rg`.

**Ripgrep integration:** the tool spawns `rg` as a child process with
`--hidden`, `--max-columns 500`, `--json` (content mode) or `--null` (other
modes), building `-B/-A/-C`, `-i`, `-o`, `-U --multiline-dotall`, `--type`,
`--glob`/`--iglob` flags from the schema (`cc.txt:380977`, the `_In` function).
Hardcoded excludes regardless of `.gitignore`: `[".git",".svn",".hg",".bzr",
".jj",".sl"]` (`cc.txt:380972`, `fIn` array) — i.e. VCS metadata dirs are
always excluded in addition to whatever `.gitignore` says.
Bundling: the strings dump shows both an install-time-only signal and a
built-in-binary signal. Verbatim, `cc.txt:262724`:
> `` ripgrep not found on PATH. Install it (brew install ripgrep / apt install ripgrep / winget install BurntSushi.ripgrep.MSVC) or use the native claude binary which embeds it. ``
and `cc.txt:288312`: `` Custom ripgrep configuration for bundled ripgrep support. `` —
so the native (non-npm/Bun-script) Claude Code binary embeds a ripgrep build;
the environment I ran `strings` against apparently expects `rg` on `PATH` or
its own embedded copy, selected by `USE_BUILTIN_RIPGREP` (found bare at
`cc.txt:263411`, no surrounding logic captured by `strings`). A permission
note is enforced at search time (`cc.txt:267044`): `` : ripgrep was found only by name on PATH, and a search outside the working directory cannot apply your Read deny rules in that configuration. Install ripgrep at an absolute path or search under the working directory. `` — i.e. Read deny-rule enforcement depends on ripgrep's resolved path being absolute.
`@vscode/ripgrep` appears once (`cc.txt:24885`) inside a long alphabetical
list of npm package names (`@sveltejs/kit`, `@strapi/strapi`,
`appium-chromedriver`, ...) that reads like a generic "packages with native
bindings" allow/skip-list, not a declared Claude Code dependency — I'm not
treating that single hit as proof Claude Code vendors that exact package.

There is also a *separate*, MCP-exposed low-level `grep` tool (distinct from
the main `Grep`/`Search` tool above), with its own fallback walker, verbatim
description (`cc.txt:392154`):
> `` "Search file contents for a regex. Uses ripgrep if available, otherwise a built-in walker." ``
confirming Claude Code ships a hand-written fallback file walker for when
`rg` truly cannot be found or run (`Ot(...)` function referenced at the same
line).

Sub-agent tool restriction evidence: a bundled agent definition
(`agents/explorer.md`, embedded string, `cc.txt:405105`) grants exactly
`tools: Read, Grep, Glob` to a read-only "explorer" sub-agent — corroborating
Grep/Glob's read-only classification from the tool-object side too.

## 4. codex (codex-rs)

Source: sparse clone at `/private/tmp/claude-501/.../scratchpad/cursor-codex/codex/codex-rs`.

**No dedicated search tool for the model.** `core/src/tools/handlers/` (the
full list of model-facing tool handlers) contains `apply_patch`, `mcp*`,
`plan`, `unified_exec` (shell), `tool_search` (BM25 discovery *over MCP/app
connector tool metadata*, not files — its own description confirms this,
`core/templates/search_tool/tool_description.md:3`: `` Searches over apps/connectors tool metadata with BM25... ``), `request_user_input`,
etc. — no `grep_files`, `file_search`, or `list_dir` handler exists there.
I grepped the whole tree for `grep_files|file_search|list_dir|ripgrep|"rg"`;
every hit outside `tui/` is either the `codex-file-search` crate itself,
sandboxing/spawn plumbing, or unrelated test/protocol names.

**`codex-file-search` powers only the TUI's `@`-mention**, not the model.
Verbatim doc comment, `tui/src/file_search.rs:1-6`:
> `` //! Session-based orchestration for `@` file searches. //! //! `ChatComposer` publishes every change of the `@token` as //! `AppEvent::StartFileSearch(query)`. This manager owns a single //! `codex-file-search` session for the current search root... ``
This is UI-only plumbing for the human's `@file` autocomplete keystrokes; it
never appears in a model tool schema.

**The model searches through the shell.** Base-instruction markdown files
(read directly, not via `strings`) tell every model variant to prefer `rg`
over `grep`. Identical line, verbatim, at
`core/gpt_5_codex_prompt.md:5`, `core/gpt_5_1_prompt.md:284`,
`core/gpt_5_2_prompt.md:250`, `core/gpt-5.1-codex-max_prompt.md:5`,
`core/gpt-5.2-codex_prompt.md:5`, and
`core/templates/model_instructions/gpt-5.2-codex_instructions_template.md:40`:
> `` - When searching for text or files, prefer using `rg` or `rg --files` respectively because `rg` is much faster than alternatives like `grep`. (If the `rg` command is not found, then use alternatives.) ``
The same guidance also appears in the multi-agent orchestrator template,
`core/templates/agents/orchestrator.md:30`. There is no tool-level
enforcement of this — it is pure prompt text; the model issues `rg` as an
ordinary shell command through the `unified_exec`/shell tool, which is not
read-only (it is a general command executor, approval-gated per codex's
sandboxing policy — not investigated further here since it carries no
search-specific classification).

## 5. fiber-zig

Source: sparse clone at `/private/tmp/claude-501/.../scratchpad/cursor-fiberzig/fiber-zig`
(`src/core/workspace/grep_search.zig`, `src/tools/filesystem/{grep_files,glob_files}.zig`,
`src/builtins/tools.zig`, `src/core/tooling/command_policy.zig`).

**Tools:** `grep_files` and `glob_files`, both wholly implemented in **Zig**
— no bundled or downloaded `rg`/`fd` binary. `grep_files` shells out to `git
grep` only when the search root is inside a non-`.gitignore`-excluded git
repo; otherwise (and for the always-run untracked-file pass) it walks the
filesystem itself and scans file bytes in Zig.

**Production model-facing descriptions** (`src/builtins/tools.zig:33-36`,
verbatim):
> `` grep_files: "Search text files for a literal substring, optionally narrowed by path/include, with output modes for matching lines, files-with-matches, or counts plus head_limit/offset pagination and bounded context_lines for matches mode. [...] Use include as the type/path filter, such as *.zig. When to use: find exact symbols, strings, TODOs, or usage sites. When NOT to use: regex is not supported; avoid unknown-concept exploration, filename lookup, known-path reads, and shell grep; do not repeat the same or equivalent search after a caller search only finds a definition." ``
> `` glob_files: "Find file paths matching a glob pattern, with mode=count for exact path counts without listing entries. [...] When to use: locate files by name, extension, or directory pattern; narrow path or pattern if candidate caps appear. When NOT to use: search file contents, read files, run find, or count non-file concepts." ``
This is the most explicit "use the tool, not the shell" language of any
agent surveyed, and it is unusual in stating a **limitation** (no regex —
`grep_files` is literal-substring only, confirmed by the implementation:
`grep_search.zig` matches via `std.mem.find`/`containsIgnoreCase`, and its
own git-grep argv test asserts `-F` (fixed-string), `grep_search.zig:401-403,
866-877`: `` gitGrepArgvForTest ... "-F" ...``).

**Arguments:**
- `grep_files`: `pattern` (literal), `path`, `include` (glob filter on
  candidate paths), `case_insensitive`, `mode` (`matches` /
  `files_with_matches` / `count`), `head_limit`, `offset`, `context_lines`
  (`src/builtins/tools.zig:253-266`).
- `glob_files`: `pattern`, `path`, `mode` (`matches` / `count`)
  (`src/builtins/tools.zig:226-232`).

**Caps** (`grep_search.zig:13-19`, `tool_dispatch.zig:47`):
- `output_cap = 200` (default `head_limit` for grep results and matches).
- `collection_cap = 2000` (matching lines collected before the scan itself
  stops, independent of what's rendered).
- `file_byte_cap = 50 * 1024 * 4` = 204,800 bytes per file read for scanning.
- `git_grep_stdout_limit = 8 MiB` for the `git grep` subprocess.
- `context_lines_cap = 5`, `context_file_byte_cap = 200 KiB` (grep_files.zig:13-14).
- `default_max_list_entries = 100` (`tool_dispatch.zig:47`) — this, not
  `output_cap`, is the real ceiling on rendered lines: `head_limit` is
  clamped to `min(head_limit, max_list_entries)` (`grep_files.zig:324`).

**Ignore handling:** `.gitignore` via `git grep` for tracked files inside a
repo, plus a separate untracked-file pass so new/unstaged files still show up
(test name, `grep_search.zig:953`: "grep search scans untracked files after
git grep tracked backend"). Outside a git repo, or when git itself is
`.gitignore`d at the root, it falls back entirely to a Zig-native walk that
still honours a static `ignored_directory_names` list (e.g. `node_modules`)
— but an **explicitly requested** path under an ignored directory name is
still searched (test, `grep_search.zig:879`: "grep search preserves
explicitly requested ignored directory roots"). Symlinks that resolve
outside the workspace root are skipped and traced
(`grep_search.zig:660-667`).

**Permission classification:** both tools declare `.reads_only_fn` returning
`true` and `.irreversible_fn` returning `false`
(`src/tools/filesystem/grep_files.zig:548-555`,
`src/tools/filesystem/glob_files.zig:431-438`), and both `ToolSpec`s set
`.requires_approval = false` (`src/builtins/tools.zig:236, 271`) — i.e.
explicitly no-approval, read-only, by construction, not by convention.

**Steering away from the shell — a second, independent mechanism.**
Beyond the tool descriptions above, fiber-zig annotates raw shell commands
themselves. Verbatim, `src/core/tooling/command_policy.zig:34-40`:
> `` if (std.mem.eql(u8, base, "cat") ...) return "safer: use read_file for file inspection"; if (std.mem.eql(u8, base, "ls")) return "safer: use glob_files for discovery"; if (is_pattern_matcher(base)) return "safer: use grep_files for exact local search"; if (std.mem.eql(u8, base, "find")) return "safer: use glob_files for discovery"; ``
So a shell command whose base token is `grep`/`ag`/`ack`/etc. (whatever
`is_pattern_matcher` covers) gets a live "safer: use grep_files..." note
attached to its result, rather than (or in addition to) a static system-prompt
line — steering happens at the point the model actually runs the shell
command, not only up front.

## 6. Rust crates ripgrep exposes as libraries

Source: `cargo info <crate>` (queries crates.io directly), run 2026-09-26.

| crate | version | licence | what it is |
|---|---|---|---|
| `grep` | 0.4.1 | Unlicense OR MIT | top-level facade: "Fast line oriented regex searching as a library" |
| `grep-searcher` | 0.1.17 | Unlicense OR MIT | the line-oriented search engine (buffering, multiline, context) |
| `grep-regex` | 0.1.14 | Unlicense OR MIT | Rust `regex` crate wired into `grep`'s `Matcher` trait |
| `grep-matcher` | 0.1.9 | Unlicense OR MIT | the `Matcher` trait itself — "with a focus on line oriented search" |
| `grep-printer` | 0.3.1 | Unlicense OR MIT | ripgrep's own result-printing `Sink` implementation |
| `grep-cli` | 0.1.12 | Unlicense OR MIT | shared CLI utilities (decompression, stdin detection, etc.) |
| `ignore` | 0.4.33 | Unlicense OR MIT | ".gitignore"/`.ignore` matching against file paths, with directory walking |
| `globset` | 0.4.20 | Unlicense OR MIT | cross-platform single-glob and glob-*set* matching |
| `ripgrep` (the CLI binary crate) | 15.2.0 | (same repo/licence) | for reference — not a library dependency target |

All eight are dual Unlicense/MIT, all published from
`https://github.com/BurntSushi/ripgrep/tree/master/crates/*`, all maintained
in the same monorepo as the `rg` binary itself. A Rust implementation of
fiber's own search tool could depend on `grep-searcher` + `grep-regex` (or
`grep-matcher` + a hand-picked regex engine) for the search core, and
`ignore` + `globset` for directory walking / `.gitignore` / glob matching,
without shelling out to `rg` at all — mirroring what fiber-zig already does
by hand in Zig (§5), and what Claude Code and pi do by shelling out instead
(§§1, 3).

## Comparison table

| | pi (built-in) | pi-rig (FFF) | Claude Code | codex | fiber-zig |
|---|---|---|---|---|---|
| Dedicated model tool? | yes (`grep`,`find`) | yes (overrides same names) | yes (`Grep`,`Glob`) | **no** — shell only | yes (`grep_files`,`glob_files`) |
| Implementation | shells out to downloaded `rg`/`fd` | Rust native lib via FFI (`@ff-labs/fff-node`), index kept live by a watcher | shells out to `rg` (bundled in native binary; PATH otherwise) + a JS fallback walker on an MCP-exposed low-level `grep` | model told to run `rg` via shell | pure Zig; `git grep` for tracked files + hand-rolled scanner for the rest |
| Regex support | yes | yes (regex, w/ escaping tricks) | yes, full ripgrep regex | yes (whatever `rg` supports) | **no — literal substring only** |
| Result cap | 100 matches / 50 KB / 500 chars/line | 100 / 1000 (mirrors built-in) | `head_limit` default 250, `maxResultSizeChars` 20000 | none (shell has no cap) | 100 rendered / 200 collected internally / 200 KB per file |
| `.gitignore` handled by | `rg`/`fd` defaults | FFF's own indexer | `rg`, plus hardcoded VCS-dir excludes | `rg` (if the model uses it) | `git grep` + static ignore list, both bypassable by an explicit path |
| Read-only classification | not found (no gate at all) | not found (no gate at all) | explicit `isReadOnly(){return!0}` | n/a (shell is not read-only) | explicit `reads_only_fn`→true, `requires_approval=false` |
| Steers model off shell grep? | no explicit text | no explicit text (parent pi text unchanged) | yes — "ALWAYS use Grep... NEVER invoke `grep` or `rg` as a Bash command" | **inverted** — tells the model to prefer shell `rg` over shell `grep` | yes, twice: tool description ("avoid... shell grep") *and* a live annotation on any `grep`-like shell command run anyway |
| Sort order | rg/fd stream order | FFF ranking (frecency + git-dirty first) | **modification time** (Glob only) | n/a | none stated for grep; glob sorts candidate paths lexically before matching |

## Surprises

1. **codex is the outlier, and in the opposite direction from everyone else.**
   Every other agent either builds a dedicated tool or (fiber-zig,
   `command_policy.zig`) actively discourages shell grep. codex's base prompt
   does the reverse: it tells the model to prefer `rg` *as a shell command*
   over `grep`, because codex has no content-search tool for the model at all
   — `codex-file-search` exists only to drive the TUI's `@`-mention.

2. **fiber-zig's `grep_files` doesn't support regex.** It's explicitly a
   literal-substring search (`git grep -F`, `std.mem.find` in the fallback
   scanner), and the tool description says so as a "When NOT to use" caveat.
   Every other agent's grep tool supports regex.

3. **The owner already shipped, then explicitly measured and rejected, a
   duplicate-name search tool.** pi-extensions issue #35 isn't hypothetical —
   it cites real usage data (156 `ffgrep`/24 `fffind` calls vs. 757+ raw
   `bash` grep/find/rg/fd calls over 30 days) as the reason duplicate tool
   names lose to the shell, and the fix was to *override the built-in names*,
   not add a better-described alternative alongside them.

4. **Claude Code enforces two different search-permission stories depending
   on how `rg` was resolved.** If ripgrep is found only by name on `PATH`
   (vs. an absolute, presumably admin-configured path), a search outside the
   working directory can't have its Read-deny rules applied and is refused
   with a specific message (`cc.txt:267044`) — the permission model is
   coupled to *how the ripgrep binary was located*, not just to the tool
   itself.

5. **fiber-zig steers away from shell grep twice, at two different points in
   the loop**: once statically in the tool's own description, and again live,
   attached to the result of any shell command that actually runs `grep`/`ls`/
   `find`/`cat` anyway (`command_policy.zig`). None of the other four agents
   have an equivalent "catch it after the fact" mechanism — pi and Claude Code
   only have the static, up-front description text (Claude Code's is far more
   forceful: "ALWAYS... NEVER...").

6. **FFF's "index" is genuinely just an in-memory structure kept live by a
   watcher, not a database.** The only thing pi-rig persists to disk is the
   frecency (ranking) store, at an explicit path the extension controls. No
   numeric memory ceiling is documented anywhere I could find for the index
   itself (README, `.d.ts` files) — worth measuring directly if fiber were to
   consider an FFI-based indexer rather than shelling out or hand-rolling a
   walker.

## Addendum: Claude Code's embedded search tools

Checked by hand on 2026-09-26 against `strings` of Claude Code 2.1.283.

Claude Code has an `EMBEDDED_SEARCH_TOOLS` switch (`EMBEDDED_SEARCH_TOOLS:()=>pC` in its environment table). Its shell setup defines shell functions that shadow `grep` and `find` and run search engines embedded in the `claude` binary itself:

- `dbe("grep","ugrep",["-G","--ignore-files","--hidden","-I",...lAn.map((e)=>`--exclude-dir=${e}`)],["-*-filter*","-*-pa…` — `grep` runs the embedded ugrep in basic-regex mode, honouring ignore files, including hidden files, skipping binary files, and excluding `lAn=[".git",".svn",".hg",".bzr",".jj",".sl"]`. Some flags fall through to the system `grep`.
- `dbe("find","bfs",["-S","dfs","-regextype","findutils-default"])` — `find` runs the embedded bfs.
- The function body falls back to the binary at `claude` (or `claude.exe`) when an override variable is unset: `[[ -x $_cc_bin ]] || _cc_bin=…`.

The owner's Claude Code sessions contain no `Grep` or `Glob` calls at all (`usage.md`), which fits this mode being on for them. The model searches with ordinary `grep` and `find` in the shell, and gets ignore-aware, fast search without a dedicated tool.
