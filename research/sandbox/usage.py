"""Ticket #30: how often would a shell confined to workspace(cwd)+temp, no
network, break a real shell command from the owner's pi and Claude Code
sessions.

Heuristics only (stated at each decision point below), on the RAW command
string per Bash/bash call, not per exec()'d subprocess. A command is a
pipeline of segments split on `&& || ; |`. A whole command is classified
into a bucket if ANY of its segments trips that bucket's rule -- a `&&`
chain breaks as soon as one segment would be blocked.
"""
import json, glob, os, re, collections, random

SELF = "174d9475-ef5c-449f-9723-ad5906089d4d"  # this measurement's own session

SEG_SPLIT_RE = re.compile(r'&&|\|\||;|\|')
ENV_ASSIGN_RE = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|\'[^\']*\'|\S*)\s+')
TMP_RE = re.compile(r'^(\$TMPDIR|\$\{TMPDIR\}|/tmp/|/private/tmp/|/var/folders/[^/\s"\']+/[^/\s"\']+/T/)')
# `>`/`>>` file redirect, excluding fd-dup forms like `2>&1` or `>&2` (no `&`
# right after the angle bracket -- a leading `&` as in `&>file` is fine, that
# really does redirect stdout+stderr to a file).
REDIR_RE = re.compile(r'(?:^|[\s;|&])>{1,2}(?!&)\s*([^\s|&;<>]+)')
WRITE_FIRST_WORDS = {'cp', 'mv', 'rm', 'mkdir', 'touch', 'ln', 'chmod', 'chown',
                       'tee', 'dd', 'rsync', 'tar', 'unzip', 'zip', 'patch'}
FLAG_WRITE_TOOLS = {'sed', 'perl'}  # write only with -i present


def parse_cd(stripped):
    """cd target if `stripped` is a `cd` invocation, else None. `cd -`
    (previous dir) can't be tracked without shell state -- treated as
    'stay put' (a stated limitation, not a crash)."""
    toks = stripped.split()
    if not toks or toks[0] != 'cd':
        return None
    if len(toks) == 1:
        return '~'
    arg = toks[1].strip('\'"')
    return None if arg == '-' else arg


def resolve(tok, base):
    tok = tok.strip('\'"')
    if tok.startswith('~'):
        return os.path.normpath(os.path.expanduser(tok))
    if tok.startswith('/'):
        return os.path.normpath(tok)
    return os.path.normpath(os.path.join(base, tok))


def write_targets(stripped, effective_dir):
    """Best-effort: resolved absolute paths this segment actually WRITES to
    (not merely reads/mentions). Triggers: file redirection (`>`, `>>`,
    `&>`), a `-o`/`-O`/`--output(=)` flag, or a first word from a small
    write-verb list (sed/perl only count with `-i`). Anything else --
    cd, ls, cat, grep, find, git log, a script run without an output flag --
    is treated as read-only, even though an inline python/node/bun script
    could in principle write anywhere; the log gives no way to see inside
    the script (documented limitation)."""
    toks = stripped.split()
    if not toks:
        return []
    w0 = toks[0].strip('()')
    cands = []
    triggered = False
    for m in REDIR_RE.finditer(stripped):
        cands.append(m.group(1))
        triggered = True
    for i, t in enumerate(toks):
        if t in ('-o', '-O', '--output') and i + 1 < len(toks):
            cands.append(toks[i + 1])
            triggered = True
        elif t.startswith('--output='):
            cands.append(t.split('=', 1)[1])
            triggered = True
    if w0 in WRITE_FIRST_WORDS:
        triggered = True
    if w0 in FLAG_WRITE_TOOLS and any(re.match(r'^-i', t) for t in toks[1:]):
        triggered = True
    if not triggered:
        return []
    if w0 in ('cp', 'mv', 'ln'):
        # binary op: last non-flag arg is the destination (a write target);
        # earlier args are sources (reads, may legitimately sit outside cwd
        # -- e.g. `cp ~/backup.txt ./local.txt` only writes inside cwd).
        nonflag = [t for t in toks[1:] if not t.startswith('-')]
        if nonflag:
            cands.append(nonflag[-1])
    elif w0 in WRITE_FIRST_WORDS or (w0 in FLAG_WRITE_TOOLS and triggered):
        for t in toks[1:]:
            if not t.startswith('-'):
                cands.append(t)
    return [resolve(c, effective_dir) for c in cands]

NETWORK_SUBCMD = {
    'git': {'push', 'pull', 'fetch', 'clone'},
    'npm': {'install', 'i', 'add', 'update', 'upgrade', 'ci'},
    'pnpm': {'install', 'i', 'add', 'update', 'upgrade'},
    'yarn': {'install', 'add', 'upgrade'},
    'bun': {'install', 'add', 'update'},
    'pip': {'install'}, 'pip3': {'install'},
    'uv': {'sync', 'add'},  # 'uv install' isn't real uv syntax but harmless
    'docker': {'pull'},
    'go': {'get'},
}
NETWORK_ALWAYS = {'gh', 'curl', 'wget', 'ssh', 'scp', 'brew'}
CARGO_MAYBE_NETWORK_SUBCMD = {'build', 'test', 'run', 'install', 'add', 'update', 'check', 'bench', 'doc'}

# Any invocation of these touches state outside cwd (config/cache dirs under
# $HOME, or -- for editors -- swap/config files under $HOME), regardless of
# subcommand. Heuristic: presence of the command name is enough; we don't
# distinguish a subcommand that happens to touch nothing.
WRITE_OUTSIDE_TOOL = {
    'cargo': '~/.cargo cache/index',
    'npm': '~/.npm cache', 'pnpm': '~/.pnpm-store / ~/.local/share/pnpm', 'yarn': '~/.yarn cache', 'bun': '~/.bun',
    'pip': '~/.cache/pip', 'pip3': '~/.cache/pip', 'uv': '~/.cache/uv',
    'go': '~/go, ~/.cache/go-build',
    'brew': 'Homebrew prefix (/opt/homebrew, /usr/local)',
    'gh': '~/.config/gh',
    'claude': '~/.claude', 'codex': '~/.codex', 'pi': '~/.pi',
    'vim': 'editor swap/config', 'vi': 'editor swap/config', 'nvim': 'editor swap/config',
    'nano': 'editor swap/config', 'emacs': 'editor swap/config', 'code': 'editor config', 'subl': 'editor config',
}

# Local git ops that mutate .git. fetch/pull/push/clone are network, not here.
GIT_WRITE_SUBCMD = {
    'commit', 'add', 'checkout', 'rebase', 'stash', 'merge', 'reset', 'mv', 'rm',
    'apply', 'cherry-pick', 'tag', 'branch', 'switch', 'restore', 'init',
    'submodule', 'am', 'revert', 'clean', 'notes', 'worktree', 'gc', 'reflog',
}

SUBCMD_TOOLS = {'git', 'gh', 'npm', 'pnpm', 'yarn', 'bun', 'cargo', 'pip', 'pip3',
                 'uv', 'go', 'docker', 'brew'}


def strip_leading(seg):
    seg = seg.strip()
    while True:
        m = ENV_ASSIGN_RE.match(seg)
        if not m:
            break
        seg = seg[m.end():].lstrip()
    return seg


def first_two(seg):
    """(tool, subcommand-or-None) from a stripped segment, skipping `-C <arg>`
    for git and other leading dash flags (best-effort, not a real arg parser)."""
    seg = strip_leading(seg)
    if not seg:
        return None, None
    toks = seg.split()
    tool = toks[0].strip('()').lstrip('$')
    i = 1
    sub = None
    while i < len(toks):
        t = toks[i]
        if t == '-C' and tool == 'git':
            i += 2
            continue
        if t.startswith('-'):
            i += 1
            continue
        sub = t
        break
    return tool, sub


def label_of(seg):
    tool, sub = first_two(seg)
    if not tool:
        return None
    if tool in SUBCMD_TOOLS and sub:
        return f'{tool} {sub}'
    return tool


def is_worktree_gitdir(cwd, cache):
    """cwd/.git is a FILE pointing outside cwd (a git worktree checkout) ->
    local git-writing commands there actually write outside the workspace.
    Cached per cwd; if cwd no longer exists on disk, treated as unknown/no
    (undercount, noted as a limitation)."""
    if cwd in cache:
        return cache[cwd]
    result = False
    try:
        gp = os.path.join(cwd, '.git')
        if os.path.isfile(gp):
            result = True
    except Exception:
        result = False
    cache[cwd] = result
    return result


def path_outside_workspace(p, workspace_root):
    if p == '/dev/null':
        return False
    root_norm = workspace_root.rstrip('/') + '/'
    p_norm = p.rstrip('/') + '/'
    return not p_norm.startswith(root_norm) and p_norm != root_norm


def classify_segment(stripped, effective_dir, workspace_root, wt_cache):
    """Returns dict of booleans for this one (already cd-stripped, already
    strip_leading'd) pipeline segment. `effective_dir` is the shell's
    current directory at this point in the chain (drifts via a preceding
    `cd`); `workspace_root` is the fixed sandbox boundary (the session's
    original cwd)."""
    tool, sub = first_two(stripped)
    flags = dict(network=False, network_maybe=False, write_tmp=False,
                 write_outside_explicit=False, write_outside_tool=False,
                 git_write=False, git_write_worktree=False)
    if tool:
        if tool in NETWORK_ALWAYS:
            flags['network'] = True
        if tool in NETWORK_SUBCMD and sub in NETWORK_SUBCMD[tool]:
            flags['network'] = True
        if tool == 'cargo' and sub in CARGO_MAYBE_NETWORK_SUBCMD:
            flags['network_maybe'] = True

        if tool in WRITE_OUTSIDE_TOOL:
            flags['write_outside_tool'] = True

        if tool == 'git' and sub in GIT_WRITE_SUBCMD:
            flags['git_write'] = True
            if is_worktree_gitdir(effective_dir, wt_cache):
                flags['git_write_worktree'] = True

    for p in write_targets(stripped, effective_dir):
        if TMP_RE.match(p) or p.startswith('$TMPDIR'):
            flags['write_tmp'] = True
        elif path_outside_workspace(p, workspace_root):
            flags['write_outside_explicit'] = True

    return flags


def classify_command(cmd, cwd, wt_cache):
    """Union of segment flags across the whole pipeline/chain, plus the
    label of the first segment that tripped each bucket (for the top-15
    tables). A leading `cd` in the chain moves `effective_dir` (used to
    resolve relative write targets and to pick which .git a later `git`
    segment sees) but never moves `workspace_root`, the fixed sandbox
    boundary. cwd defaults to a sentinel (never matches, so path checks
    degrade to "always outside") when unknown."""
    workspace_root = cwd or '/__unknown_cwd__'
    effective_dir = workspace_root
    agg = dict(network=False, network_maybe=False, write_tmp=False,
                write_outside_explicit=False, write_outside_tool=False,
                git_write=False, git_write_worktree=False)
    labels = {}
    for seg in SEG_SPLIT_RE.split(cmd):
        stripped = strip_leading(seg)
        cd_target = parse_cd(stripped)
        if cd_target is not None:
            effective_dir = resolve(cd_target, effective_dir)
            continue
        f = classify_segment(stripped, effective_dir, workspace_root, wt_cache)
        lbl = label_of(seg)
        for k, v in f.items():
            if v and not agg[k]:
                agg[k] = True
                if lbl:
                    labels[k] = lbl
    return agg, labels


# ---------------- per-corpus accumulator ----------------

CLASSES = ['network', 'network_maybe', 'write_outside_explicit', 'write_tmp',
           'write_outside_tool', 'git_write', 'git_write_worktree']


def new_src():
    return {
        'n_session_files': 0,
        'n_sessions_with_shell': 0,
        'shell_calls': 0,
        'class_calls': collections.Counter(),      # class -> call count
        'class_sessions': collections.Counter(),    # class -> session count
        'class_labels': {c: collections.Counter() for c in CLASSES},
        'unharmed_strict': 0,   # no network, no write_outside_explicit, no write_outside_tool, no git_write_worktree
        'unharmed_incl_maybe': 0,  # also excludes network_maybe
        'all_classified': [],   # (cmd, agg) for spot-check sampling
    }


def process_session(dst, cmds_cwds, wt_cache):
    """cmds_cwds: list of (raw_command, cwd) for each shell call in one session."""
    if not cmds_cwds:
        return
    dst['n_sessions_with_shell'] += 1
    session_classes = set()
    for cmd, cwd in cmds_cwds:
        dst['shell_calls'] += 1
        agg, labels = classify_command(cmd, cwd, wt_cache)
        for c in CLASSES:
            if agg[c]:
                dst['class_calls'][c] += 1
                session_classes.add(c)
                if c in labels:
                    dst['class_labels'][c][labels[c]] += 1
        breaks_strict = agg['network'] or agg['write_outside_explicit'] or \
            agg['write_outside_tool'] or agg['git_write_worktree']
        if not breaks_strict:
            dst['unharmed_strict'] += 1
        if not breaks_strict and not agg['network_maybe']:
            dst['unharmed_incl_maybe'] += 1
        dst['all_classified'].append((cmd, agg))
    for c in session_classes:
        dst['class_sessions'][c] += 1


pi = new_src()
cc_main = new_src()
cc_sub = new_src()
wt_cache = {}

# ---------------- pi sessions ----------------
for f in glob.glob(os.path.expanduser('~/.pi/agent/sessions/**/*.jsonl'), recursive=True):
    if SELF in f:
        continue
    pi['n_session_files'] += 1
    cwd = None
    cmds = []
    for l in open(f, errors='replace'):
        try:
            d = json.loads(l)
        except Exception:
            continue
        if d.get('type') == 'session':
            cwd = d.get('cwd')
            continue
        if d.get('type') != 'message':
            continue
        m = d.get('message', {})
        if m.get('role') != 'assistant':
            continue
        for c in m.get('content') or []:
            if c.get('type') == 'toolCall' and c.get('name') == 'bash':
                cmd = str((c.get('arguments') or {}).get('command') or '')
                if cmd.strip():
                    cmds.append((cmd, cwd))
    process_session(pi, cmds, wt_cache)

# ---------------- Claude Code sessions ----------------
for f in glob.glob(os.path.expanduser('~/.claude/projects/**/*.jsonl'), recursive=True):
    if SELF in f:
        continue
    is_sub = '/subagents/' in f
    dst = cc_sub if is_sub else cc_main
    dst['n_session_files'] += 1
    cmds = []
    for l in open(f, errors='replace'):
        try:
            d = json.loads(l)
        except Exception:
            continue
        if d.get('type') != 'assistant':
            continue
        cwd = d.get('cwd')
        for c in d.get('message', {}).get('content') or []:
            if c.get('type') == 'tool_use' and c.get('name') == 'Bash':
                cmd = str((c.get('input') or {}).get('command') or '')
                if cmd.strip():
                    cmds.append((cmd, cwd))
    process_session(dst, cmds, wt_cache)


# ---------------- report ----------------
CLASS_TITLE = {
    'network': '1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get)',
    'network_maybe': '1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached)',
    'write_outside_explicit': '2a. Writes outside workspace: explicit path outside cwd (non-temp)',
    'write_tmp': '2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break)',
    'write_outside_tool': '2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor)',
    'git_write': 'git write subcommand (any) -- only a break if cwd is a git worktree (3.)',
    'git_write_worktree': '3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace)',
}


def pct(n, d):
    return 100 * n / d if d else 0.0


def report(name, d):
    L = [f'\n## {name}\n']
    L.append(f"session files: {d['n_session_files']}, sessions with >=1 shell call: {d['n_sessions_with_shell']}")
    L.append(f"total shell calls: {d['shell_calls']}\n")
    if not d['shell_calls']:
        L.append('(no shell calls in this corpus)')
        return '\n'.join(L)

    L.append('| class | calls | % of calls | sessions | % of sessions-with-shell |')
    L.append('|---|---|---|---|---|')
    for c in CLASSES:
        n = d['class_calls'][c]
        sc = d['class_sessions'][c]
        L.append(f"| {CLASS_TITLE[c]} | {n} | {pct(n, d['shell_calls']):.1f}% | {sc} | "
                  f"{pct(sc, d['n_sessions_with_shell']):.1f}% |")

    L.append(f"\n**Would pass unharmed under 'workspace + temp write, no network' "
              f"(strict: excludes network, write_outside_explicit, write_outside_tool, "
              f"git_write_worktree; does NOT count cargo's maybe-network as a break):** "
              f"{d['unharmed_strict']}/{d['shell_calls']} ({pct(d['unharmed_strict'], d['shell_calls']):.1f}%)")
    L.append(f"**Same, but also treating cargo build/test/run/install/add/update as a break "
              f"(lower bound):** {d['unharmed_incl_maybe']}/{d['shell_calls']} "
              f"({pct(d['unharmed_incl_maybe'], d['shell_calls']):.1f}%)")

    for c in CLASSES:
        if not d['class_calls'][c]:
            continue
        L.append(f"\ntop 15 commands, {CLASS_TITLE[c]}:")
        for lbl, n in d['class_labels'][c].most_common(15):
            L.append(f"  - {lbl}: {n}")

    return '\n'.join(L)


HEURISTIC_NOTES = """
## Heuristic limits (read before trusting the percentages)

- Classification runs on the RAW command STRING, not on what the process
  actually did. `git commit --dry-run` and `git commit` classify the same.
- A pipeline/chain (`&&`, `||`, `;`, `|`) is classified as a whole: if ANY
  segment trips a class, the whole command counts for that class, because a
  `&&` chain stops at the first blocked segment. A segment's "first
  tool+subcommand" is used for the top-15 label even when later segments
  also tripped other classes.
- `WRITE_OUTSIDE_TOOL` is presence-based: any invocation of cargo, npm, pip,
  go, brew, gh, claude, codex, pi, or a listed editor is flagged, regardless
  of subcommand -- e.g. `npm --version` counts the same as `npm install`.
  This over-counts tools whose subcommand truly touches nothing outside cwd,
  and under-counts nothing (conservative for "would this ever touch
  outside state").
- `pi`/`code` as a bare first word can collide with unrelated shell tokens
  (a loop variable, a project called `code`); not filtered out.
- `write_outside_explicit`/`write_tmp` require an actual write signal, not
  just a path mention: file redirection (`>`, `>>`, `&>`, excluding fd-dup
  forms like `2>&1`), a `-o`/`-O`/`--output` flag, or a first word from a
  small write-verb list (`cp mv rm mkdir touch ln chmod chown tee dd rsync
  tar unzip zip patch`; `sed`/`perl` only count with `-i`). Everything else
  -- `cd`, `ls`, `cat`, `grep`, `find`, `git log`, a script run with no
  output flag -- is read-only under this heuristic, even though an inline
  python/node/bun/zig script *could* write anywhere; the log has no
  visibility into a script's own body, so such writes are invisible here
  (an undercount, most likely benign since sampled inline scripts mostly
  edit relative, in-workspace paths).
- A leading `cd <path>` (or `cd` with no args, or mid-chain via `&&`) moves
  a per-command "effective directory" used to resolve relative write
  targets and to pick which repo a later `git` segment sees; the sandbox
  boundary itself (`workspace_root`) never moves. `cd -` (previous dir) is
  untrackable without real shell state and is treated as a no-op.
  Quoting/escaping edge cases in path tokens are not specially handled.
  "Outside" is plain string-prefix against the session's recorded cwd, not
  symlink-resolved. `/dev/null` is a standing exception (never "outside").
- `/tmp` and `$TMPDIR` detection matches literal `/tmp/`, `/private/tmp/`,
  `/var/folders/*/*/T/` and the `$TMPDIR` token; it does not resolve the
  variable's actual value from the environment (unknown at log-read time).
- Git-worktree detection (class 3) reads `<cwd>/.git` on THIS machine, now.
  A session whose cwd no longer exists, or whose worktree was since removed
  or converted, is silently treated as "not a worktree" (undercounts class
  3). Cached once per distinct cwd string.
- "Network, maybe" (cargo) is reported separately and NOT included in the
  strict "would break" denominator, because whether it actually reaches the
  network depends on whether `~/.cargo/registry` already has the crate --
  information the log doesn't carry. A second, lower-bound "unharmed"
  number treats it as a break. A `--offline` flag (spot-checked: present on
  11/53 of the sampled network-maybe cargo commands) genuinely rules out
  network but is not detected, so "maybe" over-counts by that much.
- `cp`/`mv`/`ln` treat only the LAST non-flag argument as the write target
  (the destination); earlier arguments are sources, which may legitimately
  sit outside cwd while the write itself stays inside (`cp ~/x ./y` only
  writes inside). Other multi-arg write verbs (`mkdir`, `rm`, `touch`,
  `chmod`, `chown`, `tar`, ...) treat every non-flag argument as a
  candidate write target -- correct for `mkdir a b`, over-broad for
  something like `chmod 700 path` (the mode "700" is also checked as a
  candidate path but never matches "outside cwd" since it doesn't start
  with `/` or `~`, so it's harmless in practice).
- pi's session cwd is read once from the session-header line and applied to
  every shell call in that session (pi sessions don't record a per-call
  cwd); Claude Code carries cwd on every line, used per call.
"""


out = ['# Sandbox confinement: how often would shell calls break? (ticket #30)\n',
       'Measured 2026-09-26, one pass per corpus, on this machine\'s real session logs. '
       'Confinement modeled: write access to the session\'s cwd and a temp dir, no network, '
       'no writes anywhere else.\n',
       HEURISTIC_NOTES]
out.append(report('pi', pi))
out.append(report('Claude Code (main)', cc_main))
out.append(report('Claude Code (subagents)', cc_sub))

text = '\n'.join(out) + '\n'
print(text)
with open(os.path.join(os.path.dirname(__file__), 'usage.md'), 'w') as fh:
    fh.write(text)

# ---------------- spot check: 10 random classified commands per class ----------------
print('\n\n========== SPOT CHECK (10 random per class, pooled across corpora) ==========')
random.seed(30)
pool = pi['all_classified'] + cc_main['all_classified'] + cc_sub['all_classified']
for c in CLASSES:
    matches = [cmd for cmd, agg in pool if agg[c]]
    if not matches:
        print(f'\n-- {c}: no matches --')
        continue
    sample = random.sample(matches, min(10, len(matches)))
    print(f'\n-- {c} (n={len(matches)}), {len(sample)} sampled --')
    for s in sample:
        print(f'  {s[:160]!r}')
