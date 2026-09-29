"""Record retry-relevant headers and statuses for cheap provider failures.

Usage: python3 probe.py errors [vendor ...] | burst <name> ...
Reuses send() and redact() from ../provider-errors/probe.py. Keys and the
codex token are read from files at call time and never saved. Results land in
raw/<vendor>.<case>.json.
"""
import json, os, sys, time, urllib.request, urllib.error, base64
from concurrent.futures import ThreadPoolExecutor
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'provider-errors'))
import probe as pe

RAW = os.path.join(os.path.dirname(__file__), 'raw')
os.makedirs(RAW, exist_ok=True)
pe.SECRET_HEADERS |= {'anthropic-organization-id', 'openai-organization', 'openai-project', 'x-goog-request-params',
                      'cf-cache-status', 'server-timing', 'x-envoy-upstream-service-time', 'x-oai-request-id', 'x-openai-proxy-wasm', 'anthropic-workspace-id', 'x-codex-turn-state', 'report-to', 'nel'}
import re
def scrub(text):
    text = re.sub(r'org: [0-9a-f-]{36}', 'org: [redacted]', text)
    return re.sub(r'("request_id\\?":\\?")[^"\\]*', r'\1[redacted]', text)
def scrub_file(p):
    d = json.load(open(p)); t = json.dumps(d, indent=1)
    def fix(o):
        if isinstance(o, dict): return {k: ('[redacted]' if k.lower() in pe.SECRET_HEADERS else fix(v)) for k, v in o.items()}
        if isinstance(o, list): return [fix(x) for x in o]
        return scrub(o) if isinstance(o, str) else o
    json.dump(fix(d), open(p, 'w'), indent=1)
def key(n): return open(f'/tmp/{n}-key').read().strip()

def send(method, url, hdrs, b, stream=False):
    """Like pe.send but with a method and an optional raw (non-JSON) body."""
    data = b if isinstance(b, bytes) else (json.dumps(b).encode() if b is not None else None)
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    t0 = time.time(); out = {'request_bytes': len(data or b'')}
    try:
        r = urllib.request.urlopen(req, timeout=120)
        out['status'] = r.status; out['headers'] = dict(r.headers)
        out['body'] = r.read().decode(errors='replace')
    except urllib.error.HTTPError as e:
        out['status'] = e.code; out['headers'] = dict(e.headers); out['body'] = e.read().decode(errors='replace')
    except Exception as e:
        out['exception'] = repr(e)
    out['seconds'] = round(time.time() - t0, 3)
    if len(out.get('body', '')) > 6000: out['body'] = out['body'][:6000] + '...[truncated]'
    return pe.redact(out)

def save(vendor, case, method, url, r):
    r = {'case': f'{vendor}.{case}', 'method': method, 'url': url, **r}
    # The codex account id and tokens never reach a saved file; scrub bodies too.
    p = os.path.join(RAW, f'{vendor}.{case}.json'); json.dump(r, open(p, 'w'), indent=1); scrub_file(p)
    h = {k.lower(): v for k, v in r.get('headers', {}).items()}
    keep = {k: v for k, v in h.items() if 'retry' in k or 'ratelimit' in k or 'rate-limit' in k}
    print(f"{vendor}.{case:28s} {r.get('status')} {r['seconds']}s {keep} {(r.get('body') or r.get('exception',''))[:110]!r}", flush=True)
    return r

# vendor -> dict(base config)
def anthropic(k):
    H = lambda kk=k: {'x-api-key': kk, 'anthropic-version': '2023-06-01', 'content-type': 'application/json'}
    U = 'https://api.anthropic.com/v1/messages'
    ok = {'model': 'claude-haiku-4-5', 'max_tokens': 1, 'messages': [{'role': 'user', 'content': 'hi'}]}
    return [('ok', 'POST', U, H(), ok),
            ('bad-key', 'POST', U, H('sk-ant-invalid-000000'), ok),
            ('unknown-model', 'POST', U, H(), dict(ok, model='no-such-model')),
            ('malformed-missing-messages', 'POST', U, H(), {'model': 'claude-haiku-4-5', 'max_tokens': 1}),
            ('malformed-not-json', 'POST', U, H(), b'{not json'),
            ('max-tokens-huge', 'POST', U, H(), dict(ok, max_tokens=50_000_000)),
            ('wrong-method', 'GET', U, H(), None),
            ('wrong-path', 'POST', 'https://api.anthropic.com/v1/nope', H(), ok),
            ('count-tokens-ok', 'POST', U + '/count_tokens', H(), {k2: v for k2, v in ok.items() if k2 != 'max_tokens'})]

def openai(k):
    H = lambda kk=k: {'authorization': 'Bearer ' + kk, 'content-type': 'application/json'}
    U = 'https://api.openai.com/v1/responses'
    ok = {'model': 'gpt-6-luna', 'input': 'hi', 'max_output_tokens': 16, 'reasoning': {'effort': 'none'}}
    return [('ok', 'POST', U, H(), ok),
            ('bad-key', 'POST', U, H('sk-invalid-000000'), ok),
            ('unknown-model', 'POST', U, H(), dict(ok, model='no-such-model')),
            ('malformed-missing-input', 'POST', U, H(), {'model': 'gpt-6-luna'}),
            ('malformed-not-json', 'POST', U, H(), b'{not json'),
            ('max-tokens-huge', 'POST', U, H(), dict(ok, max_output_tokens=50_000_000)),
            ('wrong-method', 'DELETE', U, H(), None),
            ('wrong-path', 'POST', 'https://api.openai.com/v1/nope', H(), ok)]

def gemini(k):
    H = lambda kk=k: {'x-goog-api-key': kk, 'content-type': 'application/json'}
    B = 'https://generativelanguage.googleapis.com/v1beta/models/'
    U = B + 'gemini-3.1-flash-lite:generateContent'
    ok = {'contents': [{'parts': [{'text': 'hi'}]}], 'generationConfig': {'maxOutputTokens': 8, 'thinkingConfig': {'thinkingBudget': 0}}}
    return [('ok', 'POST', U, H(), ok),
            ('bad-key', 'POST', U, H('AIza-invalid-000000'), ok),
            ('unknown-model', 'POST', B + 'no-such-model:generateContent', H(), ok),
            ('malformed-bad-field', 'POST', U, H(), {'contents': 'nope'}),
            ('malformed-not-json', 'POST', U, H(), b'{not json'),
            ('max-tokens-huge', 'POST', U, H(), {**ok, 'generationConfig': {'maxOutputTokens': 50_000_000}}),
            ('wrong-method', 'GET', U, H(), None),
            ('wrong-path', 'POST', 'https://generativelanguage.googleapis.com/v1beta/nope', H(), ok),
            ('count-tokens-ok', 'POST', B + 'gemini-3.1-flash-lite:countTokens', H(), {'contents': ok['contents']})]

def codex(_):
    a = json.load(open(os.path.expanduser('~/.codex/auth.json')))['tokens']
    def H(tok=a['access_token']):
        return {'authorization': 'Bearer ' + tok, 'chatgpt-account-id': a['account_id'], 'originator': 'pi',
                'openai-beta': 'responses=experimental', 'content-type': 'application/json', 'accept': 'text/event-stream'}
    U = 'https://chatgpt.com/backend-api/codex/responses'
    ok = {'model': 'gpt-6-luna', 'instructions': 'Reply ok.', 'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': 'hi'}]}],
          'stream': True, 'store': False}
    return [('ok', 'POST', U, H(), ok),
            ('bad-token', 'POST', U, H('invalid'), ok),
            ('unknown-model', 'POST', U, H(), dict(ok, model='no-such-model')),
            ('malformed-missing-input', 'POST', U, H(), {'model': 'gpt-6-luna', 'stream': True, 'store': False}),
            ('wrong-method-get', 'GET', U, H(), None),
            ('wrong-method-put', 'PUT', U, H(), ok),
            ('wrong-method-patch', 'PATCH', U, H(), ok),
            ('wrong-path', 'POST', 'https://chatgpt.com/backend-api/codex/nope', H(), ok),
            ('unsupported-verb-trace', 'TRACE', U, H(), None)]

def openrouter(k):
    H = lambda kk=k: {'authorization': 'Bearer ' + kk, 'content-type': 'application/json'}
    U = 'https://openrouter.ai/api/v1/chat/completions'
    ok = {'model': 'z-ai/glm-5.3-flash', 'max_tokens': 8, 'messages': [{'role': 'user', 'content': 'hi'}]}
    return [('ok', 'POST', U, H(), ok),
            ('bad-key', 'POST', U, H('sk-or-invalid-000000'), ok),
            ('unknown-model', 'POST', U, H(), dict(ok, model='z-ai/no-such-model')),
            ('malformed-missing-messages', 'POST', U, H(), {'model': 'z-ai/glm-5.3-flash'}),
            ('max-tokens-huge', 'POST', U, H(), dict(ok, max_tokens=50_000_000)),
            ('wrong-method', 'GET', U, H(), None),
            ('wrong-path', 'POST', 'https://openrouter.ai/api/v1/nope', H(), ok)]

def muse(k):
    H = lambda kk=k: {'authorization': 'Bearer ' + kk, 'content-type': 'application/json'}
    U = 'https://api.meta.ai/v1/chat/completions'
    ok = {'model': 'muse-spark-1.3-contributor', 'max_tokens': 8, 'messages': [{'role': 'user', 'content': 'hi'}]}
    return [('ok', 'POST', U, H(), ok),
            ('bad-key', 'POST', U, H('invalid-000000'), ok),
            ('unknown-model', 'POST', U, H(), dict(ok, model='no-such-model')),
            ('malformed-missing-messages', 'POST', U, H(), {'model': 'muse-spark-1.3-contributor'}),
            ('max-tokens-huge', 'POST', U, H(), dict(ok, max_tokens=50_000_000)),
            ('wrong-method', 'GET', U, H(), None),
            ('wrong-path', 'POST', 'https://api.meta.ai/v1/nope', H(), ok)]

VENDORS = {'anthropic': anthropic, 'openai': openai, 'gemini': gemini, 'codex': codex, 'openrouter': openrouter, 'muse': muse}

def errors(vendors):
    for v in vendors:
        for case, m, u, h, b in VENDORS[v](None if v == 'codex' else key(v)):
            save(v, case, m, u, send(m, u, h, b))

def burst(name, n, conc, method, url, hdrs, body, stop_on=429):
    """Send up to n identical requests, conc at a time; stop dispatching after the first 429."""
    stop = []; res = []
    def one(i):
        if stop: return None
        r = send(method, url, hdrs, body); res.append(r)
        if r.get('status') == stop_on: stop.append(r)
        return r
    with ThreadPoolExecutor(conc) as ex: list(ex.map(one, range(n)))
    counts = {}
    for r in res: counts[r.get('status')] = counts.get(r.get('status'), 0) + 1
    first = stop[0] if stop else None
    ex_ = {}
    for r in res:
        if r.get('status') != 200: ex_.setdefault(r.get('status'), r)
    out = {'case': name, 'requests_sent': len(res), 'status_counts': counts, 'first_429': first, 'first_of_each_non_200': ex_}
    p = os.path.join(RAW, f'{name}.json'); json.dump(out, open(p, 'w'), indent=1); scrub_file(p)
    print(name, len(res), counts, flush=True)
    if first: save(name.split('.')[0], name.split('.', 1)[1] + '-first-429', method, url, first)
    return res

def muse_repeat():
    _, m, u, h, b = next(x for x in muse(key('muse')) if x[0] == 'max-tokens-huge')
    for i in (2, 3): save('muse', f'max-tokens-huge-repeat{i}', m, u, send(m, u, h, b))

if __name__ == '__main__':
    if sys.argv[1] == 'muse-repeat': muse_repeat(); sys.exit()
    if sys.argv[1] == 'scrub': [scrub_file(os.path.join(RAW, f)) for f in os.listdir(RAW)]
    elif sys.argv[1] == 'errors': errors(sys.argv[2:])
    elif sys.argv[1] == 'burst':
        which = sys.argv[2]
        cases = {
          'anthropic.count-tokens': lambda: ('anthropic', 300, 20, 'count-tokens-ok'),
          'gemini.count-tokens': lambda: ('gemini', 300, 20, 'count-tokens-ok'),
          'anthropic.model': lambda: ('anthropic', 200, 20, 'ok'),
          'openai.model': lambda: ('openai', 200, 20, 'ok'),
          'gemini.model': lambda: ('gemini', 100, 20, 'ok')}
        v, n, c, case = cases[which]()
        _, m, u, h, b = next(x for x in VENDORS[v](key(v)) if x[0] == case)
        burst(which.replace('.', '.', 1) + '.burst' if False else which + '-burst', n, c, m, u, h, b)
