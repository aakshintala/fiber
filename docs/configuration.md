# Configuration

What a person sets to change Fiber's defaults, where it is written, and which
value wins. It also covers the format of a provider's data and of an
extension's manifest. Vocabulary is `GLOSSARY.md`. Where Fiber home is, and how
its files are written safely, is [Fiber home](state.md).

## Files and format

Every file Fiber reads as configuration is JSON: configuration, an extension's
settings, an extension's manifest and a provider's data. The parser is
serde_json, which Fiber already uses for everything else
(`docs/dependencies.md`), so configuration adds no crate. A TOML parser would have added 6
crates and 167 KiB of stripped binary on macOS arm64 (`toml` 1.1 against
serde_json alone, measured on September 25, 2026).

The JSON is strict: no comments and no trailing commas. Fiber writes to these
files itself, as described in "When Fiber writes", so a comment would not
survive anyway.

Reading configuration never runs code. A repository's configuration is read
before anyone has approved anything, so Lua is not a configuration format. The
only Lua a provider runs is its optional `models()`, `quota()`,
`credential()`, `sign()` and `cost()` functions (`docs/model-routing.md`,
"Model discovery").

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
value, a list included, replaces the one below it. Each entry under a provider's
`credentials` replaces the one below it as a whole: it does not merge key by key.
Two values do not replace. `reviewer.context`: the reviewer reads the global
value and then the per-project one (`docs/permissions.md`, "What the person
tells it"). `skills.disabled`: the global list and the project's list both
apply.

The per-project file is the person's own setting for one project. It lives in
Fiber home, not the repository, so it covers every worktree of the project
(`docs/state.md`, "Projects"), and a delegate's fresh worktree has it too. A
gitignored file in the working tree would be missing from
every new worktree.

`-c` works on both doors: `fiber ask`, and the terminal, which passes it to
each session it asks the hub to start.
The key is a dotted path and the value is JSON, or a bare string when it does
not parse as JSON: `-c handoff.tokens=200000`, `-c model=openai/gpt-5.6`. It
may be given more than once. A named flag such as `--model` is shorthand for
the same thing, read before every `-c`, so `-c model=` wins over `--model`.
`FIBER_HOME` is the only environment variable of its own that the `fiber`
binary reads (`install.sh` also reads `FIBER_INSTALL_DIR` and `FIBER_VERSION`,
`docs/releasing.md`); no
environment variable overrides a key. Fiber also honours the platform's proxy
variables (`docs/dependencies.md`, "Proxies").

## What a repository may set

A repository may set a key only when the worst a hostile value can do is cost
the person time or money. It never sets a key that widens what runs without
asking, changes where requests go, or weakens a safety check. A cloned
repository is someone else's text, and it is read before anything is approved.
The one exception is code the repository declares, which runs only after a
person approves it, pinned to its exact content: those keys are marked "yes,
with approval".

Each key says whether a repository may set it. A key that says nothing is
person-only, so a key added without thought stays safe. A repository that sets
a person-only key gets a `notice` naming the key and the file, and the value
is ignored.

The table in "Keys" marks each key. Two rules settled elsewhere follow from
this one:

- A repository cannot declare a provider or change a base URL
  (`docs/model-routing.md`, "Choosing the model").
- A repository can declare extensions, hooks and MCP servers, and each runs
  only after a person approves its exact content (`docs/extensions.md`, "Code
  a repository ships"). It cannot enable an extension a person installed.

## Keys

Keys are snake_case and grouped by area. In "Repo", yes means a repository may
set the key.

In an `extensions."<name>"` key, `<name>` is the extension's full name or,
for an extension that has one, its short name (`docs/extensions.md`,
"Names"): `extensions."memory".enabled` and
`extensions."github.com/aakshintala/fiber/extensions/memory".enabled` are the
same key. A file that sets one key under both spellings is `config_invalid`.

| Key | Default | Repo | Meaning |
|---|---|---|---|
| `model` | none | yes | The default model for a new session, as `provider/model` (`docs/model-routing.md`, "Choosing the model"). |
| `thinking` | the model's own default | yes | The thinking level for a new session (`docs/model-routing.md`, "Thinking"). |
| `model_lists.refresh_after` | `"24h"` | no | How old a provider's cached model list must be before Fiber refreshes it in the background, as a duration such as `"7d"`. The model picker's refresh button ignores it (`docs/model-routing.md`, "Model discovery"). |
| `scoped_models` | none | yes | A list of model references the model picker shows; none means every installed model (`docs/tui.md`, "Swapped views"). |
| `roles."<name>"` | none | yes | A delegate's model reference, such as `"fiber:openai/gpt-5.6:xhigh"`, or an object with `model`, the reference, and `credential`, the credential label the delegate uses (`docs/delegates.md`). A repository's role cannot name a credential: its `credential` is ignored with a `notice`. |
| `hub.port` | none | no | The port of `127.0.0.1` an installed hub listens on, as well as its local socket; unset, it listens on its local socket only. `fiber hub install --port` writes it (`docs/invocation.md`, "The hub"). |
| `hub.allowed_origins` | `[]` | no | The web page origins, such as `"https://fiber.example.ts.net"`, whose websockets the hub accepts (`docs/invocation.md`, "Remote clients"). |
| `hub.idle_exit_ms` | 1800000 (30 minutes) | no | How long a hub a client started stays running with no client connected, the same default as `session.idle_exit_ms`; an installed hub never exits for being idle (`docs/invocation.md`, "The hub"). |
| `hub.default` | none | no | On a client, the name of the hub in `hubs` it uses without `--hub`; unset, it uses the local hub. |
| `hubs."<name>".address` | none | no | On a client, a hub's address: `ws://`, `wss://`, or `unix:` and a socket path (`docs/invocation.md`, "Several hubs"). The device token is in `credentials/hubs/<name>`, never here. |
| `session.idle_exit_ms` | 1800000 (30 minutes) | no | How long a session stays running with no turn, no jobs and no cache warming, whoever is connected (`docs/invocation.md`, "Lifecycle"). |
| `reviewer.model` | the session's provider's reviewer model | no | The reviewer's model (`docs/permissions.md`, "The reviewer"). |
| `reviewer.context` | none | no | The person's notes about their environment, in prose, which the reviewer reads after its fixed instructions; the global and per-project values are both read, the project's winning where they conflict (`docs/permissions.md`, "What the person tells it"). Only Fiber home's global and per-project files set it; `-c` is ignored with a `notice`. |
| `reviewer.block_limits.consecutive` | 3 | no | Consecutive blocks before a person is asked. |
| `reviewer.block_limits.session` | 20 | no | Blocks in a session before a person is asked. |
| `handoff.enabled` | true | yes | Whether automatic handoff runs (`docs/handoff.md`). |
| `handoff.tokens` | 400000 | yes | The token trigger. |
| `handoff.window_fraction` | 0.7 | yes | The trigger as a fraction of the model's context window. |
| `handoff.nudge` | true | yes | Whether the nudge is given. |
| `cache.lifetime` | `"1h"` | yes | The prompt-cache lifetime, `"5m"` or `"1h"` (`docs/prompt-cache.md`). |
| `cache.warm_idle` | false | yes | Whether an idle session keeps its prompt cache warm (`docs/prompt-cache.md`, "Warming while idle"). |
| `cache.warm_cap` | `2` | yes | How many cache lifetimes after the last turn warming stops, an integer; less than 12 (`docs/prompt-cache.md`, "Warming while idle"). |
| `keys."<action>"` | the binding in `docs/tui.md` | no | A key, or a list of keys, for a terminal action; `[]` unbinds it (`docs/tui.md`, "Bindings"). |
| `retry.attempts` | 3 | yes | Retries of a failed model call (`docs/model-routing.md`, "When a model call fails"). |
| `retry.initial_delay_ms` | 2000 | yes | The first backoff, doubling each retry. |
| `retry.max_delay_ms` | 60000 | yes | The cap on one backoff, and on a wait a server asks for. |
| `tools."<name>".max_result_bytes` | the tool's own, or 16384 | yes | The tool's result cap (`docs/tools.md`, "Bounded results"). |
| `tools."<name>".deferred` | the tool's own | yes | Whether the tool is deferred (`docs/tools.md`, "What is deferred by default"). |
| `skills.disabled` | `[]` | no | Names of skills switched off: left out of the listing, refused by the `skill` tool and not expanded by `/name`. The project's list and the global list both apply. The terminal's `/skills` view writes it (`docs/system-prompt.md`, "Skills"). |
| `web_search.backend` | the one installed | no | The search backend `web_search` uses when more than one is installed (`docs/tools.md`, "Web fetch and web search"). |
| `shell.read_only."<command>".flags` | none | no | Adds a command to the shell classifier's read-only list, with the flags it may take and stay read-only, such as `["--json", "-p"]` (`docs/tools.md`, "Search", "Other command-line tools"). |
| `diagnostics.level` | `"info"` | no | How much the diagnostic logs record: `"info"` or `"debug"` (`docs/state.md`, "What each part holds", says what each level writes). Only Fiber home's `config.json` sets it; a project's file or `-c` is ignored with a `notice`. A process reads it when it starts. |
| `budget.usd` | none | no | The most a session may spend, in US dollars billed per token, its delegates included; unset means no limit (`docs/loop.md`, "Spending budget"). |
| `quota.notice_at` | 80 | yes | The percent used of a quota window at which the model gets a notice (`docs/tools.md`, "Provider quota"). |
| `mcp.servers."<name>"` | none | yes, with approval | An MCP server ("MCP servers"). |
| `repository_extensions` | none | yes, with approval | The extension packages the repository ships: a list of objects with `path`, inside the repository, and `required`, default false (`docs/extensions.md`, "Code a repository ships"). Set anywhere but the repository's file, it is ignored with a `notice`. |
| `extensions."<name>".enabled` | true | no | Whether an installed extension loads; `false` globally and `true` in a project's file scopes it to that project (`docs/extensions.md`, "Code a repository ships"). |
| `extensions."<name>".startup_timeout_ms` | 5000 | yes | A process extension's startup deadline. |
| `extensions."<name>".commands."<command>"` | none | yes | A new name for one of the extension's commands, when two extensions clash. |
| `extensions."<name>".tools.enabled`, `extensions."<name>".tools.disabled` | none | yes | Lists of the extension's tool names to declare or leave out, as an MCP server's `tools.enabled` and `tools.disabled` do ("MCP servers"); the terminal's `/tools` switch writes them (`docs/tui.md`, "Swapped views"). |
| `extensions."<name>".hook_timeout_ms` | the callback's own | no | Overrides the timeout of every hook and watcher the extension registers, including the entries the `hooks` extension registers from configuration. |
| `hooks.order."<hook point>"` | none | no | Extension names in the order their hooks run at that point (`docs/extensions.md`, "When several hooks share a point"). |
| `providers."<name>".credential` | `default`; `fiber login` writes the first label it stores | no | The credential label a new session uses (`docs/model-routing.md`, "Which credential a session uses"). |
| `providers."<name>".credentials."<label>"` | none | no | Where that label's key comes from, when it is not stored in Fiber home ("Secrets"). |
| `tui.panel.cards` | `["session", "changed_files", "delegates", "jobs", "quota"]` | no | The cards the terminal's panel shows, in order; an extension widget is listed as `"<extension>/<widget>"`, and a widget the list does not name shows after the listed cards (`docs/tui.md`, "The panel"). The status line of the narrow layout follows the same order. |
| `tui.rail.width` | 15 | no | The rail's share of the screen's width, in percent, kept from 22 to 48 columns; dragging its edge writes it (`docs/tui.md`, "Layout"). |
| `tui.panel.width` | 21 | no | The panel's share of the screen's width, in percent, kept from 30 to 60 columns; dragging its edge writes it. |
| `tui.theme` | none | no | The theme's name: `auto`, `dark`, `light` or the name of a theme file in Fiber home's `themes/`. With none or `auto`, the theme follows the terminal's light or dark appearance (`docs/tui.md`, "Themes"). |
| `tui.reduced_motion` | false | no | Whether every animation takes its still form. On whenever a screen reader is detected (`docs/tui.md`, "Reduced motion"). |
| `tui.screen_reader` | detected | no | Forces the flat screen-reader mode on or off; `--screen-reader` sets it true (`docs/tui.md`, "Screen readers"). |
| `tui.attention.notification` | true | no | Whether the terminal sends an OSC 9 desktop notification when a session starts waiting on the person (`docs/tui.md`, "Getting the person's attention"). |
| `tui.attention.bell` | true | no | Whether it rings the bell where OSC 9 is not supported. |
| `tui.attention.title` | true | no | Whether the terminal title shows the session's state. |
| `tui.hover` | true | no | Whether hover highlights the click target under the pointer; false drops mouse mode 1003 (`docs/tui.md`, "Mouse and hover"). |
| `tui.inline_images` | true | no | Whether images show inline where the terminal speaks kitty's graphics protocol (`docs/tui.md`, "Images"). |
| `tui.logo_glyph` | `"⌇"` | no | The glyph before the name in the logo, `"⌇"` or `"≈"`, for a font without ⌇ (`docs/tui.md`, "The logo"). |
| `tui.slots."<slot>"` | none | no | The extension that fills a slot two extensions replace, such as `tui.slots."ledger_row:shell"`; the same for a key two extensions bind, as `tui.slots."key:ctrl+k"` (`docs/tui.md`, "When two extensions want one slot"). |

Hook order and hook timeouts are person-only so that the person keeps the
final say over the hooks they run. Order is precedence, not a security
boundary: every approved hook runs with the account's full rights. Phases put
redaction first whoever ships it (`docs/extensions.md`, "When several hooks
share a point").

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

The keys that may be set per model are `handoff.*`, `cache.lifetime` and `thinking`. A
per-model key may be set by a repository when the top-level key may be.

### MCP servers

Each entry under `mcp.servers` holds what `docs/mcp.md` ("Configuration")
lists:

| Field | Meaning |
|---|---|
| `command`, `args`, `env` | the program of a stdio server; an `env` value is a string, or `{ "secret": "<name>" }`, which reads `credentials/<name>` when the server starts. The name must start `mcp.<server>.`, so a server reads only its own secrets |
| `url` | a remote server |
| `required` | whether failing to start ends the session, default false |
| `startup_timeout_ms` | the startup deadline, default 5000 |
| `timeout_ms` | the call timeout |
| `declare_in_full` | whether its tools are declared in full rather than deferred, default false |
| `tools.enabled`, `tools.disabled` | lists of tool names |
| `tools."<tool>".hints` | overrides of that tool's MCP hints |

A repository may declare a server, subject to approval of its declaration and
of each file its `command` or `args` names inside the repository
(`docs/extensions.md`, "Code a repository ships"). `tools."<tool>".hints`
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

`<extension>` is named as in `extensions/` (`docs/state.md`, "What each part
holds"). The layers merge exactly as Fiber's own keys do.

- `host.config.get(key)` returns the merged value.
- `host.config.set(key, value, scope)` writes one file. `scope` is `"machine"`
  for the global file or `"project"` for the per-project file, the same words
  `host.data_dir` takes. It has no default, so the author decides. A model
  picker can save a choice for one project only.
- A repository sets only the keys the extension's manifest lists under
  `repo_settings`. Fiber cannot judge a key it does not know, but the author
  can, so the author applies the rule in "What a repository may set". Any
  other key in the repository's file gets a `notice` and is ignored. The list
  comes from the manifest of the installed or approved copy, never from a
  repository's working files. The `hooks` extension lists `hooks`, and each
  hook a repository declares there is withheld until a person approves it
  (`docs/extensions.md`, "Hooks declared in configuration"). Any entry a
  repository declares there, hook or watcher, may set `required`, default
  false, read only from the repository's file: a required entry nobody
  approved fails a run that has nobody to ask, with `hook_unapproved`.
- `fiber extension remove` deletes the extension's file in the global and every
  per-project layer, asking first in a terminal, as it does for its data
  directories.

A separate file means an extension never rewrites the person's `config.json`.

## Secrets

A secret is a file in `credentials/` in Fiber home, mode 0600. A provider's
credentials are `credentials/<name>/<label>`, one file per credential label
(`docs/model-routing.md`, "Credentials"). An extension's secrets share the
directory as single files: `host.secret(name)` reads `credentials/<name>`. An
extension declares each name it reads in its manifest's `secrets`, and
`host.secret` reads no other ("An extension's manifest"). By convention, an
extension prefixes its names with its own name, such as `acme.api_key`.
`fiber login <name>` stores a declared secret: it reads the value from a
hidden prompt, or from stdin without a terminal, and writes
`credentials/<name>` with mode 0600. A secret that is already stored is
replaced, and the command says so, because logging in again is how a key is
rotated. A name that is neither a provider nor a secret an installed extension
declares is refused, so a mistyped name is caught rather than stored where
nothing reads it.

This is one namespace, not a wall between extensions. An extension runs with
the account's full rights and could read `credentials/` with `host.fs` anyway
(`docs/extensions.md`, "Lua extensions"). The credential deny protects the
directory from tool calls, not from extensions
(`docs/permissions.md`, "Credentials").

A provider's data declares how its `default` credential is found. A person
adds or overrides a label at `providers."<name>".credentials."<label>"` in the
global or per-project file, with one of:

```json
{ "env": "OPENROUTER_API_KEY" }
{ "file": "/Users/alice/.secrets/openrouter" }
{ "command": ["op", "read", "op://Private/OpenRouter/key"] }
```

A command that succeeds runs once per process, and its key is kept. A command
that fails runs again the next time the key is read, so a fixed command works
without restarting Fiber. A command is named by its program alone in every
message, because its arguments may hold a key. A repository can never set this, because a
command runs a program and a changed source sends the key elsewhere. A
credential stored under the same label comes first. A key from an `env`
source stays in Fiber's environment, so it is visible to every command the
session runs.

Providers in one package that share a key read one stored credential: each
names the stored directory in its provider data (`credential_name`, defaulting
to its own name). `fiber login <provider>` stores the key in the directory
that provider reads, so `fiber login opencode-go` and
`fiber login opencode-zen` both store under `credentials/opencode/`.

`fiber logout <provider>` deletes the provider's stored credential. When the
provider has several labels, it refuses unless given `--as <label>` or `--all`.
When the key comes from an environment variable, a file outside Fiber home or
a command, `fiber logout` cannot remove it: it names the source and exits
non-zero.

## When Fiber reads configuration

Only the `config` crate reads these files (`docs/architecture.md`). It reads
them:

- when a session starts
- on `reload` (`docs/invocation.md`, "Driver commands"), which costs the one
  prompt-cache miss reload already costs (`docs/mcp.md`, "Reload")
- when a session resumes, where changed configuration rebuilds the preamble as
  `docs/prompt-cache.md` describes
- at each turn start, `skills.disabled` only, with the check for added and
  removed skills (`docs/system-prompt.md`, "Added and removed skills")

Nothing watches the files, so an idle Fiber does no work. A value written with
`host.config.set` is visible to that extension at once, and to other sessions
at their next reload.

So a key written while Fiber runs applies, and the terminal's `/settings` says
when (`docs/tui.md`, "Swapped views"):

- `tui.theme` at once: the terminal applies the theme it writes
- any other `tui.*` key when the terminal starts again
- `hub.*` and `hubs.*` when the hub starts again
- `diagnostics.level` when each Fiber process starts again
- `model` and `thinking` for sessions started after the write
- `skills.disabled` at the next turn start
- every other key on `/reload`, with the one prompt-cache miss it costs

A problem in a file is reported by file and key:

- An unknown key is a `notice` and is otherwise ignored. A repository written
  for a newer Fiber must not break an older one. A command that runs no
  session, such as `fiber config`, `fiber login` or `fiber models`, prints
  each notice as one line on stderr.
- Invalid JSON, or a value of the wrong type, is a startup error. A headless
  run fails with `config_invalid`.

## When Fiber writes

Fiber writes configuration in these places:

- the model picker saves the global `model`, and a thinking level as
  `models."<provider/model>".thinking`, unless the choice is marked as this
  session only (`docs/tui.md`, "Swapped views")
- dragging the rail's or the panel's edge saves the global `tui.rail.width`
  or `tui.panel.width` (`docs/tui.md`, "Layout")
- `/credential` saves the global `providers."<name>".credential`, unless the
  switch is marked as this session only, and `fiber login` writes it when it
  stores a provider's first label (`docs/model-routing.md`, "Credentials")
- `/scoped-models` saves the global `scoped_models`, and the `/keys` screen
  saves the global `keys`, only the bindings that differ from the defaults
- `host.config.set` writes an extension's settings file
- `fiber extension install --project` writes `extensions."<name>".enabled`:
  `true` in the project's file, and `false` in the global file for an
  extension it installs for the first time (`docs/extensions.md`, "Code a
  repository ships")
- `fiber config set <key> <value>` writes the global file, the per-project
  file with `--project`, or the repository's `.fiber/config.json` with
  `--repo`
- the terminal's `/settings` writes a key through the same path as
  `fiber config set`, in the layer the person picks among those the key
  allows, and `tui.theme` to the global file (`docs/tui.md`, "Swapped views")
- the terminal's `/rules` deletes one line of a rules file under the file's
  lock, leaving every other line as it was (`docs/tui.md`, "Swapped views";
  "Standing rules")
- `fiber mcp add` and `fiber mcp remove` write an entry under `mcp.servers`,
  in the same three files (`docs/mcp.md`, "Configuration")
- `fiber hub install --port` writes the global `hub.port`, and `fiber hub add`
  and `fiber hub remove` write the global `hubs` entry and, with `--default`,
  `hub.default` (`docs/invocation.md`, "The hub")

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
matches. With no cached list, the value is accepted. A reference to a model
left out with `model_unconfigured` (`docs/model-routing.md`,
"A per-account host") is written: Fiber prints that notice on stderr as one
line, `fiber: ` and its message, and exits 0, so the person can set the host
next.

`cache.warm_cap` is refused with `config_invalid` when it is 12 or more
(`docs/prompt-cache.md`, "Warming while idle"), whether
it is set with `fiber config set` or by hand.

## Standing rules

Standing rules are not configuration keys. They are two files in Fiber home:
`rules` at the top level for global rules, and `projects/<key>/rules` for one
project (`docs/permissions.md`, "Remembering a decision"). A repository has no rules
file.

Each line is one JSON object:

```json
{"decision":"allow","tool":"shell","prefix":"npm test"}
```

`decision` is `allow`, `ask` or `deny`; `tool` is the tool's name and
`prefix` what it matches (`docs/permissions.md`, "What a rule matches"). A
line saved from an approval also carries `added`, milliseconds since the Unix
epoch, and `session_id`, the session that added it, which `/rules` shows.
Unknown keys are ignored and blank lines are skipped. A missing file holds no
rules. A file or line that cannot be read denies calls until it is fixed
(`docs/permissions.md`, "Scope").

## The repository's `.fiber/` directory

```
.fiber/
  config.json                 repository configuration, including repository_extensions
  config/<extension>.json     settings for an extension, repo_settings keys only
```

The packages a repository ships may sit in any directory of it;
`repository_extensions` names them.

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
  "replaces": ["web_search"],
  "providers": { "acme": ["https://api.acme.dev/v1"] },
  "prompt": "prompt.md",
  "secrets": ["acme.api_key"],
  "opening": {
    "machine": ["index.md"],
    "project": ["index.md"],
    "budget_bytes": 25000
  }
}
```

`fiber` is the lowest Fiber version it runs on. `api` is the extension API's
major version it was written for; Fiber loads it only when that is Fiber's own
(`docs/extensions.md`, "The extension API version"). `process` is present only for
a process extension, and `exit_timeout_ms` has no default. A Lua extension's
entry script is `init.lua` at the top of its directory. `memory_mib` raises a
Lua extension's memory cap, in MiB, above the default of 1
(`docs/extensions.md`, "Loading, and cost when nothing is loaded"). `replaces`
lists the built-in tools and commands the extension replaces, and `providers`
maps each provider it registers to its base URLs; both default to none, and a
registration beyond them stops the extension loading (`docs/extensions.md`,
"What a package holds"). `prompt` names a
file in the package whose text goes in the system prompt
(`docs/system-prompt.md`, "Extension texts"). `secrets` lists the names the
extension reads with `host.secret`, and defaults to none ("Secrets").
`opening` names files whose text
goes in the opening message: `machine` lists paths in the extension's machine
data directory and `project` paths in its project data directory
(`docs/state.md`, "What each part holds"), each relative to that directory
and inside it. `budget_bytes` is the section's optional byte budget: over it,
Fiber tells the model to prune the files when it builds the opening message
and after a call by the session that declares a `writes` effect on one of them
(`docs/system-prompt.md`, "Extension sections"). All three default to none.

## A provider's data

Each provider an extension registers as data is `providers/<name>.json` in the
extension's directory. The fields are the ones `docs/model-routing.md` ("What a
provider extension declares") lists:

```json
{
  "name": "databricks",
  "credential": { "env": "DATABRICKS_TOKEN" },
  "placeholders": { "workspace": { "env": "DATABRICKS_HOST" } },
  "headers": { "x-databricks-client": "fiber" },
  "models": [
    {
      "id": "databricks-claude-opus-5",
      "protocol": "anthropic-messages",
      "base_url": "https://{workspace}/ai-gateway/anthropic",
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

- `credential` says how the `default` key is found: `env`, `file` or `command`
  as in "Secrets". A stored credential always comes first
  (`docs/model-routing.md`, "Credentials"): the provider reads the directory
  its `credential_name` names, or `credentials/<name>/` when it names none, so
  providers in one package that share a key name the same directory. A provider
  whose token expires, such as an OAuth login, declares a Lua `credential()`
  function instead.
- `compat` is a flat object of the flags the protocol reads. Fiber never
  guesses a flag, and a flag that is absent is not set:

  | Flag | Type | Read by | Effect |
  |---|---|---|---|
  | `store` | boolean | `openai-responses`, `openai-completions` | sent as the request's `store`; absent, the request has no `store` key |
  | `max_tokens` | boolean | `openai-completions` | the output limit goes in `max_tokens`; absent, it goes in `max_completion_tokens` |
  | `reasoning_object` | boolean | `openai-completions` | the thinking level goes in `reasoning: {effort}`, as OpenRouter takes it; absent, it goes in `reasoning_effort` |
  | `anthropic` | boolean | `openai-completions` | the model is Anthropic's, behind a gateway such as OpenRouter: requests carry Anthropic's `cache_control` markers on content parts, and Anthropic's strict-tool limits apply |
  | `cache_key_field` | string | `openai-completions` | a body field that also carries the cache key, such as OpenRouter's `session_id` |
  | `cache_key_header` | string | `openai-responses`, `google-generative-ai` | a header that carries the cache key, such as OpenCode's `x-opencode-session` |

  `openai-completions` always sends `stream_options.include_usage: true`,
  because OpenAI sends no usage without it (`docs/model-routing.md`,
  "openai-completions facts").
- `extra_body` is added to every request for the model. It may add a field
  or replace one such as `max_tokens`, but may not name a field listed in
  `docs/model-routing.md`, "Extra request body fields"; a model that does is
  left out with the notice `model_invalid` (`docs/model-routing.md`, "Extra
  request body fields").
- A `{name}` in a `base_url` is a per-account host. Its value is the
  extension's setting of that name, which only the person sets; `placeholders`
  may name an environment variable read when the setting is unset. A model
  whose placeholder has no value, or a value that is not a host, is left out
  with the notice `model_unconfigured` (`docs/model-routing.md`, "A per-account host").
- `web_search` is the vendor's hosted-search tool type as it is sent; absent,
  the model hosts no search. A type its protocol does not read back leaves
  the model out with `model_invalid` (`docs/model-routing.md`, "Hosted web
  search").
- `context_window` is required. A model without it is left out with the
  notice `model_invalid` (`docs/model-routing.md`, "What a provider extension
  declares").
- `thinking_levels` lists the thinking levels the model takes, and
  `thinking_default` is the model's own default, used when no session choice
  or configured value names one (`docs/model-routing.md`, "Thinking").
  Absent levels mean the model takes none. A default not among the levels
  leaves the model out with `model_invalid`.
- `deferred_tools` is set only after a probe (`docs/tools.md`, "Which tools the
  model sees"). Absent means false.
- `cost` is in US dollars per million tokens. A model priced by request size
  adds `tiers`, a list of the same four prices, each with `input_tokens_above`:
  the highest threshold the request's whole input exceeds prices the whole
  call, and below every threshold the base prices apply. The whole input
  counts cache reads and writes (`docs/model-routing.md`, "Cost"). Absent means one price at every
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
