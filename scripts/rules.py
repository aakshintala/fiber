#!/usr/bin/env python3
"""Checks that the code matches the rules and tables in the docs.

Subcommands, each named for what it checks:
  line-cap          no non-test Rust source file over 800 lines
                    (docs/code-quality.md, "Size")
  unsafe-table      the `unsafe` table in docs/code-quality.md matches the code
  dependency-list   every crate a Cargo.toml names is listed in
                    docs/dependencies.md
"""

import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

sys.path.insert(0, str(Path(__file__).resolve().parent))

from ci import is_test_file, owner, workspace_members  # noqa: E402

LINE_CAP = 800
UNSAFE = re.compile(r"\bunsafe\b")


def rust_files(members):
    """(crate, path relative to the repository) for every Rust file in a
    member, tracked or not, skipping ignored ones."""
    dirs = [m["dir"] for m in members.values()]
    listed = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", *dirs], text=True
    ).splitlines()
    return [(owner(p, members), p) for p in sorted(set(listed)) if p.endswith(".rs") and Path(p).exists()]


def over_cap(files, members, read=lambda p: Path(p).read_text()):
    failures = []
    for crate, path in files:
        rel = PurePosixPath(path).relative_to(members[crate]["dir"])
        if is_test_file(rel):
            continue
        lines = read(path).count("\n")
        if lines > LINE_CAP:
            failures.append(f"{path}: {lines} lines, over the {LINE_CAP}-line cap")
    return failures


def uses_unsafe(source):
    """Whether Rust `source` uses `unsafe`, outside comments."""
    code = re.sub(r"/\*.*?\*/", "", source, flags=re.DOTALL)
    code = re.sub(r"//[^\n]*", "", code)
    return bool(UNSAFE.search(code))


def section(markdown, heading):
    """The body of the section under `heading`, up to the next heading of
    the same or a higher level."""
    lines = markdown.splitlines()
    for i, line in enumerate(lines):
        match = re.match(r"(#+) (.*)", line)
        if match and match.group(2).strip() == heading:
            level = len(match.group(1))
            body = []
            for rest in lines[i + 1 :]:
                other = re.match(r"(#+) ", rest)
                if other and len(other.group(1)) <= level:
                    break
                body.append(rest)
            return "\n".join(body)
    raise ValueError(f"no heading {heading!r}")


SEPARATOR = re.compile(r"\|[\s:|-]+\|$")


def table_rows(markdown):
    """The cells of every table body row in `markdown`."""
    rows = []
    for line in markdown.splitlines():
        line = line.strip()
        if SEPARATOR.match(line):
            rows.pop()  # the header row
        elif line.startswith("|"):
            rows.append([cell.strip() for cell in line.strip("|").split("|")])
    return rows


def unsafe_table(markdown):
    """(crate, file) pairs the `unsafe` table lists."""
    listed = set()
    for cells in table_rows(section(markdown, "`unsafe`")):
        crate, file = cells[0].strip("`"), cells[1].strip("`")
        if crate != "none yet":
            listed.add((crate, file))
    return listed


def unsafe_mismatches(files, markdown, read=lambda p: Path(p).read_text()):
    used = {(crate, path) for crate, path in files if uses_unsafe(read(path))}
    listed = unsafe_table(markdown)
    failures = [f"{path}: uses unsafe, but the table in docs/code-quality.md does not list it" for _, path in sorted(used - listed)]
    failures += [f"{path}: listed for {crate} in docs/code-quality.md, but uses no unsafe" for crate, path in sorted(listed - used)]
    return failures


def admitted(markdown):
    """Crate names in the tables of "Runtime dependencies" and "Tests and
    development tools"."""
    names = set()
    for heading in ["Runtime dependencies", "Tests and development tools"]:
        for cells in table_rows(section(markdown, heading)):
            for part in cells[0].split(","):
                word = part.strip().strip("`")
                if re.fullmatch(r"[A-Za-z0-9_-]+", word):
                    names.add(word)
    return names


def unlisted(dependencies, markdown):
    listed = admitted(markdown)
    return [
        f"{crate} depends on {dep}, which docs/dependencies.md does not list"
        for crate, dep in sorted(dependencies)
        if dep not in listed
    ]


def cargo_dependencies():
    """(crate, dependency) for every dependency a workspace Cargo.toml
    names, other than workspace members."""
    meta = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"], text=True)
    )
    members = {p["name"] for p in meta["packages"]}
    return {
        (p["name"], d["name"]) for p in meta["packages"] for d in p["dependencies"] if d["name"] not in members
    }


def main(argv):
    if len(argv) != 1 or argv[0] not in {"line-cap", "unsafe-table", "dependency-list"}:
        print(__doc__, file=sys.stderr)
        return 2
    command = argv[0]
    if command == "dependency-list":
        failures = unlisted(cargo_dependencies(), Path("docs/dependencies.md").read_text())
    else:
        members = workspace_members()
        files = rust_files(members)
        if command == "line-cap":
            failures = over_cap(files, members)
        else:
            failures = unsafe_mismatches(files, Path("docs/code-quality.md").read_text())
    for line in failures:
        print(f"{command}: {line}")
    if not failures:
        print(f"{command}: ok")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
