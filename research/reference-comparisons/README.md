# Reference comparisons and usage evidence, moved out of the area docs (2026-10-05)

Fiber's area docs (`docs/<area>.md`) and `GLOSSARY.md` state only Fiber's rules. This file holds the comparisons with other tools (pi, Claude Code, Codex, opencode, rig) and the measurements of the owner's usage that those docs used to give as the reason for a rule. [#741](https://github.com/aakshintala/fiber/issues/741) moved them here, unchanged. Dates and figures are as they stood in each doc.

Each entry names the doc and section it came from, then quotes the text. Where a line was only one clause of a longer sentence, the quote is the whole fragment that was removed or reworded.

## Reading pi's source (from `GLOSSARY.md`)

pi is Fiber's reference for provider and wire behaviour, not for these nouns, and
the two vocabularies collide. When reading pi:

| The thing | Fiber | pi |
|---|---|---|
| The whole piece of work | session | session |
| Input arrives, Fiber works, Fiber yields | **turn** | agent run (`agent_start` … `agent_settled`) |
| One round-trip to the model | **step** | **turn** (`turn_start` / `turn_end`) |
| One message, reasoning block or tool call | **action** | message (`message_start` / `message_update`) |
| One process, launch to exit | **process** | *(unnamed)* |

## From `docs/code-quality.md`

Section "Tools enforce the rules":

> ([ADR 0002](adr/0002-module-boundaries-are-crate-boundaries.md)). codex asks
> for Rust files under 800 lines in prose, and 270 of its 3,009 source files
> are longer. A check

Section "Size":

> 800 is codex's own target,
> picked rather than measured.

## From `docs/configuration.md`

Section "Files and format":

> so configuration adds no crate. pi, Claude Code and
> the archived Zig tree all use JSON. codex uses TOML, which would have added 6
> crates and 167 KiB of stripped binary on macOS arm64 (`toml` 1.1 against
> serde_json alone, measured on September 25, 2026).

Section "Layers":

> gitignored file in the working tree, as Claude Code uses, would be missing from

## From `docs/delegates.md`

Section "Results":

>   `cancelled`. None of 529 results in the owner's pi sessions ended with a
>   requested `STATUS:` line.

Section "Limits":

> - Measured: the owner peaked at 8 running at once in pi; 17 grandchildren were
>   started, from 1.6% of children.

Section "Worktrees":

> - Measured: the owner isolated 6% of pi launches; 77% of cursor-delegate runs
>   used a path the caller made.

## From `docs/dependencies.md`

Section "Written ourselves":

> call declare `executes`, as `docs/tools.md` requires. codex parses shell with
> tree-sitter-bash; a recogniser that fails closed does not need a full parser.

Section "Supply chain":

>   condition for removing the exception, as codex's `deny.toml` does.

## From `docs/extensions.md`

Section "Extension state":

> `state_too_large`. Across the owner's pi sessions over 60 days, 10,652
>   extension state entries in 605 sessions had a median of 162 bytes, a 99th
>   percentile of 3 KB and a largest of 27 KB (`research/extension-process/`).
>   Bigger

Section "Commands and screens":

> plain, as in pi and Claude Code: `/databricks-models`

Section "Installing":

> directory at install and at every update, as pi runs `npm install` for its
> packages. Like pi, Fiber does not pass `--ignore-scripts`, so

## From `docs/handoff.md`

Section "Overflow":

> A single step's results can be large: in the owner's pi sessions the largest
> step, with each result cut at 16 KiB, was about 49,000 tokens, about 25% of a
> 200,000 token window
> ([research/compaction/usage.md](../research/compaction/usage.md), section 6).
> That is why

Section "Evidence":

> how pi,
>   codex and Claude Code handle an overflowing session, from primary sources.

Section "Evidence":

> the owner's
>   usage. 1.8% of 685 pi sessions compacted. In Claude Code the owner ran
>   `/handoff` 40 times, `/clear` 117 times and `/compact` 11 times.

Section "Cost":

>  Replayed over
> the owner's sessions, handing off at 400,000 tokens costs 0.75 to 0.92 of never
> handing off on Opus 5.5, and a handoff at 150,000 to 400,000 tokens repays its
> cost within 2 to 12 steps where sessions ran a median of 42 or more further
> steps.

## From `docs/invocation.md`

Section "What each command does":

> pi does the same with `!` and `!!` (`excludeFromContext`). Settled by
> [#148]

Section "Shutdown":

> What pi, codex and Claude Code do, and what a crash leaves behind, are
> `research/shutdown/`.

Section "Shutdown":

> codex's headless server gives itself 45 s. Past

## From `docs/loop.md`

Section "The loop":

> How pi, codex and Claude Code run a turn is

Section "What the loop does not do":

> - Detect repetition, or turn tool calls written into prose into calls. None
>   of pi, Codex, Claude Code, opencode or rig does either, and neither
>   appeared in 39,061 pi turns or 31,326 Claude Code turns of the owner's
>   sessions ([research/reply-faults](../research/reply-faults/README.md)).

## From `docs/mcp.md`

Section "MCP":

> What pi, codex and Claude
> Code do is in `research/mcp-client/`.

Section "What Fiber does with MCP":

> (HTTP+SSE). codex has none
> either.

Section "Tools and their names":

> `mcp__<server>__<tool>`,
> the convention codex and Claude Code both use. A name

Section "Starting servers":

> - codex and Claude Code wait 30 seconds.

Section "Starting servers":

> - 5 seconds is Claude Code's connect timeout.

Section "Elicitation, sampling and roots":

> sampling request.
> Neither codex nor Claude Code advertises it.

Section "Elicitation, sampling and roots":

> Fiber does not advertise roots. codex advertises none; Claude Code does.

## From `docs/model-routing.md`

Section "Model routing":

> [ADR 0007](adr/0007-protocols-are-native-providers-are-extensions.md). pi is the
> reference for wire and auth behavior. What it does is in
> [How pi does providers, auth and routing](https://github.com/aakshintala/fiber/issues/4).
> Each protocol's wire facts, read from pi and rig side by side, are in

Section "What a provider extension declares":

> `claude-sonnet-5-5` (Claude Code reviews with
>   Sonnet and never Haiku), `gpt-6-luna` (codex reviews with its luna
>   model); Google

Section "Thinking":

> or both, as pi does. A model

## From `docs/performance.md`

Section "Budgets":

> 1,000 sessions is twice the largest project in the owner's pi sessions (489).

## From `docs/permissions.md`

Section "Effects":

>  pi states the same boundary in its own
> security documentation: extensions "run with the same permissions" as the
> process, and that is outside its security boundary. Fiber's own built-in tools go through this
> seam identically.

Section "Fast paths":

> This is where nearly all of the cost is saved, and it
> is the line Claude Code draws: a fixed allowlist of state-free tools, plus
> "file writes and edits inside the project directory are allowed without a
> classifier call."

Section "Confinement":

>  pi takes the same position in its security documentation: "Real
>   isolation needs to come from the operating system or a
>   virtualization/container boundary."

Section "Confinement":

> codex confines every command by default and asks only to escape. Claude Code
> ships a sandbox that is off by default, and its path denies are enforced by
> the operating system only while that sandbox is on. How each works, and what
> a fence would have broken in the owner's sessions, is
> [research/sandbox/](../research/sandbox/).

Section "What it is shown":

>  Anthropic's published rationale
> for the same design is exactly this: the classifier is "reasoning-blind by
> design" so that "the agent can't talk the classifier into making a bad call."

Section "What it is shown":

> The rejected alternative is Codex's: send the whole transcript with an
> instruction to treat it as "untrusted evidence, not as instructions to
> follow." That label is enforced by asking the model nicely, and the prompt
> grows for the life of the session, which is why Codex needs token budgeting
> and compaction around its reviewer. Stripping is both cheaper and stronger.

Section "Headless":

> private channel. Claude Code makes the same distinction with
> `--permission-prompts host|none`.

Section "(top)":

> The evidence, including what a fence would have broken in the owner's
> sessions, is [research/sandbox/](../research/sandbox/).

## From `docs/prompt-cache.md`

Section "Cache lifetime":

> cheaper on 1 hour. Replayed on the owner's sessions with their real gaps, on
> Opus 5.5, 1 hour costs 0.88 of 5 minutes across 641 pi sessions and 0.57 across
> 169 Claude Code sessions. It costs 1.01 across 143 Claude Code subagent
> sessions, and implementation delegates run longer tools than those did
> ([research/prompt-cache/ttl.py](../research/prompt-cache/ttl.py)).

Section "Warming while idle":

> warms. pi skips warming in the same case (`isReplayable` in its
> `cache-warmer.js`).

## From `docs/state.md`

Section "Override":

> override: pi and codex each have one; Fiber has none.

Section "Override":

> pi (`~/.pi/agent`, `PI_CODING_AGENT_DIR`), codex (`~/.codex`, `CODEX_HOME`)
> and Claude Code (`~/.claude`, `CLAUDE_CONFIG_DIR`) all use one root under the
> home directory. That is the model here: no XDG split, no `~/Library`.

## From `docs/system-prompt.md`

Section "Two parts":

> ("When something changes"). Codex and Claude
> Code also carry this material in conversation messages; pi puts it in the
> system prompt
> ([research/system-prompt/reference-agents.md](../research/system-prompt/reference-agents.md)).

Section "Tool guidelines":

> `crates/tools/prompt/guidelines.md`, one `##` section per tool. pi builds its
> rules from per-tool guidelines the same way.

Section "The model's addendum":

> costs no extra cache miss. Codex keeps a separate prompt per model family for
> the same reason.

Section "Environment":

> monorepo. Codex
> and pi send no git state; Claude Code sends a snapshot.

Section "Size":

>  Codex cuts at 32 KiB across all files and only logs a
> warning.

Section "Size":

>  Claude Code
> caps its listing at 1% of the window and codex at 2%, shortening descriptions
> when over. Measured on the owner's 36 skills, the listing is about 2,500
> tokens, more than 1% of a 200,000-token window, so a cap would cut the
> person's own skills on every such model.

Section "Instruction files":

>  In the owner's sessions, 0.7% of
> pi and 3% of Claude Code sessions edited an instruction file, all through the
> model's own calls.

Section "Evidence":

> how pi, codex and Claude Code build their prompts, from primary sources.

Section "Instruction files":

> Measured on the owner's sessions, only `AGENTS.md` and `CLAUDE.md` occur, and
> 2% of sessions ran in a repository with only a `CLAUDE.md`
> ([research/system-prompt/instruction-files.md](../research/system-prompt/instruction-files.md)).

Section "Subdirectory files":

> never triggers a subdirectory file. Claude Code loads nested files the same
> way. Codex instead tells the model to look for them itself.

## From `docs/tools.md`

Section "Before a call runs":

> The faults seen in the owner's sessions, and
>   what pi, Codex, Claude Code, opencode and rig repair:
>   [research/reply-faults]

Section "Bounded results":

> - In the owner's 648 pi sessions (measured 2026-09-22 with
>   `research/tool-result-sizes/sizes.py`; sizes, so they do not depend on the
>   platform), 16 KiB cuts 1.2% of 26,829 shell results and almost no result of
>   any other tool except file reads (16.8% of 5,070), search (about 9%) and web
>   fetch (24% of 21). A cut read continues from an offset, a cut fetch leaves
>   the whole page in the artifact, and search runs through the shell
>   ("Search").

Section "Progress":

> flush. These are pi's numbers, not measured for Fiber.

Section "File tools":

> How pi, codex, Claude Code and fiber-zig do it is
> [research/file-tools/reference-agents.md]

Section "read":

> at most 20 pages; these are Claude Code's numbers. A

Section "edit":

> ASCII forms (pi's rule). Only

Section "edit":

> - In the owner's pi sessions, 32.5% of 3,837 edits carried more than one
>   block, and up to 27. Claude Code's replace-all was used in none of 505
>   edits.

Section "Stale files":

> - In the owner's Claude Code sessions, Claude Code's refusal to write a file
>   the model had not read fired 26 times; its refusal of a file modified since
>   it was read never fired. 86% to 94% of writes created files never read in
>   the session.

Section "Search":

> - In the owner's pi sessions, 83% of searches went through the shell although
>   pi offered `grep` and `find` tools; in their Claude Code sessions, all of
>   them did. codex has no search tool and tells the model to use `rg`.

Section "How it runs":

> because shell
>   functions are not passed to child processes. Claude Code does the same with
>   an embedded ugrep and bfs.

Section "Behaving like grep and find":

>  About a fifth of the owner's
>   shell `grep` calls are filters of this kind.

Section "Flags":

> unchanged, as
>   Claude Code's does. `find` with

Section "Flags":

> - The built-in handles the flags the owner's sessions use most: `-n`

Section "Other command-line tools":

>  already has onto the new tool. In the owner's pi-rig, tools under new names
>   (`ffgrep`, `fffind`: 180 calls in 30 days) lost to the shell's `grep`,
>   `rg`, `find` and `fd` (757 or more).

Section "Running a command":

>   allowed. This is Claude Code 2.1.280's rule, read from its binary.

Section "Timeout":

> - The unit is milliseconds, as in Claude Code and codex. In the owner's pi
>   sessions (pi's timeout is in seconds), 1,347 of 9,372 timeouts the model
>   set (14%) were 10,000 or more, milliseconds written into a seconds field,
>   which made a wait 1,000 times longer; the opposite mistake in a
>   milliseconds field kills the command within a second, so the model sees
>   it straight away.
> - Of 26,687 foreground shell commands in the owner's pi sessions, 4 ran
>   past 10 minutes without a timeout the model set, and 3 of those 4 were
>   hangs (115 minutes, 291 minutes and 11.2 hours).

Section "Moving to the background":

> - 5.1% of the owner's foreground pi commands ran longer than 30 s (8.6%
>   longer than 10 s, 2.9% longer than 2 minutes). Claude Code moves a
>   command at its 2-minute timeout; codex returns a still-running command
>   after 10 s.

Section "When a command ends":

> - The longest call in the owner's pi sessions, 11.2 hours, started a local
>   server with `&`; the server held the output pipe open and pi waited for
>   the pipe to close.

Section "Stopping a command":

> for at most 2 seconds more (codex's
>   drain bound). If

Section "Result and output":

>   where it reports how it finished. codex also splits evenly; pi keeps only
>   the end and Claude Code only the start. The full

Section "Terminal (`tty`)":

>   and runs no separate terminal host process. codex works the same way
>   (opt-in `tty`, raw bytes, typed input through a timed wait).

Section "Search":

> How pi, codex, Claude Code and fiber-zig search is

Section "Background jobs":

> at least 5 seconds
>   (codex's floor; [fiber-zig#8](https://github.com/aakshintala/fiber-zig/issues/8)
>   found shorter empty polls burned turns). `write`

Section "Background jobs":

>   - Source and testing: these are Claude Code's numbers, taken as they are.
>     Timing-dependent behaviour is tested

Section "Background jobs":

> `output_cap` (Claude Code's documented kill threshold).

Section "Web fetch and web search":

> How Claude Code, codex, pi and fiber-zig do it is

Section "web_fetch":

> `too_large`. Claude Code and
>   fiber-zig both use 10 MiB.

Section "web_fetch":

> Both fail with `timeout`. These are Claude
>   Code's values.

Section "web_fetch":

> - Redirects are followed, to any host, for at most 10 hops. Claude Code
>   returns a cross-host redirect to the model because it approves each host;
>   Fiber judges only the URL the model wrote, because the server chooses a
>   redirect ("Effects").

Section "web_search":

>  as markdown links. Claude Code and fiber-zig
>   both ask for this.

Section "Skills":

> - Claude Code has a `Skill` tool and codex `skills.read`. pi has the model
>   `read` the file, so pi cannot tell a skill load from any other read.

Section "What is deferred by default":

>   the owner's sessions and is not part of how they direct delegates. Measured
>   on September 25, 2026, across 686 pi sessions, 171 Claude Code sessions and
>   143 Claude Code subagent sessions, no built-in except `mcp_resources` meets
>   that. Web fetch, the rarest candidate, is used in 1% of pi sessions but 11%
>   of Claude Code sessions.
> - Deferring has a cost. Claude Code defers widely, and its tool search runs in
>   48% of those Claude Code sessions.

Section "Tool search":

> Fiber runs the search itself, as codex does: BM25

Section "Size warning":

>  The session runs anyway. 10% is the threshold at which
> Claude Code's opt-in automatic mode starts deferring tools.

Section "Size budget in CI":

> - The owner's pi setup spent about 13,800 tokens a request on 31 tools
>   ([pi-extensions#1](https://github.com/aakshintala/pi-extensions/issues/1)).

## From `docs/tui.md`

Section "The logo":

> each letter's counter shaded, as opencode draws its logo.

Section "The narrow layout":

> take its place, as pi-rig lays them out. From

Section "The narrow layout":

> right edge, as pi-rig's footer is. The

Section "Bindings":

> its other paths, as codex's shortcut overlay does. Esc

Section "Themes":

> reports a change, as pi's `light/dark` setting
> does.

Section "Screen readers":

> at start, as codex does,
> and draws flat:

Section "Screen readers":

> force the flat mode on or off, as
> Claude Code's `--ax-screen-reader` does.
