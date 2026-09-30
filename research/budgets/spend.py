#!/usr/bin/env python3
"""Measure real spend from pi and Claude Code session logs (stdlib only).

Sources:
  - pi:                 ~/.pi/agent/sessions/**/*.jsonl
  - Claude Code main:    ~/.claude/projects/*/*.jsonl  (top level, not in subagents/)
  - Claude Code subagent: ~/.claude/projects/*/*/subagents/*.jsonl

Writes research/budgets/spend.md next to this script.
"""
import glob
import json
import os
import statistics
from collections import Counter, defaultdict
from datetime import datetime

HOME = os.path.expanduser("~")
PI_GLOB = os.path.join(HOME, ".pi/agent/sessions/**/*.jsonl")
CC_GLOB = os.path.join(HOME, ".claude/projects/**/*.jsonl")
OUT_MD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "spend.md")

# USD per million tokens: input, output, cache_read, cache_write_5m, cache_write_1h
PRICES = {
    "opus":   dict(input=5,  output=25, cache_read=0.5,  cache_write_5m=6.25, cache_write_1h=10),
    "sonnet": dict(input=3,  output=15, cache_read=0.3,  cache_write_5m=3.75, cache_write_1h=6),
    "haiku":  dict(input=1,  output=5,  cache_read=0.1,  cache_write_5m=1.25, cache_write_1h=2),
    # extracted from ~/.local/share/claude/versions/<latest> via strings:
    # tier_10_50_cache_read_0_25:{input:10,output:50,cache_write_5m:12.5,cache_write_1h:20,cache_read:0.25}
    "fable":  dict(input=10, output=50, cache_read=0.25, cache_write_5m=12.5, cache_write_1h=20),
}

# Providers seen in pi logs, classified by how they're paid for (from provider name only；
# no explicit auth-type field exists in the logs).
SUBSCRIPTION_PROVIDERS = {"openai-codex", "pi-claude-cli"}  # ChatGPT/Codex login, Claude Code CLI OAuth reuse
API_KEY_PROVIDERS = {"anthropic", "opencode-go", "oc-sdk-go", "opencode", "cursor"}


def family(model):
    if not model:
        return None
    m = model.lower()
    for fam in ("opus", "sonnet", "haiku", "fable"):
        if fam in m:
            return fam
    return None


def parse_ts(ts):
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except Exception:
        return None


def pctl(vals, p):
    if not vals:
        return 0.0
    s = sorted(vals)
    k = (len(s) - 1) * (p / 100)
    f, c = int(k), min(int(k) + 1, len(s) - 1)
    if f == c:
        return s[f]
    return s[f] + (s[c] - s[f]) * (k - f)


def cc_cost(usage, model):
    fam = family(model)
    if fam is None:
        return None  # unpriced (unknown/synthetic model)
    p = PRICES[fam]
    inp = usage.get("input_tokens") or 0
    out = usage.get("output_tokens") or 0
    cread = usage.get("cache_read_input_tokens") or 0
    cc = usage.get("cache_creation") or {}
    c5m = cc.get("ephemeral_5m_input_tokens")
    c1h = cc.get("ephemeral_1h_input_tokens")
    if c5m is None and c1h is None:
        # no split available: whatever cache_creation_input_tokens says, assume 5m (default TTL)
        c5m = usage.get("cache_creation_input_tokens") or 0
        c1h = 0
    else:
        c5m = c5m or 0
        c1h = c1h or 0
    cost = (inp * p["input"] + out * p["output"] + cread * p["cache_read"]
            + c5m * p["cache_write_5m"] + c1h * p["cache_write_1h"]) / 1e6
    tokens = inp + out + cread + c5m + c1h
    return cost, tokens


class Turn:
    __slots__ = ("start", "cost", "tokens", "calls")

    def __init__(self, start):
        self.start = start
        self.cost = 0.0
        self.tokens = 0
        self.calls = 0


class Session:
    def __init__(self, source, path):
        self.source = source
        self.path = path
        self.cost = 0.0
        self.tokens = 0
        self.first_ts = None
        self.last_ts = None
        self.turns = []
        self.models = Counter()
        self.day_cost = defaultdict(float)  # only used for combining later
        self.unpriced_calls = 0

    def touch(self, ts):
        if ts is None:
            return
        if self.first_ts is None or ts < self.first_ts:
            self.first_ts = ts
        if self.last_ts is None or ts > self.last_ts:
            self.last_ts = ts

    def cur_turn(self, ts=None):
        if not self.turns:
            self.turns.append(Turn(ts))
        return self.turns[-1]

    def add_call(self, ts, cost, tokens, model):
        self.touch(ts)
        t = self.cur_turn(ts)
        t.cost += cost
        t.tokens += tokens
        t.calls += 1
        self.cost += cost
        self.tokens += tokens
        self.models[model] += 1
        if ts is not None:
            day = ts.date().isoformat()
            self.day_cost[day] += cost

    def new_turn(self, ts):
        self.touch(ts)
        self.turns.append(Turn(ts))

    def max_gap_minutes(self):
        starts = [t.start for t in self.turns if t.start is not None]
        if len(starts) < 2:
            return 0.0
        starts.sort()
        return max((b - a).total_seconds() for a, b in zip(starts, starts[1:])) / 60.0


def load_pi():
    sessions = []
    for path in glob.glob(PI_GLOB, recursive=True):
        sess = Session("pi", path)
        try:
            with open(path) as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        d = json.loads(line)
                    except Exception:
                        continue
                    ts = parse_ts(d.get("timestamp"))
                    if d.get("type") != "message":
                        continue
                    msg = d.get("message") or {}
                    role = msg.get("role")
                    if role == "user":
                        sess.new_turn(ts)
                    elif role == "assistant":
                        usage = msg.get("usage") or {}
                        cost = (usage.get("cost") or {}).get("total")
                        cost = cost if cost is not None else 0.0
                        tokens = usage.get("totalTokens")
                        if tokens is None:
                            tokens = (usage.get("input", 0) + usage.get("output", 0)
                                      + usage.get("cacheRead", 0) + usage.get("cacheWrite", 0))
                        model = msg.get("model")
                        provider = msg.get("provider")
                        sess.add_call(ts, cost, tokens, f"{provider}:{model}")
        except Exception:
            continue
        if sess.first_ts is not None:
            sessions.append(sess)
    return sessions


def is_turn_boundary(content):
    if isinstance(content, str):
        return True
    if isinstance(content, list):
        return any(isinstance(b, dict) and b.get("type") == "text" for b in content)
    return False


def load_claude_code():
    main_sessions, sub_sessions = [], []
    for path in glob.glob(CC_GLOB, recursive=True):
        rel = path[len(HOME):]
        source = "cc-sub" if "/subagents/" in rel else "cc-main"
        if "/memory/" in rel:
            continue
        sess = Session(source, path)
        seen_ids = set()
        try:
            with open(path) as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        d = json.loads(line)
                    except Exception:
                        continue
                    ts = parse_ts(d.get("timestamp"))
                    typ = d.get("type")
                    if typ == "user":
                        msg = d.get("message") or {}
                        if is_turn_boundary(msg.get("content")):
                            sess.new_turn(ts)
                        else:
                            sess.touch(ts)
                    elif typ == "assistant":
                        msg = d.get("message") or {}
                        mid = msg.get("id")
                        if mid is not None:
                            if mid in seen_ids:
                                sess.touch(ts)
                                continue
                            seen_ids.add(mid)
                        usage = msg.get("usage") or {}
                        model = msg.get("model")
                        result = cc_cost(usage, model)
                        if result is None:
                            sess.touch(ts)
                            sess.unpriced_calls += 1
                            continue
                        cost, tokens = result
                        sess.add_call(ts, cost, tokens, model)
                    else:
                        sess.touch(ts)
        except Exception:
            continue
        if sess.first_ts is not None:
            (main_sessions if source == "cc-main" else sub_sessions).append(sess)
    return main_sessions, sub_sessions


def dist_line(vals):
    if not vals:
        return "n=0"
    return (f"n={len(vals)}  p50={pctl(vals,50):.4f}  p90={pctl(vals,90):.4f}  "
            f"p99={pctl(vals,99):.4f}  max={max(vals):.4f}")


def fmt_usd(x):
    return f"${x:,.4f}"


def report_source(name, sessions):
    lines = [f"### {name}", ""]
    lines.append(f"Sessions: **{len(sessions)}**")
    unpriced = sum(getattr(s, "unpriced_calls", 0) for s in sessions)
    if unpriced:
        lines.append(f"(unpriced/unknown-model assistant calls excluded from cost: {unpriced})")
    lines.append("")

    costs = [s.cost for s in sessions]
    tokens = [s.tokens for s in sessions]
    lines.append("**1. Per-session cost / tokens**")
    lines.append("")
    lines.append("| | p50 | p90 | p99 | max |")
    lines.append("|---|---|---|---|---|")
    lines.append(f"| cost (USD) | {pctl(costs,50):.4f} | {pctl(costs,90):.4f} | {pctl(costs,99):.4f} | {max(costs) if costs else 0:.4f} |")
    lines.append(f"| tokens | {pctl(tokens,50):,.0f} | {pctl(tokens,90):,.0f} | {pctl(tokens,99):,.0f} | {max(tokens) if tokens else 0:,.0f} |")
    lines.append("")

    turn_costs = []
    calls_per_turn = []
    for s in sessions:
        for t in s.turns:
            if t.calls == 0:
                continue
            turn_costs.append(t.cost)
            calls_per_turn.append(t.calls)
    lines.append("**2. Per-turn cost (USD)**")
    lines.append("")
    lines.append(f"n={len(turn_costs)}  p50={pctl(turn_costs,50):.4f}  p90={pctl(turn_costs,90):.4f}  "
                 f"p99={pctl(turn_costs,99):.4f}  max={(max(turn_costs) if turn_costs else 0):.4f}")
    lines.append("")

    lines.append("**4. Model calls per turn (steps per turn)**")
    lines.append("")
    lines.append(f"n={len(calls_per_turn)}  p50={pctl(calls_per_turn,50):.1f}  p90={pctl(calls_per_turn,90):.1f}  "
                 f"p99={pctl(calls_per_turn,99):.1f}  max={(max(calls_per_turn) if calls_per_turn else 0)}")
    lines.append("")
    return "\n".join(lines), turn_costs


def day_totals(all_sessions_by_source):
    totals = defaultdict(lambda: defaultdict(float))  # day -> source -> cost
    for source, sessions in all_sessions_by_source.items():
        for s in sessions:
            for day, c in s.day_cost.items():
                totals[day][source] += c
    return totals


def top_sessions(all_sessions, n=5):
    ranked = sorted(all_sessions, key=lambda s: s.cost, reverse=True)[:n]
    out = []
    for s in ranked:
        dur_h = (s.last_ts - s.first_ts).total_seconds() / 3600.0 if (s.first_ts and s.last_ts) else 0.0
        n_turns = len([t for t in s.turns if t.calls])
        top_model = s.models.most_common(1)[0][0] if s.models else "?"
        gap = s.max_gap_minutes()
        unattended = f"max gap {gap/60:.1f}h" if gap >= 30 else "no long gaps"
        out.append(dict(source=s.source, path=s.path, cost=s.cost, dur_h=dur_h,
                         turns=n_turns, model=top_model, unattended=unattended))
    return out


def selftest():
    assert pctl([1, 2, 3, 4], 50) == 2.5
    assert pctl([1, 2, 3, 4], 0) == 1
    assert pctl([1, 2, 3, 4], 100) == 4
    cost, tokens = cc_cost(
        dict(input_tokens=1_000_000, output_tokens=0, cache_read_input_tokens=0),
        "claude-sonnet-5")
    assert abs(cost - 3.0) < 1e-9, cost
    cost, tokens = cc_cost(
        dict(input_tokens=0, output_tokens=1_000_000, cache_read_input_tokens=0),
        "claude-opus-5-5")
    assert abs(cost - 25.0) < 1e-9, cost
    cost, tokens = cc_cost(
        dict(input_tokens=0, output_tokens=0, cache_read_input_tokens=0,
             cache_creation={"ephemeral_1h_input_tokens": 1_000_000, "ephemeral_5m_input_tokens": 0}),
        "claude-fable-5-1")
    assert abs(cost - 20.0) < 1e-9, cost
    assert family("claude-opus-5-5") == "opus"
    assert family("claude-sonnet-5") == "sonnet"
    assert family("claude-haiku-4-5-20251001") == "haiku"
    assert family("<synthetic>") is None
    assert is_turn_boundary("hello") is True
    assert is_turn_boundary([{"type": "tool_result", "content": "x"}]) is False
    assert is_turn_boundary([{"type": "text", "text": "hi"}]) is True
    print("selftest ok")


def main():
    import sys
    if "--selftest" in sys.argv:
        selftest()
        return
    pi_sessions = load_pi()
    cc_main, cc_sub = load_claude_code()

    by_source = {"pi": pi_sessions, "cc-main": cc_main, "cc-sub": cc_sub}

    md = []
    md.append("# Owner spend: pi vs Claude Code (main + subagent)")
    md.append("")
    md.append(f"Generated {datetime.utcnow().isoformat()}Z by `spend.py`. "
               "Claude Code cost is API-equivalent (computed from tokens x price table); "
               "pi cost is the `usage.cost.total` the harness already logged, in USD, "
               "regardless of whether the underlying provider is a subscription login "
               "or an API key (see per-provider table below).")
    md.append("")

    turn_costs_by_source = {}
    for label, key in (("pi", "pi"), ("Claude Code main", "cc-main"), ("Claude Code subagent", "cc-sub")):
        section, turn_costs = report_source(label, by_source[key])
        md.append(section)
        turn_costs_by_source[key] = turn_costs

    # 3. per-day totals
    md.append("### 3. Per calendar day total spend (UTC day, all sources combined + per source)")
    md.append("")
    totals = day_totals(by_source)
    day_all = {day: sum(v.values()) for day, v in totals.items()}
    all_days_vals = list(day_all.values())
    md.append(f"All sources combined: n_days={len(all_days_vals)}  "
              f"p50={pctl(all_days_vals,50):.4f}  p90={pctl(all_days_vals,90):.4f}  "
              f"max={(max(all_days_vals) if all_days_vals else 0):.4f}")
    md.append("")
    for key, label in (("pi", "pi"), ("cc-main", "Claude Code main"), ("cc-sub", "Claude Code subagent")):
        vals = [v.get(key, 0.0) for v in totals.values()]
        vals = [v for v in vals if v > 0]
        md.append(f"{label}: n_days={len(vals)}  p50={pctl(vals,50):.4f}  p90={pctl(vals,90):.4f}  "
                  f"max={(max(vals) if vals else 0):.4f}")
    md.append("")
    md.append("Top 5 days by combined spend:")
    md.append("")
    md.append("| day | total | pi | cc-main | cc-sub |")
    md.append("|---|---|---|---|---|")
    for day, _ in sorted(day_all.items(), key=lambda kv: kv[1], reverse=True)[:5]:
        v = totals[day]
        md.append(f"| {day} | {fmt_usd(day_all[day])} | {fmt_usd(v.get('pi',0))} | "
                  f"{fmt_usd(v.get('cc-main',0))} | {fmt_usd(v.get('cc-sub',0))} |")
    md.append("")

    # 5. top 5 most expensive sessions overall
    md.append("### 5. Five most expensive sessions (across all sources)")
    md.append("")
    md.append("| source | cost | duration | turns | model | notes |")
    md.append("|---|---|---|---|---|---|")
    all_sessions = pi_sessions + cc_main + cc_sub
    for row in top_sessions(all_sessions, 5):
        md.append(f"| {row['source']} | {fmt_usd(row['cost'])} | {row['dur_h']:.1f}h | "
                  f"{row['turns']} | {row['model']} | {row['unattended']} |")
    md.append("")

    # 6. subagent vs main fraction
    md.append("### 6. Claude Code: subagent vs main spend")
    md.append("")
    main_cost = sum(s.cost for s in cc_main)
    sub_cost = sum(s.cost for s in cc_sub)
    tot = main_cost + sub_cost
    frac = (sub_cost / tot * 100) if tot else 0.0
    md.append(f"main={fmt_usd(main_cost)}  subagent={fmt_usd(sub_cost)}  "
              f"subagent share={frac:.1f}%")
    md.append("")

    # pi provider breakdown (subscription vs API key)
    md.append("### pi: cost by provider (subscription vs API key)")
    md.append("")
    md.append("| provider | payment | total cost |")
    md.append("|---|---|---|")
    prov_cost = defaultdict(float)
    for s in pi_sessions:
        for model_key, n in s.models.items():
            pass  # per-call cost not tracked per-provider separately; recompute below
    # recompute directly from files for provider totals (cheap, small corpus)
    prov_cost = defaultdict(float)
    prov_count = defaultdict(int)
    for path in glob.glob(PI_GLOB, recursive=True):
        try:
            with open(path) as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        d = json.loads(line)
                    except Exception:
                        continue
                    if d.get("type") != "message":
                        continue
                    msg = d.get("message") or {}
                    if msg.get("role") != "assistant":
                        continue
                    prov = msg.get("provider")
                    cost = ((msg.get("usage") or {}).get("cost") or {}).get("total") or 0.0
                    prov_cost[prov] += cost
                    prov_count[prov] += 1
        except Exception:
            continue
    for prov, cost in sorted(prov_cost.items(), key=lambda kv: kv[1], reverse=True):
        payment = "subscription" if prov in SUBSCRIPTION_PROVIDERS else (
            "API key" if prov in API_KEY_PROVIDERS else "unknown")
        md.append(f"| {prov} | {payment} | {fmt_usd(cost)} |")
    md.append("")
    md.append("_Payment method is inferred from provider name only (no explicit auth-type "
               "field in the logs): `openai-codex` is the ChatGPT/Codex CLI subscription "
               "login, `pi-claude-cli` reuses the Claude Code CLI OAuth login; `anthropic`, "
               "`opencode-go`, `oc-sdk-go`, `opencode`, `cursor` are API-key-routed "
               "(OpenRouter-style / Cursor). Dollar figures for subscription providers are "
               "still the API-equivalent cost the harness logs, not actual out-of-pocket spend._")
    md.append("")

    with open(OUT_MD, "w") as fh:
        fh.write("\n".join(md) + "\n")
    print(f"wrote {OUT_MD}")
    print(f"pi sessions={len(pi_sessions)} cc-main={len(cc_main)} cc-sub={len(cc_sub)}")
    print(f"total cost: pi={sum(s.cost for s in pi_sessions):.2f} "
          f"cc-main={main_cost:.2f} cc-sub={sub_cost:.2f}")


if __name__ == "__main__":
    main()
