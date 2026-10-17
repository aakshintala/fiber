# Quota view (#2526)

One run per case: `--quota-case CASE`.

- quota: the `Quota` header with `  /quota` dim, `fetching` dim and `Esc returns` dim right-aligned; `codex · default` with two windows, each a dim name, its percent and its bar, then its reset time dim (`resets in 3h12m`, `resets in 2d4h`); `claude · default` with its last value and `unreachable` in the error colour; nothing clipped.
- quota-empty: the same header without `fetching`, and one dim line `No provider reports quota` in place of the rows.
