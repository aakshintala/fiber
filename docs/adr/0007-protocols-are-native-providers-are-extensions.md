# 7. Protocols are native, providers are extensions

Date: 2026-09-22

## Status

Accepted. Settled by
[Provider and model routing](https://github.com/aakshintala/fiber/issues/12).
The contract is `docs/model-routing.md`.

## Context

Fiber has to reach five providers in v0.0.1, and a person must be able to add
or fix a provider without rebuilding Fiber. Providers differ in two ways that
change at different rates.

Wire formats are few and stable. The five providers need four between them:
Anthropic Messages, OpenAI Chat Completions, OpenAI Responses and Google's
Generative AI API. ChatGPT/codex speaks Responses. Its differences are request
fields, headers from its login and one error body, not how a stream is parsed
(`research/provider-harvest/openai-codex-responses.md`), so they are declared
flags on the Responses protocol. Parsing a streamed reply
correctly is the hardest code in the provider layer. pi lists 43 vendor quirks
it absorbs there.

Everything else about a provider is many and changes often: which models it
serves, which base URL each one uses, which flags a vendor's version of a format
needs. The owner's pi extension for the Databricks gateway is the example. It
routes about 53 models across three of those formats and two base URLs, and sets
per-model flags. None of that is wire parsing.

## Decision

Protocols are native Rust in the `provider` module, and an extension cannot add
one. Every provider is an extension, the five v0.0.1 ships included, and
extensions are fetched and installed rather than built into the binary. A
provider is data, plus an optional Lua function that discovers its model list.
Provider Lua never runs on the request path.

## Consequences

- A vendor with a genuinely new wire format needs a Fiber release. pi lets an
  extension supply its own stream parser. Fiber does not, because that is a
  second, untested implementation of the hardest code.
- The binary alone has no providers. Installing Fiber also installs the five
  first-party provider extensions, and fixing a vendor quirk is an extension
  update, not a release. See
  [Extension distribution](https://github.com/aakshintala/fiber/issues/45).
- A session that uses a provider with a discovery function creates a Lua VM,
  about 120 KiB (`research/extension-runtime/vm-isolation`). A session whose
  providers are pure data creates none.
- Every compatibility flag is declared. Fiber never infers one from a URL or a
  provider id, as pi does.

## Rejected

Built-in providers compiled into the binary, with extensions only for a sixth
provider. Every provider would work on a fresh install, but fixing a vendor
quirk would need a release. It would also leave two ways to define a provider.

Lua protocols as an escape hatch. It covers a new vendor format without a
release, and models write working Lua providers reliably
(`research/extension-runtime/pass3`: 14 of 15 ran first try). It was rejected
because nearly every vendor speaks one of the four formats, and the escape hatch
would be the least-tested path through the most fragile code.
