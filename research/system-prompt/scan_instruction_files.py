#!/usr/bin/env python3
"""Steps 2-4: for each distinct cwd (from sessions_by_cwd.json) that still exists on
disk, find instruction files in cwd, every ancestor up to $HOME, and the git root if
different. Also list nested instruction files under cwd via `git ls-files` (capped).

Writes cwd_instructions.json: per-cwd record of every instruction file found, with
byte size / line count, plus which of the fixed classification buckets it falls into.
"""
import json, os, subprocess, sys

HOME = os.path.expanduser("~")
BASENAME_PATHS = [
    "AGENTS.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    os.path.join(".claude", "CLAUDE.md"),
    "AGENTS.override.md",
    ".cursorrules",
    os.path.join(".github", "copilot-instructions.md"),
    "GEMINI.md",
]

def file_stat(path):
    try:
        with open(path, "rb") as f:
            data = f.read()
    except OSError:
        return None
    size = len(data)
    try:
        text = data.decode("utf-8", errors="replace")
    except Exception:
        text = ""
    lines = text.count("\n") + (1 if text and not text.endswith("\n") else 0)
    return {"bytes": size, "lines": lines, "tokens_est": round(size / 4)}

def files_in_dir(d):
    found = {}
    for rel in BASENAME_PATHS:
        p = os.path.join(d, rel)
        if os.path.isfile(p):
            st = file_stat(p)
            if st:
                found[rel] = {"path": p, **st}
    return found

def ancestors_up_to_home(cwd):
    """Return list of dirs from cwd up to (and including) HOME, if cwd is under HOME.
    If cwd is not under HOME, walk up to filesystem root instead (rare: /private/tmp etc)."""
    dirs = []
    cur = os.path.normpath(cwd)
    seen = set()
    while True:
        if cur in seen:
            break
        seen.add(cur)
        dirs.append(cur)
        if cur == HOME:
            break
        parent = os.path.dirname(cur)
        if parent == cur:
            break  # reached filesystem root
        cur = parent
        # stop once we've gone above HOME's parent (avoid walking whole disk for /private/tmp paths)
        if len(dirs) > 30:
            break
    return dirs

def git_root(cwd):
    try:
        out = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=5,
        )
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return None

def nested_instruction_files(cwd, git_root_dir):
    """Use git ls-files to find nested instruction-like files under cwd, capped."""
    root = git_root_dir or cwd
    try:
        out = subprocess.run(
            ["git", "-C", root, "ls-files"],
            capture_output=True, text=True, timeout=15,
        )
        if out.returncode != 0:
            return []
    except Exception:
        return []
    names = {"AGENTS.md", "CLAUDE.md", "CLAUDE.local.md", "AGENTS.override.md",
              ".cursorrules", "copilot-instructions.md", "GEMINI.md"}
    results = []
    cwd_norm = os.path.normpath(cwd)
    for rel in out.stdout.splitlines():
        base = os.path.basename(rel)
        if base in names:
            abspath = os.path.normpath(os.path.join(root, rel))
            # only count as "nested under cwd" if it's inside cwd and not the direct top-level file itself
            if abspath.startswith(cwd_norm + os.sep):
                depth = abspath[len(cwd_norm):].count(os.sep)
                if depth <= 6:
                    st = file_stat(abspath)
                    if st:
                        results.append({"rel": rel, "path": abspath, **st})
    return results

def main():
    here = os.path.dirname(__file__)
    sessions = json.load(open(os.path.join(here, "sessions_by_cwd.json")))
    by_cwd = sessions["by_cwd"]

    records = {}
    for cwd, counts in by_cwd.items():
        exists = os.path.isdir(cwd)
        rec = {"counts": counts, "exists": exists}
        if not exists:
            records[cwd] = rec
            continue

        ancestor_dirs = ancestors_up_to_home(cwd)
        ancestor_files = {}
        for d in ancestor_dirs:
            f = files_in_dir(d)
            if f:
                ancestor_files[d] = f

        groot = git_root(cwd)
        groot_files = {}
        if groot and os.path.normpath(groot) != os.path.normpath(cwd) and groot not in ancestor_files:
            f = files_in_dir(groot)
            if f:
                groot_files[groot] = f

        nested = nested_instruction_files(cwd, groot)

        rec.update({
            "git_root": groot,
            "cwd_files": ancestor_files.get(os.path.normpath(cwd), {}),
            "ancestor_files": {d: f for d, f in ancestor_files.items() if d != os.path.normpath(cwd)},
            "git_root_files": groot_files,
            "nested_files": nested,
        })
        records[cwd] = rec

    out_path = os.path.join(here, "cwd_instructions.json")
    with open(out_path, "w") as fh:
        json.dump(records, fh, indent=2, sort_keys=True)
    print("Wrote", out_path)
    n_exist = sum(1 for r in records.values() if r.get("exists"))
    print(f"{n_exist} / {len(records)} cwds exist on disk")

if __name__ == "__main__":
    main()
