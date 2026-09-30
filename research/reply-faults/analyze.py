#!/usr/bin/env python3
"""Count malformed-reply faults in the owner's pi and Claude Code sessions.

Usage: python3 analyze.py [--examples N]  -> writes results.json next to this file
and prints the tables. Read-only over ~/.pi/agent/sessions and ~/.claude/projects.
"""
import collections
import glob
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
HOME = os.path.expanduser("~")
NEX = int(sys.argv[sys.argv.index("--examples") + 1]) if "--examples" in sys.argv else 6

# Tool-call syntax that belongs in the tool channel, not in visible text.
LEAK_RE = re.compile(
    r"<tool_call>|</tool_call>|<function_calls>|<invoke[ >]|<\|tool|\[TOOL_CALLS\]|to=functions\.|"
    r"<function=|<tool_use>|<parameter name=|antml:invoke|"
    r"```json\s*\{\s*\"(name|tool|tool_name|function)\"\s*:|"
    r"^\s*\{\s*\"(name|tool_name)\"\s*:\s*\"[A-Za-z_]+\"\s*,\s*\"(arguments|parameters|input|args)\"",
    re.M,
)
# Names from other harnesses' tool dialects.
FOREIGN = {
    "apply_patch", "shell", "str_replace_editor", "str_replace_based_edit_tool", "container.exec",
    "local_shell", "exec_command", "run_terminal_cmd", "read_file", "write_file", "edit_file",
    "list_dir", "codebase_search", "grep_search", "file_search", "search_replace", "view",
    "Bash", "Read", "Edit", "Write", "Grep", "Glob", "MultiEdit", "LS", "TodoWrite", "WebFetch",
    "bash", "read", "edit", "write", "grep", "find", "ls",
}
REPEAT_TAIL_RE = re.compile(r"(.{40,400}?)\1{4,}", re.S)


def repetition(text):
    """Return a short description if the text repeats itself, else None."""
    lines = [l.strip() for l in text.splitlines() if len(l.strip()) >= 40]
    if lines:
        line, n = collections.Counter(lines).most_common(1)[0]
        if n >= 8:
            return "line x%d: %s" % (n, line[:120])
    m = REPEAT_TAIL_RE.search(text[-6000:])
    if m:
        return "run x%d: %s" % (len(m.group(0)) // len(m.group(1)), m.group(1)[:120])
    return None


def jtype(v):
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        return "array"
    if isinstance(v, dict):
        return "object"
    return "null"


def check(v, s, path="", out=None, required=True):
    """Tiny JSON-schema checker. Returns ["code path", ...] with codes:
    null_optional, string_scalar, stringified_json, missing_required, unexpected_key, enum, wrong_type."""
    out = [] if out is None else out
    if not isinstance(s, dict):
        return out
    for key in ("anyOf", "oneOf"):
        if key in s:
            if not any(not check(v, sub, path) for sub in s[key]):
                out.append("wrong_type %s (no %s branch)" % (path or "/", key))
            return out
    t = s.get("type")
    ts = t if isinstance(t, list) else [t] if t else []
    vt = jtype(v)
    if ts and not (vt in ts or (vt == "integer" and "number" in ts)):
        code = "wrong_type"
        if vt == "null" and not required:
            code = "null_optional"
        elif vt == "string" and set(ts) & {"number", "integer", "boolean"}:
            code = "string_scalar"
        elif vt == "string" and set(ts) & {"array", "object"}:
            try:
                code = "stringified_json" if jtype(json.loads(v)) in ts else code
            except ValueError:
                pass
        out.append("%s %s want %s got %s" % (code, path or "/", "|".join(ts), vt))
        return out
    if "enum" in s and v not in s["enum"]:
        out.append("enum %s %r" % (path or "/", v)[:120])
    if "const" in s and v != s["const"]:
        out.append("enum %s %r" % (path or "/", v)[:120])
    if vt == "object":
        props = s.get("properties") or {}
        req = s.get("required") or []
        for r in req:
            if r not in v:
                out.append("missing_required %s/%s" % (path, r))
        for k, sub in v.items():
            if k in props:
                check(sub, props[k], path + "/" + k, out, k in req)
            elif s.get("additionalProperties") is False:
                out.append("unexpected_key %s/%s" % (path, k[:60]))
    if vt == "array" and isinstance(s.get("items"), dict):
        for i, item in enumerate(v):
            check(item, s["items"], "%s/%d" % (path, i), out)
    return out


def load(path):
    with open(path, errors="replace") as fh:
        for i, line in enumerate(fh, 1):
            try:
                yield i, json.loads(line)
            except ValueError:
                yield i, None


def short(path):
    return path.replace(HOME, "~")


class Tally:
    def __init__(self):
        self.c = collections.Counter()           # (model, metric) -> n
        self.ex = collections.defaultdict(list)  # metric -> examples
        self.sessions = collections.Counter()    # metric -> sessions touched
        self.recovery = collections.defaultdict(list)  # metric -> turns-to-recover
        self.per_session = set()

    def add(self, model, metric, where=None, detail=None, session=None):
        self.c[(model, metric)] += 1
        self.c[("ALL", metric)] += 1
        if session and (session, metric) not in self.per_session:
            self.per_session.add((session, metric))
            self.sessions[metric] += 1
        if where and len(self.ex[metric]) < NEX:
            self.ex[metric].append("%s:%d  %s" % (short(where[0]), where[1], (detail or "")[:300]))

    def table(self, metrics, min_turns=50):
        models = sorted({m for m, _ in self.c if m != "ALL"}, key=lambda m: -self.c[(m, "turns")])
        rows = []
        for m in ["ALL"] + models:
            if m != "ALL" and self.c[(m, "turns")] < min_turns:
                continue
            rows.append([m] + [self.c[(m, k)] for k in metrics])
        small = [m for m in models if self.c[(m, "turns")] < min_turns]
        return {"columns": ["model"] + metrics, "rows": rows, "folded_small_models": small}

    def dump(self, metrics):
        rec = {}
        for k, v in self.recovery.items():
            v2 = sorted(x for x in v if x is not None)
            rec[k] = {"n": len(v), "never_in_session": sum(1 for x in v if x is None),
                      "median_turns": v2[len(v2) // 2] if v2 else None,
                      "le1": sum(1 for x in v2 if x <= 1), "le3": sum(1 for x in v2 if x <= 3)}
        return {"table": self.table(metrics), "sessions_touched": dict(self.sessions),
                "examples": dict(self.ex), "recovery": rec}


def recover_turns(turns, idx, tool):
    """Assistant turns after turns[idx] until a call to `tool` succeeds (None = never)."""
    for j in range(idx + 1, min(idx + 30, len(turns))):
        for name, ok in turns[j]["calls"]:
            if name and tool and name.lower() == tool.lower() and ok:
                return j - idx
    return None


# ---------------------------------------------------------------- pi
def run_pi():
    T = Tally()
    files = sorted(glob.glob(HOME + "/.pi/agent/sessions/**/*.jsonl", recursive=True))
    for f in files:
        offered = None
        schemas = {}
        turns = []           # per assistant turn: model, calls [(name, ok)]
        by_call = {}         # toolCallId -> (turn index, name, args, where, model)
        T.c[("ALL", "sessions")] += 1
        for i, d in load(f):
            if d is None:
                T.add("ALL", "bad_jsonl_line", (f, i))
                continue
            m = d.get("message") or {}
            sysm = m if m.get("role") == "system" else d.get("systemMessage") if d.get("type") == "compaction" else None
            if sysm:
                if d.get("type") == "compaction":
                    offered, schemas = set(), {}
                offered = set() if offered is None else offered
                for t in sysm.get("toolsAdded") or []:
                    offered.add(t["name"])
                    schemas[t["name"]] = t.get("parameters")
                for t in sysm.get("toolsRemoved") or []:
                    offered.discard(t["name"])
            role = m.get("role")
            if role == "assistant":
                model = "%s/%s" % (m.get("provider"), m.get("model"))
                T.add(model, "turns", session=f)
                parts = m.get("content") or []
                if isinstance(parts, str):
                    parts = [{"type": "text", "text": parts}]
                texts = [p.get("text") or "" for p in parts if p.get("type") == "text"]
                thinks = [p.get("thinking") or "" for p in parts if p.get("type") == "thinking"]  # may be "" (redacted)
                calls = [p for p in parts if p.get("type") == "toolCall"]
                sr = m.get("stopReason")
                T.add(model, "stop:%s" % sr)
                em = m.get("errorMessage")
                if em:
                    T.add(model, "errkind:%s" % classify_error(em), (f, i), em)
                real = "".join(texts).strip()
                if sr not in ("error", "aborted"):
                    if not calls and not real:
                        T.add(model, "empty_reply" if not thinks else "thinking_only",
                              (f, i), "stop=%s thinking=%d chars" % (sr, sum(map(len, thinks))), f)
                    if sr == "toolUse" and not calls:
                        T.add(model, "stop_tooluse_no_call", (f, i), real[:200], f)
                    if calls and sr == "stop":
                        T.add(model, "calls_with_stop_stop", (f, i), calls[0].get("name"), f)
                for t in texts:
                    mm = LEAK_RE.search(t)
                    if mm:
                        key = "leak_text_no_call" if not calls else "leak_text_with_call"
                        s = max(0, mm.start() - 60)
                        T.add(model, key, (f, i), t[s:mm.end() + 160].replace("\n", "\\n"), f)
                for t in thinks:
                    if LEAK_RE.search(t) and not calls:
                        T.add(model, "leak_thinking_no_call", (f, i), None, f)
                for kind, blob in (("text", "\n".join(texts)), ("thinking", "\n".join(thinks))):
                    r = repetition(blob) if blob else None
                    if r:
                        T.add(model, "repeat_%s" % kind, (f, i), "stop=%s %s" % (sr, r), f)
                turn = {"model": model, "calls": []}
                for c in calls:
                    T.add(model, "tool_calls")
                    name, args = c.get("name"), c.get("arguments")
                    if not isinstance(args, dict):
                        T.add(model, "args_not_object", (f, i), "%s %r" % (name, args)[:200], f)
                    elif any(k in args for k in ("_raw", "__raw", "parse_error", "_parseError")):
                        T.add(model, "args_parse_marker", (f, i), "%s %s" % (name, json.dumps(args)[:200]), f)
                    if offered is not None and name not in offered:
                        T.add(model, "name_not_offered", (f, i), "%s  offered=%d" % (name, len(offered)), f)
                        if name in FOREIGN:
                            T.add(model, "name_foreign_dialect", (f, i), name, f)
                    # Only sessions that declared their tools: schemas drift between sessions.
                    sch = schemas.get(name)
                    if sch and isinstance(args, dict):
                        T.add(model, "calls_schema_checked")
                        errs = check(args, sch)
                        if not args and (sch.get("required") or []):
                            errs = ["empty_args /"]
                        if errs:
                            codes = sorted({e.split()[0] for e in errs})
                            T.add(model, "args_schema_mismatch", (f, i), "%s: %s | %s" % (
                                name, "; ".join(errs[:3]), json.dumps(args)[:160]), f)
                            benign = {"null_optional", "string_scalar"}
                            if not set(codes) - benign:
                                T.add(model, "args_coercible_only", None, None, f)
                            for code in codes:
                                T.add(model, "schema_%s" % code, (f, i), "%s: %s" % (name, json.dumps(args)[:200]), f)
                    if name and name != name.strip() or (name and not re.match(r"^[A-Za-z0-9_.\-]+$", name)):
                        T.add(model, "name_malformed", (f, i), repr(name), f)
                    by_call[c.get("id")] = (len(turns), name, (f, i), model)
                    turn["calls"].append([name, None])
                turns.append(turn)
            elif role == "toolResult":
                info = by_call.get(m.get("toolCallId"))
                text = " ".join(p.get("text", "") for p in m.get("content") or [] if isinstance(p, dict))
                model = info[3] if info else "unknown"
                T.add(model, "tool_results")
                if info:
                    for c in turns[info[0]]["calls"]:
                        if c[0] == info[1] and c[1] is None:
                            c[1] = not m.get("isError")
                            break
                if m.get("isError"):
                    T.add(model, "tool_errors")
                    kind = None
                    if text.startswith("Validation failed for tool"):
                        kind = "result_validation_failed"
                    elif re.match(r"Tool \S+ not found", text):
                        kind = "result_unknown_tool"
                    if kind:
                        T.add(model, kind, (f, i), text.replace("\n", " | "), f)
                        if info:
                            T.recovery[kind].append((info[0], info[1], f))
        # resolve recovery for this session
        for kind, lst in T.recovery.items():
            for n, item in enumerate(lst):
                if isinstance(item, tuple) and item[2] == f:
                    lst[n] = recover_turns([{"calls": [(a, b) for a, b in t["calls"]]} for t in turns], item[0], item[1])
    return T


def classify_error(em):
    e = em.lower()
    for k, pats in (
        ("rate_limit_or_quota", ("429", "rate_limit", "usage limit", "quota", "free tier")),
        ("user_abort", ("abort",)),
        ("stream_truncated", ("stream ended", "terminated", "without finish_reason", "before a terminal")),
        ("network", ("connection error", "fetch failed", "websocket", "timed out", "econnreset")),
        ("auth", ("(401)", "(403)", "authentication", "third-party apps", "invalid credential")),
        ("bad_request_400", ("(400)", "400:", "400 {", "invalid_request")),
        ("server_5xx_or_overloaded", ("500", "502", "503", "504", "overloaded", "server_error", "internal server")),
    ):
        if any(p in e for p in pats):
            return k
    return "other"


# ---------------------------------------------------------------- Claude Code
def run_cc(sub):
    T = Tally()
    files = sorted(f for f in glob.glob(HOME + "/.claude/projects/**/*.jsonl", recursive=True)
                   if ("/subagents/" in f) == sub)
    for f in files:
        T.c[("ALL", "sessions")] += 1
        msgs = collections.OrderedDict()   # message.id -> merged turn
        results = {}                       # tool_use_id -> (is_error, text, where)
        order = []
        for i, d in load(f):
            if d is None:
                T.add("ALL", "bad_jsonl_line", (f, i))
                continue
            if d.get("type") == "assistant":
                m = d.get("message") or {}
                mid = m.get("id") or d.get("uuid")
                t = msgs.get(mid)
                if t is None:
                    t = msgs[mid] = {"model": m.get("model"), "parts": [], "stop": None, "where": (f, i),
                                     "api_error": d.get("error") if d.get("isApiErrorMessage") else None}
                    order.append(mid)
                t["parts"] += [p for p in m.get("content") or [] if isinstance(p, dict)]
                t["stop"] = m.get("stop_reason") or t["stop"]
            elif d.get("type") == "user":
                c = (d.get("message") or {}).get("content")
                if order and "[Request interrupted by user" in json.dumps(c)[:200]:
                    msgs[order[-1]]["interrupted"] = True
                for p in c if isinstance(c, list) else []:
                    if isinstance(p, dict) and p.get("type") == "tool_result":
                        x = p.get("content")
                        x = x if isinstance(x, str) else " ".join(
                            y.get("text", "") for y in x or [] if isinstance(y, dict))
                        results[p.get("tool_use_id")] = (bool(p.get("is_error")), x, (f, i))
        turns = []
        for mid in order:
            t = msgs[mid]
            model = t["model"]
            T.add(model, "turns", session=f)
            if model == "<synthetic>":
                T.add(model, "synthetic_%s" % (t["api_error"] or "other"), t["where"],
                      " ".join(p.get("text", "") for p in t["parts"])[:200], f)
                turns.append({"calls": []})
                continue
            texts = [p.get("text") or "" for p in t["parts"] if p.get("type") == "text"]
            thinks = [p.get("thinking") or "" for p in t["parts"] if p.get("type") in ("thinking", "redacted_thinking")]
            calls = [p for p in t["parts"] if p.get("type") == "tool_use"]
            T.add(model, "stop:%s" % t["stop"])
            if not calls and not "".join(texts).strip():
                k = "empty_reply" if not thinks else "thinking_only"
                T.add(model, k + ("_user_interrupted" if t.get("interrupted") else ""),
                      t["where"], "stop=%s" % t["stop"], f)
            if t["stop"] == "tool_use" and not calls:
                T.add(model, "stop_tooluse_no_call", t["where"], None, f)
            for x in texts:
                mm = LEAK_RE.search(x)
                if mm:
                    s = max(0, mm.start() - 60)
                    T.add(model, "leak_text_no_call" if not calls else "leak_text_with_call", t["where"],
                          x[s:mm.end() + 160].replace("\n", "\\n"), f)
            for kind, blob in (("text", "\n".join(texts)), ("thinking", "\n".join(thinks))):
                r = repetition(blob) if blob else None
                if r:
                    T.add(model, "repeat_%s" % kind, t["where"], "stop=%s %s" % (t["stop"], r), f)
            turn = {"calls": []}
            for c in calls:
                T.add(model, "tool_calls")
                name, args = c.get("name"), c.get("input")
                if not isinstance(args, dict):
                    T.add(model, "args_not_object", t["where"], "%s %r" % (name, args)[:200], f)
                if name in FOREIGN and name[0].islower():
                    T.add(model, "name_foreign_dialect", t["where"], name, f)
                res = results.get(c.get("id"))
                ok = res is not None and not res[0]
                if res:
                    T.add(model, "tool_results")
                if res and res[0]:
                    T.add(model, "tool_errors")
                    txt = res[1]
                    kind = None
                    if "InputValidationError" in txt:
                        kind = "result_validation_failed"
                        sub = ("json_parse" if "could not be parsed as JSON" in txt else
                               "schema_not_sent" if "schema was not sent" in txt else
                               "unexpected_param" if "unexpected parameter" in txt else
                               "constraint" if '"code"' in txt else "type_or_missing")
                        T.add(model, "validation_%s" % sub, res[2], "%s: %s" % (name, txt.replace("\n", " | ")), f)
                    elif "No such tool available" in txt:
                        kind = "result_unknown_tool"
                    if kind:
                        T.add(model, kind, res[2], "%s: %s" % (name, txt.replace("\n", " | ")), f)
                        T.recovery[kind].append((len(turns), name))
                turn["calls"].append((name, ok))
            turns.append(turn)
        for kind, lst in T.recovery.items():
            for n, item in enumerate(lst):
                if isinstance(item, tuple):
                    lst[n] = recover_turns(turns, item[0], item[1])
    return T


METRICS_TURN = ["turns", "tool_calls", "tool_errors"]
METRICS_A = ["result_validation_failed", "calls_schema_checked", "args_schema_mismatch", "args_coercible_only",
             "args_not_object", "args_parse_marker"]
METRICS_B = ["result_unknown_tool", "name_not_offered", "name_foreign_dialect", "name_malformed"]
METRICS_C = ["leak_text_no_call", "leak_text_with_call", "leak_thinking_no_call"]
METRICS_D = ["repeat_text", "repeat_thinking"]
METRICS_E = ["empty_reply", "thinking_only", "empty_reply_user_interrupted", "thinking_only_user_interrupted",
             "stop_tooluse_no_call", "calls_with_stop_stop"]


def main():
    out = {}
    for name, fn in (("pi", run_pi), ("cc_main", lambda: run_cc(False)), ("cc_subagents", lambda: run_cc(True))):
        T = fn()
        stops = sorted({k for _, k in T.c if k.startswith(("stop:", "errkind:", "synthetic_", "schema_", "validation_"))})
        metrics = METRICS_TURN + METRICS_A + METRICS_B + METRICS_C + METRICS_D + METRICS_E + stops
        out[name] = T.dump(metrics)
        out[name]["sessions"] = T.c[("ALL", "sessions")]
        print("==", name, "sessions", out[name]["sessions"])
        tab = out[name]["table"]
        cols = [c for i, c in enumerate(tab["columns"]) if i == 0 or any(r[i] for r in tab["rows"])]
        idx = [tab["columns"].index(c) for c in cols]
        print("\t".join(cols))
        for r in tab["rows"]:
            print("\t".join(str(r[i]) for i in idx))
        print("sessions touched:", out[name]["sessions_touched"])
        print("recovery:", out[name]["recovery"])
    with open(os.path.join(HERE, "results.json"), "w") as fh:
        json.dump(out, fh, indent=1)


def selftest():
    assert repetition("x" * 5 + "\n" + "\n".join(["the same long line of generated text repeats"] * 9))
    assert repetition("prefix " + "abcdefghij klmnopqrst uvwxyz0123456789 ABCD " * 6)
    assert not repetition("\n".join("distinct line number %d with enough length to count" % i for i in range(50)))
    assert LEAK_RE.search('ok <tool_call>{"name": "bash"}</tool_call>')
    assert LEAK_RE.search('```json\n{"name": "read", "arguments": {}}')
    assert not LEAK_RE.search("plain prose about tools")
    sch = {"type": "object", "required": ["a"], "additionalProperties": False, "properties": {
        "a": {"type": "array"}, "n": {"type": "number"}, "o": {"type": "string"}}}
    codes = lambda v: sorted(e.split()[0] for e in check(v, sch))
    assert codes({"a": "[1]"}) == ["stringified_json"]
    assert codes({"a": [], "n": "3"}) == ["string_scalar"]
    assert codes({"a": [], "o": None}) == ["null_optional"]
    assert codes({"z": 1}) == ["missing_required", "unexpected_key"]
    print("selftest ok")


if __name__ == "__main__":
    selftest() if "--selftest" in sys.argv else main()
