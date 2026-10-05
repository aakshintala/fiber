"""Gemini probes for #136. Key read from GEMINI_API_KEY, never written. Hard request cap and spend cap."""
import os
import json, re, sys, base64, zlib, struct, urllib.request, urllib.error, time
KEY = os.environ['GEMINI_API_KEY']
BASE = 'https://generativelanguage.googleapis.com/v1beta/models/'
RAW = 'research/google-generative-ai-probe/raw/'
PRICE = {'gemini-2.5-flash-lite': (0.10, 0.40), 'gemini-3.1-flash-lite': (0.25, 1.50), 'gemini-2.5-flash': (0.30, 2.50)}
spend = 0.0; count = 0; CAP = 1.00; MAXREQ = 60
# gemini-2.5-flash-lite returns 404 for this key (raw/lite-404-*.json); gemini-2.5-flash stands in for the 2.x side.
F, T = 'gemini-2.5-flash', 'gemini-3.1-flash-lite'
SIG = 'skip_thought_signature_validator'  # documented dummy for replayed calls on Gemini 3

def redact(s):
    return s.replace(KEY, 'REDACTED')

def call(name, model, body, stream=False, auth='header', method=None):
    global spend, count
    if model == T: [p.update({'thoughtSignature': SIG}) for c in body.get('contents', []) for p in c['parts'] if 'functionCall' in p]
    count += 1
    assert count <= MAXREQ and spend < CAP
    m = method or ('streamGenerateContent' if stream else 'generateContent')
    url = f'{BASE}{model}:{m}' + ('?alt=sse' if stream else '')
    h = {'content-type': 'application/json'}
    if auth == 'header': h['x-goog-api-key'] = KEY
    elif auth == 'query': url += ('&' if '?' in url else '?') + 'key=' + KEY
    elif auth == 'badheader': h['x-goog-api-key'] = 'not-a-key'
    req = urllib.request.Request(url, json.dumps(body).encode(), h)
    try:
        r = urllib.request.urlopen(req, timeout=60); status = r.status; hd = dict(r.headers); txt = r.read().decode()
    except urllib.error.HTTPError as e:
        status = e.code; hd = dict(e.headers); txt = e.read().decode()
    hd = {k: v for k, v in hd.items() if k.lower() not in ('set-cookie',)}
    for u in re.findall(r'"promptTokenCount":\s*(\d+)', txt)[-1:]:
        pi = int(u)
        po = sum(int(x) for x in re.findall(r'"(?:candidatesTokenCount|thoughtsTokenCount)":\s*(\d+)', txt[-600:]))
        pr = PRICE.get(model, (0.25, 1.5)); spend += (pi*pr[0] + po*pr[1]) / 1e6
    out = {'name': name, 'model': model, 'auth': auth, 'url': redact(url), 'status': status, 'headers': hd, 'body': redact(txt)}
    try: out['request'] = body
    except: pass
    json.dump(out, open(RAW + name + '.json', 'w'), indent=1)
    try: j = json.loads(txt) if not stream else None
    except Exception: j = None
    print(f'{name:40} {model:24} {status}', (txt[:160].replace('\n', ' ') if status != 200 or '-v' in sys.argv else ''))
    return status, txt

def text(txt):
    try: return ''.join(p.get('text', '') for p in json.loads(txt)['candidates'][0]['content']['parts'])
    except Exception: return None

U = lambda t: {'role': 'user', 'parts': [{'text': t}]}
base = {'contents': [U('hi')], 'generationConfig': {'maxOutputTokens': 30}}

# 1 auth
for a in ('header', 'query', 'badheader', 'none'):
    call(f'auth-{a}', F, base, auth=a)
call('auth-both', F, base, auth='query')  # query only duplicate; header variant below
# both header and query
# (handled by query path: header not set) -> add explicit both
def both():
    global count
    count += 1
    url = f'{BASE}{F}:generateContent?key={KEY}'
    req = urllib.request.Request(url, json.dumps(base).encode(), {'content-type': 'application/json', 'x-goog-api-key': 'not-a-key'})
    try: r = urllib.request.urlopen(req); s = r.status; t = r.read().decode()
    except urllib.error.HTTPError as e: s = e.code; t = e.read().decode()
    json.dump({'name': 'auth-both-badheader-goodquery', 'status': s, 'body': redact(t)}, open(RAW + 'auth-both-badheader-goodquery.json', 'w'), indent=1)
    print('auth-both-badheader-goodquery', s)
both()

# 2 systemInstruction role
for role in ('user', 'model', None):
    si = {'parts': [{'text': 'Reply with exactly one word: PINEAPPLE. Nothing else.'}]}
    if role: si['role'] = role
    for model in (F, T):
        s, t = call(f'sysrole-{role}-{model}', model, {**base, 'systemInstruction': si, 'contents': [U('Say hello.')]})
        print('   ->', text(t))

# 3 schemas
sch = {'type': 'object', 'properties': {'p': {'$ref': '#/$defs/Pt'}, 'v': {'anyOf': [{'type': 'string'}, {'type': 'integer'}]}}, 'required': ['p'], 'additionalProperties': False,
       '$defs': {'Pt': {'type': 'object', 'properties': {'x': {'type': 'integer'}, 'y': {'type': 'integer'}}, 'required': ['x', 'y']}}}
osub = {'type': 'object', 'properties': {'p': {'type': 'object', 'properties': {'x': {'type': 'integer'}, 'y': {'type': 'integer'}}, 'required': ['x', 'y']}, 'v': {'type': 'string'}}, 'required': ['p']}
def tool(field, s): return {'tools': [{'functionDeclarations': [{'name': 'plot', 'description': 'Plot a point', field: s}]}],
                            'toolConfig': {'functionCallingConfig': {'mode': 'ANY'}}, 'contents': [U('Plot point x=3 y=4 with v "a".')], 'generationConfig': {'maxOutputTokens': 100}}
for model in (F, T):
    for name, field, s in (('json-schema-in-parametersJsonSchema', 'parametersJsonSchema', sch), ('json-schema-in-parameters', 'parameters', sch), ('openapi-subset-in-parameters', 'parameters', osub)):
        st, t = call(f'schema-{name}-{model}', model, tool(field, s))
        if st == 200: print('   ->', re.findall(r'"functionCall":.*?\}\s*\}', t, re.S)[:1] or t[:200])
# parameters with only $ref, no $defs? and anyOf only
call(f'schema-anyof-only-in-parameters-{F}', F, tool('parameters', {'type': 'object', 'properties': {'v': {'anyOf': [{'type': 'string'}, {'type': 'integer'}]}}}))

# 4 function-call ids
def hist(cid, rid):
    fc = {'name': 'get_w', 'args': {'city': 'Paris'}}; fr = {'name': 'get_w', 'response': {'temp': 20}}
    if cid is not None: fc['id'] = cid
    if rid is not None: fr['id'] = rid
    return {'contents': [U('Weather in Paris?'), {'role': 'model', 'parts': [{'functionCall': fc}]}, {'role': 'user', 'parts': [{'functionResponse': fr}]}],
            'tools': [{'functionDeclarations': [{'name': 'get_w', 'description': 'weather', 'parameters': {'type': 'object', 'properties': {'city': {'type': 'string'}}}}]}],
            'generationConfig': {'maxOutputTokens': 40}}
for model in (F, T):
    for nm, cid in (('none', None), ('good', 'call_abc-123'), ('bad-space-slash', 'call abc/123!'), ('long65', 'a'*65)):
        s, t = call(f'id-{nm}-{model}', model, hist(cid, cid))
# also: does the model emit an id on its own?
for model in (F, T):
    st, t = call(f'id-emitted-{model}', model, tool('parameters', osub))
    print('   emitted id present:', '"id"' in t)

# 5 image in functionResponse.parts
def png(rgb):
    def ch(t, d): c = struct.pack('>I', len(d)) + t + d; return c + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    raw = b''.join(b'\x00' + bytes(rgb) * 16 for _ in range(16))
    return b'\x89PNG\r\n\x1a\n' + ch(b'IHDR', struct.pack('>IIBBBBB', 16, 16, 8, 2, 0, 0, 0)) + ch(b'IDAT', zlib.compress(raw)) + ch(b'IEND', b'')
img = base64.b64encode(png((255, 0, 0))).decode()
def imgreq(withimg=True):
    fr = {'name': 'shot', 'response': {'note': 'screenshot attached'}}
    if withimg: fr['parts'] = [{'inlineData': {'mimeType': 'image/png', 'data': img}}]
    return {'contents': [U('Take a screenshot then tell me the single colour of the image.'), {'role': 'model', 'parts': [{'functionCall': {'name': 'shot', 'args': {}}}]}, {'role': 'user', 'parts': [{'functionResponse': fr}]}],
            'tools': [{'functionDeclarations': [{'name': 'shot', 'description': 'screenshot'}]}], 'generationConfig': {'maxOutputTokens': 60}}
for model in (F, T):
    s, t = call(f'imgparts-{model}', model, imgreq()); print('   ->', text(t))
# control: same image as a plain user part
s, t = call(f'img-plain-user-part-{F}', F, {'contents': [{'role': 'user', 'parts': [{'text': 'Single colour of this image?'}, {'inlineData': {'mimeType': 'image/png', 'data': img}}]}], 'generationConfig': {'maxOutputTokens': 30}}); print('   ->', text(t))

# 6 SSE error triggers
trig = [
 ('sse-huge-maxoutput', F, {**base, 'generationConfig': {'maxOutputTokens': 10000000}}),
 ('sse-bad-mime', F, {**base, 'generationConfig': {'responseMimeType': 'text/bogus'}}),
 ('sse-empty-contents', F, {'contents': []}),
 ('sse-toolresponse-noname', F, {'contents': [{'role': 'user', 'parts': [{'functionResponse': {'name': 'x', 'response': {}}}]}]}),
 ('sse-long-gen-cut', F, {'contents': [U('Count from 1 to 3000 separated by spaces.')], 'generationConfig': {'maxOutputTokens': 8000, 'temperature': 2.0}}),
]
for nm, model, b in trig:
    st, t = call(nm, model, b, stream=True)
    print('   sse error events:', [l for l in t.splitlines() if l.startswith('data:') and '"error"' in l][:2], 'lines', len(t.splitlines()))
# 7 VALIDATED
def vreq(mode):
    return {'tools': [{'functionDeclarations': [{'name': 'plot', 'description': 'Plot a point', 'parameters': osub}]}],
            'toolConfig': {'functionCallingConfig': {'mode': mode}}, 'contents': [U('Say hi in one word, no tools.')], 'generationConfig': {'maxOutputTokens': 100}}
for model in (F, T):
    for mode in ('AUTO', 'ANY', 'VALIDATED', 'NONE'):
        s, t = call(f'mode-{mode}-{model}', model, vreq(mode)); print('   ->', text(t), '| fc' if 'functionCall' in t else '')
# VALIDATED with a prompt that tempts a tool call
for model in (F, T):
    for mode in ('AUTO', 'VALIDATED'):
        b = vreq(mode); b['contents'] = [U('Plot x=3 y=4.')]
        s, t = call(f'mode-tempt-{mode}-{model}', model, b); print('   ->', re.findall(r'"functionCall":.*?\}\s*\}', t, re.S)[:1] or text(t))
print('requests', count, 'est spend $%.5f' % spend)
