# Configuration

What a person sets to change Fiber's defaults, where it is written, and which
value wins. It also covers the format of a provider's data and of an
extension's manifest. Vocabulary is `GLOSSARY.md`. Where Fiber home is, and how
its files are written safely, is [Fiber home](state.md).

## Files and format

Every file Fiber reads as configuration is JSON: configuration, an extension's
settings, an extension's manifest and a provider's data. The parser is
serde_json, which Fiber already uses for everything else
(`docs/dependencies.md`), so configuration adds no crate. pi, Claude Code and
the archived Zig tree all use JSON. codex uses TOML, which would have added 6
crates and 167 KiB of stripped binary on macOS arm64 (`toml` 1.1 against
serde_json alone, measured on September 25, 2026).

The JSON is strict: no comments and no trailing commas. Fiber writes to these
files itself, as described in "When Fiber writes", so a comment would not
survive anyway.

Reading configuration never runs code. A repository's configuration is read
before anyone has approved anything, so Lua is not a configuration format. The
only Lua a provider runs is its optional `models()`, `quota()`,
`credential()` and `sign()` functions (`docs/model-routing.md`, "Model
discovery").

Configuration never holds a secret. Secrets live in `credentials/`
("Secrets").

## Layers

Configuration comes in layers. A later layer wins over an earlier one, key by
key:

1. built-in defaults
2. global: `config.json` at the top of Fiber home
3. repository: `.fiber/config.json` in the workspace
4. per project: `projects/<key>/config.json` in Fiber home
5. per run: `-c key=value` on the command line

`~/.fiber/config.json` sets `"model": "openrouter/anthropic/claude-sonnet-5"`.
A repository's `.fiber/config.json` sets `"model":
"databricks/databricks-claude-opus-5"`. Sessions in that repository use the
Databricks model, unless the person's `projects/<key>/config.json` sets
another, or they pass `-c model=...` for one run.

Objects merge key by key, so a layer changes only the keys it names. Any other
value, a list included, replaces the one below it. A provider's `credential` replaces
the one below it as a whole: it does not merge key by key.

The per-project file is the person's own setting for one project. It lives in
Fiber home, not the repository, so it covers every worktree of the project
(`docs/state.md`, "Projects"), and a delegate's fresh worktree has it too. A
gitignored file in the working tree, as Claude Code uses, would be missing from
every new worktree.

`-c` works on both doors: `fiber ask`, and the terminal, which passes it to
each session it asks the hub to start.
The key is a dotted path and the value is JSON, or a bare string when it does
not parse as JSON: `-c handoff.tokens=200000`, `-c model=openai/gpt-5.6`. It
may be given more than once. A named flag such as `--model` is shorthand for
the same thing. `FIBER_HOME` is the only environment variable Fiber reads for
itself; no environment variable overrides a key.

## What a repository may set

A repository may set a key only when the worst a hostile value can do is cost
the person time or money. It never sets a key that widens what runs without
asking, changes where requests go, or weakens a safety check. A cloned
repository is someone else's text, and it is read before anything is approved.

Each key says whether a repository may set it. A key that says nothing is
person-only, so a key added without thought stays safe. A repository that sets
a person-only key gets a `notice` naming the key and the file, and the value
is ignored.

The table in "Keys" marks each key. Two rules settled elsewhere follow from
this one:

- A repository cannot declare a provider or change a base URL
  (`docs/model-routing.md`, "Choosing the model").
- A repository can declare MCP servers, which start only after a person
  approves them (`docs/mcp.md`, "A repository's servers"). It cannot declare
  or enable an extension (`docs/extensions.md`, "Extensions in a
  repository").

## Keys

Keys are snake_case and grouped by area. In "Repo", yes means a repository may
set the key.

| Key | Default | Repo | Meaning |
|---|---|---|---|
| `model` | none | yes | The default model for a new session, as `provider/model` (`docs/model-routing.md`, "Choosing the model"). |
| `roles."<name>"` | none | yes | A delegate's model reference, such as `"fiber:openai/gpt-5.6:xhigh"` (`docs/delegates.md`). |
| `session.idle_exit_ms` | 1800000 (30 minutes) | no | How long a session stays running with no turn and no jobs, whoever is connected (`docs/invocation.md`, "Lifecycle"). |
| `reviewer.model` | the session's provider's reviewer model | no | The reviewer's model (`docs/permissions.md`, "The reviewer"). |
| `reviewer.block_limits.consecutive` | 3 | no | Consecutive blocks before a person is asked. |
| `reviewer.block_limits.session` | 20 | no | Blocks in a session before a person is asked. |
| `handoff.enabled` | true | yes | Whether automatic handoff runs (`docs/handoff.md`). |
| `handoff.tokens` | 400000 | yes | The token trigger. |
| `handoff.window_fraction` | 0.7 | yes | The trigger as a fraction of the model's context window. |
| `handoff.nudge` | true | yes | Whether the nudge is given. |
| `cache.lifetime` | `"1h"` | yes | The prompt-cache lifetime, `"5m"` or `"1h"` (`docs/prompt-cache.md`). |
| `retry.attempts` | 3 | yes | Retries of a failed model call (`docs/model-routing.md`, "When a model call fails"). |
| `retry.initial_delay_ms` | 2000 | yes | The first backoff, doubling each retry. |
| `retry.max_delay_ms` | 60000 | yes | The cap on one backoff, and on a wait a server asks for. |
| `tools."<name>".max_result_bytes` | the tool's own, or 16384 | yes | The tool's result cap (`docs/tools.md`, "Bounded results"). |
| `tools."<name>".deferred` | the tool's own | yes | Whether the tool is deferred (`docs/tools.md`, "What is deferred by default"). |
| `web_search.backend` | the one installed | no | The search backend `web_search` uses when more than one is installed (`docs/tools.md`, "Web fetch and web search"). |
| `shell.read_only."<command>".flags` | none | no | Adds a command to the shell classifier's read-only list, with the flags it may take and stay read-only, such as `["--json", "-p"]` (`docs/tools.md`, "Search", "Other command-line tools"). |
| `budget.usd` | none | no | The most a session may spend, in US dollars billed per token, its delegates included; unset means no limit (`docs/loop.md`, "Spending budget"). |
| `quota.notice_at` | 80 | yes | The percent used of a quota window at which the model gets a notice (`docs/tools.md`, "Provider quota"). |
| `mcp.servers."<name>"` | none | yes, with approval | An MCP server ("MCP servers"). |
| `extensions."<name>".enabled` | true | no | Whether an installed extension loads; `false` globally and `true` in a project's file scopes it to that project (`docs/extensions.md`, "Extensions in a repository"). |
| `extensions."<name>".startup_timeout_ms` | 5000 | yes | A process extension's startup deadline. |
| `extensions."<name>".commands."<command>"` | none | yes | A new name for one of the extension's commands, when two extensions clash. |
| `extensions."<name>".tools.enabled`, `extensions."<name>".tools.disabled` | none | yes | Lists of the extension's tool names to declare or leave out, as an MCP server's `tools.enabled` and `tools.disabled` do ("MCP servers"); the terminal's `/tools` switch writes them (`docs/tui.md`, "Swapped views"). |
| `extensions."<name>".hook_timeout_ms` | the hook's own | no | Overrides the timeout of every hook the extension registers. |
| `hooks.order."<hook point>"` | none | no | Extension names in the order their hooks run at that point (`docs/extensions.md`, "When several hooks share a point"). |
| `providers."<name>".credential` | the provider's own | no | Where the provider's credential comes from ("Secrets"). |
| `tui.panel.cards` | `["session", "changed_files", "delegates", "jobs", "quota"]` | no | The cards the terminal's panel shows, in order; an extension widget is listed as a card too (`docs/tui.md`, "The panel"). The status line of the narrow layout follows the same order. |
| `tui.theme` | none | no | The theme's name: `dark`, `light` or a theme file in Fiber home. With none, the theme follows the terminal's light or dark appearance (`docs/tui.md`, "Themes"). |
| `tui.reduced_motion` | false | no | Whether every animation takes its still form. On whenever a screen reader is detected (`docs/tui.md`, "Reduced motion"). |
| `tui.screen_reader` | detected | no | Forces the flat screen-reader mode on or off; `--screen-reader` sets it true (`docs/tui.md`, "Screen readers"). |
| `tui.attention.notification` | true | no | Whether the terminal sends an OSC 9 desktop notification when a session starts waiting on the person (`docs/tui.md`, "Getting the person's attention"). |
| `tui.attention.bell` | true | no | Whether it rings the bell where OSC 9 is not supported. |
| `tui.attention.title` | true | no | Whether the terminal title shows the session's state. |
| `tui.hover` | true | no | Whether hover highlights the click target under the pointer; false drops mouse mode 1003 (`docs/tui.md`, "Mouse and hover"). |
| `tui.inline_images` | true | no | Whether images show inline where the terminal speaks kitty's graphics protocol (`docs/tui.md`, "Images"). |
| `tui.logo_glyph` | `"⌇"` | no | The glyph before the name in the logo, `"⌇"` or `"≈"`, for a font without ⌇ (`docs/tui.md`, "The logo"). |
| `tui.slots."<slot>"` | none | no | The extension that fills a slot two extensions replace, such as `tui.slots."ledger_row:shell"`; the same for a key two extensions bind, as `tui.slots."key:ctrl+k"` (`docs/tui.md`, "When two extensions want one slot"). |

Hook order and hook timeouts are person-only because a redaction hook depends
on both. A repository that could move another hook in front of it, or cut its
timeout, could send text out before the secret is removed.

### Per model

Some keys can be set for one model. They sit under `models."provider/model"`
and win over the same key at the top level of the same layer:

```json
{
  "handoff": { "tokens": 400000 },
  "models": {
    "databricks/databricks-claude-opus-5": {
      "handoff": { "window_fraction": 0.5 },
      "cache": { "lifetime": "5m" }
    }
  }
}
```

The keys that may be set per model are `handoff.*` and `cache.lifetime`. A
per-model key may be set by a repository when the top-level key may be.

### MCP servers

Each entry under `mcp.servers` holds what `docs/mcp.md` ("Configuration")
lists:

| Field | Meaning |
|---|---|
| `command`, `args`, `env` | the program of a stdio server |
| `url` | a remote server |
| `required` | whether failing to start ends the session, default false |
| `startup_timeout_ms` | the startup deadline, default 5000 |
| `timeout_ms` | the call timeout |
| `declare_in_full` | whether its tools are declared in full rather than deferred, default false |
| `tools.enabled`, `tools.disabled` | lists of tool names |
| `tools."<tool>".hints` | overrides of that tool's MCP hints |

A repository may declare a server, subject to approval. `tools."<tool>".hints`
is person-only: a repository that marked a tool `readOnlyHint` would skip the
reviewer for it.

## Extension settings

An extension's own settings live in their own file in each layer, never in
`config.json`:

| Layer | File |
|---|---|
| global | `config/<extension>.json` in Fiber home |
| repository | `.fiber/config/<extension>.json` |
| per project | `projects/<key>/config/<extension>.json` in Fiber home |
| per run | `-c extensions."<extension>".settings.<key>=value` |

`<extension>` is slugged as for `extensions/` (`docs/state.md`, "What each part
holds"). The layers merge exactly as Fiber's own keys do.

- `host.config.get(key)` returns the merged value.
- `host.config.set(key, value, scope)` writes one file. `scope` is `"machine"`
  for the global file or `"project"` for the per-project file, the same words
  `host.data_dir` takes. It has no default, so the author decides. A model
  picker can save a choice for one project only.
- A repository sets only the keys the extension's manifest lists under
  `repo_settings`. Fiber cannot judge a key it does not know, but the author
  can, so the author applies the rule in "What a repository may set". Any
  other key in the repository's file gets a `notice` and is ignored.
- `fiber extension remove` deletes the extension's file in the global and every
  per-project layer, asking first in a terminal, as it does for its data
  directories.

A separate file means an extension never rewrites the person's `config.json`.

## Secrets

A secret is a file in `credentials/` in Fiber home, mode 0600:
`credentials/<name>`. Provider credentials are already stored there
(`docs/model-routing.md`, "Credentials"). An extension's secrets share the
same directory and the same names. `host.secret(name)` reads
`credentials/<name>`. By convention, an extension prefixes its names with its
own name, such as `acme.api_key`. `fiber login <name>` stores one.

This is one namespace, not a wall between extensions. An extension runs with
the account's full rights and could read `credentials/` with `host.fs` anyway
(`docs/extensions.md`, "Lua extensions"). The credential deny protects the
directory from tool calls, not from extensions
(`docs/permissions.md`, "Credentials").

A provider's data declares how its credential is found. A person can override
that at `providers."<name>".credential` in the global or per-project file, with
one of:

```json
{ "env": "OPENROUTER_API_KEY" }
{ "file": "/Users/alice/.secrets/openrouter" }
{ "command": ["op", "read", "op://Private/OpenRouter/key"] }
```

A command runs once per process. A repository can never set this, because a
command runs a program and a changed source sends the key elsewhere.

Providers in one package that share a key read one stored credential: each
names the stored file in its provider data (`credential_name`, defaulting to
its own name). `fiber login <provider>` stores the key in the stored
credential that provider reads, so `fiber login opencode-go` and
`fiber login opencode-zen` both store `credentials/opencode`.

`fiber logout <provider>` deletes that stored credential. When the provider's
key comes from an environment variable, a file outside Fiber home or a
command, `fiber logout` cannot remove it: it names the source and exits
non-zero.

## When Fiber reads configuration

Only the `config` crate reads these files (`docs/architecture.md`). It reads
them:

- when a session starts
- on `reload` (`docs/invocation.md`, "Driver commands"), which costs the one
  prompt-cache miss reload already costs (`docs/mcp.md`, "Reload")
- when a session resumes, where changed configuration rebuilds the preamble as
  `docs/prompt-cache.md` describes

Nothing watches the files, so an idle Fiber does no work. A value written with
`host.config.set` is visible to that extension at once, and to other sessions
at their next reload.

A problem in a file is reported by file and key:

- An unknown key is a `notice` and is otherwise ignored. A repository written
  for a newer Fiber must not break an older one.
- Invalid JSON, or a value of the wrong type, is a startup error. A headless
  run fails with `config_invalid`.

## When Fiber writes

Fiber writes configuration in four places:

- the model picker saves the global `model`
- `host.config.set` writes an extension's settings file
- `fiber config set <key> <value>` writes the global file, the per-project
  file with `--project`, or the repository's `.fiber/config.json` with
  `--repo`
- `fiber mcp add` and `fiber mcp remove` write an entry under `mcp.servers`,
  in the same three files (`docs/mcp.md`, "Configuration")

Each write takes the lock, reads the file, changes one key and writes the whole
file back by renaming a temporary file over it (`docs/state.md`, "Concurrent
access"). Keys are written sorted with a 2-space indent, so a hand-chosen key
order is lost on the first write.

`fiber config get <key>` prints the effective value and the layer or flag it
came from. `fiber config set` never touches the network, for any key; it checks
the value's type. `fiber config set --repo` refuses a key a repository may not
set ("What a repository may set"), before it writes anything.

`fiber config set model <ref>` also checks the reference against the cached
model list, by the rules in `docs/model-routing.md`, "Naming a model". A
reference that matches nothing fails with `no_model` and names the closest
matches. With no cached list, the value is accepted.

## Standing rules

Standing rules are not configuration keys. They are two files in Fiber home:
`rules` at the top level for global rules, and `projects/<key>/rules` for one
project (`docs/permissions.md`, "Remembering a decision"). A repository has no rules
file.

## The repository's `.fiber/` directory

```
.fiber/
  config.json                 repository configuration
  config/<extension>.json     settings for an approved extension, repo_settings keys only
  extensions/<name>/          extensions the repository ships
```

## An extension's manifest

An extension's manifest is `extension.json` at the top of its directory. It
holds what `docs/extensions.md` ("What a package holds") lists:

```json
{
  "name": "github.com/acme/fiber-acme",
  "version": "v1.4.0",
  "fiber": "0.3.0",
  "api": 1,
  "depends": { "github.com/acme/oauth-helper": "v1.2.0" },
  "binaries": {
    "darwin-arm64": { "url": "https://...", "sha256": "..." }
  },
  "process": {
    "program": "node",
    "args": ["dist/main.js"],
    "required": false,
    "exit_timeout_ms": 2000
  },
  "install": ["npm", "ci"],
  "memory_mib": 8,
  "repo_settings": ["workspace_url"],
  "prompt": "prompt.md"
}
```

`fiber` is the lowest Fiber version it runs on. `api` is the extension API's
major version it was written for; Fiber loads it only when that is Fiber's own
(`docs/extensions.md`, "The extension API version"). `process` is present only for
a process extension, and `exit_timeout_ms` has no default. A Lua extension's
entry script is `init.lua` at the top of its directory. `memory_mib` raises a
Lua extension's memory cap, in MiB, above the default of 1
(`docs/extensions.md`, "Loading, and cost when nothing is loaded"). `prompt` names a
file in the package whose text goes in the system prompt
(`docs/system-prompt.md`, "Extension texts").

## A provider's data

Each provider an extension registers as data is `providers/<name>.json` in the
extension's directory. The fields are the ones `docs/model-routing.md` ("What a
provider extension declares") lists:

```json
{
  "name": "databricks",
  "credential": { "env": "DATABRICKS_TOKEN" },
  "headers": { "x-databricks-client": "fiber" },
  "models": [
    {
      "id": "databricks-claude-opus-5",
      "protocol": "anthropic-messages",
      "base_url": "https://example.cloud.databricks.com/ai-gateway/anthropic",
      "compat": { "store": false },
      "deferred_tools": true,
      "extra_body": {},
      "context_window": 1000000,
      "max_output_tokens": 128000,
      "input": ["text", "image"],
      "cost": { "input": 5.0, "output": 25.0, "cache_read": 0.5, "cache_write": 6.25 }
    }
  ]
}
```

- `credential` says how the key is found: `env`, `file` or `command` as in
  "Secrets". A stored credential always comes first
  (`docs/model-routing.md`, "Credentials"): the provider reads the file its
  `credential_name` names, or `credentials/<name>` when it names none, so
  providers in one package that share a key name the same file. A provider
  whose token expires, such as an OAuth login, declares a Lua `credential()`
  function instead.
- `compat` is a flat object of the flags the protocol reads. Fiber never
  guesses a flag, and a flag that is absent is not set:

  | Flag | Type | Read by | Effect |
  |---|---|---|---|
  | `store` | boolean | `openai-responses`, `openai-completions` | sent as the request's `store`; absent, the request has no `store` key |
  | `max_tokens` | boolean | `openai-completions` | the output limit goes in `max_tokens`; absent, it goes in `max_completion_tokens` |
  | `reasoning_object` | boolean | `openai-completions` | the effort goes in `reasoning: {effort}`, as OpenRouter takes it; absent, it goes in `reasoning_effort` |
  | `anthropic` | boolean | `openai-completions` | the model is Anthropic's, behind a gateway such as OpenRouter: requests carry Anthropic's `cache_control` markers on content parts, and Anthropic's strict-tool limits apply |
  | `cache_key_field` | string | `openai-completions` | a body field that also carries the cache key, such as OpenRouter's `session_id` |
  | `cache_key_header` | string | `openai-responses`, `google-generative-ai` | a header that carries the cache key, such as OpenCode's `x-opencode-session` |

  `openai-completions` always sends `stream_options.include_usage: true`,
  because OpenAI sends no usage without it (`docs/model-routing.md`,
  "openai-completions facts").
- `deferred_tools` is set only after a probe (`docs/tools.md`, "Which tools the
  model sees"). Absent means false.
- `cost` is in US dollars per million tokens. A model priced by request size
  adds `tiers`, a list of the same four prices, each with `input_tokens_above`:
  the highest threshold the request's input tokens exceed prices the whole call,
  and below every threshold the base prices apply. What counts as input
  tokens, such as whether cache reads count, is the vendor's definition, and
  the package declares its thresholds by it. Absent means one price at every
  size.
- `subscription` is `true` for a model a subscription login serves; its `cost`
  is then the vendor's API prices (`docs/model-routing.md`, "Cost"). Absent
  means false.

A provider with a `models()` function returns a list in exactly the shape of
`models`. Fiber caches it at `cache/models/<name>.json`.

A person changes provider data only through an extension. A local server such
as Ollama is an extension directory holding one provider file, installed from
its path. A wrong context window is fixed in a local copy of the extension or
upstream. Configuration has no second source of provider data, so there are no
merge rules between the two.

## Not settled here

- What a skill and a prompt template are to Fiber
  (`docs/extensions.md`, "What a package holds")
- Where an MCP server's OAuth token is stored (`docs/mcp.md`)
