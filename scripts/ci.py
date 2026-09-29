#!/usr/bin/env python3
"""CI selection and the verdict behind the `CI` check (docs/ci.md).

Subcommands:
  select --base REV       what a diff from REV runs, as JSON
  plan SELECTION.json     which CI jobs run, as GitHub step outputs
  verdict                 exit 0 only if every selected job passed and every
                          other job was skipped
  ticket                  the issue a pull request body resolves
  bug-filter FILE...      the nextest filter for the tests in FILE...
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import PurePosixPath

# docs/ci.md, "Selection".
RUN_ALL_NAMES = {"Cargo.lock", "Cargo.toml", "rust-toolchain.toml"}
# docs/ci.md, "On every pull request that changes code": one shard per 25
# mutants, at most 6.
MUTANTS_PER_SHARD = 25
MAX_SHARDS = 6


def is_docs_file(path):
    return path.endswith(".md") or path.startswith(("docs/", "research/"))


def runs_all(path):
    return PurePosixPath(path).name in RUN_ALL_NAMES or path.startswith(".github/")


def owner(path, members):
    """The workspace member whose directory holds `path`, or None."""
    matches = [n for n, m in members.items() if path.startswith(m["dir"] + "/")]
    return max(matches, key=lambda n: len(members[n]["dir"]), default=None)


def dependents(touched, members):
    """`touched` plus every member that depends on one of them, transitively."""
    selected = set(touched)
    changed = True
    while changed:
        changed = False
        for name, member in members.items():
            if name not in selected and selected & set(member["deps"]):
                selected.add(name)
                changed = True
    return selected


def classify(files, members):
    """What a diff touching `files` runs: docs alone, everything, or crates."""
    if all(is_docs_file(f) for f in files):
        return {"mode": "docs", "packages": []}
    if any(runs_all(f) for f in files):
        return {"mode": "all", "packages": sorted(members)}
    touched = {owner(f, members) for f in files} - {None}
    return {"mode": "crates", "packages": sorted(dependents(touched, members))}


def shard_count(mutants):
    if mutants <= 0:
        return 0
    return min(MAX_SHARDS, -(-mutants // MUTANTS_PER_SHARD))


def plan(selection, event, bug, mutants):
    """Which jobs run for `selection` on `event`, and the mutant shards."""
    pr = event == "pull_request"
    code = selection["mode"] != "docs"
    shards = shard_count(mutants) if pr and code else 0
    jobs = {
        "docs": pr,
        "lint": pr and code,
        # The backstop on `main` compiles the whole workspace on every push.
        "test": not pr or bool(selection["packages"]),
        "mutants": shards > 0,
        "bug_base": pr and code and bug,
    }
    return {"jobs": jobs, "shards": list(range(shards))}


def verdict(needs, jobs):
    """Failures, one line each; empty when `CI` passes."""
    failures = []
    select = needs.get("select", {}).get("result")
    if select != "success":
        failures.append(f"select: {select}, so the selection failed")
        return failures
    for name, need in sorted(needs.items()):
        if name == "select":
            continue
        result = need.get("result")
        if name not in jobs:
            failures.append(f"{name}: not in the selection")
        elif jobs[name] and result != "success":
            failures.append(f"{name}: selected, but {result}")
        elif not jobs[name] and result != "skipped":
            failures.append(f"{name}: not selected, but {result}")
    return failures


TICKET = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)\b", re.IGNORECASE)


def ticket(body):
    match = TICKET.search(body or "")
    return match.group(1) if match else None


def is_test_file(rel):
    """Whether `rel`, a path relative to its crate, is a test file
    (docs/code-quality.md, "Size")."""
    parts = PurePosixPath(rel).parts
    name = parts[-1] if parts else ""
    return name == "tests.rs" or name.endswith("_tests.rs") or parts[:1] == ("tests",)


def test_filter(files, members):
    """The nextest filter selecting every test in the test files among
    `files`, and the packages that own them."""
    terms, packages = [], set()
    for path in files:
        name = owner(path, members)
        if name is None or not path.endswith(".rs"):
            continue
        rel = PurePosixPath(path).relative_to(members[name]["dir"])
        if not is_test_file(rel):
            continue
        parts = list(rel.parts)
        if parts[0] == "tests":
            binary = PurePosixPath(parts[1]).stem
            terms.append(f"binary_id({name}::{binary})")
        elif parts[0] == "src":
            modules = parts[1:-1]
            stem = PurePosixPath(parts[-1]).stem
            if stem != "tests":
                modules.append(stem[: -len("_tests")])
            prefix = "::".join(modules + ["tests", ""])
            terms.append(f"(package({name}) & test(/^{re.escape(prefix)}/))")
        else:
            continue
        packages.add(name)
    return " | ".join(terms), sorted(packages)


def workspace_members():
    meta = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"], text=True)
    )
    root = meta["workspace_root"]
    dirs = {}
    for package in meta["packages"]:
        dirs[package["name"]] = os.path.relpath(os.path.dirname(package["manifest_path"]), root)
    by_dir = {d: n for n, d in dirs.items()}
    members = {}
    for package in meta["packages"]:
        deps = []
        for dep in package["dependencies"]:
            if dep.get("path"):
                other = by_dir.get(os.path.relpath(dep["path"], root))
                if other:
                    deps.append(other)
        members[package["name"]] = {"dir": dirs[package["name"]], "deps": deps}
    return members


def git_lines(*args):
    return [line for line in subprocess.check_output(["git", *args], text=True).splitlines() if line]


def changed_files(base):
    """Files that differ from the merge base with `base`, uncommitted and
    untracked changes included."""
    merge_base = git_lines("merge-base", base, "HEAD")[0]
    files = set(git_lines("diff", "--name-only", merge_base))
    files |= set(git_lines("ls-files", "--others", "--exclude-standard"))
    return sorted(files)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("select")
    p.add_argument("--base", required=True)
    p = sub.add_parser("plan")
    p.add_argument("selection")
    p.add_argument("--event", required=True)
    p.add_argument("--bug", default="false")
    p.add_argument("--mutants", type=int, default=0)
    sub.add_parser("verdict")
    sub.add_parser("ticket")
    p = sub.add_parser("bug-filter")
    p.add_argument("files", nargs="*")
    args = parser.parse_args(argv)

    if args.command == "select":
        selection = classify(changed_files(args.base), workspace_members())
        print(json.dumps(selection))
    elif args.command == "plan":
        with open(args.selection) as f:
            selection = json.load(f)
        result = plan(selection, args.event, args.bug == "true", args.mutants)
        print(f"mode={selection['mode']}")
        print(f"packages={' '.join(selection['packages'])}")
        print(f"jobs={json.dumps(result['jobs'])}")
        print(f"shards={json.dumps(result['shards'])}")
        print(f"shard_total={len(result['shards'])}")
    elif args.command == "verdict":
        failures = verdict(json.loads(os.environ["NEEDS"]), json.loads(os.environ.get("JOBS") or "{}"))
        for line in failures:
            print(line)
        if failures:
            return 1
        print("every selected job passed and every other job was skipped")
    elif args.command == "ticket":
        number = ticket(sys.stdin.read())
        if number:
            print(number)
    elif args.command == "bug-filter":
        expression, packages = test_filter(args.files, workspace_members())
        print(json.dumps({"filter": expression, "packages": packages}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
