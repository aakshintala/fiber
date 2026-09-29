# Probe #132: Anthropic Messages facts pi and rig disagree on. Usage: python3 probe.py stream|beta|choice|caller|cache
import json, sys, copy, urllib.request, urllib.error, http.client
KEY=open('/tmp/anthropic-key').read().strip()
BASE='https://api.anthropic.com/v1/messages'
M='claude-sonnet-5-5'; PIN,POUT=2/1e6,10/1e6; CAP=1.00
spend=0.0; RAW=[]
def hdrs(h): return {k:v for k,v in h.items() if k.lower() not in ('anthropic-organization-id','anthropic-workspace-id','set-cookie','x-api-key','authorization','cf-ray','traceresponse')}
def track(u):
    global spend
    u=u or {}
    spend+=((u.get('input_tokens',0)+u.get('cache_creation_input_tokens',0)*1.25)*PIN+u.get('cache_read_input_tokens',0)*PIN*.1+u.get('output_tokens',0)*POUT)
    assert spend<CAP, 'cap'
def req(body, label, url=BASE, stream=False, extra=None):
    assert spend<CAP-0.05
    h={'x-api-key':KEY,'anthropic-version':'2023-06-01','content-type':'application/json'}; h.update(extra or {})
    if stream: body={**body,'stream':True}
    r=urllib.request.Request(url,data=json.dumps(body).encode(),headers=h)
    try: resp=urllib.request.urlopen(r,timeout=120); st=resp.status
    except urllib.error.HTTPError as e: resp=e; st=e.code
    out={'label':label,'url':url,'status':st,'headers':hdrs(dict(resp.headers)),'request':body}
    raw=resp.read().decode()
    if stream and st==200:
        ev=[]; cur=None
        for line in raw.split('\n'):
            if line.startswith('event:'): cur=line[6:].strip()
            elif line.startswith('data:'):
                d=json.loads(line[5:]); ev.append((cur,d))
                if cur=='message_start': track(d['message'].get('usage'))
                if cur=='message_delta': track({'output_tokens':d['usage'].get('output_tokens',0)})
        out['events']=[{'event':e,'type':d.get('type'),**({'stop_reason':d['delta'].get('stop_reason'),'stop_sequence':d['delta'].get('stop_sequence')} if e=='message_delta' else {}),**({'block':d['content_block']} if e=='content_block_start' else {})} for e,d in ev]
        out['order']=[e for e,_ in ev]; out['raw_sse']=raw
    else:
        try: out['body']=json.loads(raw)
        except Exception: out['body']=raw
        if isinstance(out['body'],dict) and 'usage' in out['body']: track(out['body']['usage'])
    RAW.append(out); return out
TOOL={'name':'get_weather','description':'Get the weather for a city.','input_schema':{'type':'object','properties':{'city':{'type':'string'}},'required':['city']}}
def B(text,**kw): return {'model':M,'max_tokens':kw.pop('max_tokens',64),'messages':[{'role':'user','content':text}],**kw}
def save(n):
    json.dump(RAW,open(f'raw/{n}.json','w'),indent=1); print('spend so far ~$%.4f'%spend)
w=sys.argv[1]
if w=='stream':
    cases=[('plain text',B('Say hi in three words.')),
     ('tool use',B('What is the weather in Paris? Use the tool.',tools=[TOOL],max_tokens=200)),
     ('max_tokens',B('Count from 1 to 50.',max_tokens=5)),
     ('stop_sequence',B('Count from 1 to 10 separated by spaces.',stop_sequences=['5'])),
     ('thinking on',B('What is 17*23?',thinking={'type':'adaptive'},output_config={'effort':'medium'},max_tokens=2000)),
     ('thinking + tool',B('What is the weather in Paris? Use the tool.',thinking={'type':'adaptive'},output_config={'effort':'medium'},tools=[TOOL],max_tokens=2000)),
     ('invalid request (400)',B('x',max_tokens=-1)),
     ('thinking forced by hard question',B('How many primes are below 300? Work it out carefully.',thinking={'type':'adaptive'},output_config={'effort':'high'},max_tokens=3000))]
    for n,b in cases:
        o=req(b,'stream '+n,stream=True); print(n,o['status'],o.get('order') and [x for x in o['order'] if x not in('content_block_delta',)],[ (e.get('stop_reason')) for e in o.get('events',[]) if e['event']=='message_delta'])
    save('stream')
if w=='beta':
    b=B('Say hi in three words.')
    b2=B('What is the weather in Paris? Use the tool.',tools=[TOOL],max_tokens=200)
    for n,bb in (('plain',b),('tool',b2)):
        for u in (BASE,BASE+'?beta=true'):
            o=req(bb,f'beta {n} {u[-10:]}',url=u); print(n,u[-10:],o['status'],sorted(o['body'].keys()) if isinstance(o['body'],dict) else o['body'], [c.get('caller') for c in o['body'].get('content',[])] )
    for u in (BASE,BASE+'?beta=true'):
        o=req(b2,'beta stream '+u[-10:],url=u,stream=True); print('stream',u[-10:],o['status'],o.get('order') and sorted(set(o['order'])))
    o=req(b,'beta bad value',url=BASE+'?beta=false'); print('beta=false',o['status'])
    save('beta')
if w=='choice':
    for tc in ({'type':'auto'},{'type':'none'},{'type':'any'},{'type':'tool','name':'get_weather'}):
        o=req(B('Say hi.',tool_choice=tc),'tool_choice no tools '+tc['type']); print(tc['type'],o['status'],json.dumps(o['body'])[:300])
    o=req(B('Say hi.',tools=[],tool_choice={'type':'auto'}),'tool_choice with tools=[]'); print('tools=[] auto',o['status'],json.dumps(o['body'])[:300])
    save('choice')
if w=='caller':
    b=B('What is the weather in Paris? Use the tool.',tools=[TOOL],max_tokens=200)
    r=req(b,'caller t1'); print(r['status'],json.dumps(r['body']['content']))
    tu=[c for c in r['body']['content'] if c['type']=='tool_use'][0]
    def t2(block,label):
        m=b['messages']+[{'role':'assistant','content':[block]},{'role':'user','content':[{'type':'tool_result','tool_use_id':tu['id'],'content':'sunny'}]}]
        o=req({**b,'messages':m},label); print(label,o['status'],json.dumps(o['body'])[:200])
    t2(tu,'replay with caller as received')
    t2({k:v for k,v in tu.items() if k!='caller'},'replay without caller')
    t2({**tu,'caller':{'type':'bogus'}},'replay caller type bogus')
    t2({**tu,'caller':{'type':'code_execution_20250825','tool_id':'srvtoolu_x'}},'replay caller code_execution')
    # try to trigger a non-direct caller: programmatic tool calling
    CE={'type':'code_execution_20250825','name':'code_execution'}
    T2={**TOOL,'allowed_callers':['code_execution_20250825']}
    o=req({'model':M,'max_tokens':1500,'tools':[CE,T2],'messages':[{'role':'user','content':'Use code execution to call get_weather for Paris and Rome and print both.'}]},'programmatic tool calling attempt',extra={'anthropic-beta':'advanced-tool-use-2025-11-20'})
    print('ptc',o['status'],json.dumps(o['body'])[:1200])
    save('caller')
if w=='cache':
    F=' '.join(f'Filler sentence number {i} to make this block long enough.' for i in range(1)) 
    def blk(i): return {'type':'text','text':f'Block {i}. '+F,'cache_control':{'type':'ephemeral'}}
    for n in (4,5):
        b=B('x',max_tokens=8); b['system']=[blk(i) for i in range(n-1)]; b['messages']=[{'role':'user','content':[{'type':'text','text':'hi','cache_control':{'type':'ephemeral'}}]}]
        o=req(b,f'{n} cache_control markers'); print(n,o['status'],json.dumps(o['body'])[:300])
    b=B('x',max_tokens=8); b['system']=[blk(i) for i in range(4)]; b['messages']=[{'role':'user','content':[{'type':'text','text':'hi','cache_control':{'type':'ephemeral'}}]}]
    b['tools']=[{**TOOL,'cache_control':{'type':'ephemeral'}}]
    o=req(b,'6 markers (tools + 4 system + 1 message... =6)'); print(6,o['status'],json.dumps(o['body'])[:300])
    save('cache')
if w=='ptc':
    CE={'type':'code_execution_20250825','name':'code_execution'}
    T2={**TOOL,'allowed_callers':['code_execution_20250825']}
    x={'anthropic-beta':'advanced-tool-use-2025-11-20'}
    b={'model':M,'max_tokens':1500,'tools':[CE,T2],'messages':[{'role':'user','content':'Use code execution to call get_weather for Paris and Rome and print both.'}]}
    r=req(b,'ptc t1',extra=x); c=r['body']['content']; print([ (k['type'],k.get('caller')) for k in c])
    tus=[k for k in c if k['type']=='tool_use']
    def t2(content,label,ctr=True):
        m=b['messages']+[{'role':'assistant','content':content},{'role':'user','content':[{'type':'tool_result','tool_use_id':t['id'],'content':'sunny'} for t in tus]}]
        bb={**b,'messages':m}
        if ctr: bb['container']=r['body']['container']['id']
        o=req(bb,label,extra=x); print(label,o['status'],json.dumps(o['body'])[:400])
    t2(c,'ptc replay with caller')
    t2([{k:v for k,v in blk.items() if k!='caller'} for blk in c],'ptc replay without caller')
    o=req({**b,'messages':[{'role':'user','content':'Say hi.'}],'stream':False},'x') if False else None
    # streaming shape of caller
    o=req({**B('What is the weather in Paris? Use the tool.',tools=[TOOL],max_tokens=200)},'stream tool caller',stream=True)
    print([e['block'] for e in o['events'] if e['event']=='content_block_start'])
    save('ptc')
if w=='thinking-enabled':
    o=req(B('What is 17*23?',thinking={'type':'enabled','budget_tokens':1024},max_tokens=2000),'thinking enabled'); print(o['status'],json.dumps(o['body']))
    json.dump(RAW,open('raw/thinking-enabled.json','w'),indent=1)
