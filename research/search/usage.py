import json, glob, os, re, collections

# Excludes the live session that produced this measurement (its prompt text
# is this very ticket brief, which would self-contaminate the counts below).
SELF = "174d9475-ef5c-449f-9723-ad5906089d4d"

# ---------------- shell command classification ----------------
SEG_SPLIT_RE = re.compile(r'&&|\|\||;|\|')
ENV_ASSIGN_RE = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|\'[^\']*\'|\S*)\s+')
SEARCH_TRUNC_RE = re.compile(
    r'limit reached|more matches available|Continue with cursor=|results? truncated|truncated', re.I)


def strip_leading(seg):
    seg = seg.strip()
    while True:
        m = ENV_ASSIGN_RE.match(seg)
        if not m:
            break
        seg = seg[m.end():].lstrip()
    return seg


def classify_segment(seg):
    """First real command word of a pipeline segment -> a search-command label, or None."""
    seg = strip_leading(seg)
    if not seg:
        return None
    words = seg.split()
    w0 = words[0].strip('()')
    if w0 == 'git' and len(words) > 1 and words[1] == 'grep':
        return 'git grep'
    if w0 in ('rg', 'grep', 'find', 'fd', 'ag', 'ack', 'tree'):
        return w0
    if w0 == 'ls':
        for wd in words[1:]:
            if wd.startswith('-') and not wd.startswith('--') and 'R' in wd:
                return 'ls -R'
    return None


def search_cmds_in(cmd):
    found = set()
    for seg in SEG_SPLIT_RE.split(cmd):
        c = classify_segment(seg)
        if c:
            found.add(c)
    return found


def flags_in_segment(seg, cmdname):
    """rg/grep/git-grep flags in one segment, short combos split into single flags."""
    seg = strip_leading(seg)
    words = seg.split()
    skip = 2 if cmdname == 'git grep' else 1
    flags = []
    for tok in words[skip:]:
        if not tok.startswith('-') or tok == '-':
            continue
        if tok.startswith('--'):
            flags.append(tok.split('=')[0])
        elif tok[1:].isalpha():
            flags.extend('-' + ch for ch in tok[1:])
        else:
            flags.append(tok)
    return flags


def shell_err_bucket(text, is_error):
    if not is_error:
        return None
    t = (text or '').strip()
    if 'yolo-seatbelt' in t or 'Blocked by' in t:
        return 'blocked (sandbox)'
    if 'auto mode classifier' in t or ('denied' in t.lower() and 'permission' in t.lower()):
        return 'permission denied'
    mcc = re.search(r'Command exited with code (\d+)', t)
    mcc2 = re.match(r'^Exit code (\d+)', t)
    code = int((mcc or mcc2).group(1)) if (mcc or mcc2) else None
    body = re.sub(r'^Exit code \d+\s*', '', t)
    body = re.sub(r'\s*Command exited with code \d+\s*$', '', body).strip()
    if code == 1 and body in ('', '(no output)'):
        return 'no matches (exit 1, empty output)'
    if code == 1:
        return 'exit 1 (rg/grep convention: no match, or real error)'
    line = body.splitlines()[0][:70] if body else f'(exit {code})'
    return line


def norm_search_err(msg):
    if not msg:
        return '(empty)'
    m = msg
    if 'Path not found' in m:
        return 'path not found'
    if 'regex parse error' in m or 'unclosed group' in m:
        return 'invalid regex'
    if '[fd error]' in m and 'not a directory' in m:
        return 'search path not a directory'
    if 'No files found matching pattern' in m:
        return 'no files found'
    if 'No valid search paths' in m:
        return 'no valid search path'
    return 'other'


def result_text(content):
    """CC tool_result content: str or list of {type:text}. pi: list of {type:text}."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return ''.join(c.get('text', '') for c in content if isinstance(c, dict) and c.get('type') == 'text')
    return ''


def pctl(v, p):
    if not v:
        return 0
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))]


def size_stats(sizes):
    if not sizes:
        return "n=0"
    cuts = (8, 16, 50)
    shares = ' '.join(f'>{c}K {100*sum(x > c*1024 for x in sizes)/len(sizes):.1f}%' for c in cuts)
    return (f"n={len(sizes)} p50={pctl(sizes,.5)} p90={pctl(sizes,.9)} "
            f"p99={pctl(sizes,.99)} max={max(sizes)}  {shares}")


# ---------------- per-source accumulator ----------------
PI_DEDICATED = {'grep', 'ffgrep', 'find', 'fffind'}
CC_DEDICATED = {'Grep', 'Glob'}
READ_NAME = {'pi': 'read', 'cc': 'Read'}


def new_src():
    return {
        'n_sessions': 0,
        'ded_calls': collections.Counter(),       # tool -> call count
        'ded_sessions': collections.Counter(),     # tool -> session count
        'ded_sizes': [],
        'ded_errors': collections.Counter(),       # tool -> error count
        'ded_err_bucket': collections.Counter(),
        'ded_trunc': 0,
        'ded_args': collections.defaultdict(collections.Counter),  # tool -> arg key -> count
        'ded_followup_total': 0,
        'ded_followup_hit': 0,
        'shell_calls': 0,
        'shell_sessions_with_search': 0,
        'shell_search_calls': 0,
        'shell_cmd_counts': collections.Counter(),
        'shell_sizes': [],
        'shell_errors': 0,
        'shell_err_bucket': collections.Counter(),
        'shell_flags': collections.Counter(),
        'shell_followup_total': 0,
        'shell_followup_hit': 0,
        'all_tool_names': collections.Counter(),
    }


pi, cc_main, cc_sub = new_src(), new_src(), new_src()


def process_session(dst, calls, results, read_name):
    """calls: ordered [(id, name, args)]. results: id -> (text, is_error)."""
    session_ded_tools = set()
    session_shell_search = False
    for i, (cid, name, args) in enumerate(calls):
        dst['all_tool_names'][name] += 1
        text, is_error = results.get(cid, ('', False))
        is_dedicated = name in PI_DEDICATED or name in CC_DEDICATED
        is_bash = name in ('bash', 'Bash')
        cmd = str(args.get('command') or '') if is_bash else ''
        scmds = search_cmds_in(cmd) if is_bash else set()

        if is_dedicated:
            dst['ded_calls'][name] += 1
            session_ded_tools.add(name)
            dst['ded_sizes'].append(len(text.encode('utf-8', 'replace')))
            if is_error:
                dst['ded_errors'][name] += 1
                dst['ded_err_bucket'][norm_search_err(text)] += 1
            if SEARCH_TRUNC_RE.search(text):
                dst['ded_trunc'] += 1
            for k in args:
                dst['ded_args'][name][k] += 1
            # follow-up: next call a read whose path is in this result text
            if i + 1 < len(calls):
                _, nname, nargs = calls[i + 1]
                if nname == read_name:
                    dst['ded_followup_total'] += 1
                    path = nargs.get('path') or nargs.get('file_path') or ''
                    if path and path in text:
                        dst['ded_followup_hit'] += 1

        elif is_bash:
            dst['shell_calls'] += 1
            if scmds:
                session_shell_search = True
                dst['shell_search_calls'] += 1
                for c in scmds:
                    dst['shell_cmd_counts'][c] += 1
                dst['shell_sizes'].append(len(text.encode('utf-8', 'replace')))
                if is_error:
                    dst['shell_errors'] += 1
                    b = shell_err_bucket(text, is_error)
                    if b:
                        dst['shell_err_bucket'][b] += 1
                for seg in SEG_SPLIT_RE.split(cmd):
                    c = classify_segment(seg)
                    if c in ('rg', 'grep', 'git grep'):
                        for fl in flags_in_segment(seg, c):
                            dst['shell_flags'][fl] += 1
                if i + 1 < len(calls):
                    _, nname, nargs = calls[i + 1]
                    if nname == read_name:
                        dst['shell_followup_total'] += 1
                        path = nargs.get('path') or nargs.get('file_path') or ''
                        if path and path in text:
                            dst['shell_followup_hit'] += 1

    dst['n_sessions'] += 1
    for t in session_ded_tools:
        dst['ded_sessions'][t] += 1
    if session_shell_search:
        dst['shell_sessions_with_search'] += 1


# ---------------- pi sessions ----------------
for f in glob.glob(os.path.expanduser("~/.pi/agent/sessions/**/*.jsonl"), recursive=True):
    if SELF in f:
        continue
    calls, results = [], {}
    for l in open(f, errors="replace"):
        try:
            d = json.loads(l)
        except Exception:
            continue
        if d.get("type") != "message":
            continue
        m = d.get("message", {})
        role = m.get("role")
        if role == "assistant":
            for c in m.get("content") or []:
                if c.get("type") == "toolCall":
                    calls.append((c.get("id"), c.get("name"), c.get("arguments") or {}))
        elif role == "toolResult":
            results[m.get("toolCallId")] = (result_text(m.get("content")), bool(m.get("isError")))
    if calls:
        process_session(pi, calls, results, 'read')

# ---------------- Claude Code sessions ----------------
for f in glob.glob(os.path.expanduser("~/.claude/projects/**/*.jsonl"), recursive=True):
    if SELF in f:
        continue
    is_sub = "/subagents/" in f
    dst = cc_sub if is_sub else cc_main
    calls, results = [], {}
    for l in open(f, errors="replace"):
        try:
            d = json.loads(l)
        except Exception:
            continue
        t = d.get("type")
        if t == "assistant":
            for c in d.get("message", {}).get("content") or []:
                if c.get("type") == "tool_use":
                    calls.append((c.get("id"), c.get("name"), c.get("input") or {}))
        elif t == "user":
            cont = d.get("message", {}).get("content")
            if not isinstance(cont, list):
                continue
            for c in cont:
                if isinstance(c, dict) and c.get("type") == "tool_result":
                    results[c.get("tool_use_id")] = (result_text(c.get("content")), bool(c.get("is_error")))
    if calls:
        process_session(dst, calls, results, 'Read')


# ---------------- report ----------------
def report(name, d):
    L = [f"\n## {name}\n", f"sessions with any tool call: {d['n_sessions']}\n"]

    all_names = sorted(n for n in d['all_tool_names'] if re.search(r'grep|find|glob|fff', n, re.I))
    web = sorted(n for n in d['all_tool_names'] if 'search' in n.lower() and n not in all_names)
    L.append(f"tool names matching grep/find/glob/fff: {all_names or '(none)'}"
              + (f"  (excluded as web search, not code search: {web})" if web else ""))

    L.append("\n**1. Dedicated search tool calls**\n")
    L.append("| tool | calls | sessions using | share of sessions |")
    L.append("|---|---|---|---|")
    ded_total = sum(d['ded_calls'].values())
    for tool, n in d['ded_calls'].most_common():
        sc = d['ded_sessions'][tool]
        pct = 100 * sc / d['n_sessions'] if d['n_sessions'] else 0
        L.append(f"| {tool} | {n} | {sc} | {pct:.1f}% |")
    if not d['ded_calls']:
        L.append("| (none in this corpus) | | | |")

    L.append("\n**2. Shell search calls**\n")
    L.append(f"shell (bash/Bash) calls: {d['shell_calls']}, containing a search command: "
              f"{d['shell_search_calls']} ({100*d['shell_search_calls']/d['shell_calls'] if d['shell_calls'] else 0:.1f}%)")
    L.append("| shell search command | calls |")
    L.append("|---|---|")
    for c, n in d['shell_cmd_counts'].most_common():
        L.append(f"| {c} | {n} |")
    total_search_actions = ded_total + d['shell_search_calls']
    if total_search_actions:
        L.append(f"\nof all search actions ({total_search_actions}): "
                  f"{100*d['shell_search_calls']/total_search_actions:.1f}% via shell, "
                  f"{100*ded_total/total_search_actions:.1f}% via a dedicated tool")

    L.append("\n**3. Result sizes**\n")
    L.append(f"dedicated tools: {size_stats(d['ded_sizes'])}")
    L.append(f"shell search: {size_stats(d['shell_sizes'])}")

    L.append("\n**4. Errors**\n")
    if ded_total:
        ded_err_total = sum(d['ded_errors'].values())
        L.append(f"dedicated tools: {ded_err_total}/{ded_total} ({100*ded_err_total/ded_total:.2f}%)")
        for b, n in d['ded_err_bucket'].most_common():
            L.append(f"  - {b}: {n}")
    else:
        L.append("dedicated tools: no calls")
    if d['shell_search_calls']:
        L.append(f"shell search: {d['shell_errors']}/{d['shell_search_calls']} "
                  f"({100*d['shell_errors']/d['shell_search_calls']:.2f}%) nonzero-exit/isError")
        for b, n in d['shell_err_bucket'].most_common(10):
            L.append(f"  - {b}: {n}")
    else:
        L.append("shell search: no calls")

    L.append("\n**5. Arguments of dedicated tools**\n")
    for tool in sorted(d['ded_args']):
        n = d['ded_calls'][tool]
        L.append(f"{tool} (n={n}):")
        for k, c in d['ded_args'][tool].most_common():
            L.append(f"  - {k}: {100*c/n:.1f}% ({c})")
    if not d['ded_args']:
        L.append("(no dedicated-tool calls in this corpus)")
    if d['shell_flags']:
        L.append("\nshell rg/grep/git-grep flags, top 20:")
        for fl, c in d['shell_flags'].most_common(20):
            L.append(f"  - {fl}: {c}")

    L.append("\n**6. Truncation signals**\n")
    if ded_total:
        L.append(f"dedicated-tool results with a truncation/limit notice: {d['ded_trunc']}/{ded_total} "
                  f"({100*d['ded_trunc']/ded_total:.1f}%)")
    else:
        L.append("no dedicated-tool calls in this corpus")

    L.append("\n**7. Follow-up read of a path from the search result**\n")
    if d['ded_followup_total']:
        L.append(f"dedicated tools: next call is a read, path from result: "
                  f"{d['ded_followup_hit']}/{d['ded_followup_total']} "
                  f"({100*d['ded_followup_hit']/d['ded_followup_total']:.1f}%)")
    else:
        L.append("dedicated tools: no next-call-is-read cases")
    if d['shell_followup_total']:
        L.append(f"shell search: next call is a read, path from result: "
                  f"{d['shell_followup_hit']}/{d['shell_followup_total']} "
                  f"({100*d['shell_followup_hit']/d['shell_followup_total']:.1f}%)")
    else:
        L.append("shell search: no next-call-is-read cases")

    return "\n".join(L)


out = ["# Search: built-in tools or the shell? (ticket #54)\n",
       "Measured 2026-09-26. One process, one pass over each session log, counts and "
       "shares only (no timings).\n",
       f"pi sessions with any tool call: {pi['n_sessions']}\n",
       f"Claude Code main sessions with any tool call: {cc_main['n_sessions']}\n",
       f"Claude Code subagent sessions with any tool call: {cc_sub['n_sessions']}\n",
       "\nShell-search detection: a bash/Bash command is split on `&& || ; |`; each "
       "segment's first word (after stripping env-var assignments) is checked against "
       "rg/grep/git grep/find/fd/ls -R/tree/ag/ack. Follow-up is a rough proxy: the very "
       "next tool call in the transcript is a read whose path string appears in the "
       "search result text.\n"]
out.append(report("pi", pi))
out.append(report("Claude Code (main)", cc_main))
out.append(report("Claude Code (subagents)", cc_sub))

text = "\n".join(out) + "\n"
print(text)
with open(os.path.join(os.path.dirname(__file__), "usage.md"), "w") as fh:
    fh.write(text)
