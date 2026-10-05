import os
# Probe #134: instructions vs input system message (answer + cache), tool with no strict key, store default.
# Model gpt-6-luna direct, https://api.openai.com/v1/responses. Prices $0.10/M in, $0.01/M cached, $0.50/M out.
import json, urllib.request, uuid, time
KEY=open(os.path.expanduser('~/.config/probe-keys/openai-key')).read().strip(); URL='https://api.openai.com/v1/responses'; M='gpt-6-luna'
RAW=[]; SPEND=0.0; CAP=0.75; N=0
def call(body,label):
    global SPEND,N
    N+=1; assert N<=40 and SPEND<CAP
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    try: r=json.load(urllib.request.urlopen(req,timeout=120))
    except urllib.error.HTTPError as e: r={'http_error':e.code,'body':json.loads(e.read().decode() or '{}')}
    u=r.get('usage') or {}; c=(u.get('input_tokens_details') or {}).get('cached_tokens') or 0
    SPEND+=((u.get('input_tokens') or 0)-c)*1e-7+c*1e-8+(u.get('output_tokens') or 0)*5e-7
    RAW.append({'label':label,'request':body,'response':r}); return r
def txt(r): return ''.join(c.get('text','') for i in r.get('output',[]) if i['type']=='message' for c in i['content'])
def show(l,r):
    u=r.get('usage') or {}; print(f"{l:28s} in={u.get('input_tokens')} cached={(u.get('input_tokens_details') or {}).get('cached_tokens')} ans={txt(r)!r} {r.get('http_error','')}")
def fill(n): return f'[{n}] You are a test harness. '+' '.join(f'Rule {i}: keep the workspace tidy and report each file you change, with its path and reason.' for i in range(110))
Q='What is the number in Rule 7? Answer with the digit only.'
def req(form,n):
    b={'model':M,'reasoning':{'effort':'none'},'max_output_tokens':64,'input':[{'role':'user','content':Q}]}
    if form=='instructions': b['instructions']=fill(n)
    else: b['input'].insert(0,{'role':'system','content':fill(n)})
    return b
for grp in ('A','B'):  # two independent nonces, one per form
    n=uuid.uuid4().hex[:8]; f='instructions' if grp=='A' else 'input-system'
    for k in (1,2):
        r=call(req(f,n),f'{f} #{k}'); show(f'{f} #{k}',r); time.sleep(3)
# switch: warm with instructions, then same text as input system message, then back
n=uuid.uuid4().hex[:8]
for f in ('instructions','instructions','input-system','input-system','instructions'):
    r=call(req(f,n),f'switch {f}'); show('switch '+f,r); time.sleep(3)
# strict default
TOOL={'type':'function','name':'f','description':'Takes a and optional b.','parameters':{'type':'object','properties':{'a':{'type':'string'},'b':{'type':'string'}},'required':['a']}}
r=call({'model':M,'reasoning':{'effort':'none'},'max_output_tokens':64,'tools':[TOOL],'input':'Call f with a="x".'},'tool no strict'); print('no strict:',r.get('http_error') or json.dumps(r.get('tools')), [i['type'] for i in r.get('output',[])])
r=call({'model':M,'reasoning':{'effort':'none'},'max_output_tokens':64,'tools':[{**TOOL,'strict':True}],'input':'Call f with a="x".'},'tool strict true'); print('strict true:',(r.get('http_error'),r.get('body')) if r.get('http_error') else json.dumps(r.get('tools')))
r=call({'model':M,'reasoning':{'effort':'none'},'max_output_tokens':64,'tools':[{**TOOL,'strict':False}],'input':'Call f with a="x".'},'tool strict false'); print('strict false:',r.get('http_error') or json.dumps(r.get('tools')))
# store default (no store key); response object echoes it
r=call({'model':M,'reasoning':{'effort':'none'},'max_output_tokens':16,'input':'hi'},'store default'); print('store echo:',r.get('store'))
print('spend est $%.4f in %d requests'%(SPEND,N))
json.dump(RAW,open('raw/probe.json','w'),indent=1)
