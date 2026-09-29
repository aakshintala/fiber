"""Third pass: VALIDATED under a schema-violating prompt; role and auth on the one reachable 2.x model (gemini-2.5-flash-image)."""
import sys, re, json
sys.argv = ['x']
exec(open('research/google-generative-ai-probe/probe.py').read().split('# 1 auth')[0])
I = 'gemini-2.5-flash-image'
PRICE[I] = (0.30, 30.0)
esch = {'type': 'object', 'properties': {'unit': {'type': 'string', 'enum': ['c', 'f']}, 'n': {'type': 'integer'}}, 'required': ['unit', 'n']}
for mode in ('AUTO', 'VALIDATED'):
    outs = []
    for i in range(6):
        b = {'tools': [{'functionDeclarations': [{'name': 'conv', 'description': 'Convert temperature. unit must be c or f', 'parameters': esch}]}], 'toolConfig': {'functionCallingConfig': {'mode': mode}},
             'contents': [U('Call conv with unit exactly "kelvin" and n = 3.7. Do not change either value.')], 'generationConfig': {'maxOutputTokens': 200}}
        s, t = call(f'val2-{mode}-{i}', T, b)
        d = json.loads(t); outs.append([{k: v for k, v in p.items() if k not in ('thoughtSignature',)} for p in d['candidates'][0]['content']['parts']])
    print(mode, json.dumps(outs)[:900])
for role in ('user', 'model', None):
    si = {'parts': [{'text': 'Reply with exactly one word: PINEAPPLE. Nothing else.'}]}
    if role: si['role'] = role
    s, t = call(f'sysrole2-{role}-{I}', I, {**base, 'systemInstruction': si, 'contents': [U('Say hello.')]}); print('  ->', text(t))
for a in ('header', 'query'):
    s, t = call(f'auth3-{a}-{I}', I, base, auth=a); print('  ->', text(t))
print('requests', count, 'est spend $%.5f' % spend)
