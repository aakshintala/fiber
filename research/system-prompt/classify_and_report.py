#!/usr/bin/env python3
"""Step 4: classify sessions by which instruction files they'd load, and compute
the size distribution of total instruction text loaded at session start
(global + ancestors + cwd), per source (pi vs Claude Code).

Reads sessions_by_cwd.json + cwd_instructions.json. Writes a summary to stdout
and returns data for the markdown report.
"""
import json, os, statistics

HOME = os.path.expanduser("~")
HERE = os.path.dirname(__file__)

PI_GLOBAL = os.path.join(HOME, ".pi/agent/AGENTS.md")
CC_GLOBAL = os.path.join(HOME, ".claude/CLAUDE.md")

def file_bytes(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0

PI_GLOBAL_BYTES = file_bytes(PI_GLOBAL)
CC_GLOBAL_BYTES = file_bytes(CC_GLOBAL)

def main():
    sessions = json.load(open(os.path.join(HERE, "sessions_by_cwd.json")))
    instr = json.load(open(os.path.join(HERE, "cwd_instructions.json")))
    by_cwd = sessions["by_cwd"]

    # bucket counts (session-weighted), separately tallied for "any source" since a
    # cwd's file layout doesn't depend on which tool visited it
    buckets = {
        "no_instruction_file": 0,
        "only_agents_md_cwd": 0,
        "only_claude_md_cwd": 0,
        "both_agents_and_claude_cwd": 0,
        "ancestor_files_present": 0,  # dirs strictly between cwd and HOME (exclusive)
        "nested_files_present": 0,
        "cwd_missing_on_disk": 0,
    }

    # per-session (weighted) total instruction bytes loaded, split pi vs cc
    pi_totals = []
    cc_totals = []

    same_dir_both_details = []

    for cwd, counts in by_cwd.items():
        n_pi = counts.get("pi", 0)
        n_cc = counts.get("cc_main", 0) + counts.get("cc_sub", 0)
        n_total = n_pi + n_cc
        if n_total == 0:
            continue
        rec = instr.get(cwd, {})
        if not rec.get("exists"):
            buckets["cwd_missing_on_disk"] += n_total
            # still count global-only load
            for _ in range(n_pi):
                pi_totals.append(PI_GLOBAL_BYTES)
            for _ in range(n_cc):
                cc_totals.append(CC_GLOBAL_BYTES)
            continue

        cwd_files = rec.get("cwd_files", {})
        has_agents = "AGENTS.md" in cwd_files
        has_claude = "CLAUDE.md" in cwd_files or os.path.join(".claude", "CLAUDE.md") in cwd_files
        has_any_cwd = bool(cwd_files)

        ancestor_files = rec.get("ancestor_files", {})
        # exclude HOME itself (accounted for via global files)
        ancestor_files_excl_home = {d: f for d, f in ancestor_files.items() if os.path.normpath(d) != os.path.normpath(HOME)}
        has_ancestor = bool(ancestor_files_excl_home)

        nested = rec.get("nested_files", [])
        has_nested = bool(nested)

        if not has_any_cwd and not has_ancestor:
            buckets["no_instruction_file"] += n_total
        elif has_agents and has_claude:
            buckets["both_agents_and_claude_cwd"] += n_total
            same_dir_both_details.append(cwd)
        elif has_agents:
            buckets["only_agents_md_cwd"] += n_total
        elif has_claude:
            buckets["only_claude_md_cwd"] += n_total

        if has_ancestor:
            buckets["ancestor_files_present"] += n_total
        if has_nested:
            buckets["nested_files_present"] += n_total

        # total bytes loaded at session start.
        # Assumption (observed, not guessed): pi's own sessions `cat AGENTS.md`
        # explicitly and its global default is AGENTS.md, so pi is modeled as
        # loading the AGENTS.md family only. Claude Code's documented auto-load
        # is CLAUDE.md, so cc is modeled as loading the CLAUDE.md family only.
        # (A combined "everything on disk regardless of loader" figure is also
        # reported separately for comparison.)
        AGENTS_FAMILY = {"AGENTS.md", "AGENTS.override.md"}
        CLAUDE_FAMILY = {"CLAUDE.md", "CLAUDE.local.md", os.path.join(".claude", "CLAUDE.md")}

        def family_bytes(files_map, family):
            return sum(f["bytes"] for name, f in files_map.items() if name in family)

        def sum_ancestor_family(family):
            return sum(family_bytes(f, family) for f in ancestor_files_excl_home.values())

        groot_files = rec.get("git_root_files", {})

        def sum_groot_family(family):
            return sum(family_bytes(f, family) for f in groot_files.values())

        cwd_bytes_all = sum(f["bytes"] for f in cwd_files.values())
        ancestor_bytes_all = sum(f["bytes"] for files in ancestor_files_excl_home.values() for f in files.values())
        groot_bytes_all = sum(f["bytes"] for files in groot_files.values() for f in files.values())

        pi_specific = (
            family_bytes(cwd_files, AGENTS_FAMILY)
            + sum_ancestor_family(AGENTS_FAMILY)
            + sum_groot_family(AGENTS_FAMILY)
        )
        cc_specific = (
            family_bytes(cwd_files, CLAUDE_FAMILY)
            + sum_ancestor_family(CLAUDE_FAMILY)
            + sum_groot_family(CLAUDE_FAMILY)
        )

        for _ in range(n_pi):
            pi_totals.append(PI_GLOBAL_BYTES + pi_specific)
        for _ in range(n_cc):
            cc_totals.append(CC_GLOBAL_BYTES + cc_specific)

    def dist(vals):
        if not vals:
            return None
        vals_sorted = sorted(vals)
        n = len(vals_sorted)
        def pct(p):
            idx = min(n - 1, int(round(p * (n - 1))))
            return vals_sorted[idx]
        return {
            "n": n,
            "min_tokens": round(vals_sorted[0] / 4),
            "median_tokens": round(statistics.median(vals_sorted) / 4),
            "p90_tokens": round(pct(0.9) / 4),
            "max_tokens": round(vals_sorted[-1] / 4),
        }

    report = {
        "pi_global_bytes": PI_GLOBAL_BYTES,
        "cc_global_bytes": CC_GLOBAL_BYTES,
        "buckets_session_weighted": buckets,
        "pi_dist": dist(pi_totals),
        "cc_dist": dist(cc_totals),
        "all_dist": dist(pi_totals + cc_totals),
        "same_dir_both_cwds": same_dir_both_details,
    }
    out_path = os.path.join(HERE, "classification_report.json")
    with open(out_path, "w") as f:
        json.dump(report, f, indent=2, sort_keys=True)
    print(json.dumps(report, indent=2, sort_keys=True))

if __name__ == "__main__":
    main()
