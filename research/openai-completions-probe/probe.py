"""Probes for #133. usage: probe.py openai|openrouter. Saves raw to raw/<vendor>-<name>.json. Spend-capped."""
import os
import json, sys, urllib.request, urllib.error, time
V = sys.argv[1]
CFG = {
 'openai': dict(url='https://api.openai.com/v1/chat/completions', key='OPENAI_API_KEY', model='gpt-6-luna', pin=0.1e-6, pout=0.5e-6, cap=0.5),
 'openrouter': dict(url='https://openrouter.ai/api/v1/chat/completions', key='OPENROUTER_API_KEY', model='z-ai/glm-5.3-flash', pin=0.15e-6, pout=0.5e-6, cap=1.0),
}[V]
KEY = os.environ[CFG['key']]
spend = 0.0; n = 0
TOOL = [{'type':'function','function':{'name':'get_weather','description':'Get weather','parameters':{'type':'object','properties':{'city':{'type':'string'}},'required':['city']}}}]
def call(name, body, stream=False):
    global spend, n
    import os
    if os.path.exists(f'raw/{V}-{name}.json') and json.load(open(f'raw/{V}-{name}.json')).get('status')==200: return json.load(open(f'raw/{V}-{name}.json'))
    n += 1
    assert n <= 40 and spend < CFG['cap'], 'cap'
    body = {'model': CFG['model'], **body}
    if stream: body['stream'] = True
    req = urllib.request.Request(CFG['url'], data=json.dumps(body).encode(), headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'})
    out = {'request': body, 'events': []}
    try:
        r = urllib.request.urlopen(req, timeout=120)
        out['status'] = r.status
        out['headers'] = {k: v for k, v in r.headers.items() if k.lower() in ('content-type','openai-processing-ms','x-request-id')}
        if stream:
            for line in r.read().decode().splitlines():
                if line.startswith('data: '):
                    d = line[6:]
                    out['events'].append(d if d == '[DONE]' else json.loads(d))
                elif line.strip():
                    out['events'].append('RAW:'+line)
        else:
            out['body'] = json.load(r)
    except urllib.error.HTTPError as e:
        out['status'] = e.code; out['body'] = e.read()[:1500].decode()
    us = [e['usage'] for e in out['events'] if isinstance(e, dict) and e.get('usage')] or ([out['body'].get('usage')] if isinstance(out.get('body'), dict) and out['body'].get('usage') else [])
    if us:
        u = us[-1]; spend += (u.get('prompt_tokens',0)*CFG['pin'] + u.get('completion_tokens',0)*CFG['pout'])
    else: spend += 0.0005
    json.dump(out, open(f'raw/{V}-{name}.json','w'), indent=1)
    print(name, out['status'], (out['body'] if 'body' in out and out['status']!=200 else ''), flush=True)
    time.sleep(1)
    return out
U = [{'role':'user','content':'Say ok.'}]
W = [{'role':'user','content':'What is the weather in Paris? Use the tool.'}]
def run(): pass
if V == 'openai':
    R = {'reasoning_effort':'none'}
    call('mt-max_tokens', {'messages':U,'max_tokens':50,**R})
    call('mt-max_completion_tokens', {'messages':U,'max_completion_tokens':50,**R})
    call('stream-include_usage', {'messages':U,'max_completion_tokens':50,'stream_options':{'include_usage':True},**R}, True)
    call('stream-no_include_usage', {'messages':U,'max_completion_tokens':50,**R}, True)
    call('stream-tool', {'messages':W,'tools':TOOL,'max_completion_tokens':100,'stream_options':{'include_usage':True},**R}, True)
    call('stream-length', {'messages':[{'role':'user','content':'Count from 1 to 100.'}],'max_completion_tokens':16,**R}, True)
    call('stream-reasoning-low', {'messages':[{'role':'user','content':'What is 17*23?'}],'max_completion_tokens':400,'reasoning_effort':'low','stream_options':{'include_usage':True}}, True)
    call('cache_control-on-openai', {'messages':[{'role':'user','content':[{'type':'text','text':'Say ok.','cache_control':{'type':'ephemeral'}}]}],'max_completion_tokens':50,**R})
else:
    R = {'reasoning':{'effort':'low'}}
    call('max_tokens', {'messages':U,'max_tokens':300,**R})
    call('max_completion_tokens', {'messages':U,'max_completion_tokens':300,**R})
    call('stream-include_usage', {'messages':U,'max_tokens':300,'stream_options':{'include_usage':True},**R}, True)
    call('stream-no_include_usage', {'messages':U,'max_tokens':300,**R}, True)
    call('stream-reasoning', {'messages':[{'role':'user','content':'What is 17*23?'}],'max_tokens':1500,'reasoning':{'effort':'medium'}}, True)
    t = call('stream-tool', {'messages':W,'tools':TOOL,'max_tokens':1500,'reasoning':{'effort':'low'}}, True)
    call('stream-length', {'messages':[{'role':'user','content':'Count from 1 to 100.'}],'max_tokens':300,'reasoning':{'effort':'low'}}, True)
    # replay: does the field change prompt_tokens?
    RS = 'The user greets me. ' * 60
    hist = lambda extra: U + [{'role':'assistant','content':'ok', **extra}, {'role':'user','content':'Again.'}]
    for nm, ex in [('none',{}),('reasoning',{'reasoning':RS}),('reasoning_content',{'reasoning_content':RS}),('reasoning_details',{'reasoning_details':[{'type':'reasoning.text','text':RS,'index':0,'format':'unknown'}]})]:
        call('replay-'+nm, {'messages':hist(ex),'max_tokens':600,'reasoning':{'effort':'low'}})
    S = {'role':'system','content':[{'type':'text','text':'You are terse.','cache_control':{'type':'ephemeral'}}]}
    cc = lambda ttl=None: {'type':'ephemeral', **({'ttl':ttl} if ttl else {})}
    call('cc-system', {'messages':[S]+U,'max_tokens':50,**R})
    call('cc-system-ttl1h', {'messages':[{**S,'content':[{**S['content'][0],'cache_control':cc('1h')}]}]+U,'max_tokens':50,**R})
    call('cc-system-ttl5m', {'messages':[{**S,'content':[{**S['content'][0],'cache_control':cc('5m')}]}]+U,'max_tokens':50,**R})
    call('cc-last-message', {'messages':[{'role':'user','content':[{'type':'text','text':'Say ok.','cache_control':cc()}]}],'max_tokens':50,**R})
    tl = json.loads(json.dumps(TOOL)); tl[0]['function']['cache_control'] = cc()
    call('cc-last-tool-in-function', {'messages':U,'tools':tl,'max_tokens':50,**R})
    tl2 = json.loads(json.dumps(TOOL)); tl2[0]['cache_control'] = cc()
    call('cc-last-tool-top', {'messages':U,'tools':tl2,'max_tokens':50,**R})
    call('cc-bogus-type', {'messages':[{'role':'user','content':[{'type':'text','text':'Say ok.','cache_control':{'type':'bogus'}}]}],'max_tokens':50,**R})
print('spend est', round(spend,4), 'requests', n)
if V == 'openrouter' and 'pinned' in sys.argv:
    RS = 'The user greets me. ' * 60
    P = {'provider':{'only':['OpenInference'],'allow_fallbacks':False}}
    for nm, ex in [('none',{}),('reasoning',{'reasoning':RS}),('reasoning_content',{'reasoning_content':RS}),('reasoning_details',{'reasoning_details':[{'type':'reasoning.text','text':RS,'index':0,'format':'unknown'}]})]:
        call('pinned-replay-'+nm, {'messages':U+[{'role':'assistant','content':'ok',**ex},{'role':'user','content':'Again.'}],'max_tokens':600,'reasoning':{'effort':'low'},**P})
    call('effort-none-400', {'messages':U,'max_tokens':50,'reasoning':{'effort':'none'}})
    print('spend est', round(spend,4), 'requests', n)
