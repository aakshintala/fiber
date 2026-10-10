# Completions (#1631)

One run per case: `--completions CASE`.

- slash: the panel sits above the input box in the shared frame with ▄ ▀ edges and the ▌ stripe; eight rows of name in one fixed column, dim description and right-aligned tag, the focused row `›` on a full-width accent bar, sized to its content and centred, with a `1–8 of 40 · ↓ 32 more` footer.
- slash-filtered: the input reads `/re`, only matching rows show, matched letters bold; same frame, names aligned, first row barred.
- slash-hint: the `review` row `›` on the accent bar with its `<path>` hint, and the `login` description cut with … keeping its `command` tag; same frame.
- at: the input reads `@test`, two file rows (lock.rs, cancel.rs) in the shared frame, the first `›` on the accent bar.
- at-empty: the input reads `@zzz`, one dim `no files match` row in the shared frame.
- narrow-slash: the same framed `/` panel in a 100x40 terminal, above the input box with the narrow status rows below.
- narrow-at: the same framed `@` panel in a 100x40 terminal, above the input box with the narrow status rows below.
