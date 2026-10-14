#!/usr/bin/env python3
"""Convert a foreign agent transcript into ordinary Fiber session directories.

Usage: convert_session.py --format claude INPUT.jsonl OUTPUT_DIR

Reads one Claude Code transcript (JSONL, one entry per line) and writes one
session directory per conversation branch (main and sidechain) holding an
ordinary `events.jsonl` log: `session_started` first, contiguous `seq` from
0, `schema_version` 1, one session id per output session. Mapped tool
calls become `tool_call_requested`/`tool_call_started` pairs carrying the
mapping table's declared effects, so the `review` jig can replay them; each
`compact_boundary` becomes a completed handoff.

Only the mapping table below is supported. A known unmappable tool is
skipped and counted with a content-free diagnostic naming its source type,
never its content. Corrupt input fails with a file/line diagnostic that
quotes no transcript text. An existing destination is never overwritten.

This is a standard-library research script, not a provider and not a
reviewer-input builder: it preserves source tool names and arguments and
maps known source effects to the existing `DeclaredEffects` shape. See
research/decision-models/README.md for the mapping table.
"""

import argparse
import json
import os
import sys
from collections import Counter

SCHEMA_VERSION = 1
WORKSPACE = "/synthetic/claude-import"
BASE_TS = 1700000000000

# Source tool name -> declared effects. `paths_from` names the input key
# holding the touched paths, when the tool declares any.
EFFECTS = {
    "Bash": {"effects": ["executes"], "reversible": False},
    "Edit": {"effects": ["writes"], "reversible": True, "paths_from": "file_path"},
    "Write": {"effects": ["writes"], "reversible": True, "paths_from": "file_path"},
    "Read": {"effects": ["reads"], "reversible": True, "paths_from": "file_path"},
    "WebSearch": {"effects": ["network"], "reversible": True},
    "WebFetch": {"effects": ["network"], "reversible": True},
}


class Session:
    """One output session's lines, with deterministic ids and clocks."""

    def __init__(self, session_id):
        self.session_id = session_id
        self.lines = []
        self.turns = 0
        self.actions = 0
        self.turn = None
        self.pending = {}

    def envelope(self, kind, payload, turn=None, action=None):
        line = {
            "kind": kind,
            "session_id": self.session_id,
            "ts": BASE_TS + len(self.lines) * 1000,
            "schema_version": SCHEMA_VERSION,
        }
        if turn is not None:
            line["turn_id"] = turn
        if action is not None:
            line["action_id"] = action
        line["seq"] = len(self.lines)
        line["payload"] = payload
        self.lines.append(line)
        return line

    def new_turn(self):
        self.turns += 1
        self.turn = "t_%d" % self.turns
        return self.turn

    def new_action(self):
        self.actions += 1
        return "a_%d" % self.actions

    def person_turn(self, texts):
        turn = self.new_turn()
        self.envelope(
            "turn_started",
            {
                "input": [
                    {
                        "type": "message",
                        "content": [{"type": "text", "text": text}],
                        "command_id": "c_%s_%d" % (self.session_id, self.turns),
                        "source": "driver",
                    }
                    for text in texts
                ]
            },
            turn=turn,
        )

    def session_message(self, text, from_session):
        self.envelope(
            "steering_applied",
            {
                "content": [{"type": "text", "text": text}],
                "command_id": "c_peer_%s" % self.session_id,
                "from_session_id": from_session,
                "source": "session",
            },
            turn=self.turn,
        )

    def handoff(self):
        self.envelope(
            "handoff_completed",
            {"outcome": "completed", "tokens_before": 120},
            turn=self.turn,
        )


def fail(message):
    sys.stderr.write("convert_session: %s\n" % message)
    return 1


def convert(entries, main, side, skipped):
    """Fold parsed Claude entries into the output sessions."""
    for (_lineno, entry) in entries:
        session = side if bool(entry.get("isSidechain")) else main
        kind = entry.get("type")
        if kind == "user":
            message = entry.get("message") or {}
            if entry.get("isMeta") or entry.get("isCompactSummary"):
                continue
            content = message.get("content")
            texts = []
            results = []
            if isinstance(content, str):
                if content.strip():
                    texts = [content.strip()]
            elif isinstance(content, list):
                for block in content:
                    if not isinstance(block, dict):
                        continue
                    if block.get("type") == "text" and block.get("text"):
                        texts.append(block["text"])
                    elif block.get("type") == "tool_result" and block.get("tool_use_id"):
                        results.append(block)
            for block in results:
                resolve_result(block, session, skipped)
            if texts:
                session.person_turn(texts)
        elif kind == "assistant":
            message = entry.get("message") or {}
            content = message.get("content")
            if not isinstance(content, list):
                continue
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    if block.get("id"):
                        request_call(block, session, main, skipped)
                    else:
                        skipped["tool:?"] += 1
        elif kind == "system":
            if entry.get("subtype") == "compact_boundary":
                session.handoff()
        else:
            skipped["entry:%s" % (kind,)] += 1


def request_call(block, session, main, skipped):
    """Map one tool_use block to a request, message, or counted skip."""
    name = block.get("name")
    tool_id = block.get("id")
    arguments = block.get("input")
    if not isinstance(name, str) or not isinstance(arguments, dict):
        skipped["tool:?"] += 1
        return
    if name == "SendMessage":
        # Another session's words: a session message, never driver input
        # and never a reviewed call of its own.
        text = arguments.get("text")
        if isinstance(text, str) and text:
            session.session_message(text, "s_peer")
        return
    if name == "SubagentHandback":
        # The subagent's words enter the recipient only as a session
        # message; the sidechain keeps the request without a start.
        action = session.new_action()
        session.pending[tool_id] = (action, None, arguments)
        session.envelope(
            "tool_call_requested",
            {"name": name, "arguments": arguments},
            turn=session.turn,
            action=action,
        )
        text = arguments.get("text")
        if isinstance(text, str) and text:
            main.session_message(text, session.session_id)
        return
    mapped = EFFECTS.get(name)
    if mapped is None:
        skipped["tool:%s" % name] += 1
        return
    action = session.new_action()
    session.pending[tool_id] = (action, mapped, arguments)
    session.envelope(
        "tool_call_requested",
        {"name": name, "arguments": arguments},
        turn=session.turn,
        action=action,
    )


def resolve_result(block, session, skipped):
    """Close a pending call with its started effects, by tool_use_id."""
    tool_id = block.get("tool_use_id")
    pending = session.pending.pop(tool_id, None)
    if pending is None:
        skipped["unlinked-result"] += 1
        return
    (action, mapped, arguments) = pending
    if mapped is None:
        return
    declared = {"effects": list(mapped["effects"]), "reversible": mapped["reversible"]}
    paths_from = mapped.get("paths_from")
    if paths_from is not None and isinstance(arguments.get(paths_from), str):
        declared["paths"] = [arguments[paths_from]]
    session.envelope(
        "tool_call_started",
        declared,
        turn=session.turn,
        action=action,
    )


def parse(path):
    """Parse the transcript; corrupt lines fail naming file and line."""
    entries = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for lineno, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                entry = json.loads(line)
            except ValueError:
                return None, fail("%s:%d: not JSON" % (path, lineno))
            if not isinstance(entry, dict):
                return None, fail("%s:%d: not an object" % (path, lineno))
            entries.append((lineno, entry))
    return entries, 0


def write_sessions(dest, sessions):
    for session_id, session in sessions.items():
        if not session.lines:
            continue
        session.envelope(
            "session_started",
            {
                "workspace": WORKSPACE,
                "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            },
        )
        # session_started goes first: move the last line to the front and
        # renumber, so seq stays contiguous from 0.
        started = session.lines.pop()
        session.lines.insert(0, started)
        for seq, line in enumerate(session.lines):
            line["seq"] = seq
            line["ts"] = BASE_TS + seq * 1000
        out = os.path.join(dest, session_id)
        os.makedirs(out)
        with open(os.path.join(out, "events.jsonl"), "w", encoding="utf-8") as handle:
            for line in session.lines:
                # Compact separators, as the session log itself writes:
                # greps over the wire form match `"kind":"..."`.
                handle.write(json.dumps(line, sort_keys=False, separators=(",", ":")) + "\n")


def main(argv):
    parser = argparse.ArgumentParser(prog="convert_session.py")
    parser.add_argument("--format", required=True)
    parser.add_argument("input")
    parser.add_argument("output")
    args = parser.parse_args(argv)
    if args.format != "claude":
        return fail("unsupported format %r" % args.format)
    if os.path.exists(args.output):
        return fail("refusing to overwrite existing %s" % args.output)
    try:
        entries, status = parse(args.input)
    except OSError as error:
        return fail("cannot read %s: %s" % (args.input, error))
    if entries is None:
        return status
    main_session = Session("s_main")
    side_session = Session("s_side")
    skipped = Counter()
    convert(entries, main_session, side_session, skipped)
    sessions = {"s_main": main_session, "s_side": side_session}
    try:
        os.makedirs(args.output)
        write_sessions(args.output, sessions)
    except OSError as error:
        return fail("cannot write %s: %s" % (args.output, error))
    wrote = sorted(session_id for session_id, session in sessions.items() if session.lines)
    total = sum(len(session.lines) for session in sessions.values())
    skips = ", ".join("%s x%d" % (name, count) for name, count in sorted(skipped.items()))
    sys.stdout.write(
        "convert_session: wrote %d sessions (%s, %d lines)%s\n"
        % (len(wrote), ",".join(wrote), total, ("; skipped " + skips) if skips else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
