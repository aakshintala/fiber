#!/usr/bin/env python3
"""What the owner's pi and Claude Code sessions say about what a TUI must show."""
import glob, json, os, statistics as st
from collections import Counter, defaultdict
from datetime import datetime

HOME = os.path.expanduser("~")

def ts(s):
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
    except Exception:
        return None

def lines(path):
    with open(path, errors="replace") as f:
        for l in f:
            try:
                yield json.loads(l)
            except Exception:
                pass

PI_BG = {"Agent", "subagent_spawn", "agent_bg", "bash_bg", "TaskExecute", "process"}
CC_WRAPPERS = ("<task-notification>", "<command-", "<local-command", "<system-reminder>", "Caveat:", "[Request interrupted")

def new():
    return dict(prompts=0, tools=0, texts=0, interrupts=0, queued=0, ask=0, denied=0, bg=0,
                parallel_agents=0, t0=None, t1=None, per_turn=[], cur=0, runs=[], run=0)

def bump_time(s, t):
    if t:
        s["t0"] = t if s["t0"] is None else min(s["t0"], t)
        s["t1"] = t if s["t1"] is None else max(s["t1"], t)

def prompt(s):
    if s["prompts"]:
        s["per_turn"].append(s["cur"])
    s["prompts"] += 1
    s["cur"] = 0

def tool(s, n=1):
    s["tools"] += n; s["cur"] += n; s["run"] += n

def text(s):
    if s["run"]:
        s["runs"].append(s["run"])
    s["run"] = 0
    s["texts"] += 1

def close(s):
    if s["prompts"]:
        s["per_turn"].append(s["cur"])
    if s["run"]:
        s["runs"].append(s["run"])
    return s

def pi_session(path):
    s = new(); head = None
    for e in lines(path):
        head = head or e
        bump_time(s, ts(e.get("timestamp", "")))
        if e.get("type") != "message":
            continue
        m = e["message"]; c = m.get("content") or []
        if isinstance(c, str):
            c = [{"type": "text", "text": c}]
        if m.get("role") == "user":
            prompt(s)
        elif m.get("role") == "assistant":
            calls = [p for p in c if p.get("type") == "toolCall"]
            if any(p.get("type") == "text" and p.get("text", "").strip() for p in c):
                text(s)
            tool(s, len(calls))
            s["ask"] += sum(p.get("name") in ("ask_user", "ask_user_question") for p in calls)
            s["bg"] += sum(p.get("name") in PI_BG for p in calls)
            if sum(p.get("name") in ("Agent", "subagent_spawn") for p in calls) > 1:
                s["parallel_agents"] += 1
            if m.get("stopReason") == "aborted":
                s["interrupts"] += 1
    return head, close(s)

CC_DELEGATE = ("Agent", "Task", "mcp__plugin_cursor-delegate_cursor-delegate__cursor_run", "mcp__cursor-delegate__cursor_run")

def cc_session(path):
    s = new(); per_msg = Counter()
    for e in lines(path):
        bump_time(s, ts(e.get("timestamp", "")))
        t = e.get("type")
        if t == "attachment" and (e.get("attachment") or {}).get("type") == "queued_command" and e["attachment"].get("commandMode") == "prompt":
            s["queued"] += 1
        if t == "user" and not e.get("isMeta"):
            c = e["message"].get("content")
            if isinstance(c, str):
                c = [{"type": "text", "text": c}]
            for p in c or []:
                if p.get("type") == "tool_result":
                    body = json.dumps(p.get("content"))
                    if p.get("is_error") and "doesn't want to proceed" in body:
                        s["denied"] += 1
                elif p.get("type") == "text":
                    tx = p.get("text", "").lstrip()
                    if tx.startswith("[Request interrupted"):
                        s["interrupts"] += 1
                    elif not tx.startswith(CC_WRAPPERS):
                        prompt(s)
        if t == "assistant":
            c = e["message"].get("content") or []
            calls = [p for p in c if p.get("type") == "tool_use"]
            if any(p.get("type") == "text" and p.get("text", "").strip() for p in c):
                text(s)
            tool(s, len(calls))
            for p in calls:
                i = p.get("input") or {}
                if p.get("name") == "AskUserQuestion":
                    s["ask"] += 1
                if i.get("run_in_background") or p.get("name") == "Monitor":
                    s["bg"] += 1
            mid = e["message"].get("id")
            for p in calls:
                if p.get("name") in CC_DELEGATE:
                    per_msg[mid] += 1; s["delegates"] = s.get("delegates", 0) + 1
    s["parallel_agents"] = sum(1 for v in per_msg.values() if v > 1)
    s["max_parallel"] = max(per_msg.values(), default=0)
    return close(s)

def interval(path):
    t = [ts(e.get("timestamp", "")) for e in lines(path)]
    t = [x for x in t if x]
    return (min(t), max(t)) if t else None

def max_overlap(iv):
    ev = sorted([(a, 1) for a, b in iv] + [(b, -1) for a, b in iv])
    cur = best = 0
    for _, d in ev:
        cur += d; best = max(best, cur)
    return best

def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else 0

def report(name, sess, children):
    sess = [s for s in sess if s["prompts"] > 0]
    n = len(sess)
    print(f"\n== {name}: {n} main sessions with a prompt")
    def share(f):
        return f"{sum(1 for s in sess if f(s))}/{n} ({100*sum(1 for s in sess if f(s))//max(n,1)}%)"
    turns = [s["prompts"] for s in sess]
    print(f"prompts per session: p50 {pct(turns,.5)}  p90 {pct(turns,.9)}  max {max(turns)}")
    hrs = [(s["t1"] - s["t0"]) / 3600 for s in sess if s["t0"]]
    print(f"wall hours per session: p50 {pct(hrs,.5):.1f}  p90 {pct(hrs,.9):.1f}")
    pt = [x for s in sess for x in s["per_turn"]]
    print(f"tool calls per prompt: p50 {pct(pt,.5)}  p90 {pct(pt,.9)}  p99 {pct(pt,.99)}  max {max(pt)}")
    runs = [x for s in sess for x in s["runs"]]
    print(f"tool calls between two pieces of assistant text: p50 {pct(runs,.5)}  p90 {pct(runs,.9)}  max {max(runs)}")
    tot = lambda k: sum(s[k] for s in sess)
    print(f"interrupts: {tot('interrupts')} total; sessions with one: {share(lambda s: s['interrupts'])}")
    print(f"messages typed mid-turn (queued): {tot('queued')} total; sessions with one: {share(lambda s: s['queued'])}")
    print(f"questions to the user (ask tools): {tot('ask')}; sessions: {share(lambda s: s['ask'])}")
    print(f"permission denials: {tot('denied')}; sessions: {share(lambda s: s['denied'])}")
    print(f"background launches: {tot('bg')}; sessions with one: {share(lambda s: s['bg'])}")
    print(f"messages launching >1 agent at once: {tot('parallel_agents')}; sessions: {share(lambda s: s['parallel_agents'])}")
    if name == "Claude Code":
        d = [s.get("delegates", 0) for s in sess if s.get("delegates")]
        print(f"sessions launching delegates (Agent or cursor_run): {len(d)}/{n}; per session p50 {pct(d,.5)} p90 {pct(d,.9)} max {max(d)}")
        c = Counter(min(s.get("max_parallel", 0), 5) for s in sess if s.get("delegates"))
        print("  most delegates launched in one message: " + ", ".join(f"{k if k<5 else '5+'}: {c[k]}" for k in sorted(c)))
    mo = [max_overlap(iv) for iv in children.values() if iv]
    mains_with_children = len(mo)
    print(f"sessions with delegates (child transcripts): {mains_with_children}/{n}")
    if mo:
        c = Counter(min(x, 5) for x in mo)
        print("  max delegates running at once: " + ", ".join(f"{k if k<5 else '5+'}: {c[k]}" for k in sorted(c)))
        cnt = [len(iv) for iv in children.values()]
        print(f"  delegates per session: p50 {pct(cnt,.5)}  p90 {pct(cnt,.9)}  max {max(cnt)}")

# pi
pi_main, pi_children = [], defaultdict(list)
for p in glob.glob(f"{HOME}/.pi/agent/sessions/**/*.jsonl", recursive=True):
    head, s = pi_session(p)
    parent = (head or {}).get("parentSession")
    if parent:
        iv = (s["t0"], s["t1"]) if s["t0"] else None
        if iv:
            pi_children[parent].append(iv)
    else:
        pi_main.append(s)
report("pi", pi_main, pi_children)

# Claude Code
cc_main, cc_children = [], defaultdict(list)
for p in glob.glob(f"{HOME}/.claude/projects/*/*.jsonl"):
    cc_main.append(cc_session(p))
for p in glob.glob(f"{HOME}/.claude/projects/*/*/subagents/*.jsonl"):
    iv = interval(p)
    if iv:
        cc_children[p.split("/subagents/")[0]].append(iv)
report("Claude Code", cc_main, cc_children)
