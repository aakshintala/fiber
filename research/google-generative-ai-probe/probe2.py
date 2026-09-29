"""Second pass: auth on 3.1, other 2.x ids, SSE on 3.1, blue image control, VALIDATED samples. Reuses probe.py helpers."""
import sys, re, json
sys.argv = ['x']
src = open('research/google-generative-ai-probe/probe.py').read().split('# 1 auth')[0]
exec(src)
full = open('research/google-generative-ai-probe/probe.py').read()
def seg(a, b): return full.split(a)[1].split(b)[0]
exec(seg('# 3 schemas', 'for model in (F, T):\n    for name, field'))
exec(seg('# 4 function-call ids', 'for model in (F, T):\n    for nm, cid'))
exec(seg('# 5 image in functionResponse.parts', 'for model in (F, T):\n    s, t = call(f\'imgparts'))
# 2.5-flash-image is the one 2.x model answering; try tools on it
s_, t_ = call('x2-flash-image-tool', 'gemini-2.5-flash-image', hist('call_abc-123','call_abc-123')); print(t_[:300])
s_, t_ = call('x2-flash-image-imgparts', 'gemini-2.5-flash-image', imgreq()); print(t_[:300])
# 1 auth on 3.1
for a in ('header', 'query'):
    call(f'auth2-{a}-{T}', T, base, auth=a)
call('auth2-badheader-goodquery', T, base, auth='query')  # placeholder overwritten below
count += 1
req = urllib.request.Request(f'{BASE}{T}:generateContent?key={KEY}', json.dumps(base).encode(), {'content-type': 'application/json', 'x-goog-api-key': 'not-a-key'})
try: r = urllib.request.urlopen(req); s, t = r.status, r.read().decode()
except urllib.error.HTTPError as e: s, t = e.code, e.read().decode()
json.dump({'name': 'auth2-badheader-goodquery', 'model': T, 'status': s, 'body': redact(t)}, open(RAW + 'auth2-badheader-goodquery.json', 'w'), indent=1); print('badheader+goodquery', s)
req = urllib.request.Request(f'{BASE}{T}:streamGenerateContent?alt=sse&key={KEY}', json.dumps(base).encode(), {'content-type': 'application/json'})
r = urllib.request.urlopen(req); t = r.read().decode(); count += 1
open(RAW + 'auth2-query-stream.txt', 'w').write(redact(f'status {r.status}\n' + t)); print('query stream', r.status)
# ids on 3.1: bad id, and check reply text
for nm, cid in (('good', 'call_abc-123'), ('bad-space-slash', 'call abc/123!'), ('long65', 'a'*65)):
    s, t = call(f'id2-{nm}-{T}', T, hist(cid, cid)); print('  ->', text(t))
# mismatched response id
h = hist('call_1', 'call_ZZZ'); s, t = call(f'id2-mismatch-{T}', T, h); print('  ->', text(t))
# emitted id shape
s, t = call(f'id2-emitted-{T}', T, tool('parameters', osub)); print(re.findall(r'"id":\s*"[^"]*"', t))
# image control blue, and image without note
img = base64.b64encode(png((0, 0, 255))).decode()
s, t = call(f'imgparts2-blue-{T}', T, imgreq()); print('  ->', text(t))
b = imgreq(False); s, t = call(f'imgparts2-none-{T}', T, b); print('  ->', text(t))
# SSE on 3.1
call('sse2-stream-ok', T, base, stream=True)
call('sse2-huge-maxoutput', T, {**base, 'generationConfig': {'maxOutputTokens': 10000000}}, stream=True)
call('sse2-long-gen', T, {'contents': [U('Count from 1 to 2000 separated by spaces.')], 'generationConfig': {'maxOutputTokens': 3000}}, stream=True)
# VALIDATED vs AUTO, 6 samples on a tricky enum schema, no forced call
esch = {'type': 'object', 'properties': {'unit': {'type': 'string', 'enum': ['c', 'f']}, 'n': {'type': 'integer'}}, 'required': ['unit', 'n']}
for mode in ('AUTO', 'VALIDATED', 'ANY'):
    outs = []
    for i in range(4):
        b = {'tools': [{'functionDeclarations': [{'name': 'conv', 'description': 'Convert temperature', 'parameters': esch}]}], 'toolConfig': {'functionCallingConfig': {'mode': mode}},
             'contents': [U('Convert about twenty degrees kelvin please, use whichever unit letter you like.')], 'generationConfig': {'maxOutputTokens': 200}}
        s, t = call(f'val-{mode}-{i}', T, b)
        outs.append((s, re.sub(r'\s+', '', ''.join(re.findall(r'"functionCall":.*?\}\s*\}', t, re.S)[:1])) or text(t)))
    print(mode, outs)
# VALIDATED + allowedFunctionNames
b = {'tools': [{'functionDeclarations': [{'name': 'conv', 'description': 'c', 'parameters': esch}]}], 'toolConfig': {'functionCallingConfig': {'mode': 'VALIDATED', 'allowedFunctionNames': ['conv']}}, 'contents': [U('hi')], 'generationConfig': {'maxOutputTokens': 60}}
call('val-allowed-names', T, b)
call('mode-lowercase-validated', T, {**b, 'toolConfig': {'functionCallingConfig': {'mode': 'validated'}}})
print('requests', count, 'est spend $%.5f' % spend)
