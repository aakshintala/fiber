import os
# Probe #95/#134: what OpenAI Responses (GPT-6 Luna via OpenRouter, store:false) accepts when a
# reasoning item is resent, dropped, rebuilt, tampered with or sent to another model.
# Usage: python3 resp.py [explore|main|include]
import json, urllib.request, uuid, copy, sys, time
KEY=open(os.path.expanduser('~/.config/probe-keys/openrouter-key')).read().strip()
URL='https://openrouter.ai/api/v1/responses'
GPT,SON='openai/gpt-6-luna','anthropic/claude-sonnet-5'
RAW=[]
def call(body,label):
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    try: r=json.load(urllib.request.urlopen(req,timeout=300))
    except urllib.error.HTTPError as e: r={'error':e.code,'body':e.read()[:600].decode()}
    RAW.append({'label':label,'request':body,'response':r}); return r
def use(r):
    if r.get('error'): return f"ERR {r.get('error')} {r.get('body','')}"
    u=r.get('usage') or {}; d=u.get('input_tokens_details') or {}; o=u.get('output_tokens_details') or {}
    return f"in={u.get('input_tokens')} cached={d.get('cached_tokens')} reasoning={o.get('reasoning_tokens')} status={r.get('status')} items={[i['type'] for i in r.get('output',[])]}"
FILL=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change with its path and reason.' for i in range(230))
TOOLS=[{'type':'function','name':'read_file','description':'Reads a file and returns its contents.','strict':False,
        'parameters':{'type':'object','properties':{'path':{'type':'string'}},'required':['path'],'additionalProperties':False}}]
def turn1(n,model=GPT):
    return {'model':model,'store':False,'include':['reasoning.encrypted_content'],'reasoning':{'effort':'high','summary':'auto'},
            'tools':TOOLS,'prompt_cache_key':n,'max_output_tokens':4096,
            'input':[{'role':'developer','content':f'[{n}] You are a test harness. '+FILL},
                     {'role':'user','content':'Work out which of 391, 401 and 437 is prime, without tools. Then use read_file on /tmp/<that number>.txt and tell me how many words it has.'}]}
def turn2(b1,r1):
    b=copy.deepcopy(b1); fc=[i for i in r1['output'] if i['type']=='function_call'][0]
    b['input']+=copy.deepcopy(r1['output'])+[{'type':'function_call_output','call_id':fc['call_id'],'output':'alpha beta gamma delta'}]
    return b
def edit_reasoning(b,f):
    b['input']=[x for x in (f(i) if i.get('type')=='reasoning' else i for i in b['input']) if x is not None]; return b
drop=lambda i:None
def flip(s): return s[:40]+('A' if s[40]!='A' else 'B')+s[41:]
tamper=lambda i:{**i,'encrypted_content':flip(i['encrypted_content'])}
RIG={'type','id','summary','content','encrypted_content','signature','status'}
rig_rebuild=lambda i:{k:v for k,v in i.items() if k in RIG}
minimal=lambda i:{'type':'reasoning','id':i['id'],'summary':[],'encrypted_content':i['encrypted_content']}
no_id=lambda i:{k:v for k,v in i.items() if k!='id'}
no_enc=lambda i:{k:v for k,v in i.items() if k!='encrypted_content'}
def save(name): json.dump(RAW,open(f'raw-{name}.json','w'),indent=1)
which=sys.argv[1]
if which=='explore':
    n=uuid.uuid4().hex[:8]; r=call(turn1(n),'t1'); print(use(r))
    for i in r.get('output',[]): print(json.dumps({k:(v[:60]+'…' if isinstance(v,str) and len(v)>60 else v) for k,v in i.items()}))
    save('resp-explore')
if which=='main':
    for _ in range(4):
        n=uuid.uuid4().hex[:8]; b1=turn1(n); r1=call(b1,'t1'); print('t1',use(r1))
        if any(i['type']=='reasoning' for i in r1.get('output',[])) and any(i['type']=='function_call' for i in r1['output']): break
    print('reasoning item keys:',sorted(next(i for i in r1['output'] if i['type']=='reasoning')))
    b2=turn2(b1,r1); r2=None
    for name,f in [('unchanged (warm)',lambda b:b),('unchanged (read)',lambda b:b),('reasoning dropped',lambda b:edit_reasoning(b,drop)),
                   ('encrypted_content altered',lambda b:edit_reasoning(b,tamper)),('rig rebuild (no format)',lambda b:edit_reasoning(b,rig_rebuild)),
                   ('id + encrypted_content only',lambda b:edit_reasoning(b,minimal)),('no id',lambda b:edit_reasoning(b,no_id)),
                   ('no encrypted_content',lambda b:edit_reasoning(b,no_enc)),
                   ('Sonnet 5, unchanged',lambda b:(b.update(model=SON,provider={'only':['anthropic']}),b)[1]),
                   ('Sonnet 5, reasoning dropped',lambda b:(b.update(model=SON,provider={'only':['anthropic']}),edit_reasoning(b,drop))[1])]:
        r=call(f(copy.deepcopy(b2)),'t2 '+name); print(f'{"t2 "+name:36s}',use(r))
        if name=='unchanged (warm)': r2=r
        time.sleep(2)
    b3=copy.deepcopy(b2); b3['input']+=copy.deepcopy(r2['output'])+[{'role':'user','content':'Now say done.'}]
    for name,f in [('unchanged (warm)',lambda b:b),('unchanged (read)',lambda b:b),('all reasoning dropped',lambda b:edit_reasoning(b,drop))]:
        r=call(f(copy.deepcopy(b3)),'t3 '+name); print(f'{"t3 "+name:36s}',use(r)); time.sleep(2)
    save('resp-main')
if which=='include':
    for name,reason in [('effort none + include',{'effort':'none'}),('no reasoning key + include',None),('effort minimal + include',{'effort':'minimal'})]:
        n=uuid.uuid4().hex[:8]; b=turn1(n)
        if reason is None: b.pop('reasoning')
        else: b['reasoning']=reason
        r=call(b,name); print(f'{name:32s}',use(r))
        for i in r.get('output',[]):
            if i['type']=='reasoning': print('   reasoning keys',sorted(i),'enc' if i.get('encrypted_content') else 'no-enc')
    save('resp-include')
