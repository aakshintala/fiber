# Completions (#1631)

One run per case: `--completions CASE`.

- slash: the panel sits above the input box, eight rows of name, dim description and right-aligned tag, with a `1–8 of 40 · ↓ 32 more` footer.
- slash-filtered: the input reads `/re`, only matching rows show, matched letters bold.
- slash-hint: the `review` row focused with its orange `<path>` hint, and the `login` description cut with … keeping its `command` tag.
- at: the input reads `@test`, two file rows (lock.rs, cancel.rs).
- at-empty: the input reads `@zzz`, one dim `no files match` row.
- narrow-slash: the same `/` panel in a 100x40 terminal, above the input box with the narrow status rows below.
- narrow-at: the same `@` panel in a 100x40 terminal, above the input box with the narrow status rows below.
