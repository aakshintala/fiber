#!/usr/bin/env python3
"""Convert a foreign agent transcript into ordinary Fiber session directories.

Usage: convert_session.py --format (claude|pi) INPUT.jsonl OUTPUT_DIR

Reads one Claude Code transcript or one pi session file (JSONL, one entry
per line) and writes ordinary `events.jsonl` session directories: one per
conversation branch for Claude (`s_main`, plus `s_side` when subagent
entries exist), one active-branch session (`s_pi`) for pi. Every output
session holds `session_started` first, contiguous `seq` from 0, and
`schema_version` 1. Mapped tool calls become `tool_call_requested` /
`tool_call_started` pairs carrying the mapping table's declared effects, so
the `review` jig can replay them; each Claude `compact_boundary` and each pi
compaction entry becomes a completed handoff.

The session workspace is preserved from the source transcript (Claude
`cwd`, pi session-header `cwd`) and validated to be an absolute path; the
converter never invents one. A conflicting or missing workspace fails with
a file/line diagnostic that quotes no transcript text.

Only the mapping tables below are supported. A known unmappable tool is
skipped and counted with a content-free diagnostic naming its source type,
never its content. Corrupt input fails with a file/line diagnostic that
quotes no transcript text. An existing destination is never overwritten.

This is a standard-library research script, not a provider and not a
reviewer-input builder: it preserves source tool names and arguments and
maps known source effects to the existing `DeclaredEffects` shape. See
research/decision-models/README.md for the mapping tables.
"""

import argparse
import json
import os
import sys
from collections import Counter

SCHEMA_VERSION = 1
BASE_TS = 1700000000000

# Claude source tool name -> declared effects. `paths_from` names the input
# key holding the touched paths, when the tool declares any.
CLAUDE_EFFECTS = {
    "Bash": {"effects": ["executes"], "reversible": False},
    "Edit": {"effects": ["writes"], "reversible": True, "paths_from": "file_path"},
    "Write": {"effects": ["writes"], "reversible": True, "paths_from": "file_path"},
    "Read": {"effects": ["reads"], "reversible": True, "paths_from": "file_path"},
    "WebSearch": {"effects": ["network"], "reversible": True},
    "WebFetch": {"effects": ["network"], "reversible": True},
}

# pi toolCall name -> declared effects. `paths_from` names the arguments
# key holding the touched path, when the tool declares any. Session-management
# calls (todo_write, jobs, ask_user, ...) declare no Fiber effects and are
# skipped with a count, never guessed.
PI_EFFECTS = {
    "bash": {"effects": ["executes"], "reversible": False},
    "read": {"effects": ["reads"], "reversible": True, "paths_from": "path"},
    "grep": {"effects": ["reads"], "reversible": True, "paths_from": "path"},
    "find": {"effects": ["reads"], "reversible": True, "paths_from": "path"},
    "ffgrep": {"effects": ["reads"], "reversible": True, "paths_from": "path"},
    "ls": {"effects": ["reads"], "reversible": True, "paths_from": "path"},
    "edit": {"effects": ["writes"], "reversible": True, "paths_from": "path"},
    "write": {"effects": ["writes"], "reversible": True, "paths_from": "path"},
}

# pi entry types that are never person input: extension traffic, model
# bookkeeping, and the session header itself.
PI_NON_PERSON = {
    "session",
    "session_info",
    "model_change",
    "thinking_level_change",
    "custom",
    "custom_message",
    "context_edit",
}


class Session:
    """One output session's lines, with deterministic ids and clocks."""

    def __init__(self, session_id, workspace):
        self.session_id = session_id
        self.lines = []
        self.turns = 0
        self.actions = 0
        self.turn = None
        self.pending = {}
        self.envelope(
            "session_started",
            {
                "workspace": workspace,
                "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            },
        )

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

    def handoff(self, tokens_before=120):
        self.envelope(
            "handoff_completed",
            {"outcome": "completed", "tokens_before": tokens_before},
            turn=self.turn,
        )


def fail(message):
    sys.stderr.write("convert_session: %s\n" % message)
    return 1


def check_workspace(value, path, lineno):
    """Validate a source workspace: an absolute path, quoting no content."""
    if not isinstance(value, str) or not value.startswith("/"):
        return None, fail("%s:%d: bad workspace" % (path, lineno))
    return value, 0


def convert_claude(entries, main, side, skipped, path):
    """Fold parsed Claude entries into the output sessions."""
    for (lineno, entry) in entries:
        session = side if bool(entry.get("isSidechain")) else main
        kind = entry.get("type")
        if kind == "user":
            raw = entry.get("message")
            if raw is None:
                message = {}
            elif isinstance(raw, dict):
                message = raw
            else:
                return fail("%s:%d: bad message" % (path, lineno))
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
            raw = entry.get("message")
            if raw is None:
                message = {}
            elif isinstance(raw, dict):
                message = raw
            else:
                return fail("%s:%d: bad message" % (path, lineno))
            content = message.get("content")
            if not isinstance(content, list):
                continue
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    if block.get("id"):
                        request_call(block, session, main, skipped, CLAUDE_EFFECTS)
                    else:
                        skipped["tool:?"] += 1
        elif kind == "system":
            if entry.get("subtype") == "compact_boundary":
                session.handoff()
        else:
            skipped["entry:%s" % (kind,)] += 1
    return 0


def claude_workspace(entries, path):
    """The transcript's preserved workspace: every `cwd` must agree."""
    workspace = None
    workspace_lineno = 0
    for (lineno, entry) in entries:
        if "cwd" not in entry:
            continue
        candidate, status = check_workspace(entry.get("cwd"), path, lineno)
        if candidate is None:
            return None, status
        if workspace is None:
            workspace = candidate
            workspace_lineno = lineno
        elif candidate != workspace:
            return None, fail(
                "%s:%d: conflicting workspace with %s:%d"
                % (path, lineno, path, workspace_lineno)
            )
    if workspace is None:
        return None, fail("%s: no workspace (cwd) in transcript" % path)
    return workspace, 0


def request_call(block, session, main, skipped, table):
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
    mapped = table.get(name)
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


def pi_branch(entries, path):
    """The active branch: the last entry's parent-linked ancestors, oldest
    first. Siblings off that branch are never merged into the conversion."""
    by_id = {}
    for (lineno, entry) in entries:
        entry_id = entry.get("id")
        if not isinstance(entry_id, str) or not entry_id:
            return None, fail("%s:%d: entry without an id" % (path, lineno))
        if entry_id in by_id:
            return None, fail("%s:%d: duplicate entry id" % (path, lineno))
        parent = entry.get("parentId")
        if parent is not None and not isinstance(parent, str):
            return None, fail("%s:%d: bad parent link" % (path, lineno))
        by_id[entry_id] = (lineno, entry)
    if not by_id:
        return [], 0
    last = entries[-1][1].get("id")
    branch = []
    seen = set()
    cursor = last
    while cursor is not None:
        if cursor in seen:
            lineno, _entry = by_id[cursor]
            return None, fail("%s:%d: parent cycle" % (path, lineno))
        seen.add(cursor)
        found = by_id.get(cursor)
        if found is None:
            return None, fail("%s: dangling parent link" % path)
        lineno, entry = found
        branch.append((lineno, entry))
        cursor = entry.get("parentId")
    branch.reverse()
    return branch, 0


def pi_texts(content):
    """The person texts of a pi user message: text blocks only."""
    if isinstance(content, str):
        return [content.strip()] if content.strip() else []
    if not isinstance(content, list):
        return []
    texts = []
    for block in content:
        if (
            isinstance(block, dict)
            and block.get("type") == "text"
            and isinstance(block.get("text"), str)
            and block.get("text").strip()
        ):
            texts.append(block["text"])
    return texts


def convert_pi(branch, session, skipped, path):
    """Fold the active branch's entries into the output session."""
    for (lineno, entry) in branch:
        kind = entry.get("type")
        if kind in PI_NON_PERSON:
            skipped["entry:%s" % kind] += 1
            continue
        if kind == "message":
            message = entry.get("message")
            if not isinstance(message, dict):
                return fail("%s:%d: bad message" % (path, lineno))
            role = message.get("role")
            content = message.get("content")
            if role == "user":
                texts = pi_texts(content)
                if texts:
                    session.person_turn(texts)
            elif role == "assistant":
                if not isinstance(content, list):
                    continue
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "toolCall":
                        pi_request(block, session, skipped)
            elif role == "toolResult":
                pi_resolve(message, session, skipped)
            else:
                skipped["message:%s" % (role,)] += 1
        elif kind == "compaction":
            # A checkpoint summary: the handoff boundary, never person
            # input. The summary itself is not converted.
            tokens = entry.get("tokensBefore")
            session.handoff(tokens if isinstance(tokens, int) else 120)
        else:
            skipped["entry:%s" % (kind,)] += 1
    return 0


def pi_request(block, session, skipped):
    """Map one pi toolCall block to a request or a counted skip."""
    name = block.get("name")
    tool_id = block.get("id")
    arguments = block.get("arguments")
    if not isinstance(tool_id, str) or not isinstance(name, str) or not isinstance(
        arguments, dict
    ):
        skipped["tool:?"] += 1
        return
    mapped = PI_EFFECTS.get(name)
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


def pi_resolve(message, session, skipped):
    """Close a pending pi call with its started effects, by toolCallId."""
    tool_id = message.get("toolCallId")
    if not isinstance(tool_id, str):
        skipped["unlinked-result"] += 1
        return
    pending = session.pending.pop(tool_id, None)
    if pending is None:
        skipped["unlinked-result"] += 1
        return
    (action, mapped, arguments) = pending
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


def convert_claude_file(path, dest):
    try:
        entries, status = parse(path)
    except OSError as error:
        return fail("cannot read %s: %s" % (path, error))
    if entries is None:
        return status
    workspace, status = claude_workspace(entries, path)
    if workspace is None:
        return status
    main_session = Session("s_main", workspace)
    side_session = Session("s_side", workspace)
    skipped = Counter()
    status = convert_claude(entries, main_session, side_session, skipped, path)
    if status:
        return status
    sessions = {"s_main": main_session}
    if any(bool(entry.get("isSidechain")) for _, entry in entries):
        sessions["s_side"] = side_session
    return write_out(dest, sessions, skipped)


def convert_pi_file(path, dest):
    try:
        entries, status = parse(path)
    except OSError as error:
        return fail("cannot read %s: %s" % (path, error))
    if entries is None:
        return status
    if not entries:
        return fail("%s: no session header" % path)
    first_lineno, header = entries[0]
    if (
        header.get("type") != "session"
        or header.get("version") != 3
        or not isinstance(header.get("id"), str)
        or not header.get("id")
    ):
        return fail("%s:%d: no version-3 session header" % (path, first_lineno))
    workspace, status = check_workspace(header.get("cwd"), path, first_lineno)
    if workspace is None:
        return status
    branch, status = pi_branch(entries[1:], path)
    if branch is None:
        return status
    session = Session("s_pi", workspace)
    skipped = Counter()
    skipped["entry:session"] += 1
    siblings = len(entries) - 1 - len(branch)
    if siblings > 0:
        # Entries off the active branch are never merged in.
        skipped["sibling-branch"] += siblings
    status = convert_pi(branch, session, skipped, path)
    if status:
        return status
    return write_out(dest, {"s_pi": session}, skipped)


def write_sessions(dest, sessions):
    for session_id, session in sessions.items():
        if not session.lines:
            continue
        out = os.path.join(dest, session_id)
        os.makedirs(out)
        with open(os.path.join(out, "events.jsonl"), "w", encoding="utf-8") as handle:
            for line in session.lines:
                # Compact separators, as the session log itself writes:
                # greps over the wire form match `"kind":"..."`.
                handle.write(json.dumps(line, sort_keys=False, separators=(",", ":")) + "\n")


def write_out(dest, sessions, skipped):
    try:
        os.makedirs(dest)
        write_sessions(dest, sessions)
    except OSError as error:
        return fail("cannot write %s: %s" % (dest, error))
    wrote = sorted(session_id for session_id, session in sessions.items() if session.lines)
    total = sum(len(session.lines) for session in sessions.values())
    skips = ", ".join("%s x%d" % (name, count) for name, count in sorted(skipped.items()))
    sys.stdout.write(
        "convert_session: wrote %d sessions (%s, %d lines)%s\n"
        % (len(wrote), ",".join(wrote), total, ("; skipped " + skips) if skips else "")
    )
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="convert_session.py")
    parser.add_argument("--format", required=True)
    parser.add_argument("input")
    parser.add_argument("output")
    args = parser.parse_args(argv)
    if os.path.exists(args.output):
        return fail("refusing to overwrite existing %s" % args.output)
    if args.format == "claude":
        return convert_claude_file(args.input, args.output)
    if args.format == "pi":
        return convert_pi_file(args.input, args.output)
    return fail("unsupported format %r" % args.format)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
