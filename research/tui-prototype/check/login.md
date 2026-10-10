# Login (#1736)

One run per case: `--login CASE`.

- providers: four providers under `Providers` with `browser` or `key` tags telling OAuth from key providers, and two extension credentials under `Secrets`; a centred panel with ▄ ▀ edges and the ▌ stripe, `Log in` bold accent with a dim ✕, the focused row `›` on a full-width accent bar, and a bold-key legend foot.
- waiting: the panel titled `anthropic` with no provider list: `Open this URL to log in to anthropic:`, the long URL cut from the left keeping its tail, and a dim `Waiting for the browser…` line; a bold-key `y copy URL · Esc back` legend; the provider list's height and one width across the steps (a few cells wider than the list, for the longest legend).
- key: the panel titled `google` with no provider list: `Label (--as): default.` and `Key for google:`, the key masked as eight dots with a block cursor; a bold-key `Tab key or label · Enter submit · Esc back` legend; the provider list's height and one width across the steps (a few cells wider than the list, for the longest legend).
- done: the panel titled `anthropic` with no provider list: a `✓ Logged in to anthropic.` outcome line in the success colour; a bold-key `Esc back` legend; the provider list's height and one width across the steps (a few cells wider than the list, for the longest legend).
- failed: the panel titled `anthropic` with no provider list: a `Login to anthropic failed: token expired.` outcome line, the reason in the error colour; a bold-key `Esc back` legend; the provider list's height and one width across the steps (a few cells wider than the list, for the longest legend).
