import tempfile
import unittest
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path

_loader = SourceFileLoader("check_docs", str(Path(__file__).resolve().parent.parent / "check-docs"))
check_docs = module_from_spec(spec_from_loader("check_docs", _loader))
_loader.exec_module(check_docs)

TARGET = """# Target

## The gate

## `unsafe`

```
## Not a heading
```
"""


class CheckDocs(unittest.TestCase):
    def check(self, source):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir()
            (root / "scripts").mkdir()
            (root / "docs/target.md").write_text(TARGET)
            (root / "docs/source.md").write_text(source)
            return check_docs.check_file("docs/source.md", root)

    def test_passes_links_citations_and_paths_that_resolve(self):
        source = (
            "[t](target.md#the-gate) [u](target.md#unsafe) [s](#top) [x](https://example.com/nope)\n"
            '(`docs/target.md`, "The gate") (`target.md`, "`unsafe`")\n'
            "`scripts/` `docs/<area>.md` `docs/*.md`\n"
            "# Top\n"
        )
        self.assertEqual(self.check(source), [])

    def test_fails_a_link_to_a_missing_file_or_anchor(self):
        source = "[a](missing.md) [b](target.md#nope)\n"
        self.assertEqual(
            self.check(source),
            ["docs/source.md:1: link to missing.md: no such file", "docs/source.md:1: link to target.md#nope: no such anchor"],
        )

    def test_fails_a_citation_of_a_missing_heading_even_across_lines(self):
        source = 'Intro\n(`docs/target.md`,\n"The\nold gate")\n(`docs/target.md`, "Not a heading")\n'
        self.assertEqual(
            self.check(source),
            [
                'docs/source.md:2: citation of docs/target.md, "The old gate": no such heading',
                'docs/source.md:5: citation of docs/target.md, "Not a heading": no such heading',
            ],
        )

    def test_fails_a_citation_of_a_missing_file(self):
        self.assertEqual(
            self.check('(`docs/gone.md`, "The gate")\n'),
            [
                "docs/source.md:1: citation of docs/gone.md: no such file",
                "docs/source.md:1: path docs/gone.md: does not exist",
            ],
        )

    def test_fails_a_backticked_path_that_does_not_exist(self):
        self.assertEqual(self.check("`scripts/check`\n"), ["docs/source.md:1: path scripts/check: does not exist"])

    def test_ignores_fenced_code(self):
        self.assertEqual(self.check("```\n[a](missing.md) `scripts/nope`\n```\n"), [])

    def test_anchors_follow_github(self):
        self.assertEqual(check_docs.slug("Tests and development tools"), "tests-and-development-tools")
        self.assertEqual(check_docs.slug("`unsafe`"), "unsafe")
        self.assertEqual(check_docs.slug("What a panic leaves"), "what-a-panic-leaves")
        self.assertEqual(check_docs.anchors("# A\n## A\n"), {"a", "a-1"})


if __name__ == "__main__":
    unittest.main()
