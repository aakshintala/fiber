# Snapshot pages

About ten real pages under open licences, each at most 300 KiB and all
together at most 2 MiB. Each entry gives the source URL, the fetch date and
the licence. Fetched 2026-10-06 with `curl -sL`.

| File | Source URL | Licence |
|---|---|---|
| `rust-option.html` | <https://doc.rust-lang.org/std/option/enum.Option.html> | MIT/Apache-2.0 (the Rust standard library docs) |
| `rust-ownership.html` | <https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html> | MIT/Apache-2.0 (the Rust book) |
| `rust-data-types.html` | <https://doc.rust-lang.org/book/ch03-02-data-types.html> | MIT/Apache-2.0 (the Rust book) |
| `python-introduction.html` | <https://docs.python.org/3/tutorial/introduction.html> | PSF-2.0 (the Python docs) |
| `python-datastructures.html` | <https://docs.python.org/3/tutorial/datastructures.html> | PSF-2.0 (the Python docs) |
| `python-errors.html` | <https://docs.python.org/3/tutorial/errors.html> | PSF-2.0 (the Python docs) |
| `wiki-toml.html` | <https://en.wikipedia.org/wiki/Toml> | CC BY-SA 4.0 (Wikipedia) |
| `mdn-anchor.html` | <https://developer.mozilla.org/en-US/docs/Web/HTML/Element/a> | CC BY-SA 2.5 (MDN) |
| `mdn-table.html` | <https://developer.mozilla.org/en-US/docs/Web/HTML/Element/table> | CC BY-SA 2.5 (MDN) |
| `mdn-code.html` | <https://developer.mozilla.org/en-US/docs/Web/HTML/Element/code> | CC BY-SA 2.5 (MDN) |

Each `<name>.md` beside a page is its expected markdown. The snapshot test
(`pages_tests.rs`) writes an expected file only when `FIBER_UPDATE_PAGES=1`
is set and `CI` is not; otherwise a difference fails with a unified diff,
and a missing expected file fails.
