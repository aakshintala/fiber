#!/usr/bin/env python3
"""Step 1 + step 5: collect distinct cwds + session counts across pi and Claude Code
(main/subagents), and detect sessions that edited an instruction file.

Writes sessions_by_cwd.json: {cwd: {"pi": n, "cc_main": n, "cc_sub": n}}
"""
import json, glob, os
from collections import defaultdict

HOME = os.path.expanduser("~")
INSTR_BASENAMES = {"AGENTS.md", "CLAUDE.md", "CLAUDE.local.md", "AGENTS.override.md"}

def iter_jsonl(path):
    try:
        with open(path, "r", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    continue
    except OSError:
        return

def pi_sessions():
    out = []
    for path in glob.glob(os.path.join(HOME, ".pi/agent/sessions/**/*.jsonl"), recursive=True):
        cwd = None
        edited = False
        for d in iter_jsonl(path):
            if d.get("type") == "session" and cwd is None:
                cwd = d.get("cwd")
            if d.get("type") == "message":
                content = d.get("message", {}).get("content")
                if isinstance(content, list):
                    for b in content:
                        if isinstance(b, dict) and b.get("type") == "toolCall" and b.get("name") in ("edit", "write"):
                            p = b.get("arguments", {}).get("path", "") or ""
                            if os.path.basename(p) in INSTR_BASENAMES:
                                edited = True
        out.append((path, cwd, edited))
    return out

def cc_sessions():
    out = []
    for path in glob.glob(os.path.join(HOME, ".claude/projects/**/*.jsonl"), recursive=True):
        is_sub = "/subagents/" in path
        cwd = None
        edited = False
        for d in iter_jsonl(path):
            if cwd is None and "cwd" in d:
                cwd = d.get("cwd")
            if d.get("type") == "assistant":
                content = d.get("message", {}).get("content", [])
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "tool_use":
                            if block.get("name") in ("Edit", "Write", "MultiEdit"):
                                fp = block.get("input", {}).get("file_path", "") or ""
                                if os.path.basename(fp) in INSTR_BASENAMES:
                                    edited = True
        out.append((path, cwd, is_sub, edited))
    return out

def main():
    pi = pi_sessions()
    cc = cc_sessions()

    by_cwd = defaultdict(lambda: {"pi": 0, "cc_main": 0, "cc_sub": 0})
    pi_edit_count = cc_main_edit_count = cc_sub_edit_count = 0

    for path, cwd, edited in pi:
        if cwd:
            by_cwd[cwd]["pi"] += 1
        if edited:
            pi_edit_count += 1

    for path, cwd, is_sub, edited in cc:
        if cwd:
            by_cwd[cwd]["cc_sub" if is_sub else "cc_main"] += 1
        if edited:
            if is_sub:
                cc_sub_edit_count += 1
            else:
                cc_main_edit_count += 1

    result = {
        "by_cwd": dict(by_cwd),
        "totals": {
            "pi_sessions": len(pi),
            "cc_main_sessions": sum(1 for _, _, s, _ in cc if not s),
            "cc_sub_sessions": sum(1 for _, _, s, _ in cc if s),
            "pi_sessions_no_cwd": sum(1 for _, cwd, _ in pi if not cwd),
            "cc_sessions_no_cwd": sum(1 for _, cwd, _, _ in cc if not cwd),
        },
        "edits": {
            "pi_sessions_edited_instr_file": pi_edit_count,
            "cc_main_sessions_edited_instr_file": cc_main_edit_count,
            "cc_sub_sessions_edited_instr_file": cc_sub_edit_count,
        },
    }

    out_path = os.path.join(os.path.dirname(__file__), "sessions_by_cwd.json")
    with open(out_path, "w") as f:
        json.dump(result, f, indent=2, sort_keys=True)
    print("Wrote", out_path)
    print(json.dumps(result["totals"], indent=2))
    print(json.dumps(result["edits"], indent=2))
    print("Distinct cwds:", len(by_cwd))

if __name__ == "__main__":
    main()
