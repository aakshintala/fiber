import os
# Probe: does OpenAI Responses deferral (defer_loading + client tool_search) work
# through OpenRouter on GPT-6 Luna, what does a deferred tool cost, and does
# loading one keep the cache? Usage: python3 probe_defer_responses.py
import json, urllib.request, time, uuid, copy
KEY=os.environ['OPENROUTER_API_KEY']
URL='https://openrouter.ai/api/v1/responses'
MODEL='openai/gpt-6-luna'
def call(body):
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    try: r=json.load(urllib.request.urlopen(req,timeout=180))
    except urllib.error.HTTPError as e: return {'error':e.code,'body':e.read()[:600].decode()}
    return r
def use(r):
    if 'error' in r and r['error']: return f"ERR {r.get('error')} {r.get('body','')}"
    u=r.get('usage',{}); d=u.get('input_tokens_details') or {}
    return f"in={u.get('input_tokens')} cached={d.get('cached_tokens')}"
FILLER=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change with its path and reason.' for i in range(230))
def fn(n, defer=False):
    t={'type':'function','name':n,'description':f'The {n} tool. '+('Does a thing carefully. '*20),
       'parameters':{'type':'object','properties':{'path':{'type':'string'},'limit':{'type':'integer'}},'required':['path'],'additionalProperties':False},'strict':False}
    if defer: t['defer_loading']=True
    return t
DIRECT=[fn(n) for n in ['alpha','bravo','charlie','delta','echo']]
CAL=['calendar_create_event','calendar_list_events','calendar_delete_event']
def cal_ns(defer):
    return {'type':'namespace','name':'calendar','description':'Calendar tools: create, list and delete events.',
            'tools':[fn(n,defer) for n in CAL]}
SEARCH={'type':'tool_search','execution':'client','description':'Searches deferred tools. Use it to find tools not provided upfront.',
        'parameters':{'type':'object','properties':{'query':{'type':'string'}},'required':['query'],'additionalProperties':False}}
def body(nonce, tools, user='Create a calendar event titled standup at /tmp/cal. Use the right tool.'):
    return {'model':MODEL,'instructions':f'[{nonce}] You are a test harness.\n'+FILLER,'tools':tools,
            'input':[{'role':'user','content':user}],'prompt_cache_key':nonce,'max_output_tokens':400,
            'reasoning':{'effort':'low'},'store':False}
n=uuid.uuid4().hex[:12]
print('no tools        ', use(call(body(n+'a',[]))))
print('direct only     ', use(call(body(n+'b',DIRECT))))
print('direct+cal full ', use(call(body(n+'c',DIRECT+[cal_ns(False)]))))
b=body(n,DIRECT+[cal_ns(True),SEARCH])
r1=call(b); print('deferred+search ', use(r1))
time.sleep(3); r2=call(b); print('same again      ', use(r2))
out=[i for i in r2.get('output',[]) if i.get('type')!='reasoning']
print('output items:', json.dumps(out)[:900])
sc=[i for i in r2.get('output',[]) if i.get('type')=='tool_search_call']
if sc:
    c=sc[0]; f=copy.deepcopy(b)
    f['input']+= [ {k:v for k,v in c.items() if k in('type','call_id','execution','arguments','status')},
        {'type':'tool_search_output','call_id':c['call_id'],'status':'completed','execution':'client','tools':[cal_ns(True)]}]
    time.sleep(3); r3=call(f); print('after load      ', use(r3))
    print('r3 output:', json.dumps([i for i in r3.get('output',[]) if i.get('type')!='reasoning'])[:600])
