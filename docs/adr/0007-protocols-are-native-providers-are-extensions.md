# 7. Protocols are native, providers are extensions

Date: 2026-09-22

## Status

Accepted. Settled by
[Provider and model routing](https://github.com/aakshintala/fiber/issues/12)
and
[Providers: the full set Fiber ships](https://github.com/aakshintala/fiber/issues/209).
The contract is `docs/model-routing.md`.

## Context

Fiber ships eleven first-party providers: Anthropic, OpenAI, the Gemini API,
ChatGPT/codex, OpenRouter, OpenCode, Databricks, muse, AWS Bedrock, Google
Vertex and Azure. A first-party provider is one Fiber can probe and re-record.
A person must be able to add or fix a provider without rebuilding Fiber.
Providers differ in two ways that change at different rates.

Wire formats are few and stable. The eleven providers need five between them:
Anthropic Messages, OpenAI Chat Completions, OpenAI Responses, Google's
Generative AI API and Bedrock Converse. ChatGPT/codex speaks Responses. Its
differences are request fields, headers from its login and one error body, not
how a stream is parsed
(`research/provider-harvest/openai-codex-responses.md`), so they are declared
flags on the Responses protocol. Claude on Bedrock speaks Anthropic Messages in
AWS event-stream framing, which is also a declared flag. Parsing a streamed
reply correctly is the hardest code in the provider layer. pi lists 43 vendor
quirks it absorbs there.

Everything else about a provider is many and changes often: which models it
serves, which base URL each one uses, which flags a vendor's version of a format
needs. The owner's pi extension for the Databricks gateway is the example. It
routes about 53 models across three of those formats and two base URLs, and sets
per-model flags. None of that is wire parsing.

Authentication varies more than wire formats do. Most vendors take a key. The
clouds use their own schemes: a token that expires, or a signature over each
request, as AWS SigV4 is. Subscription logins each add vendor steps after the
OAuth handshake: codex reads an account id from its token, and Copilot swaps
its GitHub token for a second one. Anthropic's and Google's terms forbid their
subscription logins in other harnesses.

## Decision

Protocols are native Rust in the `provider` module, and an extension cannot add
one. Every provider is an extension, the eleven first-party ones included, and
extensions are fetched and installed rather than built into the binary. The
one exception is the `scripted` provider, which reaches no network and reads a
script file, so an extension can be tested with tooling everyone has
(`docs/testing.md`, "Testing an extension").

A provider is data, plus up to four optional Lua functions: `models()`
discovers its model list, `quota()` reports its quota, `credential()` returns a
token and its expiry, and `sign()` adds headers to a request. Only `sign()`
runs on the request path. It receives the method, the URL, the headers and the
SHA-256 of the body, and returns headers to add. It never sees or changes the
body.

Authentication beyond a key is extension code, not Rust. An OAuth login is an
extension's `credential()`, built on native host calls for the parts every
flow shares. The lock that makes two sessions refresh a token once stays
native.

ChatGPT/codex is the only subscription login Fiber ships. A subscription login
ships when its vendor permits use from other harnesses and Fiber can probe it.

## Consequences

- A vendor with a genuinely new wire format needs a Fiber release. pi lets an
  extension supply its own stream parser. Fiber does not, because that is a
  second, untested implementation of the hardest code.
- The binary alone has no providers. Installing Fiber installs every
  first-party extension, the eleven providers included, from the release's
  extensions archive, and `fiber update` updates them with the binary. They
  stay ordinary extensions: a person may remove any of them, and fixing a
  vendor quirk or a cloud's sign-in is an extension update, not a release. See
  [Extension distribution](https://github.com/aakshintala/fiber/issues/45).
- A session that uses a provider with a Lua function creates a Lua VM, about
  120 KiB (`research/extension-runtime/vm-isolation`). A session whose
  providers are pure data creates none.
- A provider with `sign()` puts Lua on the request path. It is bounded by its
  declared timeout like every callback, and a retry signs again.
- Every compatibility flag is declared. Fiber never infers one from a URL or a
  provider id, as pi does.
- A provider that cannot be probed, such as a vendor whose subscription Fiber
  does not hold, is left to the community.

## Rejected

Built-in providers compiled into the binary, with extensions only for other
providers. Every provider would work on a fresh install, but fixing a vendor
quirk would need a release. It would also leave two ways to define a provider.

Lua protocols as an escape hatch. It covers a new vendor format without a
release, and models write working Lua providers reliably
(`research/extension-runtime/pass3`: 14 of 15 ran first try). It was rejected
because nearly every vendor speaks one of the five formats, and the escape hatch
would be the least-tested path through the most fragile code.

Native OAuth flows and cloud authentication in Rust. Each vendor adds its own
steps after the handshake, so a native flow grows a branch per vendor, and a
vendor's change needs a Fiber release.

A `sign()` that can change the body. No scheme Fiber ships needs it. It can be
revisited when one does.
