# Code mode as a cost lever

Research for [#445](https://github.com/aakshintala/fiber/issues/445): does code mode, where the model writes a small program of tool calls instead of making each call itself, cut cost? Gathered 2026-10-30 from published evidence and aggregates of the owner's own sessions. Claims cite their URL or file path; inference is marked as such.

Evidence labels: "current" means Opus 5.5, Opus 5, Sonnet 5 or 5.5, Fable 5 or 5.1, GPT-6 family, Haiku 5.5. "Older" means anything else, with the model named.

## 1. Verdict

1. Code mode is a real lever, but a conditional one. It pays for large tool catalogs (MCP), fan-out of roughly 6 to 30 or more calls, and big outputs that can be filtered in code. It does not pay for the short, dependent bash/read/edit chains that make up the owner's usage.
2. The headline numbers (98.7%, 99.9%, 81%) mostly measure hiding tool definitions or looping over dozens of calls, not step savings in a coding loop. Tool search or deferred loading gets much of the definition saving without code.
3. The strongest current-model, coding-agent number is Factory's: 15% cheaper on Opus 5.5 versus direct calls, with the saving concentrated in the 12.5% of scripts that make six or more calls. It is a vendor claim with no isolated experiment.
4. For the owner's sessions the implied saving is single-digit to low-double-digit percent of cost (Claude Code 3% to 8%, pi 7% to 15% on a generous read-only bound), because 94% to 98% of input tokens are already cache reads and a saved step is cheap. Neither pi `codemode` nor any MCP tool appears in 10,227 pi session files.
5. Recommendation: not a cost lever worth prioritising for Fiber's built-in tools. Worth building as the #444-seam extension if MCP fan-out, latency, or keeping bulky output out of context matters; measure on Fiber's own traffic first.

## 2. What each reference does

### pi (`builtin:codemode`)
- A `codemode` tool takes raw JavaScript (not JSON, no fence), run as an async function body in QuickJS (WASM) with no Node APIs, file system, network or timers. Scripts reach the world through `tools.<name>(args)` and `models`. `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/docs/codemode.md`.
- Result: `Script completed` or `Script failed`, wall time, then text/image items in order; `max_output_tokens` default 10000 keeps start and end and writes the full text to a temp file; partial output survives a failure; calls made before a failure are not undone; calls still running at script end are cancelled (docs/codemode.md "Scripts").
- Globals: `text`, `image`, `console.log`, `return`, `exit`, `store`/`load` (JSON kept across calls in the session transcript), `ALL_TOOLS`, `searchTools` (BM25), `describeTool`, `describeNamespace`, `models` (classifier and image models only).
- Limits: 256 MB VM, two workers per process (`PI_CODEMODE_MAX_WORKERS`), scripts wait up to 60 s for a worker, no nested codemode.
- Opt-in for built-ins: `"defaultTools": ["+codemode"]` (`docs/cli.md` "Enable codemode"). Auto-activated when an MCP server with `codemode` exposure connects; MCP default exposure is `codemode` (`docs/mcp.md` lines 195-204, 230). `codemode.mode: "only"` hides every other tool from the model (`docs/settings.md:43`).
- Declaration budget: tool declarations in the description share 3000 estimated tokens (`codemode.inlineBudget`); the rest is found with `searchTools()`. Deferred tools are left out so the description does not change while servers connect (cache stability).
- Inner calls: each goes through the normal tool pipeline, so extension `tool_call`/`tool_result` hooks and permission gates apply, with `parentToolCallId` set (`docs/mcp.md:252`). The nested record keeps up to 256 calls, 8 KiB of arguments per call, 32 KiB total (`dist/core/nested-tool-calls.js`).
- Stated rationale: composition and keeping data out of context ("a neat little sandbox to compose tool calls"), not a benchmark. Earendil's post gives no direct-versus-codemode comparison ([earendil.com/posts/you-said-no-mcp](https://earendil.com/posts/you-said-no-mcp/)). The system prompt guideline is "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output" (`dist/extensions/codemode/tool.js:43`).
- Open pi issues show cache and operations costs: [#11114](https://github.com/earendil-works/pi/issues/11114) (codemode `only` mode caused a prompt-cache hit drop from about 99% to 13.7%), [#11529](https://github.com/earendil-works/pi/issues/11529) (tool order busts the cache), [#11350](https://github.com/earendil-works/pi/issues/11350) (long scripts starve other sessions), [#10426](https://github.com/earendil-works/pi/issues/10426) (`only` mode hides tools but does not stop direct calls).

### codex (`code_mode`)
- A freeform `exec` tool takes raw JavaScript run in a fresh V8 isolate as an async module, plus a `wait` tool that resumes a running cell (`codex-rs/code-mode-protocol/src/description.rs`). Nested tools are on a global `tools` object. Helpers: `text`, `image`, `audio`, `store`/`load`, `notify`, `setTimeout`, `ALL_TOOLS`, `yield_control`. First-line pragma `// @exec: {"yield_time_ms":..., "max_output_tokens":...}`; default 10000 tokens.
- Status: feature `code_mode` is `Stage::UnderDevelopment`, default off; `code_mode_only` also under development; `code_mode_host` and `code_mode_interrupt` stable (`codex-rs/features/src/lib.rs:1259-1340`). A model's catalog entry can set `tool_mode` to override per model (`codex-rs/core/src/tools/mod.rs:67-77`). Read at commit `2f452e74b2dfd7e927f90d8b1d573beae521b9c8`.
- Nested tools are dispatched through the router per call (`core/src/tools/code_mode/mod.rs` `submit_nested_tool`). Open bugs show the visibility and hook cost: [#23411](https://github.com/openai/codex/issues/23411) `exec` did not fire `PreToolUse` hooks, [#49908](https://github.com/openai/codex/issues/49908) list already-completed calls when a script fails, [#55926](https://github.com/openai/codex/issues/55926) SDK stream omits nested MCP outcomes, [#57466](https://github.com/openai/codex/issues/57466) nested call ids not persisted in rollout; and host-process failures ([#53091](https://github.com/openai/codex/issues/53091), [#41507](https://github.com/openai/codex/issues/41507): host crash breaks every tool call in `code_mode_only`).
- No measurements in the tree or docs I found; a web search found no published token figures.

### opencode v2
- "Code Mode is the default" for MCP servers; `codemode: false` per server exposes tools directly. Tools appear as `tools.context_7.resolve_library_id(...)`. Instructions are omitted when the agent's permissions deny `execute` for that server ([opencode.ai/v2/docs/mcp-servers](https://opencode.ai/v2/docs/mcp-servers/)). The page makes no token or cost claims.
- Implementation: `packages/codemode` on branch v2, "a lightweight, pure interpreter for a JavaScript-like language", deliberately not a sandbox for real JS, so only implemented syntax runs; unsupported syntax returns an `UnsupportedSyntax` diagnostic. Hooks `tool.before`/`tool.after` surround every call a program makes; a failing `before` hook denies the call. `packages/codemode/AGENTS.md`: "Do not add a speculative generic permission or approval policy... enforce domain authorization inside each provided tool." ([README](https://github.com/anomalyco/opencode/blob/v2/packages/codemode/README.md)).
- Scope: MCP tools only, i.e. the catalog-size case.

### Factory (Droid 1.0 "Script tool")
- Programmatic tool calling: the model writes JavaScript run in a sandbox with "no file, network, or system access of its own". On by default for ten models (GPT-6.1 Sol, Terra, GPT-6 Astra, GPT-6 Sol, GPT-5.6 Sol, Claude Fable 5.1, Fable 5, Opus 5.5, Opus 5, Kimi K3). Printed output capped, full results logged, independent calls concurrent, 5-minute wait then background mode, a separate tool to check or stop a script, "Each tool call has its own row" in the timeline. The page does not say how inner calls are permission-checked. ([factory.com/news/frontier-harness](https://factory.com/news/frontier-harness))
- Numbers in section 3.

### Claude Code
- No code mode in the CLI that I could find. Anthropic's programmatic tool calling is an API feature that needs the code execution tool (Python in a container), supported on current Claude models; a community MCP project says Claude Code subscriptions do not get it ([platform.claude.com PTC docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling)). I found no Claude Code release note either way.
- Claude Code is Factory's lowest comparison on Opus 5.5 (57.6% on Terminal-Bench 4 versus Droid 65.2%), but the page compares whole harnesses.

### Also
- OpenClaw ships experimental "Code Mode" (`exec`/`wait`, QuickJS or Node executor): nested calls follow the normal policy, approval and hook path; "It suits large catalogs and models that reliably write short programs. Small catalogs or weaker models may do better with direct exposure or Tool Search." ([docs.openclaw.ai/tools/code-mode](https://docs.openclaw.ai/tools/code-mode.md))
- Anthropic's API PTC is Python and calls tools as `asyncio.gather`-able functions; `disable_parallel_tool_use: true` is not supported with it.

## 3. Measured evidence

"Parallel on" means the baseline allowed native parallel tool calls. "Cache" means whether the source accounted for prompt caching.

| Source | Models | Baseline | Saving | Parallel in baseline | Cache | Notes |
|---|---|---|---|---|---|---|
| [Factory, Droid 1.0](https://factory.com/news/frontier-harness) | Opus 5.5 (current) | Droid direct calls | Cost -15%; savings concentrate in the 12.5% of Scripts with 6+ calls | Not stated | Not stated for this figure | Vendor, tasks not named; benchmark table compares harnesses and does not isolate Script |
| Factory single session | GPT-6.1 Sol high (current) | Droid direct calls | 10 to 5 model calls, 56.8 s to 41.6 s (-27%) | Not stated | n/a | n=1 |
| Factory Linear and CI examples | not stated | Modeled direct calls at 3 to 10 calls per request | 3.1M input tokens to about 61K (151 issues, 312 calls); 2 requests versus 59 to 187 | Modeled 3 to 10 calls per request, so partly | Modeled, 28K fixed tokens | Direct figures are estimates, not runs |
| Factory Terminal-Bench 4 | Opus 5.5 / GPT-6.1 Sol (current) | Other harnesses | Droid $4.73 vs lower-cost rivals by up to 24%; Sol 12% cheaper than Codex | Not stated | List-price token cost | Whole-harness comparison, PTC contribution not isolated |
| [withone.ai on pi](https://www.withone.ai/blog/pi-codemode-one-mcp-server/md) | Sonnet 5.5 (current) | pi, same MCP tools declared direct | 324,426 to 66,200 total tokens, $0.43 to $0.056, 142 s to 34 s, 10 to 4 turns, 63 vs 62 calls | Implicitly yes: 62 calls in 10 turns | Not discussed; cost presumably from pi usage | One run each, vendor of the MCP server, 29-comment classification fan-out; the direct run emitted 21K output tokens writing every request body |
| [Anthropic PTC docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling) | "a production Claude model" | Direct tool calls | -38% billed input tokens on a 75-tool project agent, accuracy unchanged; +8% cost on tau2-bench (1 to 2 sequential calls per turn); 20% to 40% typical across traffic with 10 to 49 tool definitions | Not stated | Billed input tokens, not stated | Vendor. Sequential single-call work gets no benefit |
| [Anthropic engineering post](https://www.anthropic.com/engineering/code-execution-with-mcp) | not stated | Loading all MCP definitions and passing data through the model | 150,000 to 2,000 tokens (98.7%) | n/a | No | Saving comes from progressive tool disclosure and keeping a transcript out of the model, not step count |
| [Cloudflare MCP](https://blog.cloudflare.com/code-mode-mcp/) | not stated | Tool per endpoint | 1.17M tokens to about 1,000 (2,500 endpoints) | n/a | No | The baseline is theoretical |
| [Cloudflare Code Mode post](https://blog.cloudflare.com/code-mode/) | not stated | Tool calling | No numbers | n/a | n/a | Hypothesis only: "an enormous amount of real-world TypeScript in their training set" |
| [WorkOS demo of Cloudflare](https://workos.com/blog/cloudflare-code-mode-cuts-token-usage-by-81) | not stated | Direct MCP | -32% tokens (1 event), -81% (31 events) | Not mentioned | Not mentioned | Demo, one run per task |
| [AIMultiple](https://research.aimultiple.com/code-execution-with-mcp) | GPT-4.1 (older) | Bright Data MCP, definitions loaded | Total tokens -77.4% (775K to 175K); output tokens about 2.2x; latency +7% (9.66 s to 10.37 s); success 100% both | Not stated | No | Independent, 50 runs per task |
| [arXiv 2608.06370, "The Bitter Lesson of Tool Calling"](https://arxiv.org/abs/2608.06370) | 14 models incl. Sonnet 5, GPT-5.6 Luna/Sol/Terra (current) and Haiku 4.5, Sonnet 4.5/4.6, Opus 4.8, GPT-4o, 4.1, 5, 5.4 (older) | JSON tool calling, BFCL v4 subset (309 items, echo stubs) | Accuracy: equal or better in 11 of 14 models; tokens: PTC costs more below about 26 calls (about 1.5x input on chaining), cheaper above (N=48: 3,535 vs 5,097); latency about half on chaining for 13 of 14 models, GPT-5 2.8x | Not stated | Not addressed | Independent. Small ablations (n=31 to 52), stubs measure argument serialisation only; GPT-4o, 4.1, 5.4-mini lost 19.7% to 26.9% accuracy to newline-in-script failures |
| [Speakeasy](https://www.speakeasy.com/blog/how-we-reduced-token-usage-by-100x-dynamic-toolsets-v2) | Sonnet 4.5 (older) | Static toolsets of 40 to 400 tools | Dynamic toolsets (not code mode) -91% to -97% input tokens, but about +50% time and 2 to 3x more calls | n/a | n/a | Evidence that tool search captures the definition saving |

Reading across: the biggest numbers come from the definition-loading effect (a catalog the baseline never needs to load) or from fan-out loops over 29 to 312 calls. The only coding-agent figure on current models is Factory's 15%, and it is unisolated.

## 4. Counter-evidence and risks

- Sequential and small workloads do not benefit: tau2-bench +8% cost (Anthropic PTC docs); PTC costs more tokens below about 26 calls (arXiv 2608.06370); Anthropic's own "weak fit" list is strictly sequential workflows, few calls with small responses, and tools needing feedback between calls.
- Output tokens rise: AIMultiple saw 2.2x output tokens and 7% more latency (GPT-4.1, older). Output is the expensive side of the bill. In the withone run the opposite happened because the direct run wrote 29 request bodies by hand; it depends on the task.
- Native parallel tool calls already collapse many steps. Factory's modeled baseline uses 3 to 10 calls per request; the withone direct run did 62 calls in 10 turns; most other numbers above do not say. Wherever a source does not state it, treat its step saving as overstated.
- Caching: nearly no source prices it. Factory reports 98% cache reads in production. A tool-call step on a cached prefix costs about a tenth of its raw input price, so "N calls cost N steps of re-reading the transcript" overstates the cost. The cache also makes a code-mode description change costly: pi #11114 and #11529 are cache-busting bugs.
- Failure modes: models writing broken scripts, especially escaping in multi-line JSON arguments (arXiv: GPT-4o, GPT-4.1, GPT-5.4-mini; mitigated by raw-source freeform input as pi and codex use); Ronacher: it "does not work well" with smaller models and inside-MCP double JSON escaping confuses them; MCP servers returning inconsistent output so a probe with 5 items works and the full batch fails ([lucumr.pocoo.org](https://lucumr.pocoo.org/2026/10/6/codemode/)); a community extension, pi-codemode-guard, exists to compile model scripts for pi's QuickJS.
- Silent loss of information: in the withone run the script dropped three dead comments and reported nothing skipped, while the direct run reported the three. Filtering in code hides what the model did not look at.
- Debugging and visibility: failing programs are harder to trace; error messages are often generic ([Arize](https://arize.com/blog/code-mode/), [ContextBolt](https://contextbolt.com/blog/code-mode-mcp/), vendor-authored). Codex needed issues for listing completed calls on failure (#49908) and nested-call persistence (#57466).
- Permissions: approval prompts show code, not a named call; a sandbox does not sandbox the credentials it is handed; hook bypass is a real bug class (codex #23411). Pi's author recommends containers because pi has no built-in permission system.
- Operations: pi's two-worker limit and starvation (#11350), codex host crashes (#53091, #41507, #42527) and Windows start-up failures (#58137).
- Definition savings are separable: tool search or deferred loading gets 91% to 97% of the definition saving without code (Speakeasy, Sonnet 4.5, older, at about +50% time); ContextBolt recommends fewer tools, then deferral, then code mode only for large intermediate data; Anthropic guidance favours plain calls below about ten tools.
- Consensus is thinner than claimed. Codex has it off by default (under development). Pi enables it automatically only when MCP servers are present. Opencode defaults it only for MCP. Claude Code has none. Only Factory enables it for built-in tools by default.
- Caveat on sources: Factory, withone, Anthropic and Cloudflare are vendors; several 2026 blog numbers are secondhand. I did not re-run any benchmark.

## 5. The owner's usage

Method: a local analysis script over the owner's Claude Code and pi session logs, reporting aggregates only. The script is not in this directory because it reads the owner's local session paths. A "step" is one model response; a "run" is a maximal sequence of consecutive tool-using steps with no user turn between them and ended by a step with no tool call. Tool-result tokens are chars divided by 4, cumulative per session and reset when input tokens halve (compaction). Cost for Claude Code is in input-token equivalents (input 1, cache read 0.1, 1-hour cache write 2, output 5); for pi it is the logged dollar cost. Each sample is the most recent 300 sessions with at least 10 steps in the segment (24 for interactive Claude Code, which is all there are in the window).

Sessions in the window are mostly autonomous: lanes and delegate jobs run with one prompt, so runs are far longer than an interactive person-driven session. Segment results:

| Segment | Sessions / steps | Models (current) | Calls per step | Tool steps with parallel calls | Run length in steps: median / p90 | Runs of 6+ steps: share of runs, share of tool steps | Bash + read + edit share | Tool results as share of input tokens / cost | Cache-read share of input |
|---|---|---|---|---|---|---|---|---|---|
| Claude Code interactive (`cli`) | 24 / 2,755 | Opus 5.5 | 0.80 | 8.8% | 2 / 8 | 17%, 53% | Bash 81, Read 1.8, Edit 1.0 | 17% / 15% | 98.5% |
| Claude Code `--bg` sessions | 300 / 70,463 | Sonnet 5.5, Opus 5.5, Haiku 5.5 | 0.97 | 14.3% | 3 / 10 | 28%, 61% | Bash 84, Read 1.7, Edit 1.4, SendMessage 6.9 | 18% / 15% | 97.8% |
| Claude Code headless (`sdk-cli`) | 300 / 6,891 | Sonnet 5.5 mostly | 1.16 | 19.3% | 15 / 41 | 97%, 99.5% | Bash 92, Read 5.6, Edit 1.8 | 28% / 20% | 95.7% |
| pi, repo work | 300 / 36,750 | gpt-6-luna 95%, gpt-6.1-sol, muse-spark | 1.29 | 12.3% | 23 / 106 | 91%, 99.6% | bash 45, read 35, jobs 9, edit 6.5 | 46% / about 54% (approximate) | 95.9% |
| pi, automated reviews | 300 / 7,494 | muse-spark, gpt-6-luna, deepseek, gpt-6.1-sol | 1.49 | 19.9% | 11 / 25 | 98%, 99.7% | bash 56, read 37, edit 3.7 | 47% / about 60% (approximate) | 94.0% |

Other facts:
- Tool mix: no MCP tool or `codemode` call in the top ten tools of any segment. Zero of 10,227 pi session files contain a `codemode` call or an `mcp__` tool call. Bash was 49% read utilities and 12% read-only git in interactive Claude Code; 30% read utilities, 15% `gh`, 14% git writes in pi repo work.
- Latency (pi, from logged durations): model time is 26% of model-plus-tool time (529,000 s versus 1,480,000 s over the repo-work sample); the rest is tool time (builds, tests, waits). Mean model time per step was about 14 s on gpt-6-luna at xhigh or high thinking.

Implied saving. I removed the interior steps of runs under three assumptions and report the share of total cost they carried. These are gross figures: they ignore the cost of writing the script (output tokens) and any dependent step the model needed to see, so they are upper bounds on step-removal saving.

| Scenario | Claude Code interactive | Claude Code bg | Claude Code headless | pi repo work | pi automated |
|---|---|---|---|---|---|
| Collapse consecutive read-only steps, run of 2 or more | 3.1% | 4.6% | 13.2% | 14.4% | 28.9% |
| Same, runs of 4 or more | 1.1% | 1.4% | 7.1% | 7.3% | 18.6% |
| Same, runs of 6 or more | 0 | 0.5% | 2.6% | 3.6% | 7.3% |
| Consecutive "quiet" steps (200 or fewer output tokens), run of 2 or more | 6.4% | 8.5% | 6.3% | 33% (unreliable: pi may not log reasoning tokens in `output`) | 40% (unreliable) |
| Ceiling: collapse every tool run to one step | 49% | 66% | 85% | 94% | 85% |

The ceiling is not credible: the model needs the intermediate results for most edit, build and test cycles. Read-only bash is a strict classifier (git read, `gh` view, grep, cat and similar).

What this implies:
- Claude Code: a plausible step-removal saving of 3% to 8% of cost. Add some saving from filtering tool output: tool results are 15% to 20% of cost; even if a third of that is filterable, that is another 3% to 7%. Total about 5% to 12% at best, minus script-writing output.
- pi on gpt-6-luna: 7% to 15% of a small bill. The 300 repo-work sessions cost $105 in total; the pi sample is dominated by a cheap model, so the absolute saving is small.
- Latency: removing 15% of steps removes about 4% of wall time (26% model share), more if tool time is concurrent. Factory's single-session 27% saving fits a short exploratory fix with a fast tool.
- The owner's runs are long because the work is sequential, not because it is batchable. Factory itself says the savings concentrate in the 12.5% of scripts with six or more calls. The data also shows the largest share of read-only chains is short (2 to 3 steps), where code mode is weakest.
- Parallel tool calls are already used: 9% to 20% of tool-using steps carry more than one call. The study does not tell me how many extra parallel calls are possible.
- MCP is the case where code mode helps most (definition savings, fan-out), and the owner uses no MCP tools, so those gains are not on the table for the owner today.

## 6. Design implications for Fiber

(For a build under #445. `docs/extensions.md` "Running a tool" already makes a codemode extension a user of `host.tool`.)

- Lua versus JavaScript. Every reference runs JavaScript (pi QuickJS, codex V8, opencode own JS-subset interpreter, Factory, OpenClaw) except Anthropic's API, which is Python. The only argument for JavaScript is a hypothesis (Cloudflare's training-data claim) and the arXiv paper's Python results; nobody I found measured Lua. Lua is the zero-dependency choice that fits the embedded runtime and `docs/performance.md`; a JS engine is a binary-size and RSS cost (pi gives each script a 256 MB VM and caps two workers). If Lua, expect lower first-try script quality and measure it. Whichever language: take raw source as the tool input, not JSON, because newline-in-JSON escaping broke multi-line scripts on GPT-4o, GPT-4.1 and GPT-5.4-mini (arXiv 2608.06370); pi and codex both do this.
- Result shape. Return the script's own output only, with a status line, wall time, an output cap that keeps start and end and spills the full text to a file path, and partial output on failure. Always list the inner calls that completed and the ones that did not, because side effects are not undone (the codex #49908 gap). `host.tool` already returns `status`, `content`, `reason`/`error` as an ordinary value; keep that so scripts can `allSettled`. Give scripts structured results where tools have schemas; pi passes `structuredContent` to scripts and text to the model.
- Inner-call display. Show each inner call as a child row under the outer call with its own elapsed time, collapsed by default to a count and expanded on demand (Factory timeline rows; pi `nestedCalls` capped at 256 calls). Record inner calls in the event log with the outer call id (codex #57466, #55926 show the cost of not doing so). Use the real tools' renderers (pi #10662).
- Permission review. Fiber's rule (admission is the outer call; the script is shown via `executes`; inner calls skip standing asks but still hit credential deny, standing denies and `before_tool`/`after_tool`) matches pi and OpenClaw in keeping hooks on inner calls, and it avoids per-call prompts. The recorded risks are: the reviewer reads code rather than a named call (ContextBolt); hook bypass was a real codex bug (#23411), so a test must exercise a hook on an inner call; and an approval covers a whole script's side effects. Consider limiting a script's inner tools to those whose effects the outer approval named, and show a summary of which tools the script mentions (pi's `reachesTools` does this for MCP wait-for-server).
- Cache. Keep the codemode tool description constant for the session: list nothing that changes when MCP servers connect (pi defers MCP tools out of the description for this reason) and never re-declare tools mid-session (pi #11114, #11529). Fiber's cache rule already forbids it.
- Where to expose first. Only for deferred or MCP tools and fan-out of 6 or more calls; keep built-in `read`/`bash`/`edit` as direct tools, since the owner's chains are short, dependent, and already cached.
- Before building: instrument Fiber's own traffic for runs of read-only steps with 4 or more calls and for tool output over a threshold, then compare against the Factory 12.5% figure. Cheap to compute from the existing event log, and it answers the question for Fiber's real users.

## 7. Sources

- Factory: https://factory.com/news/frontier-harness (fetched through a summarising tool; numbers quoted from that summary and one verification fetch)
- pi installed docs: `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/docs/codemode.md`, `docs/mcp.md`, `docs/settings.md`, `docs/extensions.md`, `docs/cli.md`, `CHANGELOG.md`; code `dist/extensions/codemode/tool.js`, `dist/core/nested-tool-calls.js`
- pi repo issues: https://github.com/earendil-works/pi/issues/11114, /11529, /11350, /10426, /10662
- Earendil, "You said no MCP": https://earendil.com/posts/you-said-no-mcp/
- Ronacher, "What is Codemode": https://lucumr.pocoo.org/2026/10/6/codemode/
- withone.ai measurement on pi: https://www.withone.ai/blog/pi-codemode-one-mcp-server/md
- codex: https://github.com/openai/codex (commit 2f452e74b2dfd7e927f90d8b1d573beae521b9c8), files `codex-rs/code-mode-protocol/src/description.rs`, `codex-rs/features/src/lib.rs`, `codex-rs/core/src/tools/mod.rs`, `codex-rs/core/src/tools/code_mode/`; issues #23411, #49908, #55926, #57466, #53091, #41507, #58137
- opencode: https://opencode.ai/v2/docs/mcp-servers/ ; https://github.com/anomalyco/opencode/blob/v2/packages/codemode/README.md and `AGENTS.md`
- OpenClaw: https://docs.openclaw.ai/tools/code-mode.md
- Anthropic: https://www.anthropic.com/engineering/code-execution-with-mcp ; https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling
- Cloudflare: https://blog.cloudflare.com/code-mode/ ; https://blog.cloudflare.com/code-mode-mcp/ ; WorkOS: https://workos.com/blog/cloudflare-code-mode-cuts-token-usage-by-81
- arXiv 2608.06370 (The Bitter Lesson of Tool Calling): https://arxiv.org/abs/2608.06370 and https://arxiv.org/html/2608.06370v1
- AIMultiple: https://research.aimultiple.com/code-execution-with-mcp
- Speakeasy: https://www.speakeasy.com/blog/how-we-reduced-token-usage-by-100x-dynamic-toolsets-v2
- Arize: https://arize.com/blog/code-mode/ ; ContextBolt: https://contextbolt.com/blog/code-mode-mcp/
- Fiber: `docs/extensions.md` "Running a tool" (lines 526-560), issue #445
- Owner usage: aggregates from a local script over the owner's session logs (not included; it reads local session paths)
