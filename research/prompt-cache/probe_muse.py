import os
import json, urllib.request, time, uuid, copy
KEY=open(os.path.expanduser('~/.config/probe-keys/muse-key')).read().strip()
URL='https://api.meta.ai/v1/responses'
M1,M2='muse-spark-1.3-contributor','muse-spark-1.2-contributor'
def call(body):
    req=urllib.request.Request(URL,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    try: r=json.load(urllib.request.urlopen(req,timeout=120))
    except urllib.error.HTTPError as e: return {'error':e.code,'body':e.read()[:300].decode()}
    u=r.get('usage',{}); return {'in':u.get('input_tokens'),'cached':(u.get('input_tokens_details') or {}).get('cached_tokens'),'raw_usage':u}
FILLER=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change with its path and reason.' for i in range(160))
def tools():
    return [{'type':'function','name':n,'description':f'The {n} tool. '+('Does a thing carefully. '*20),
             'parameters':{'type':'object','properties':{'path':{'type':'string'},'limit':{'type':'integer'}},'required':['path']}}
            for n in ['alpha','bravo','charlie','delta','echo']]
def base(nonce, model=M1):
    return {'model':model,'instructions':f'[{nonce}] You are a test harness.\n'+FILLER,'tools':tools(),
            'input':[{'role':'user','content':f'[{nonce}] Say ok.'}],'max_output_tokens':64,'store':False,'reasoning':{'effort':'low'}}
def variant(name, mutate, model2=None):
    n=uuid.uuid4().hex[:12]; b=base(n)
    w=call(b); time.sleep(4); c=call(b); time.sleep(4)
    v=copy.deepcopy(b); mutate(v); r=call(v)
    print(f'{name:34s} warm={w.get("cached")}/{w.get("in")} control={c.get("cached")}/{c.get("in")} variant={r.get("cached")}/{r.get("in")} {r.get("error","")} {r.get("body","")}',flush=True)
def rev_tools(v): v['tools']=v['tools'][::-1]
def swap_keys(v):
    t=v['tools'][0]; t['parameters']={'required':['path'],'properties':{'limit':{'type':'integer'},'path':{'type':'string'}},'type':'object'}
def append(v): v['input']+= [{'role':'assistant','content':'ok'},{'role':'user','content':'Again.'}]
def edit_head(v): v['instructions']=v['instructions'].replace('test harness','test rig')
def edit_tail(v): v['instructions']+=' One more rule.'
def model(v): v['model']=M2
def tool_choice(v): v['tool_choice']='none'
def effort(v): v['reasoning']={'effort':'high'}
def cache_key(v): v['prompt_cache_key']='other-'+uuid.uuid4().hex
def add_tool(v): v['tools'].append({'type':'function','name':'zulu','description':'z','parameters':{'type':'object','properties':{}}})
print(json.dumps(call(base('smoke'))))
for name,f in [('identical (no change)',lambda v:None),('append messages',append),('tools reversed',rev_tools),
               ('one schema keys reordered',swap_keys),('tool added at end',add_tool),('edit start of instructions',edit_head),
               ('edit end of instructions',edit_tail),('tool_choice none',tool_choice),('reasoning effort high',effort),
               ('different prompt_cache_key',cache_key),('switch model 1.3c -> 1.2c',model)]:
    variant(name,f)
