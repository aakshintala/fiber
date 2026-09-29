import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rules  # noqa: E402

MEMBERS = {"log": {"dir": "crates/log", "deps": []}}

CODE_QUALITY = """# Code quality

## `unsafe`

| Crate | File | Why |
|---|---|---|
| none yet | | |

## Types
"""

LISTED = CODE_QUALITY.replace("| none yet | | |", "| `tools` | `crates/tools/src/pty.rs` | pre_exec |")

DEPENDENCIES = """# Dependencies

## Runtime dependencies

| Crate | Used for |
|---|---:|
| serde, serde_json | wire formats |
| all of the above together | |

## Waiting on other decisions

| Crate | Needed if |
|---|---|
| rusqlite, SQLite bundled | a database |

## Tests and development tools

| Crate or tool | Kind | Used for |
|---|---|---|
| insta | dev-dependency | snapshots |
"""


class LineCap(unittest.TestCase):
    def test_fails_a_source_file_over_800_lines(self):
        files = [("log", "crates/log/src/big.rs"), ("log", "crates/log/src/ok.rs")]
        sizes = {"crates/log/src/big.rs": 801, "crates/log/src/ok.rs": 800}
        failures = rules.over_cap(files, MEMBERS, read=lambda p: "x\n" * sizes[p])
        self.assertEqual(failures, ["crates/log/src/big.rs: 801 lines, over the 800-line cap"])

    def test_test_files_have_no_cap(self):
        files = [("log", "crates/log/src/tests.rs"), ("log", "crates/log/src/a_tests.rs"), ("log", "crates/log/tests/t.rs")]
        self.assertEqual(rules.over_cap(files, MEMBERS, read=lambda p: "x\n" * 5000), [])


class UnsafeTable(unittest.TestCase):
    def test_finds_unsafe_outside_comments(self):
        self.assertTrue(rules.uses_unsafe("fn f() { unsafe { g() } }"))
        self.assertTrue(rules.uses_unsafe("unsafe fn f() {}"))
        self.assertFalse(rules.uses_unsafe("#![deny(unsafe_code)]\n// unsafe here\n/* unsafe\n */ fn f() {}"))

    def test_passes_when_code_and_table_agree(self):
        self.assertEqual(rules.unsafe_mismatches([("log", "crates/log/src/lib.rs")], CODE_QUALITY, read=lambda p: "fn f() {}"), [])
        files = [("tools", "crates/tools/src/pty.rs")]
        self.assertEqual(rules.unsafe_mismatches(files, LISTED, read=lambda p: "unsafe { x() }"), [])

    def test_fails_unsafe_the_table_does_not_list(self):
        failures = rules.unsafe_mismatches([("log", "crates/log/src/lib.rs")], CODE_QUALITY, read=lambda p: "unsafe { x() }")
        self.assertEqual(failures, ["crates/log/src/lib.rs: uses unsafe, but the table in docs/code-quality.md does not list it"])

    def test_fails_a_listed_file_without_unsafe(self):
        failures = rules.unsafe_mismatches([("tools", "crates/tools/src/pty.rs")], LISTED, read=lambda p: "fn f() {}")
        self.assertEqual(failures, ["crates/tools/src/pty.rs: listed for tools in docs/code-quality.md, but uses no unsafe"])


class DependencyList(unittest.TestCase):
    def test_reads_the_admitted_tables_only(self):
        self.assertEqual(rules.admitted(DEPENDENCIES), {"serde", "serde_json", "insta"})

    def test_fails_a_dependency_that_is_not_listed(self):
        deps = {("contract", "serde"), ("log", "insta"), ("log", "rusqlite")}
        self.assertEqual(rules.unlisted(deps, DEPENDENCIES), ["log depends on rusqlite, which docs/dependencies.md does not list"])


if __name__ == "__main__":
    unittest.main()
