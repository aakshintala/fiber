"""Trigger provider failures on purpose and record status, headers and body.

Usage: python3 probe.py [case-name-substring ...]
Keys come from MUSE_API_KEY and OPENROUTER_API_KEY. Request headers are
never recorded. Results land in raw/<case>.json.
"""
import json, os, re, sys, time, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor

MUSE = os.environ['MUSE_API_KEY']
OR = os.environ['OPENROUTER_API_KEY']
RAW = os.path.join(os.path.dirname(__file__), 'raw')

# endpoint name -> (url, key, model, protocol)
ENDPOINTS = {
    'muse-completions': ('https://api.meta.ai/v1/chat/completions', MUSE, 'muse-spark-1.3-contributor', 'completions'),
    'muse-responses':   ('https://api.meta.ai/v1/responses', MUSE, 'muse-spark-1.3-contributor', 'responses'),
    'muse-messages':    ('https://api.meta.ai/v1/messages', MUSE, 'muse-spark-1.3-contributor', 'messages'),
    'or-completions-haiku': ('https://openrouter.ai/api/v1/chat/completions', OR, 'anthropic/claude-haiku-4.5', 'completions'),
    'or-completions-luna':  ('https://openrouter.ai/api/v1/chat/completions', OR, 'openai/gpt-6-luna', 'completions'),
    'or-responses-luna':    ('https://openrouter.ai/api/v1/responses', OR, 'openai/gpt-6-luna', 'responses'),
    'or-messages-haiku':    ('https://openrouter.ai/api/v1/messages', OR, 'anthropic/claude-haiku-4.5', 'messages'),
}

def body(proto, model, text='Say ok.', stream=False):
    if proto == 'responses':
        b = {'model': model, 'input': text, 'max_output_tokens': 16}
    else:
        b = {'model': model, 'max_tokens': 16, 'messages': [{'role': 'user', 'content': text}]}
    if stream: b['stream'] = True
    return b

def tool(proto, params):
    if proto == 'completions':
        return {'type': 'function', 'function': {'name': 'read', 'description': 'Read a file.', 'parameters': params}}
    if proto == 'responses':
        return {'type': 'function', 'name': 'read', 'description': 'Read a file.', 'parameters': params}
    return {'name': 'read', 'description': 'Read a file.', 'input_schema': params}

GOOD_SCHEMA = {'type': 'object', 'properties': {'path': {'type': 'string'}}, 'required': ['path']}
BAD_SCHEMA = {'type': 'object', 'properties': {'path': {'type': 'banana'}}, 'required': ['path']}

def headers(proto, key):
    h = {'Content-Type': 'application/json'}
    if key is not None:
        h['Authorization'] = 'Bearer ' + key
        if proto == 'messages': h['x-api-key'] = key
    if proto == 'messages': h['anthropic-version'] = '2023-06-01'
    return h

def send(url, hdrs, b, stream):
    data = json.dumps(b).encode()
    req = urllib.request.Request(url, data=data, headers=hdrs)
    t0 = time.time(); out = {'request_bytes': len(data)}
    try:
        r = urllib.request.urlopen(req, timeout=300)
        out['status'] = r.status; out['headers'] = dict(r.headers)
        if stream:
            lines = []
            for line in r:
                lines.append([round(time.time() - t0, 3), line.decode(errors='replace').rstrip('\n')])
            out['stream'] = lines
        else:
            out['body'] = r.read().decode(errors='replace')
    except urllib.error.HTTPError as e:
        out['status'] = e.code; out['headers'] = dict(e.headers); out['body'] = e.read().decode(errors='replace')
    except Exception as e:
        out['exception'] = repr(e)
    out['seconds'] = round(time.time() - t0, 3)
    if 'body' in out and len(out['body']) > 20000: out['body'] = out['body'][:20000] + '...[truncated]'
    return redact(out)

SECRET_HEADERS = {'set-cookie', 'proxy-status', 'cf-ray', 'x-request-id', 'x-generation-id', 'request-id'}

def redact(out):
    # Cookies, request ids and the encoded client address are not evidence.
    out['headers'] = {k: ('[redacted]' if k.lower() in SECRET_HEADERS else v) for k, v in out.get('headers', {}).items()}
    for k in ('body',):
        if k in out: out[k] = re.sub(r'"user_id":"[^"]*"', '"user_id":"[redacted]"', out[k])
    return out

def overflow_text(model):
    # About 1.3x the advertised window, in ~1-token words.
    n = {'anthropic/claude-haiku-4.5': 260_000}.get(model, 1_400_000)
    return 'hello ' * n

CASES = {}
for ep, (url, key, model, proto) in ENDPOINTS.items():
    def add(name, b, k=key, stream=False, u=url):
        CASES[f'{ep}.{name}'] = (u, proto, k, b, stream)
    add('ok-stream', body(proto, model, stream=True), stream=True)
    add('bad-key', body(proto, model), k='sk-fiber-probe-invalid-000000000000')
    add('missing-key', body(proto, model), k=None)
    add('unknown-model', body(proto, model.split('/')[0] + '/no-such-model' if '/' in model else 'no-such-model'))
    b = body(proto, model); b['tools'] = [tool(proto, BAD_SCHEMA)]; add('bad-schema', b)
    b = body(proto, model, stream=True); b['tools'] = [tool(proto, BAD_SCHEMA)]; add('bad-schema-stream', b, stream=True)
    tc = {'completions': 'required', 'responses': 'required', 'messages': {'type': 'any'}}[proto]
    b = body(proto, model); b['tools'] = [tool(proto, GOOD_SCHEMA)]; b['tool_choice'] = tc; add('tool-choice-required', b)
    b = body(proto, model); b['max_tokens' if proto != 'responses' else 'max_output_tokens'] = 50_000_000; add('max-tokens-huge', b)
    add('overflow', body(proto, model, overflow_text(model)))
    add('overflow-stream', body(proto, model, overflow_text(model), stream=True), stream=True)

# Dense text: OpenRouter estimates Latin text at about 4 characters a token, so
# random alphanumerics pass its check and reach the vendor, whose tokenizer
# splits them far finer. (CJK does not work: OpenRouter counts it at one token
# a character.)
import random
_rng = random.Random(96)
DENSE = ''.join(_rng.choice('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789') for _ in range(700_000))
for pin in ('anthropic', 'amazon-bedrock'):
    b = body('completions', 'anthropic/claude-haiku-4.5', DENSE); b['provider'] = {'only': [pin]}
    CASES[f'or-completions-haiku.overflow-dense-{pin}'] = (ENDPOINTS['or-completions-haiku'][0], 'completions', OR, b, False)
    b = dict(b, stream=True)
    CASES[f'or-completions-haiku.overflow-dense-{pin}-stream'] = (ENDPOINTS['or-completions-haiku'][0], 'completions', OR, b, True)
    b = body('messages', 'anthropic/claude-haiku-4.5', DENSE); b['provider'] = {'only': [pin]}
    CASES[f'or-messages-haiku.overflow-dense-{pin}'] = (ENDPOINTS['or-messages-haiku'][0], 'messages', OR, b, False)

# 4.0M characters: OpenRouter's estimate stays under Luna's 1.05M window.
DENSE_LUNA = ''.join(_rng.choice('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789') for _ in range(4_000_000))
for ep in ('or-completions-luna', 'or-responses-luna'):
    url, key, model, proto = ENDPOINTS[ep]
    CASES[f'{ep}.overflow-dense'] = (url, proto, key, body(proto, model, DENSE_LUNA), False)
    CASES[f'{ep}.overflow-dense-stream'] = (url, proto, key, body(proto, model, DENSE_LUNA, stream=True), True)

class Padded(dict):
    # Serialises with 8 MB of JSON whitespace: bytes without tokens.
    pass
_dumps = json.dumps
def _padded_dumps(o, *a, **k):
    s = _dumps(o, *a, **k)
    return s[:-1] + ' ' * 8_000_000 + '}' if isinstance(o, Padded) else s
json.dumps = _padded_dumps
CASES['muse-completions.padded-8mb'] = (ENDPOINTS['muse-completions'][0], 'completions', MUSE, Padded(body('completions', 'muse-spark-1.3-contributor')), False)

def burst(n=170):
    # muse advertises 150 requests a minute; each request here is ~10 tokens.
    url, key, model, proto = ENDPOINTS['muse-completions']
    b = body(proto, model); b['max_tokens'] = 1
    with ThreadPoolExecutor(32) as ex:
        rs = list(ex.map(lambda _: send(url, headers(proto, key), b, False), range(n)))
    json.dump({'case': 'muse-completions.burst', 'n': n, 'statuses': [r.get('status') for r in rs],
               'first_429': next((r for r in rs if r.get('status') == 429), None)}, open(os.path.join(RAW, 'muse-completions.burst.json'), 'w'), indent=1)
    print('burst', {s: [r.get('status') for r in rs].count(s) for s in set(r.get('status') for r in rs)})

if __name__ == '__main__':
    if sys.argv[1:] == ['burst']:
        os.makedirs(RAW, exist_ok=True); burst(); sys.exit()
    os.makedirs(RAW, exist_ok=True)
    want = sys.argv[1:]
    for name, (url, proto, key, b, stream) in CASES.items():
        if want and not any(w in name for w in want): continue
        r = send(url, headers(proto, key), b, stream)
        r = {'case': name, 'url': url, 'stream': stream, **r}
        json.dump(r, open(os.path.join(RAW, name + '.json'), 'w'), indent=1)
        tail = r.get('body') or ''.join(l[1] for l in r.get('stream', [])[-3:]) or r.get('exception', '')
        print(f"{name:45s} {r.get('status')} {r['seconds']:7.2f}s {tail[:200]!r}", flush=True)
