# Replays the owner's sessions under idle cache warming with a cap, and compares
# the cost with no warming. Prices are multiples of base input price. Session
# loaders come from ttl.py; only the Claude Code subagent loader and the
# "interactive" tagging are new here.
#
# Model. L is the lifetime. After a request at time t the session refreshes just
# before t+L, t+2L, ... up to cap = k*L after the request (refresh j happens for
# j = 1..k). Each refresh reads the whole current context (context * r); its one
# output token is ignored. A request that follows a gap g is warm if g <= (k+1)*L
# (k = 0 is no warming), and then costs the usual read of the old context plus a
# write of the growth; otherwise it rewrites the whole context. After a session's
# last request nobody returns, so it spends all k refreshes: the tail.
import glob, json, os, sys, types
from statistics import median
import ttl

L = 3600
W = 2.0
MODELS = {'opus-5.5': (0.05, 4.0), 'sonnet-5': (0.10, 2.0)}  # (read multiple, $/MTok base input)
KS = list(range(0, 19))
PI_THRESHOLD_PROBABILITY = 0.15  # IDLE_CONTINUATION_PROBABILITY in pi's cache-warmer.js
MIN_SAVING_USD = 0.05

def _one(fn, path):
    # Reuse ttl.py's loader for one file by pointing its glob at that file.
    saved = ttl.glob
    ttl.glob = types.SimpleNamespace(glob=lambda *a, **k: [path])
    try: return fn()
    finally: ttl.glob = saved

def _interactive_pi(path):
    # More than one human prompt. A pi run started by a script sends exactly one.
    n = 0
    for l in open(path, errors='ignore'):
        try: e = json.loads(l)
        except: continue
        if (e.get('message') or {}).get('role') == 'user': n += 1
    return n > 1

def _interactive_cc(path):
    for l in open(path, errors='ignore'):
        try: e = json.loads(l)
        except: continue
        if e.get('type') == 'assistant': return e.get('entrypoint') == 'cli'
    return False

def pi():
    out = []
    for p in glob.glob(os.path.expanduser('~/.pi/agent/sessions/**/*.jsonl'), recursive=True):
        for s in _one(ttl.pi_sessions, p): out.append((s, _interactive_pi(p)))
    return out

def cc():
    out = []
    for p in glob.glob(os.path.expanduser('~/.claude/projects/**/*.jsonl'), recursive=True):
        if '/subagents/' in p: continue
        for s in _one(ttl.cc_sessions, p): out.append((s, _interactive_cc(p)))
    return out

def ccsub():
    # Same as ttl.cc_sessions but for /subagents/ files, whose entries are sidechain.
    out = []
    for p in glob.glob(os.path.expanduser('~/.claude/projects/**/subagents/*.jsonl'), recursive=True):
        c, seen = [], set()
        for l in open(p, errors='ignore'):
            try: e = json.loads(l)
            except: continue
            if e.get('type') != 'assistant' or not e.get('timestamp'): continue
            m = e.get('message') or {}
            if m.get('id') in seen: continue
            seen.add(m.get('id')); u = m.get('usage') or {}
            v = (u.get('input_tokens') or 0) + (u.get('cache_read_input_tokens') or 0) + (u.get('cache_creation_input_tokens') or 0)
            if v: c.append((v, ttl.ts(e['timestamp'])))
        if len(c) > 1: out.append((sorted(c, key=lambda x: x[1]), _interactive_cc(p)))
    return out

def cost(s, k, r, T=0):
    """Cost of session s with cap k lifetimes; contexts under T tokens are not warmed."""
    total = s[0][0] * W
    for (a, ta), (b, tb) in zip(s, s[1:]):
        kk = k if a >= T else 0
        g = tb - ta
        total += min(int(g // L), kk) * a * r
        total += (a * r + max(0, b - a) * W) if g <= (kk + 1) * L else b * W
    last = s[-1][0]
    if last >= T: total += k * last * r
    return total

def threshold_tokens(r, price, rule):
    if rule == 'miss':   # rebuild minus read is worth at least $0.05
        per = (W - r) * price * 1e-6
    else:                # pi's rule: p * miss - warm >= $0.05, with the 1h rebuild
        per = (PI_THRESHOLD_PROBABILITY * (W - r) - r) * price * 1e-6
    return int(MIN_SAVING_USD / per) + 1

def pct(xs, q):
    xs = sorted(xs); return xs[min(len(xs) - 1, q * len(xs) // 100)]

def table(sessions, r, T=0):
    base = [cost(s, 0, r, T) for s in sessions]
    rows = []
    for k in KS:
        c = [cost(s, k, r, T) for s in sessions]
        ratio = [x / y for x, y in zip(c, base)]
        rows.append((k, sum(c) / sum(base), pct(ratio, 10), pct(ratio, 50), pct(ratio, 90),
                     sum(x > 1.000001 for x in ratio) / len(ratio)))
    return rows

def show(rows):
    print('| cap (lifetimes) | total vs none | p10 | p50 | p90 | sessions costing more |')
    print('|---|---|---|---|---|---|')
    for k, t, a, b, c, f in rows:
        print(f'| {k} | {t:.3f} | {a:.3f} | {b:.3f} | {c:.3f} | {f:.0%} |')

if __name__ == '__main__':
    src = {'pi main': pi(), 'Claude Code main': cc(), 'Claude Code subagents': ccsub()}
    for name, ss in src.items():
        for subset, sel in (('interactive', [s for s, i in ss if i]), ('all', [s for s, i in ss])):
            gaps = [tb - ta for s in sel for (_, ta), (_, tb) in zip(s, s[1:])]
            last = [s[-1][0] for s in sel]
            print(f'\n## {name}, {subset}: {len(sel)} sessions, {len(gaps)} gaps ({sum(g > L for g in gaps)} over 1 lifetime, '
                  f'{sum(g > 4*L for g in gaps)} over 4, {sum(g > 12*L for g in gaps)} over 12); last context p10/p50/p90 '
                  f'{pct(last,10)}/{pct(last,50)}/{pct(last,90)} tokens; sessions under 50k/135k tokens at the end: '
                  f'{sum(x<50000 for x in last)}/{sum(x<135000 for x in last)}')
            for m, (r, price) in MODELS.items():
                print(f'\n### {name}, {subset}, read {r}x ({m})\n'); show(table(sel, r))
            print(f'\n### {name}, {subset}, read 0.1x (round figure)\n'); show(table(sel, 0.1))
            # threshold variant, best case per model
            for m, (r, price) in MODELS.items():
                for rule in ('miss', 'pi'):
                    T = threshold_tokens(r, price, rule)
                    print(f'\n### {name}, {subset}, {m}, threshold rule {rule}: skip contexts under {T} tokens '
                          f'({sum(s[-1][0] < T for s in sel)} of {len(sel)} sessions end below it)\n'); show(table(sel, r, T))
