"""Summarise yesterday's tool calls per session for the status line."""
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Call:
    session: str
    tool: str
    ms: int
    ok: bool


def load_calls(path: Path) -> list[Call]:
    calls = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            print(f"skip bad line: {exc}", file=sys.stderr)
            continue
        calls.append(Call(row["session"], row["tool"], row["ms"], row.get("ok", True)))
    return calls


def report(calls: list[Call]) -> str:
    by_tool: Counter[str] = Counter()
    slow: list[Call] = []
    for call in calls:
        by_tool[call.tool] += 1
        if call.ms > 5_000:
            slow.append(call)
    lines = [f"{tool}: {n}" for tool, n in by_tool.most_common(5)]
    lines.append(f"slow calls (>5s): {len(slow)}")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path, help="JSONL tool-call log")
    args = parser.parse_args()
    print(report(load_calls(args.log)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
