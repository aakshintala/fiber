#!/usr/bin/env python3
"""Build reviewed-call cases from the owner's Claude Code session transcripts.

Reads ~/.claude/projects/**/*.jsonl (main and subagent sessions alike) and
writes one JSON file per reviewed tool call to the local-only case directory
(default /Users/aakshintala/.claude/jobs/409b2209/tmp/2244-cases).

Label source: each tool_result entry in the transcripts carries a
`permissionDecision` object written by Claude Code itself. Only entries whose
`reasonType` is `classifier` (the auto-mode classifier's own decision) become
cases, mapped mechanically:

    decision=accept + reasonType=classifier  ->  label `allow`
    decision=reject + reasonType=classifier  ->  label `check`

Every other reasonType (mode, subcommandResults, rule, safetyCheck, other, or
absent) and every other source is counted and excluded, never relabelled.

Case shape follows docs/permissions.md ("What it is shown") and ADR 0003:
human messages and tool calls only. Assistant prose, tool results, attachments
and queue entries are stripped. Tool calls render as name + arguments only.

Usage (sharded so each run finishes in seconds):
  extract_cases.py shard --index I --of N --shard-dir <dir>
  extract_cases.py merge --shard-dir <dir>   (repeat per shard)

`merge` reads every shard dir in order, renumbers cases to c000001... in a
deterministic order (shard, file path, position), writes them plus
manifest.json into the case directory, and prints the manifest (counts and
aggregates only).

No case text is printed and nothing is written under the repository.
"""

import argparse
import json
import os
import sys
from collections import Counter
from pathlib import Path

TRANSCRIPT_ROOT = Path.home() / ".claude" / "projects"
CASE_DIR = Path(
    os.environ.get(
        "CASE_DIR_2244",
        "/Users/aakshintala/.claude/jobs/409b2209/tmp/2244-cases",
    )
)


def render_call(name, args):
    try:
        arg_text = json.dumps(args, sort_keys=True, ensure_ascii=False)
    except (TypeError, ValueError):
        arg_text = str(args)
    return f"{name} {arg_text}"


def process_file(fp, stats, tool_hist, sidechain, emit):
    """Parse one transcript; call emit(payload_without_id) per case."""
    try:
        fh = open(fp, errors="ignore")
    except OSError:
        return
    with fh:
        stats["files_scanned"] += 1
        humans = []
        calls = []
        by_id = {}
        pos = 0
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except ValueError:
                stats["unparseable_lines"] += 1
                continue
            pos += 1
            t = e.get("type")
            if t == "assistant":
                msg = e.get("message")
                content = msg.get("content") if isinstance(msg, dict) else None
                if not isinstance(content, list):
                    continue
                sc = bool(e.get("isSidechain"))
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_use" and b.get("id"):
                        by_id[b["id"]] = (len(calls), pos, sc)
                        calls.append((pos, render_call(b.get("name"), b.get("input"))))
            elif t == "user":
                msg = e.get("message")
                content = msg.get("content") if isinstance(msg, dict) else None
                if isinstance(content, str):
                    if content.strip():
                        humans.append((pos, content.strip()))
                elif isinstance(content, list):
                    for b in content:
                        if isinstance(b, dict) and b.get("type") == "text" and b.get("text"):
                            humans.append((pos, b["text"]))
                pd = e.get("permissionDecision")
                if not isinstance(pd, dict) or not isinstance(content, list):
                    continue
                for b in content:
                    if (
                        not isinstance(b, dict)
                        or b.get("type") != "tool_result"
                        or not b.get("tool_use_id")
                    ):
                        continue
                    tid = b["tool_use_id"]
                    decision = pd.get("decision")
                    reason = pd.get("reasonType")
                    source = pd.get("source")
                    stats[f"pd:{decision}/{reason}/{source}"] += 1
                    if reason != "classifier" or source != "config":
                        stats["excluded:non_classifier"] += 1
                        continue
                    linked = by_id.get(tid)
                    if linked is None:
                        stats["excluded:unlinked_call"] += 1
                        continue
                    idx, cpos, sc = linked
                    if decision == "accept":
                        label = "allow"
                    elif decision == "reject":
                        label = "check"
                    else:
                        stats["excluded:unknown_decision"] += 1
                        continue
                    prior_humans = [x for p, x in humans if p < cpos]
                    prior_calls = [c for p, c in calls[:idx] if p < cpos]
                    tool = calls[idx][1].split(" ", 1)[0]
                    emit(
                        {
                            "tool": tool,
                            "label": label,
                            "sidechain": sc,
                            "n_person_messages": len(prior_humans),
                            "n_prior_calls": len(prior_calls),
                            "state": "\n".join(
                                [f"[Person {n}]: {x}" for n, x in enumerate(prior_humans, 1)]
                                + [f"[Call {n}] {c}" for n, c in enumerate(prior_calls, 1)]
                                + [f"Under review: {calls[idx][1]}"]
                            ),
                        }
                    )
                    stats[f"label:{label}"] += 1
                    tool_hist[tool] += 1
                    sidechain[f"sidechain:{sc}"] += 1


def cmd_shard(args):
    files = sorted(TRANSCRIPT_ROOT.glob("**/*.jsonl"))
    mine = [fp for i, fp in enumerate(files) if i % args.of == args.index]
    shard_dir = Path(args.shard_dir)
    shard_dir.mkdir(parents=True, exist_ok=True)
    for old in shard_dir.glob("*.json"):
        old.unlink()
    stats, tool_hist, sidechain = Counter(), Counter(), Counter()
    n = 0

    def emit(payload):
        nonlocal n
        with open(shard_dir / f"s{n:06d}.json", "w") as fh:
            json.dump(payload, fh, ensure_ascii=False)
        n += 1

    for fp in mine:
        process_file(fp, stats, tool_hist, sidechain, emit)
    summary = {
        "files": [str(fp) for fp in mine],
        "n_cases": n,
        "stats": dict(stats),
        "tools": dict(tool_hist),
        "sidechain": dict(sidechain),
    }
    with open(shard_dir / "shard.json", "w") as fh:
        json.dump(summary, fh, indent=1, sort_keys=True)
    print(f"shard {args.index}/{args.of}: {len(mine)} files -> {n} cases", flush=True)


def cmd_merge(args):
    stats, tool_hist, sidechain = Counter(), Counter(), Counter()
    staged = []  # (shard_order, path) for deterministic renumbering
    for shard_dir in args.shard_dir:
        d = Path(shard_dir)
        summary = json.load(open(d / "shard.json"))
        for k, v in summary["stats"].items():
            stats[k] += v
        for k, v in summary["tools"].items():
            tool_hist[k] += v
        for k, v in summary["sidechain"].items():
            sidechain[k] += v
        for fp in sorted(d.glob("s[0-9]*.json")):
            staged.append(fp)
    CASE_DIR.mkdir(parents=True, exist_ok=True)
    for old in CASE_DIR.glob("*.json"):
        old.unlink()
    CASE_DIR.joinpath(".lock").unlink(missing_ok=True)
    n = 0
    for fp in staged:
        payload = json.load(open(fp))
        n += 1
        payload["id"] = f"c{n:06d}"
        with open(CASE_DIR / f"c{n:06d}.json", "w") as fh:
            json.dump(payload, fh, ensure_ascii=False)
    manifest = {
        "files_scanned": stats["files_scanned"],
        "n_cases": n,
        "labels": {"allow": stats["label:allow"], "check": stats["label:check"]},
        "sidechain": dict(sidechain),
        "tool_histogram": dict(
            sorted(tool_hist.items(), key=lambda kv: -kv[1])
        ),
        "excluded": {k: v for k, v in stats.items() if k.startswith("excluded:")},
        "permission_decision_combos": {
            k: v for k, v in stats.items() if k.startswith("pd:")
        },
    }
    with open(CASE_DIR / "manifest.json", "w") as fh:
        json.dump(manifest, fh, indent=1, sort_keys=True)
    print(json.dumps(manifest, indent=1, sort_keys=True))


def main(argv):
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("shard")
    p.add_argument("--index", type=int, required=True)
    p.add_argument("--of", type=int, required=True)
    p.add_argument("--shard-dir", required=True)
    p = sub.add_parser("merge")
    p.add_argument("--shard-dir", action="append", required=True)
    args = ap.parse_args(argv)
    if args.cmd == "shard":
        cmd_shard(args)
    else:
        cmd_merge(args)


if __name__ == "__main__":
    main(sys.argv[1:])
