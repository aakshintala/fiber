# Overalls: embed Fiber in it, or fork it as Fiber's factory

Research for [#3879](https://github.com/aakshintala/fiber/issues/3879).
No recommendation is ruled here: the direction is the owner's call.

## What was read

- Upstream `ghuntley/overalls` at trunk commit
  `4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29` (2026-11-01). Below, `ov:<path>`
  means that path at that commit; each first citation also gives its
  `github.com` blob URL. Upstream launches 2026-11-03 and its docs may change
  at launch, so this file needs a re-check after launch as a new commit or
  issue.
- `research/codex-usage/README.md`, `research/openrouter-cost/README.md`,
  `research/budgets/README.md` and `research/budgets/spend.md` for usage and
  cost; [#3878](https://github.com/aakshintala/fiber/issues/3878) as context
  for durability.
- Fiber behavior is cited by repo path at the ticket's base `d00a65691`.
  Research has no area doc, so no `docs/<area>.md` page is cited as authority;
  Fiber paths below are evidence pointers, not spec citations.
- Anything the sources do not settle is marked **not found**.

Upstream calls itself "Research preview. Not for consumption" and several
cited ADRs are marked `Implementation: partial`
([README](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/README.md),
`ov:README.md`).

## Q1. Embed: what it takes for Fiber to be a first-class agent

### Fiber as a preinstalled guest CLI

Upstream installs pinned Codex 0.149.0 and Claude Code 2.1.238 in the default
NixOS and Ubuntu images, configured natively (`/etc/codex/config.toml`,
`/etc/claude-code/managed-settings.json`), with automatic client updates
disabled so image input review governs versions
([ADR-0167](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0167-native-guest-coding-clis.md),
`ov:docs/architecture/decisions/0167-native-guest-coding-clis.md`, status
superseded by ADR-0168, whose image, Ziggy, closure and provider-boundary
decisions remain in force). ADR-0168 then sets dangerous defaults (Codex
`approval_policy = "never"`, Claude `bypassPermissions`)
([ADR-0168](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0168-guest-coding-cli-dangerous-defaults.md),
`ov:docs/architecture/decisions/0168-guest-coding-cli-dangerous-defaults.md`).
The shared package configuration is
`ov:nix/domains/artifacts/guest/coding-clis.nix`. Preinstalled software is the
established mechanism, not continuous enforcement: root keeps full control of
its writable disk
([ADR-0076](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0076-default-guest-toolset.md),
`ov:docs/architecture/decisions/0076-default-guest-toolset.md`).

What Fiber needs: a `fiber` binary in both image families through the same
image-derivation mechanism, plus whatever first-run state a guest Fiber home
needs. No new mechanism has to be invented; the toolset precedent covers it.

Upstream change needed: yes. A new pinned client in the images requires a
user-approved decision of the ADR-0167 kind: "The user approved both
clients", while "Pi and OpenCode were explicitly removed from scope" and "No
custom launcher, model-catalog synchronizer or Marshall agent integration is
introduced" (ADR-0167). Whether upstream takes Fiber is the user's call;
there is no standing API for it, and the record shows custom launchers
rejected and two outside harnesses removed from scope. A Fiber preinstall
would also have to answer ADR-0168's dangerous-defaults question: guest CLIs
must not stop for approval prompts.

### Fiber's model calls through Ziggy

Ziggy's canonical endpoint is `https://llm.internal`. It speaks native OpenAI
Responses, chat completions and Anthropic Messages with no API translation,
accepts the literal query `beta=true` only on the Messages routes, adds
provider credentials at the edge, and treats guest-supplied credentials and
identity headers as conferring no authority. Production hosts enable only
OpenAI, so Claude receives `provider_unavailable`; model catalogs may include
models Ziggy does not provide and no catalog synchronization is promised
([ziggy.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/services/ziggy.md),
`ov:docs/services/ziggy.md`).

What Fiber needs: a provider entry pointed at `llm.internal`. Fiber's side
supports this shape: an extension registers a provider with its base URLs
(`fiber.provider(name, { models, quota, credential, sign, cost })`,
`docs/extensions.md`), and the first-party `openai` package already speaks
`openai-responses` (`providers/openai/providers/openai.json`). Fiber requires
a credential per provider entry (`docs/model-routing.md`, "Keys, tokens and
OAuth"); the Claude-on-Ziggy precedent shows the accepted form — a nonsecret
sentinel that creates no authority (`ov:docs/services/ziggy.md`).

Upstream change needed: none for the route itself, as long as Fiber speaks a
native protocol against an enabled provider. Everything else is upstream
policy: enabling a provider or model is a separate operator decision (cf.
ADR-0168 open questions); any Fiber-specific translation or catalog sync
repeats what ADR-0167 rejected. Inference authority comes from Marshall
admission bound to current VM placement, never from client settings, so a
Fiber CLI in the guest gets inference only under that admission, and a
disabled provider answers `provider_unavailable` before admission or provider
effects.

### A Fiber hub inside a machine, attached from outside

Audi implements authoritative DNS, an SSH gateway, browser sign-in exchanges
and leased HTTP/WebSocket forwarding; Marshall holds identity, access,
placement and publication authority, and Audi binds guest connections to
current placement
([audi.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/services/audi.md),
`ov:docs/services/audi.md`). Porsche is the SSH management gateway: it turns
interactive and one-shot management commands into authenticated Marshall API
requests
([porsche/README](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/porsche/README.md),
`ov:porsche/README.md`). Terminal sharing goes through Porsche attach plus a
Marshall gateway stream with authority rechecked every 5 seconds; direct SSH
to the guest was rejected for that path because it bypasses the shared
browser session and its writer authority
([ADR-0223](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0223-porsche-terminal-attach.md),
`ov:docs/architecture/decisions/0223-porsche-terminal-attach.md`).

What Fiber needs: the hub runs in the machine as the owning account, and a
person who reaches that machine over SSH forwards `run/hub` (`ssh -L
/tmp/fiber-hub.sock:.../.fiber/run/hub host`) and attaches; a forwarded local
socket needs no token because only that account can open either end, and
Fiber ships no relay (`docs/invocation.md`, hub sections). Audi already
supplies the SSH transport, so attach-over-SSH needs no new upstream surface;
sharing the session through the browser terminal would bypass writer
authority and is out of scope for the same reason ADR-0223 rejects direct
SSH for terminal sharing.

Upstream change needed: none for the transport. The hub binary still needs
the preinstall above, and attach needs whatever SSH grant the machine's roles
already carry.

### Fiber sessions in Marshall's UI

Marshall owns agent roots, threads, turns, ordered events, model context and
recovery; the browser workspace reads threads and prompts through LiveView
([ADR-0099](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0099-marshall-owned-agent-threads.md),
`ov:docs/architecture/decisions/0099-marshall-owned-agent-threads.md`,
status superseded by ADR-0100;
`ov:marshall/lib/marshall_web/live/agent_threads_live.ex`). A Fiber session
log (`docs/state.md`) is not a Marshall thread, and nothing read maps one to
the other.

Upstream change needed: yes — a Marshall API or LiveView surface that reads
Fiber sessions, or a rule promoting them into threads. Whether upstream takes
it is **not found**: no statement for or against exists in the sources read,
beyond ADR-0167's standing "no Marshall agent integration" line. This is the
owner's call with upstream.

### Embed upstream-change summary

| Piece | Upstream change? | Stance evidence |
|---|---|---|
| Preinstalled `fiber` CLI | Yes: image derivations plus a user-approved client decision | User approved two clients; rejected custom launchers; removed pi and OpenCode from scope (ADR-0167) |
| Model calls via Ziggy | None for a native-protocol route on an enabled provider; operator decision for providers/models | Guest creds confer no authority; no catalog sync promised; disabled providers refuse first (`ov:docs/services/ziggy.md`) |
| Hub attach over SSH | None for transport; preinstall plus existing SSH grants | Audi SSH gateway exists; direct-SSH terminal sharing rejected for writer-authority reasons (ADR-0223) |
| Sessions in Marshall UI | Yes: new Marshall read surface | Standing "no Marshall agent integration" (ADR-0167); acceptance **not found** |

## Q2. Fork and replace: service-by-service map

Control-plane rule that shapes every row: Marshall owns the event loop and
PostgreSQL is authority for thread order, model work, tool commands and
executor fences; Ziggy polls Marshall's control API for assigned model calls
(one-second `agent_next` cadence until `ziggy_work` hints have producers),
each call durable and claimed once before any provider attempt, and a claimed
call that loses its result becomes uncertain and is never replayed
([ADR-0100](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0100-pulled-agent-work-over-ziggy-control.md),
`ov:docs/architecture/decisions/0100-pulled-agent-work-over-ziggy-control.md`;
transport since 2026-10-05 runs over the control WebSocket as `ziggy.*`
requests, ADR-0142). The guest runs only `overalls agent-executor` as a
service and receives fenced tool commands; Marshall owns thread history,
model calls, child-agent joins and recovery
([agent-threads.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/development/agent-threads.md),
`ov:docs/development/agent-threads.md`).

### Marshall's agent loop → a Fiber session

Upstream: `ov:marshall/lib/marshall/agents/loop.ex`,
`ov:marshall/lib/marshall/agents/model_calls.ex`,
`ov:marshall/lib/marshall/agents/turn_coordinator.ex`,
`ov:marshall/lib/marshall/agents/model_protocol.ex` (stateless Responses
requests for turns, completed output retained only in authorized thread
state), `ov:marshall/lib/marshall/agents/executor_rpc.ex`, and
`ov:ziggy/lib/ziggy/agent_worker.ex` (ADR-0100 implementation evidence).
Fiber side: a session with its loop (`docs/loop.md`, `docs/state.md`).

Replace. The fork drops server-owned turns, PostgreSQL-claimed running
transitions, child-thread joins and the Ziggy pull worker, and runs the
model loop inside the machine instead. Keep in sync with upstream: the
closed executor RPC shapes and fence/epoch/lease rules (a stale fence never
commits a tool result); the `ziggy.*` control call classes (unauthorized,
version refusal, completion conflict); single-active-turn and
queued-prompt-isolation invariants; and the no-automatic-replay rule for
uncertain effects, which matches Fiber's own no-rerun rule (see Q4).

### `agent-executor` → Fiber's tools

Upstream: `ov:lyra/src/agent.rs` (guest executor polling
`https://llm.internal/agent/v1/`, closed RPCs, 20-second command deadline,
4 KiB output limit, lease/stop callback with process-group kill),
`ov:lyra/src/agent/native_command.rs`. Fiber side: the tool seam
(`docs/tools.md`).

Replace. The fork drops the pulled-command model, executor leases and the
closed tool set, and lets Fiber tools run locally in the machine. Keep in
sync with upstream: the lease/fence discipline for any tool effect the fork
still reports to Marshall; profile-scoped agent capabilities
(`ov:docs/architecture/decisions/0103-profile-scoped-agent-capabilities.md`);
the server-owned tools that never return to the guest (web search, ADR-0107;
guest-executed web fetch, ADR-0108); managed skills and instruction
collection (ADR-0178/ADR-0179), which set what guidance a thread consumes;
and the attachment/artifact body limits on the executor RPC path.

### Marshall's thread store → Fiber's session log

Upstream: the Ash agent domain (`ov:marshall/lib/marshall/agents.ex`),
prompt/executor/turn coordinators, and PostgreSQL as durable authority;
thread history survives VM deletion until explicit user deletion (ADR-0099).
Fiber side: the session log as the only state of record (`docs/state.md`).

Replace, with the sharpest durability trade in the map (see Q4): the fork
moves durable agent state from control-plane PostgreSQL to a log file on the
guest disk. Keep in sync with upstream: turn/event ordering and prompt
idempotency rules; the payload-free audit rule (no prompts, command
arguments, tool output or credentials in audit/diagnostics); prompt
authorization roles and wake semantics; and inference retention knobs
(`services.marshall.inference.detailDays` 90, `aggregateDays` 365,
`ov:docs/services/ziggy.md`). Thread retention policy itself is **not
found** in the sources read.

### Ziggy → Fiber's providers

Upstream: `ov:ziggy/lib/ziggy/route.ex` (exact beta-query allowance, native
bodies unchanged), `ov:ziggy/lib/ziggy/upstream.ex`,
`ov:ziggy/lib/ziggy/agent_worker.ex` (route selection for agent calls);
deployment inputs `services.ziggy.enabledProviders` (default
`["openai" "anthropic"]`), SOPS secrets (`ZIGGY_CREDENTIAL`,
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`), private Audi frontend and CA trust
(`ov:docs/services/ziggy.md`). Fiber side: first-party provider packages
(`providers/*`) and extension-registered providers (`docs/extensions.md`,
`docs/model-routing.md`).

Replace for inference routing and credential custody: the fork drops the
host-local proxy, edge-added credentials and Ziggy admission/accounting, and
points Fiber providers at vendors (or a kept Ziggy — see below). Keep in
sync with upstream: the native API surface and its refusal rules (exact
`beta=true` allowance, no translation, disabled providers refuse before
effects); the custody boundary (provider keys never in images, guests,
monitoring or diagnostics; Marshall and Audi never hold provider keys);
usage accounting shape (token-only estimates with model provenance; missing
usage is unknown, never zero); and the model catalog, with the standing
warning that no catalog synchronization is promised, so the fork's catalog
will diverge by design. A half-measure the map allows: keep Ziggy as the
inference edge and replace only the loop, which keeps admission, custody
and accounting intact.

### Porsche and Audi → the hub and remote attach

Upstream: Porsche (`ov:porsche/`, `ov:protocol/lib/overalls_protocol/ssh_command.ex`,
`ov:protocol/lib/overalls_protocol/terminal_attach.ex`, attach mode and
closed exit/close-code/header tables; gateway stream
`ov:marshall/lib/marshall_web/terminal_gateway_api.ex`; client commands
ADR-0222; Mila over Porsche SSH, ADR-0220) and Audi (authoritative DNS and
SSH gateway per
[ADR-0047](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0047-audi-ingress-and-authoritative-dns.md),
`ov:docs/architecture/decisions/0047-audi-ingress-and-authoritative-dns.md`;
fleet deployment on two DNS replicas, ADR-0090/ADR-0091; wildcard custody
ADR-0105; private `.internal` DNS/TLS, ADR-0063). Fiber side: the hub, which
speaks the session socket lines and accepts remote clients with terminal
powers; the person wraps the port themselves (`tailscale serve`, reverse
proxy, or `ssh -L`) and hubs never talk to each other (`docs/invocation.md`,
"Remote access" and hub sections).

Replace for machine access: the fork drops management-command SSH, the
terminal gateway stream and Audi-mediated DNS/SSH, and reaches the
machine's hub over the operator's own SSH or tailnet. Keep in sync with
upstream: the SSH command grammar and header/exit tables if the fork keeps
serving `overalls`-style clients; the gateway stream's ticket authority and
5-second recheck if it keeps browser attach; Audi placement/publication
authority and the 20-second A-record TTL cap if it keeps stable hostnames;
and terminal writer authority
(`ov:docs/architecture/decisions/0019-terminal-session-and-writer-authority.md`),
which is what makes shared attach safe.

## Q3. Subscriptions and per-call cost

### Can a machine use the owner's Claude or Codex subscription?

Codex, through Ziggy, indirectly yes. Ziggy routes OpenAI calls to a
host-local Underclass origin selected by the enrolled-host configuration
(ADR-0099 implementation evidence; ADR-0100), and Underclass is a Rust proxy
that pools Codex and Copilot subscriptions — OAuth tokens from a per-host
device flow — behind an OpenAI-compatible API
([ADR-0081](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0081-independent-underclass-pools.md),
`ov:docs/architecture/decisions/0081-independent-underclass-pools.md`;
[underclass.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/services/underclass.md),
`ov:docs/services/underclass.md`). So a guest Codex prompt travels
Codex → `llm.internal/openai/v1` → Ziggy → host Underclass → provider,
billed to the pooled subscription. Limits: only models of enabled providers
(prod: OpenAI only); each host's pool is onboarded separately with no
automatic failover; quota stickiness and catalogs may differ per host; and
pooling accounts is the operator's external acceptance — "Provider
subscription terms and live upstream behavior are external acceptance
obligations; source licensing does not establish permission to pool
accounts" (ADR-0081).

Claude subscription, through Ziggy: no path. Underclass pools Codex and
Copilot only; Ziggy's Anthropic side needs an operator `ANTHROPIC_API_KEY`
(API-key billing, not a subscription) and stays disabled in production
(`ov:docs/services/ziggy.md`).

By Fiber's own OAuth in the guest: Codex maybe, Claude no. ChatGPT/codex is
the only subscription login Fiber ships; the Claude subscription is reachable
only by running Claude Code, as a harness extension (`docs/model-routing.md`,
"Keys, tokens and OAuth"; `docs/delegates.md` quota row). A Fiber OAuth login
is extension code needing a browser, a one-shot localhost callback, and a
reachable token endpoint (`docs/model-routing.md`). Whether a guest's network
policy and terminal allow completing that flow is **not found** in the
sources read; and in-guest Claude Code points at Ziggy, which answers
`provider_unavailable` while Anthropic stays disabled.

### Per-call billing cost at the owner's usage

Subscriptions are flat, so the marginal per-call price on a subscription is
$0; the API-equivalent rates come from the owner's measured usage. From
`research/budgets/spend.md` (generated 2026-09-30, pi plus Claude Code logs
2026-07-25 to 2026-09-29) and `research/budgets/README.md`:

- pi: per-turn p50 $0.0174 (n=1465) over p50 8 model calls per turn, so about
  $0.002 per call; per-session p50 $0.05. $247 of pi's $296 ran through the
  ChatGPT/codex login — nearly all spend runs on subscriptions.
- Claude Code main: per-turn p50 $0.5297 over p50 4 calls per turn, so about
  $0.13 per call at API prices; per-session p50 $3.88. $3,470 API-equivalent
  over 34 days, on a $100 Max plan.

Through Overalls, billing visibility is token estimates, not subscription
metering: Ziggy cost estimates are token-only with model provenance, and
missing usage is unknown, never zero (`ov:docs/services/ziggy.md`);
Underclass records one token-usage row per upstream attempt and exposes
usage queries and read-only stats behind the admin token
(`ov:docs/services/underclass.md`). Subscription list prices are **not
found** in the sources read, except the $100 Max plan figure above; per-call
rate alone is therefore: ~$0 marginal on subscriptions, ~$0.002–0.13/call
API-equivalent at the owner's measured usage, token-estimated inside
Overalls.

## Q4. Durability: what survives a host or VM crash, read against #3878

#3878's problem: when a session's process crashes, its background jobs keep
running unwatched and its delegates shut down on the severed lifeline; on
resume the work is reported `orphaned`. Its candidate fixes are a per-job
keeper process holding the exit status on a socket (D1) and a delegate that
carries on and finishes (D2), both still undecided.

### VM crash, Fiber session running in the machine

A VM crash kills every guest process: the hub, sessions, keepers and
delegates. The guest disk persists across stop/start, and after a confirmed
unexpected crash Lilly restarts from the existing disk at most three times
in ten minutes, then marks the VM failed
([ADR-0005](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0005-vm-lifecycle-and-storage.md),
`ov:docs/architecture/decisions/0005-vm-lifecycle-and-storage.md`; placement
superseded by ADR-0037, crash budget carried forward). Per-VM actors keep a
durable journal so a launch attempt is never submitted twice across Lilly or
actor death
([ADR-0017](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0017-durable-vm-actors-and-host-ownership.md),
`ov:docs/architecture/decisions/0017-durable-vm-actors-and-host-ownership.md`).

Read against #3878: the Fiber session log on the guest disk survives, so a
resumed Fiber can replay history — but every #3878 mechanism dies with the
machine. A D1 keeper is a guest process holding a guest-local socket: the
crash takes the socket and the held exit status with it, so on resume the
job is `orphaned` exactly as today. A D2 carry-on delegate dies mid-task
with its parent. In other words, #3878 hardens Fiber against its own process
crashing, not against the machine crashing underneath it; inside a guest, a
VM crash replays the ticket's problem one layer up, with the disk as the
only survivor.

### The embed path: Marshall-owned thread state

A thread's durable state lives in control-plane PostgreSQL, not in the
machine. PostgreSQL is the sole durable application authority with fsync,
full-page writes and synchronous commit on the single-host deployment
([ADR-0048](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/architecture/decisions/0048-single-host-service-deployment.md),
`ov:docs/architecture/decisions/0048-single-host-service-deployment.md`).
Thread history even survives VM deletion until explicit user deletion
(ADR-0099). A claimed model call that loses its result becomes uncertain and
is never automatically replayed (ADR-0100) — the same no-rerun rule #3878
inherits from #40's "a crash ends the turn". So on the embed path, a VM
crash loses in-flight executor work (the guest half) while the thread,
its history and its terminal outcome accounting survive in Marshall.

### Host crash

Host failure can leave disks unavailable (ADR-0005 consequences). If devices
cannot be reattached, guest data is lost and only explicit destructive
reinstallation with reconciliation recovers
([host-provisioning.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/operations/host-provisioning.md),
`ov:docs/operations/host-provisioning.md`, "EC2 storage loss and deliberate
recovery"). Host recovery backups exist but are not enabled until an operator
supplies an S3 destination and encrypted backup document
([backups.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/operations/backups.md),
`ov:docs/operations/backups.md`). The single-host deployment makes no
availability-through-machine-loss claim (ADR-0048). Collateral the fork or
embed must count: Underclass pool loss needs provider re-onboarding and can
invalidate rotated refresh tokens (`ov:docs/services/underclass.md`); Ziggy
accounting spool recovery is designed not to repeat provider effects
(ADR-0100). So a Fiber session log on a guest disk survives VM crashes and
ordinary stop/start, survives host crashes only if host backups were enabled
beforehand, and a Marshall-owned thread survives both short of losing the
database host itself.

## Q5. Hosting: minimum hardware and monthly cost for one person

Linux-first, per the ticket. Minimum, from the provisioning and deployment
sources:

- x86_64 bare metal with working `/dev/kvm` (readiness actually creates and
  closes a KVM VM), root or passwordless sudo, Python 3, OpenSSH, lsblk and
  iproute2
  ([host-provisioning.md](https://github.com/ghuntley/overalls/blob/4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29/docs/operations/host-provisioning.md),
  `ov:docs/operations/host-provisioning.md`).
- On Hetzner: two physical disks in `storage.system`, a reviewed guest quota,
  and the `guestCpuCores` / `guestMemoryMiB` / `arcMaxMiB` budget triple
  (same source). A single host runs one Marshall, one Lilly and PostgreSQL
  18 together — an explicitly supported nonredundant deployment (ADR-0048).
- Real deployment inputs: a public DNS name and authenticated SMTP
  (ADR-0048); the application zone delegates to Audi DNS, which the fleet
  runs on two independent control-plane hosts
  (`ov:docs/services/audi.md`). Whether one host suffices for the DNS
  replica pair is **not found**.
- Hetzner Cloud VMs are KVM-less service hosts only (no Lilly), so they
  cannot host machines (`ov:docs/operations/host-provisioning.md`, "Hetzner
  Cloud hosts"). EC2 bare metal with local NVMe is the documented
  alternative.

Monthly cost, cheapest x86_64 with `/dev/kvm`, in USD/month convention
(Hetzner bills in EUR; convert at the current rate):

- AX41 (Ryzen 5 3600, 64 GB DDR4, 2×512 GB NVMe, 1 Gbit/s unlimited): from
  €59/month excluding VAT, no setup fee
  ([matrix](https://www.hetzner.com/dedicated-rootserver/matrix-ax/),
  [AX41](https://www.hetzner.com/dedicated-rootserver/ax41/), hetzner.com
  prices fetched 2026-11-01). Its two NVMe disks meet the two-physical-disk
  requirement. Next steps up: AX42 from €79/month plus €49 setup; Server
  Auction refurbished from €72.70/month (same site).
- A domain on top (registrar-dependent; **not found** in the sources read),
  plus an operator-supplied S3 destination if host backups are wanted
  (`ov:docs/operations/backups.md`). SMTP is a host-local relay
  (`ov:docs/architecture/decisions/0040-host-local-smtp-relay.md`) and
  PostgreSQL runs on the same host, so neither adds a service bill.

So one person self-hosts on one AX41-class host at about €59/month before
VAT plus a domain: single-host Marshall, Lilly and database included, no
failover, backups only with operator-supplied S3.

## Embed versus fork, at full strength

**Embed.** Upstream already solved the hard parts Fiber would otherwise
rebuild: durable agent authority in Marshall and PostgreSQL, bounded crash
recovery with fenced actors, edge-held provider credentials with admission
and accounting, pooled Codex/Copilot subscriptions behind one endpoint,
managed SSH/DNS/terminal access, and host recovery backups. A Fiber that
embeds rides all of it: preinstall puts the CLI in every machine, Ziggy
serves its model calls with subscription billing already pooled, Audi SSH
carries hub attach, and threads in Marshall's UI outlive any machine. The
price is upstream permission for every step — image, client policy, UI —
from a project that rejected custom launchers, removed two outside harnesses
from scope, keeps agent ownership server-side by decision, and launches in
two days with partial implementations across the cited ADRs. Fiber becomes a
guest of someone else's control plane, shaped by its fences: 20-second
commands, 4 KiB outputs, closed tools, no catalog sync, OpenAI-only
production inference.

**Fork.** A fork keeps the factory shape — Firecracker microVMs on ZFS,
per-machine disks and addresses, private inference, SSH and browser attach —
and replaces the brain: Fiber sessions instead of Marshall threads, Fiber
tools instead of the pulled executor, the session log instead of PostgreSQL
thread state, Fiber providers instead of Ziggy, the hub instead of
Porsche/Audi. Nothing needs upstream permission, and no 20-second command
ceiling or closed tool set constrains the agent. The price is owning every
hard part embed gets for free: durable agent authority moves to a log file
on a guest disk that dies with unbacked hosts, credential custody and
subscription pooling must be rebuilt (Underclass is a separate upstream
project with its own per-host onboarding and no failover), usage accounting
and audit rules must be re-implemented to the same standard, and every
protocol, fence and refusal rule in Q2's keep-in-sync list becomes a
divergence to track against a fast-moving MIT upstream that can be copied
but not steered.

Upstream read: `4d9501b92eeb8a4f8f944d73aa4fbfd1db864f29` (trunk, 2026-11-01).
Re-check after the 2026-11-03 launch: ADR statuses (0167 superseded,
0005/0037/0048 superseded, most partial), production provider enablement,
Claude/Anthropic readiness, and any first-class-agent API may all have
moved. No recommendation: the direction is the owner's call.
