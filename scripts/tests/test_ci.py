import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import ci  # noqa: E402

MEMBERS = {
    "contract": {"dir": "crates/contract", "deps": []},
    "log": {"dir": "crates/log", "deps": ["contract"]},
    "config": {"dir": "crates/config", "deps": ["contract"]},
    "loop": {"dir": "crates/loop", "deps": ["contract", "log"]},
    "loop-extra": {"dir": "crates/loop/extra", "deps": []},
}


class Classify(unittest.TestCase):
    def test_markdown_docs_and_research_run_the_docs_job_alone(self):
        files = ["docs/ci.md", "README.md", "crates/loop/prompt/system.md", "research/x/run.sh"]
        self.assertEqual(ci.classify(files, MEMBERS), {"mode": "docs", "packages": []})

    def test_manifests_toolchain_and_workflows_run_everything(self):
        everything = sorted(MEMBERS)
        for trigger in ["Cargo.lock", "crates/log/Cargo.toml", "rust-toolchain.toml", ".github/workflows/ci.yml"]:
            with self.subTest(trigger=trigger):
                result = ci.classify(["docs/ci.md", trigger], MEMBERS)
                self.assertEqual(result, {"mode": "all", "packages": everything})

    def test_a_crate_change_runs_it_and_its_dependents(self):
        result = ci.classify(["crates/contract/src/lib.rs"], MEMBERS)
        self.assertEqual(result, {"mode": "crates", "packages": ["config", "contract", "log", "loop"]})

    def test_dependents_are_transitive(self):
        self.assertEqual(ci.dependents({"log"}, MEMBERS), {"log", "loop"})

    def test_a_leaf_crate_runs_alone(self):
        result = ci.classify(["crates/config/src/lib.rs", "docs/configuration.md"], MEMBERS)
        self.assertEqual(result, {"mode": "crates", "packages": ["config"]})

    def test_the_innermost_crate_owns_a_file(self):
        self.assertEqual(ci.owner("crates/loop/extra/src/lib.rs", MEMBERS), "loop-extra")
        self.assertEqual(ci.owner("crates/loop/src/lib.rs", MEMBERS), "loop")
        self.assertIsNone(ci.owner("crates/logger/src/lib.rs", MEMBERS))

    def test_code_outside_every_crate_runs_no_crate(self):
        result = ci.classify(["scripts/ci.py"], MEMBERS)
        self.assertEqual(result, {"mode": "crates", "packages": []})


class Plan(unittest.TestCase):
    def test_shards_are_one_per_25_mutants_at_most_6(self):
        cases = {0: 0, 1: 1, 25: 1, 26: 2, 150: 6, 151: 6, 1000: 6}
        for mutants, shards in cases.items():
            with self.subTest(mutants=mutants):
                self.assertEqual(ci.shard_count(mutants), shards)

    def test_a_docs_only_pull_request_runs_the_docs_job_alone(self):
        result = ci.plan({"mode": "docs", "packages": []}, "pull_request", True, 40)
        self.assertEqual(
            result,
            {"jobs": {"docs": True, "lint": False, "test": False, "mutants": False, "bug_base": False}, "shards": []},
        )

    def test_a_code_pull_request_runs_what_it_selected(self):
        result = ci.plan({"mode": "crates", "packages": ["log"]}, "pull_request", True, 40)
        self.assertEqual(
            result,
            {"jobs": {"docs": True, "lint": True, "test": True, "mutants": True, "bug_base": True}, "shards": [0, 1]},
        )

    def test_a_pull_request_that_selects_no_crate_skips_the_tests(self):
        jobs = ci.plan({"mode": "crates", "packages": []}, "pull_request", False, 0)["jobs"]
        self.assertEqual(jobs, {"docs": True, "lint": True, "test": False, "mutants": False, "bug_base": False})

    def test_the_backstop_runs_the_tests_alone(self):
        result = ci.plan({"mode": "docs", "packages": []}, "push", True, 40)
        self.assertEqual(
            result,
            {"jobs": {"docs": False, "lint": False, "test": True, "mutants": False, "bug_base": False}, "shards": []},
        )


class Verdict(unittest.TestCase):
    JOBS = {"docs": True, "test": True, "mutants": False}

    def needs(self, **results):
        return {name: {"result": result} for name, result in results.items()}

    def test_passes_when_selected_jobs_passed_and_the_rest_skipped(self):
        needs = self.needs(select="success", docs="success", test="success", mutants="skipped")
        self.assertEqual(ci.verdict(needs, self.JOBS), [])

    def test_fails_when_a_selected_job_failed_or_was_skipped(self):
        for result in ["failure", "cancelled", "skipped"]:
            with self.subTest(result=result):
                needs = self.needs(select="success", docs="success", test=result, mutants="skipped")
                self.assertEqual(ci.verdict(needs, self.JOBS), [f"test: selected, but {result}"])

    def test_fails_when_an_unselected_job_ran(self):
        needs = self.needs(select="success", docs="success", test="success", mutants="success")
        self.assertEqual(ci.verdict(needs, self.JOBS), ["mutants: not selected, but success"])

    def test_fails_when_the_selection_fails(self):
        needs = self.needs(select="failure", docs="skipped", test="skipped", mutants="skipped")
        self.assertEqual(ci.verdict(needs, {}), ["select: failure, so the selection failed"])

    def test_fails_on_a_job_the_selection_does_not_name(self):
        needs = self.needs(select="success", docs="success", test="success", mutants="skipped", extra="skipped")
        self.assertEqual(ci.verdict(needs, self.JOBS), ["extra: not in the selection"])


class Ticket(unittest.TestCase):
    def test_reads_the_resolved_issue(self):
        self.assertEqual(ci.ticket("Does a thing.\n\nResolves #212\n"), "212")
        self.assertEqual(ci.ticket("fixes #7"), "7")
        self.assertEqual(ci.ticket("Closed #9."), "9")

    def test_finds_none_without_a_keyword(self):
        self.assertIsNone(ci.ticket("See #212"))
        self.assertIsNone(ci.ticket(""))
        self.assertIsNone(ci.ticket(None))


class TestFilter(unittest.TestCase):
    def test_maps_test_files_to_their_tests(self):
        files = [
            "crates/log/src/writer_tests.rs",
            "crates/log/src/fold/tests.rs",
            "crates/log/src/tests.rs",
            "crates/loop/tests/turns.rs",
            "crates/loop/tests/support/mod.rs",
            "crates/log/src/writer.rs",
            "docs/events.md",
        ]
        expression, packages = ci.test_filter(files, MEMBERS)
        self.assertEqual(
            expression.split(" | "),
            [
                r"(package(log) & test(/^writer::tests::/))",
                r"(package(log) & test(/^fold::tests::/))",
                r"(package(log) & test(/^tests::/))",
                "binary_id(loop::turns)",
                "binary_id(loop::support)",
            ],
        )
        self.assertEqual(packages, ["log", "loop"])

    def test_no_test_files_gives_an_empty_filter(self):
        self.assertEqual(ci.test_filter(["crates/log/src/writer.rs"], MEMBERS), ("", []))

    def test_test_files_are_named_or_under_tests(self):
        self.assertTrue(ci.is_test_file("src/tests.rs"))
        self.assertTrue(ci.is_test_file("src/a/b_tests.rs"))
        self.assertTrue(ci.is_test_file("tests/journey.rs"))
        self.assertFalse(ci.is_test_file("src/tests_helper.rs"))
        self.assertFalse(ci.is_test_file("src/a/tests/fixture.rs"))


if __name__ == "__main__":
    unittest.main()
