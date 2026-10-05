import os
# Probe #95/#132: what Anthropic accepts when a turn's signed thinking is resent,
# dropped, tampered with or sent to another model, and what each does to the cache.
# Usage: python3 anth.py [explore|main|redacted]
import json, urllib.request, uuid, copy, sys, time
KEY=os.environ['OPENROUTER_API_KEY']
URL='https://openrouter.ai/api/v1/messages'
SON,HAI='anthropic/claude-sonnet-5','anthropic/claude-haiku-4.5'
RAW=[]
def call(body, label):
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json','anthropic-version':'2023-06-01'})
    try: r=json.load(urllib.request.urlopen(req,timeout=300))
    except urllib.error.HTTPError as e: r={'error':e.code,'body':e.read()[:600].decode()}
    RAW.append({'label':label,'request':body,'response':r}); return r
def use(r):
    if 'error' in r: return f"ERR {r['error']} {r.get('body','')}"
    u=r.get('usage',{}); return f"in={u.get('input_tokens')} read={u.get('cache_read_input_tokens')} write={u.get('cache_creation_input_tokens')} stop={r.get('stop_reason')} blocks={[c['type'] for c in r.get('content',[])]}"
FILL=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change with its path and reason.' for i in range(230))
TOOLS=[{'name':'read_file','description':'Reads a file and returns its contents.','input_schema':{'type':'object','properties':{'path':{'type':'string'}},'required':['path']}}]
THINK={'type':'enabled','budget_tokens':1024}
def turn1(n, model=SON, think=THINK, user=None):
    b={'model':model,'max_tokens':2048,'thinking':think,'provider':{'only':['anthropic']},'tools':TOOLS,
       'system':[{'type':'text','text':f'[{n}] You are a test harness. '+FILL,'cache_control':{'type':'ephemeral'}}],
       'messages':[{'role':'user','content':[{'type':'text','text':user or 'Use read_file on /tmp/a.txt, then tell me how many words it has. Think briefly first.','cache_control':{'type':'ephemeral'}}]}]}
    if think is None: b.pop('thinking')
    return b
def strip_cc(b):
    for m in b['messages']:
        if isinstance(m['content'],list):
            for p in m['content']: p.pop('cache_control',None)
def turn2(b1, r1):
    b=copy.deepcopy(b1); strip_cc(b)
    tu=[c for c in r1['content'] if c['type']=='tool_use'][0]
    b['messages']+=[{'role':'assistant','content':r1['content']},
                    {'role':'user','content':[{'type':'tool_result','tool_use_id':tu['id'],'content':'alpha beta gamma delta','cache_control':{'type':'ephemeral'}}]}]
    return b
def turn3(b2, r2):
    b=copy.deepcopy(b2); strip_cc(b)
    b['messages']+=[{'role':'assistant','content':r2['content']},
                    {'role':'user','content':[{'type':'text','text':'Now say done.','cache_control':{'type':'ephemeral'}}]}]
    return b
def assistant_idx(b): return [i for i,m in enumerate(b['messages']) if m['role']=='assistant']
def edit_thinking(b, f, which=0):
    m=b['messages'][assistant_idx(b)[which]]
    m['content']=[x for x in (f(c) if c['type'] in ('thinking','redacted_thinking') else c for c in m['content']) if x is not None]
    return b
drop=lambda c: None
as_text=lambda c: {'type':'text','text':c.get('thinking','')} if c['type']=='thinking' else None
def flip(s): return s[:40]+('A' if s[40]!='A' else 'B')+s[41:]
tamper_sig=lambda c: {**c,'signature':flip(c['signature'])}
tamper_text=lambda c: {**c,'thinking':c['thinking']+' extra'}
empty_text=lambda c: {**c,'thinking':''}
def save(name): json.dump(RAW,open(f'raw-{name}.json','w'),indent=1)
which=sys.argv[1]
if which=='explore':
    n=uuid.uuid4().hex[:8]; b=turn1(n); r=call(b,'t1'); print(use(r)); print(json.dumps(r.get('content'),indent=1)[:1500] if 'content' in r else r)
    save('explore')
if which=='main':
    n=uuid.uuid4().hex[:8]; b1=turn1(n); r1=call(b1,'t1'); print('t1', use(r1))
    b2=turn2(b1,r1)
    V=[('unchanged (write)',lambda b:b),('unchanged (read)',lambda b:b),
       ('thinking dropped',lambda b:edit_thinking(b,drop)),
       ('signature altered',lambda b:edit_thinking(b,tamper_sig)),
       ('thinking text altered',lambda b:edit_thinking(b,tamper_text)),
       ('thinking text emptied',lambda b:edit_thinking(b,empty_text)),
       ('thinking as text block',lambda b:edit_thinking(b,as_text)),
       ('thinking off, block kept',lambda b:(b.pop('thinking'),b)[1]),
       ('thinking off, block dropped',lambda b:(b.pop('thinking'),edit_thinking(b,drop))[1]),
       ('Haiku 4.5, unchanged',lambda b:(b.update(model=HAI),b)[1]),
       ('Haiku 4.5, thinking dropped',lambda b:(b.update(model=HAI),edit_thinking(b,drop))[1]),
       ('Haiku 4.5, thinking as text',lambda b:(b.update(model=HAI),edit_thinking(b,as_text))[1])]
    r2=None
    for name,f in V:
        r=call(f(copy.deepcopy(b2)),'t2 '+name); print(f'{"t2 "+name:36s}',use(r))
        if name=='unchanged (write)': r2=r
        time.sleep(2)
    b3=turn3(b2,r2)
    for name,f in [('unchanged (write)',lambda b:b),('unchanged (read)',lambda b:b),
                   ('turn-1 thinking dropped',lambda b:edit_thinking(b,drop,0)),
                   ('all thinking dropped',lambda b:edit_thinking(edit_thinking(b,drop,0),drop,1)),
                   ('thinking off, blocks kept',lambda b:(b.pop('thinking'),b)[1]),
                   ('Haiku 4.5, unchanged',lambda b:(b.update(model=HAI),b)[1])]:
        r=call(f(copy.deepcopy(b3)),'t3 '+name); print(f'{"t3 "+name:36s}',use(r)); time.sleep(2)
    save('main')
if which=='redacted':
    MAGIC='ANTHROPIC_MAGIC_STRING_TRIGGER_REDACTED_THINKING_46C9A13E193C177646C7398A98432ECCCE4C1253'
    for model in (SON,HAI):
        n=uuid.uuid4().hex[:8]; b1=turn1(n,model,user=MAGIC+' Then use read_file on /tmp/a.txt.'); r1=call(b1,'t1 '+model); print(model,'t1',use(r1))
        if 'content' not in r1 or not any(c['type']=='tool_use' for c in r1['content']): continue
        b2=turn2(b1,r1); other=HAI if model==SON else SON
        for name,f in [('unchanged',lambda b:b),('redacted dropped',lambda b:edit_thinking(b,drop)),
                       ('redacted data altered',lambda b:edit_thinking(b,lambda c:{**c,'data':flip(c['data'])} if c['type']=='redacted_thinking' else c)),
                       ('to other model, unchanged',lambda b:(b.update(model=other),b)[1])]:
            r=call(f(copy.deepcopy(b2)),'t2 '+name); print(f'  t2 {name:30s}',use(r)); time.sleep(2)
    save('redacted')
if which=='fake-redacted':
    for model in (SON,HAI):
        n=uuid.uuid4().hex[:8]; b1=turn1(n,model); r1=call(b1,'t1 '+model); b2=turn2(b1,r1)
        sig=[c for c in r1['content'] if c['type']=='thinking'][0]['signature']
        for name,blk in [('garbage data',{'type':'redacted_thinking','data':'Zm9vYmFy'}),
                         ('thinking signature as data',{'type':'redacted_thinking','data':sig})]:
            b=copy.deepcopy(b2); a=b['messages'][assistant_idx(b)[0]]; a['content']=[blk]+[c for c in a['content'] if c['type']!='thinking']
            r=call(b,f't2 {model} {name}'); print(f'{model:28s} {name:28s}',use(r)); time.sleep(2)
    save('fake-redacted')
if which=='hosts':
    n=uuid.uuid4().hex[:8]; b1=turn1(n); r1=call(b1,'t1 anthropic'); print('t1 anthropic',use(r1)); b2=turn2(b1,r1)
    for host in ['amazon-bedrock','google-vertex']:
        for name,f in [('unchanged',lambda b:b),('thinking dropped',lambda b:edit_thinking(b,drop))]:
            b=f(copy.deepcopy(b2)); b['provider']={'only':[host],'allow_fallbacks':False}
            r=call(b,f't2 {host} {name}'); print(f'{host:16s} {name:18s}',use(r), (r.get('provider') or ''))
            time.sleep(2)
    save('hosts')
