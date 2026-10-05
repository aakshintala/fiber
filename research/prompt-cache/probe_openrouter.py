import os
import json, urllib.request, time, uuid, copy, sys
KEY=open(os.path.expanduser('~/.config/probe-keys/openrouter-key')).read().strip()
URL='https://openrouter.ai/api/v1/chat/completions'
SON,HAI,GPT='anthropic/claude-sonnet-5','anthropic/claude-haiku-4.5','openai/gpt-6-luna'
def call(body):
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    try: r=json.load(urllib.request.urlopen(req,timeout=180))
    except urllib.error.HTTPError as e: return {'error':e.code,'body':e.read()[:300].decode()}
    if 'error' in r: return {'error':'x','body':str(r['error'])[:300]}
    u=r.get('usage',{}); d=u.get('prompt_tokens_details') or {}
    return {'in':u.get('prompt_tokens'),'read':d.get('cached_tokens'),'write':d.get('cache_write_tokens'),'prov':r.get('provider'),'u':u}
FILLER=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change with its path and reason.' for i in range(230))
def tools():
    return [{'type':'function','function':{'name':n,'description':f'The {n} tool. '+('Does a thing carefully. '*20),
             'parameters':{'type':'object','properties':{'path':{'type':'string'},'limit':{'type':'integer'}},'required':['path']}}}
            for n in ['alpha','bravo','charlie','delta','echo']]
def cc(text, ttl=None):
    c={'type':'ephemeral'}; 
    if ttl: c['ttl']=ttl
    return [{'type':'text','text':text,'cache_control':c}]
def base(nonce, model, anth=True):
    sysm=f'[{nonce}] You are a test harness.\n'+FILLER
    b={'model':model,'tools':tools(),'max_tokens':32,'session_id':nonce,
       'messages':[{'role':'system','content':cc(sysm) if anth else sysm},{'role':'user','content':cc(f'[{nonce}] Say ok.') if anth else f'[{nonce}] Say ok.'}]}
    if anth: b['provider']={'only':['anthropic']}
    else: b['prompt_cache_key']=nonce
    return b
def strip_cc(msgs):
    for m in msgs:
        if isinstance(m['content'],list):
            for p in m['content']: p.pop('cache_control',None)
def mark_last(msgs, ttl=None):
    m=msgs[-1]
    if isinstance(m['content'],str): m['content']=cc(m['content'],ttl)
    else: m['content'][-1]['cache_control']={'type':'ephemeral',**({'ttl':ttl} if ttl else {})}
def row(name,*rs):
    print(f'{name:40s} '+'  '.join(f'{k}=r{r.get("read")}/w{r.get("write")}/in{r.get("in")}{" ERR "+str(r.get("error"))+" "+r.get("body","") if r.get("error") else ""}' for k,r in rs),flush=True)
def variant(name, mutate, model, anth=True, pre=None):
    n=uuid.uuid4().hex[:12]; b=base(n,model,anth)
    if pre: pre(b)
    w=call(b); time.sleep(3); c=call(b); time.sleep(3)
    v=copy.deepcopy(b); mutate(v); r=call(v)
    row(name,('warm',w),('ctl',c),('var',r))
def rev_tools(v): v['tools']=v['tools'][::-1]
def swap_keys(v): v['tools'][0]['function']['parameters']={'required':['path'],'properties':{'limit':{'type':'integer'},'path':{'type':'string'}},'type':'object'}
def add_tool(v): v['tools'].append({'type':'function','function':{'name':'zulu','description':'z','parameters':{'type':'object','properties':{}}}})
def append(v,anth=True):
    strip_cc(v['messages'][1:]) if anth else None
    v['messages']+=[{'role':'assistant','content':'ok'},{'role':'user','content':'Again.'}]
    if anth: mark_last(v['messages'])
def edit_head(v):
    s=v['messages'][0]; 
    if isinstance(s['content'],list): s['content'][0]['text']=s['content'][0]['text'].replace('test harness','test rig')
    else: s['content']=s['content'].replace('test harness','test rig')
def edit_tail(v):
    s=v['messages'][0]
    if isinstance(s['content'],list): s['content'][0]['text']+=' One more rule.'
    else: s['content']+=' One more rule.'
def parallel(k, anth=True):
    def f(v):
        if anth: strip_cc(v['messages'][1:])
        calls=[{'id':f'call_{i}','type':'function','function':{'name':'alpha','arguments':json.dumps({'path':f'f{i}.txt'})}} for i in range(k)]
        v['messages'].append({'role':'assistant','content':None,'tool_calls':calls})
        for i in range(k): v['messages'].append({'role':'tool','tool_call_id':f'call_{i}','content':f'contents of f{i}'})
        if anth: mark_last(v['messages'])
    return f
def turns(k):
    def f(v):
        strip_cc(v['messages'][1:])
        for i in range(k): v['messages']+=[{'role':'assistant','content':f'ok {i}'},{'role':'user','content':f'next {i}'}]
        mark_last(v['messages'])
    return f
def to_model(m): return lambda v: v.update(model=m)
def tc_required(v): v['tool_choice']='required'
def tc_named(v): v['tool_choice']={'type':'function','function':{'name':'alpha'}}
def reasoning(v): v['reasoning']={'effort':'low'}; v['max_tokens']=2048
def key(v): v['prompt_cache_key']='other-'+uuid.uuid4().hex
which=sys.argv[1] if __name__=="__main__" else ""
if which=='anth':
    for name,f in [('identical',lambda v:None),('append 1 turn',append),('tools reversed',rev_tools),('schema keys reordered',swap_keys),
                   ('tool added at end',add_tool),('edit start of system',edit_head),('edit end of system',edit_tail),
                   ('tool_choice required',tc_required),('tool_choice named',tc_named),('reasoning on',reasoning),
                   ('15 parallel calls, end breakpoint only',parallel(15)),('12 turns appended, end breakpoint only',turns(12)),
                   ('switch Sonnet 5 -> Haiku 4.5',to_model(HAI))]:
        variant(name,f,SON)
elif which=='ttl':
    # 1h breakpoint at end of a prefix written at 5m
    n=uuid.uuid4().hex[:12]; b=base(n,SON); w=call(b); time.sleep(3)
    v=copy.deepcopy(b); strip_cc(v['messages'][1:]); v['messages']+=[{'role':'assistant','content':'ok'},{'role':'user','content':'Again.'}]; mark_last(v['messages'],'1h')
    r=call(v); row('5m prefix then 1h end breakpoint',('warm5m',w),('1h',r)); print(json.dumps(r['u']))
elif which=='gpt':
    for name,f in [('identical',lambda v:None),('append 1 turn',lambda v:append(v,False)),('tools reversed',rev_tools),('schema keys reordered',swap_keys),
                   ('tool added at end',add_tool),('edit start of system',edit_head),('edit end of system',edit_tail),
                   ('tool_choice required',tc_required),('reasoning low',reasoning),('different prompt_cache_key',key),
                   ('15 parallel calls appended',parallel(15,False))]:
        variant(name,f,GPT,anth=False)
