# How three reference coding agents build their system prompt

Primary-source research only. Every claim below is tied to a specific file/line
in one of the sources listed at the end. No prose from any agent's actual
prompt text is reproduced beyond short structural fragments (under ~20 words).

## 1. pi (`@earendil-works/pi-coding-agent`)

### Q1 — Sections and order, system vs user vs developer message

pi builds one structured "system message" with named sections, assembled by
`buildSystemPromptSections()` in
`dist/core/system-prompt.js:67-116`. In order:

1. `preamble` — fixed identity string (or a fully custom prompt, replacing
   everything else, if `customPrompt`/`SYSTEM.md` is set).
2. `tools` — one line per enabled tool from `toolSnippets`.
3. `rules` — deduped guideline bullets (tool guidelines + prompt guidelines +
   two hardcoded rules).
4. `docs` — pointers to pi's own README/docs/examples paths (self-referential
   help, only relevant when the user asks about pi itself).
5. `addendum` — the resolved `APPEND_SYSTEM.md` content, if any.
6. `project_context` — one `<project_instructions path="...">` block per
   discovered `AGENTS.md`/`CLAUDE.md` file.
7. `skills` — `<available_skills>` XML block (see Q7).
8. `cwd` — the working directory, as its own section.
9. any caller-supplied custom sections.

Everything after `preamble` is wrapped as `<section_name>...</section_name>`
(`system-prompt.js:110-115`). There is **no separate developer message and no
first-user-message env block** — pi sends exactly one system message; cwd is
the only "environment" fact in it, and there is no date, platform, shell, git
status, or model-name section anywhere in `system-prompt.js` or
`agent-session.js` (confirmed by grep: zero hits for `process.platform`,
`os.platform`, "Today"/"Current date", or git status/branch in
`dist/core/system-prompt.js` and `dist/core/agent-session.js`).

### Q2 — Project instruction files

- Filenames tried per directory, in this precedence order:
  `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, `CLAUDE.MD`
  (`dist/core/resource-loader.js:33`, first match wins per directory).
- Directories searched: the agent directory (global, `<agent-dir>` = default
  `~/.pi/agent`) plus the cwd and **every parent directory up to the
  filesystem root** (`loadProjectContextFiles`, `resource-loader.js:82-109`;
  the loop only stops when `dirname(currentDir) === currentDir`, i.e.
  filesystem root — it does not stop at a git root).
- A linked worktree's own context file is de-duplicated against the main
  repo's copy of the same filename via `findShadowedContextFile`
  (`resource-loader.js:62-81`), to avoid applying the same file twice.
- Merge order: global file first, then ancestor-to-cwd context files
  concatenated as separate `<project_instructions>` blocks (not merged text) —
  `system-prompt.js:24-29`. No numeric byte/size cap exists for context files
  in this loader (unlike Codex).
- `docs/configuration.md:39-45`: "Context files are separate from project
  `.pi` configuration... A context file applies whenever Pi runs in its
  directory or anywhere below it," and discovery "does not require project
  trust."
- Lazy subdirectory loading: not supported — all context files from agent-dir
  down through cwd are loaded eagerly at startup/reload; nothing in
  `resource-loader.js` loads a file only when the model touches a file under
  it.
- `SYSTEM.md` / `APPEND_SYSTEM.md`: project (`.pi/SYSTEM.md`,
  `.pi/APPEND_SYSTEM.md`, only if project is trusted) takes precedence over
  the corresponding agent-dir file; same-named files are not combined
  (`docs/configuration.md:37`, `discoverSystemPromptFile`/
  `discoverAppendSystemPromptFile`, `resource-loader.js:813-834`).

### Q3 — Environment block

Only `cwd` (as `<cwd>...</cwd>`). No date, no platform/OS, no git
branch/status, no shell, no model name anywhere in the system prompt
(verified by grep across `dist/core` and `dist/modes`; `process.platform`
checks that exist are all UI/keybinding/sandbox logic, not prompt content).

### Q4 — Mid-session changes

No automatic re-read. `docs/configuration.md:5`: "Run `/reload` after manually
changing settings, keybindings, instructions, or resources." The only file
watchers found in `dist/modes/interactive/interactive-mode.js` are for themes
and the git-branch footer widget, not context files. `_rebuildSystemPrompt`
(`dist/core/agent-session.js:991`) is called on tool-set changes, not on a
timer or file watcher for `AGENTS.md`/`CLAUDE.md`.

### Q5 — Per-model/provider variants

None found. `buildSystemPromptSections` takes no model/provider parameter;
grep for model-family branching in `system-prompt.js`/`agent-session.js`
returned nothing.

### Q6 — User/extension override mechanisms

- `SYSTEM.md` (agent-dir or `.pi/`) fully replaces the default prompt
  (`customPrompt` branch, `system-prompt.js:76-79`).
- `APPEND_SYSTEM.md` (agent-dir or `.pi/`) adds an `addendum` section.
- CLI/SDK: `systemPrompt` / `appendSystemPrompt` options, plus
  `systemPromptOverride` / `appendSystemPromptOverride` functions exposed to
  extensions (`resource-loader.js:126-134, 382-396`).
- `forceSystemPrompt` bypasses the structured-sections builder entirely and
  ships opaque text (`system-prompt.js:121-124`).

### Q7 — Skills listing

`formatSkillsForPrompt()` (`dist/core/skills.js:275-298`) emits, inside the
`skills` section: an intro sentence, a resolution-path instruction, then
`<available_skills>` containing one `<skill><name>/<description>/<location>`
per non-`disable-model-invocation` skill. `docs/skills.md`: "Pi scans
configured skill locations and adds each skill's name, description, and path
to the system prompt. It does not add the full instructions" — full
`SKILL.md` is read on demand by the model.

---

## 2. Codex (build `0.155.1`, `~/.codex/packages/standalone/current/bin/codex`;
cross-checked against a fresh sparse clone of `openai/codex` `codex-rs`)

### Q1 — Sections and order, system vs user vs developer message

Codex has no monolithic system prompt string. It builds a **"world state"** of
independently-diffed sections (`codex-rs/core/src/context/world_state/mod.rs`)
and injects each section as its own message with its own `role`
(`"developer"` or `"user"`) only when it changes turn-to-turn. Base
instructions are delivered as a **separate role="developer" message**
(`BaseInstructionsFragment`, `role()` → `"developer"`,
`requires_separate_message()` → `true`,
`core/src/context/base_instructions.rs:12-18`) — there is no "system" role
concept in the request at all; everything is developer- or user-scoped.

`build_world_state_for_step` (`core/src/session/world_state.rs:116-330`) adds
sections in this order: `ModelInstructionsState` (per-model base
instructions, Q5) → `TokenBudgetContext` → `ContextWindowGuidanceState` →
`RealtimeState` → `AgentsMdState` (AGENTS.md / host user-instructions, role
`"user"`) → `PermissionsState` → `CompactPermissionsState` →
`CollaborationModeState` → `PersistentModeState` → `EnvironmentsState` (cwd,
shell, date, timezone, network, filesystem, subagents — Q3, role `"user"`) →
`EnvironmentsInstructionsState` → `AppsInstructionsState` →
`PluginsInstructionsState` → `ToolsState` → `MultiAgentUsageHintState` →
`MultiAgentModeState` → `ManagedDeveloperInstructionsState`.

### Q2 — Project instruction files

- Filenames, in order: `AGENTS.override.md`, `AGENTS.md`, then each entry of
  the configured `project_doc_fallback_filenames` list (comment header,
  `core/src/agents_md.rs:1-18`; `candidate_filenames`, `agents_md.rs:271-294`).
- Project root found by walking up from cwd until a
  `project_root_markers` entry is found (default `[".git"]`); an empty marker
  list disables traversal (`agents_md.rs:11-14`, `default_project_root_markers`).
- Files collected **from the project root down to cwd, inclusive**, and
  concatenated in that root-to-leaf order (`agents_md.rs:15-17, 200-224`) — the
  opposite convention from pi's "no walk past project root" but same
  direction (root first).
- Untrusted project: project docs are skipped entirely; only host-supplied
  `user_instructions` still load (`load_project_instructions`,
  `agents_md.rs:63-67`).
- Size cap: `project_doc_max_bytes`, a hard byte budget across **all**
  discovered files combined; content is truncated once the remaining budget
  hits 0, with a `tracing::warn!("project doc exceeds remaining budget;
  truncating")` (`agents_md.rs:129-179` — this exact string is also present
  in the shipped binary's `strings` output).
- No lazy subdirectory loading: all AGENTS.md files from root to cwd are read
  eagerly per turn/environment, not on file touch.
- User/thread-scoped `Instructions` (host-supplied, e.g. CLI `-i`/API field)
  are concatenated ahead of file-sourced entries, separated by
  `\n\n--- project-doc ---\n\n` on the first transition from
  user/internal to project-sourced text (`AGENTS_MD_SEPARATOR`,
  `agents_md.rs:47-49, 405-421`).
- With more than one attached environment, each environment's files are
  labeled `for \`{environment_id}\` with root {cwd}` instead of using the
  legacy unlabeled concatenation (`environment_labeled_text`,
  `agents_md.rs:427-464`).

### Q3 — Environment block

`EnvironmentsState` (`core/src/context/world_state/environment.rs:28-38`)
carries, per environment: `cwd`, `status`, `shell` (name only), `error`; plus
session-wide `shell_version` (PowerShell only, gated by a feature flag),
`current_date`, `timezone`, `network` (allowed/denied domains), `filesystem`
(permission-profile-derived read/write roots), `subagents`. Rendered as
`<cwd>`, `<shell>`, `<shell_version>`, `<current_date>`, `<timezone>`, a
`<network_access .../>`-style element, and a filesystem element
(`push_environment_values`/`RenderedEnvironments::body`,
`environment.rs:220-322`). `current_date` is **date-only**
(`format("%Y-%m-%d")`, `core/src/session/turn_context.rs:833-835`), not
date+time. **No git status or branch appears anywhere in
`core/src/context/world_state/`** (grep for `git_status`/`GitStatus`/branch
in that tree returns nothing) — this is a Codex-specific negative finding,
not a general one. Role for this fragment is `"user"`
(`environment.rs:203-205`). A *separate*, feature-gated
`CurrentTimeReminder` fragment (`core/src/context/current_time_reminder.rs`)
can inject a full `%Y-%m-%d %H:%M:%S UTC` string as its own
`role="developer"` message — a different mechanism from the date-only
`environments` section.

### Q4 — Mid-session changes

Everything is diff-based by construction (`WorldStateSection::render_diff`,
`world_state/mod.rs`). Concretely for AGENTS.md:
`AgentsMdState::render_diff` (`world_state/agents_md.rs:50-77`) emits nothing
if unchanged; on change it prepends a literal "These AGENTS.md instructions
replace all previously provided AGENTS.md instructions" notice (confirmed
also in the shipped binary's strings), or, if the files disappeared, "The
previously provided AGENTS.md instructions no longer apply." The environment
block does the same for date/shell/network/filesystem changes
(`current_date_removed`, `shell_version_removed` flags,
`environment.rs:145-196`), including a `<current_date status="unavailable"
/>` marker when the clock read fails. Model-instruction changes on a
mid-session model switch re-send instructions via `ModelSwitchInstructions`
(role `"developer"`) — `world_state/model.rs:44-60`.

### Q5 — Per-model/provider variants

Confirmed and explicit. `render_model_instructions()`
(`codex-rs/prompts/src/model_instructions.rs:8-17`) pulls
`instructions_template` from the **model catalog entry** for the active
model (`ResolvedModelMessages::from_model`, `model_messages.rs:70-87`); a
model with no catalog template gets empty base instructions plus a warning.
The repo ships distinct human-readable reference copies per model family at
`codex-rs/core/{gpt_5_1_prompt.md, gpt_5_2_prompt.md, gpt_5_codex_prompt.md,
gpt-5.1-codex-max_prompt.md, gpt-5.2-codex_prompt.md}` (68–331 lines each,
confirmed distinct file names via `find`). Config also supports a global
override (`base_instructions` / `model_instructions_file`, below) that
bypasses the per-model catalog value entirely.

### Q6 — User/extension override mechanisms

`config.toml` keys, resolved in this precedence
(`core/src/config/mod.rs:3980-3996`): explicit `base_instructions` string >
`model_instructions_file` (path, read as a file) > legacy `instructions` key
> model-catalog default. `developer_instructions` is a distinct override
"injected as a separate message" (doc comment, `config/mod.rs:688-689`) — the
append-style analogue to pi's `APPEND_SYSTEM.md`, but delivered as its own
developer-role message rather than concatenated text. Additional toggle keys
gate whole instruction families on/off: `include_permissions_instructions`,
`include_apps_instructions`, `include_collaboration_mode_instructions`,
`include_environment_context` (all `bool`, default `true`,
`config/mod.rs:3997-3999+`).

### Q7 — Skills listing

`render_available_skills_body()` (`codex-rs/ext/skills/src/catalog_prompt.rs:
81-106`) emits a `## Skills` heading, an intro line (one of three variants
depending on whether skills use host paths, resource locators, or aliases),
an optional `### Skill roots` alias table, then `### Available skills` with
one line per skill: `- {name}: {description} ({locator_kind}: {locator})`
(`render.rs:263-265`). Usage rules (trigger conditions, progressive
disclosure, "read `SKILL.md` completely before acting") are a fixed block
appended after the list, also defined in `catalog_prompt.rs`.

---

## 3. Claude Code (`~/.local/share/claude/versions/2.1.283`, via `strings`)

### Q1 — Sections and order, system vs user vs developer message

Three canonical, mutually-exclusive base-identity strings exist as constants
(`Dpe`, `E2e`, `C2e` in the deobfuscated bundle) and are selected by `k4()`
based on execution context, not model:

- `"You are Claude Code, Anthropic's official CLI for Claude."` — default
  interactive CLI (also forced for the `vertex` provider regardless of
  context).
- `"You are Claude Code, Anthropic's official CLI for Claude, running within
  the Claude Agent SDK."` — non-interactive SDK use **with** an
  append-system-prompt configured.
- `"You are a Claude agent, built on Anthropic's Claude Agent SDK."` —
  non-interactive SDK use with no append-system-prompt (this is the exact
  string present in this run's own system prompt, since this session is a
  non-interactive subagent invocation).

Beyond that identity line, the system message is assembled by joining
cacheable blocks (`x2e`/`ei()` in the bundle) that include a fingerprint/
attribution header block, the identity string, and other fixed blocks; one
block is explicitly tagged `cacheScope:"org"` for managed/org-wide content,
separately cacheable from the rest (`Lpe()`). Skills listing (Q7) is baked
into this same system message and is present "every turn"
(`"context = this skill's one-line listing in the system prompt, included
every turn"`).

Everything environment/state-shaped — working directory, platform, shell, OS
version, date, model identity/knowledge-cutoff, git status, user email,
project/account context, output style, language preference, and
CLAUDE.md/AGENTS.md instruction content — is **not** in the system message.
Each is its own typed "attachment" injected into the conversation (visible as
`<system-reminder>`-wrapped text in a turn), built by dedicated
render/diff functions (`m2e`, `g2e`, `y2e`, etc., all in the same bundle
region). This matches exactly what this very session's own context shows:
separate `# Environment` and `# userEmail`/`# gitStatus` blocks, each with
its own "IMPORTANT, this may not be relevant" framing, distinct from the
model-identity sentence ("You are powered by the model named Sonnet 5...").

### Q2 — Project instruction files

- File types recognized, with an explicit precedence/type tag: `User`,
  `Project`, `Local`, `Managed`, `AutoMem`, plus `AutoMemPinned`
  (array `JY = ["User","Project","Local","Managed","AutoMem"]`, schema
  `Ynn` with `type` enum defaulting to `"User"`). This corresponds to
  `~/.claude/CLAUDE.md` (User), project `CLAUDE.md` (Project),
  `CLAUDE.local.md` (Local), an org-pushed managed-policy CLAUDE.md
  (Managed), and generated memory files (AutoMem/AutoMemPinned, rendered
  under a `# Pinned memories (apply to every conversation)` heading for the
  pinned variant).
- Discovery scope, per the built-in `/doctor prompt-audit` subcommand's own
  description of what it audits: "CLAUDE.md, CLAUDE.local.md and AGENTS.md in
  the project root and its ancestor and nested directories, and the
  instruction files they import"; also `.claude/CLAUDE.md`,
  `.claude/AGENTS.md`, and `~/.claude/CLAUDE.md`. So discovery walks **both**
  ancestors (up) and nested/descendant directories (down) from the project
  root, and CLAUDE.md/AGENTS.md files can `@import` other files.
- Precedence/scope note baked into the same audit description: files under
  `~/.claude` apply to every project ("mark any edit proposed there as
  affecting all projects"), and a conflict between a global (`~/.claude` or
  ancestor) file and a project file is flagged rather than resolved
  automatically.
- Lazy subdirectory loading: the built-in `/doctor` command's own
  description recommends "migrate always-loaded CLAUDE.md guidance into lazy
  skills and nested CLAUDE.md files" — i.e. the root/ancestor CLAUDE.md is
  always-loaded, while nested CLAUDE.md files are the lazy-loading
  mechanism, consistent with per-directory lazy loading as the codebase
  touches files there.
- Size cap: no CLAUDE.md-specific byte cap string was found; the only
  concrete content-size cap found is for **skills**: "SKILL.md is larger
  than the 1 MB Claude Code loads for a skill."
- Rendering: unchanged instruction sets are emitted as a plain list of
  `{path, type, content}` entries; a changed set instead renders "Instruction
  files were re-read {reason}; these differ from their earlier copies" (or,
  if none remain, "these values replace the earlier ones") plus, per removed
  file, "Instructions no longer present: {path}" (`y2e()`).
- The instructions block is preceded by a fixed preamble: "Codebase and user
  instructions are shown below. Be sure to adhere to these instructions.
  IMPORTANT: These instructions OVERRIDE any default behavior and you MUST
  follow them exactly as written." — the literal text prefixing CLAUDE.md
  content in this very transcript.

### Q3 — Environment block

A dedicated zod-validated `environment` attachment schema (`Cpe`) carries:
`workingDirectory`, `isWorktree` (bool), `isGitRepo` (bool),
`additionalWorkingDirectories`, `platform`, `shell`, `osVersion`,
`scratchpadDirectory`. Header text: `"# Environment"` /
`"You have been invoked in the following environment: "` — identical to the
`# Environment` block shown at the top of this session. **No git branch and
no git status live in this schema** — git status/branch/`perforceMode` are a
*different* attachment type, `session_context`
(`$7t = ["userEmail","attachedProject","gitStatus","perforceMode"]`), each
rendered as its own `"# {key}\n{value}"` block under the shared preamble "As
you answer the user's questions, you can use the following context:" plus an
"IMPORTANT... may or may not be relevant... snapshot in time" disclaimer —
again, the exact wording present verbatim in this session's own context,
confirming git status is explicitly snapshotted, not live. Date is yet a
third, separate attachment type (`date`, schema `hH = {date, changed?}`),
date-only, no time component. Model identity/knowledge-cutoff is a fourth,
separate attachment type (`model`), rendered as "You are powered by the
model named {marketingName}. The exact model ID is {modelId}." + "Assistant
knowledge cutoff is {knowledgeCutoff}." — verbatim what this session's own
context shows.

### Q4 — Mid-session changes

All four attachment types (`environment`, `date`, `session_context`,
`instructions`) are explicitly diffed and re-announced, each with a
human-readable reason drawn from a shared enum
`QY = ["session_start","compaction","policy_refresh","directory_added",
"settings_sync","account_change","hooks_invalidate","policy_verdict"]`
(mapped to prose by `_O()`, e.g. `"policy_refresh"` → "after the
organization's managed settings changed"). Concrete re-announcement text
found: for date, "The date has changed. Today's date is now {date}. No need
to announce the new date — the user's own clock shows it."; for environment,
"# Environment update" plus specific worktree-loss text ("The primary
working directory announced earlier no longer applies."); for
instructions/CLAUDE.md, the replace/remove notices in Q2; for skills, "New
skills discovered in {dir}, now available via the Skill tool:" — confirming
Claude Code file-watches skill directories and injects newly-found skills
mid-session. Output style and language-preference changes get their own
dedicated re-announcement blocks (`# Output Style: {name}\n{prompt}`, "The
output style was reset to the default...", "# Language\nAlways respond in
{lang}...").

### Q5 — Per-model/provider variants

The three base-identity strings (Q1) vary by **execution
context/provider** (`vertex` forces the CLI string; interactive vs.
non-interactive SDK; append-system-prompt present or not) — not by which
model is selected. No per-model-family prompt branching was found (unlike
Codex's per-model catalog templates).

### Q6 — User/extension override mechanisms

- Managed/org policy: a distinct, separately-cacheable (`cacheScope:"org"`)
  system-message block for organization-pushed instructions, which "apply
  even if the user's instructions say otherwise" (attribution-reminder logic
  explicitly special-cases a `managed` vs `user`-settable line).
- `CLAUDE.md`/`CLAUDE.local.md`/`AGENTS.md` at User/Project/Local scope, as
  in Q2, are the append-style mechanism (there is no separate
  `SYSTEM.md`/`APPEND_SYSTEM.md` file pair as in pi/Codex — instructions
  files are the only lever).
- Output style (`# Output Style: {name}`) and language preference are
  runtime-settable prompt-replacing/appending attachments, not files.
- `--append-system-prompt` (SDK/CLI, inferred from the `hasAppendSystemPrompt`
  branch selecting the `E2e` identity string) and custom subagent
  definitions (`agents/`) each carry their own instruction text merged in by
  the harness.
- Attribution-line policy itself is a small append-style block controlled by
  a system-reminder the harness inserts (the block quoted at the top of this
  very conversation).

### Q7 — Skills listing

Header: `"The following skills are available for use with the Skill tool:"`
— present verbatim in this session's own context. Each entry is "this
skill's one-line listing in the system prompt, included every turn" (name +
one-line description); the dash marker in `/doctor`'s skill-usage table
means "not in the current listing, costs nothing" — i.e. skills can be
excluded from the per-turn listing (e.g. `disable-model-invocation`-style
skills) while still being invocable by name, mirroring pi's model. Full
`SKILL.md` content loads only when the skill actually runs, capped at 1 MB
(Q2). New skill files discovered during a session are appended to the
listing live (Q4).

---

## Cross-cutting comparison

| | pi | Codex | Claude Code |
|---|---|---|---|
| System-prompt shape | one structured system message, named sections | no system role; base instructions as a separate `developer` message, everything else as diffed `user`/`developer` fragments | one system message (identity + org block + tools + skills) + many typed "attachment" messages |
| Env facts in the *system* message | cwd only | none (env is a `user`-role world-state section) | none (env is its own attachment type) |
| Git status/branch | absent everywhere | absent everywhere in `world_state/` | present, but as a separate, explicitly *snapshotted* `session_context` attachment, not the env block |
| Date | absent | date-only, in the `environments` section; separate full-timestamp `developer` reminder also exists | date-only, its own attachment type |
| Mid-session file/date change | nothing automatic; `/reload` required | re-diffed and re-sent every turn automatically, with explicit "replace"/"no longer applies" notices | re-diffed and re-sent automatically on 8 named triggers, with explicit "replace"/"no longer applies" notices |
| Per-model prompt variants | none found | yes — base instructions come from the model catalog (`instructions_template`), with distinct reference prompts per model family | none found — variant selection is by execution context/provider, not model |
| Skills listing | `<available_skills>` XML, name+description+path, in the `skills` system-prompt section | `### Available skills`, `- name: description (locator)`, Markdown, in a world-state section | plain list in the system message, name+one-line description, "included every turn" |

## Sources

**pi** (`/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/`):
- `dist/core/resource-loader.js` (context-file discovery, SYSTEM.md/APPEND_SYSTEM.md resolution)
- `dist/core/system-prompt.js` (section assembly, ordering)
- `dist/core/skills.js` (`formatSkillsForPrompt`)
- `dist/core/agent-session.js` (`_rebuildSystemPrompt`, no auto file-watch for context files)
- `docs/configuration.md`, `docs/skills.md`, `docs/how-pi-works.md`

**Codex** (binary: `~/.codex/packages/standalone/current/bin/codex`, version
`0.155.1`, via `strings -n 30`; source: fresh sparse clone of
`github.com/openai/codex` at `/private/tmp/claude-501/codex-src/codex-rs`):
- `codex-rs/core/src/agents_md.rs` (AGENTS.md discovery, root-marker walk, byte budget, separators)
- `codex-rs/core/src/context/base_instructions.rs` (developer-role, separate-message base instructions)
- `codex-rs/core/src/context/world_state/environment.rs` (environment_context fields and rendering)
- `codex-rs/core/src/context/world_state/model.rs`, `agents_md.rs` (mid-session diff/replace notices)
- `codex-rs/core/src/context/current_time_reminder.rs`
- `codex-rs/core/src/session/world_state.rs` (section ordering, `include_environment_context` gate)
- `codex-rs/core/src/session/turn_context.rs` (date formatting)
- `codex-rs/core/src/config/mod.rs` (`base_instructions`, `model_instructions_file`, `developer_instructions`, `include_*` keys)
- `codex-rs/prompts/src/model_instructions.rs`, `model_messages.rs` (per-model catalog templates)
- `codex-rs/core/{gpt_5_1_prompt.md,gpt_5_2_prompt.md,gpt_5_codex_prompt.md,gpt-5.1-codex-max_prompt.md,gpt-5.2-codex_prompt.md}` (per-model reference prompts)
- `codex-rs/ext/skills/src/catalog_prompt.rs`, `render.rs` (skills listing format)

**Claude Code** (binary: `~/.local/share/claude/versions/2.1.283`, via
`strings -n 15`):
- Identity-string selection: constants `Dpe`/`E2e`/`C2e` and selector `k4()`
- Environment attachment: schema `Cpe` (`workingDirectory, isWorktree, isGitRepo, additionalWorkingDirectories, platform, shell, osVersion, scratchpadDirectory`), headers `"# Environment"` / `"# Environment update"`
- Session-context attachment: `$7t = ["userEmail","attachedProject","gitStatus","perforceMode"]`, renderer `m2e()`
- Date attachment: schema `hH`, renderer `g2e()`
- Model-identity attachment: renderers `Lnn()`/`Nnn()`/`s2e()`
- Instructions/CLAUDE.md attachment: type list `JY = ["User","Project","Local","Managed","AutoMem"]`, renderer `y2e()`, preamble string at strings-offset ~174544
- Re-read trigger reasons: `QY` enum + `_O()` mapper
- Skills listing header and per-turn-listing semantics: strings around offsets 110783/128546/154625
- Built-in `/doctor prompt-audit` subcommand description (discovery scope, ancestor+nested walk, import files, lazy nested CLAUDE.md guidance)
