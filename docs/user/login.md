# Signing in

Fiber signs in per provider. The two sign-ins in the friends release are **codex** (ChatGPT login) and **opencode-go** (Muse). How labels, stored credentials and the reviewer pick work is [Logging in](../model-routing.md), [Credentials](../model-routing.md), and [Keys, tokens and OAuth](../model-routing.md).

## codex

`fiber login codex` signs in with the ChatGPT subscription through the browser. On a machine without a browser, add `--device` to sign in with a device code instead. The full flag list is [Commands and flags](../invocation.md).

codex is a subscription login: the only other login of that kind Fiber ships, and the reviewer prompt below still applies to it.

## opencode-go

`fiber login opencode-go` stores one Muse key, read from `OPENCODE_API_KEY`. The same stored key serves both OpenCode providers (`opencode-go` and `opencode-zen`), which name one shared stored credential, so signing in once covers both.

## Labels

`fiber login <provider> --as <label>` stores the credential under that label. Without `--as`, the label is the account's email when the login reveals one, and `default` otherwise. A login whose label is already stored is refused, and the message names `--as`. The first label a provider stores becomes its default.

`fiber logout <provider>` deletes a stored key; with several labels it needs `--as` or `--all`.

## After the key is stored

The login asks which model reviews that provider's calls, with the saved pick preselected, else the package default. The pick is saved per provider. Without a terminal the credential is stored and nothing is asked.

## From inside the terminal

`/login` signs in from the terminal with the same flow as `fiber login`: a provider list, then a browser wait, a device-code step, or a key entry, then the same reviewer step. The steps are [Logging in](../tui.md).

`/logout` deletes a stored key from the terminal with the same steps as `fiber logout`: a label list, a confirm naming the provider and label, then the removed line. The steps are [Logging out](../tui.md).
