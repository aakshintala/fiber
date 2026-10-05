import os
import json, urllib.request, uuid
KEY=open(os.path.expanduser('~/.config/probe-keys/openrouter-key')).read().strip()
def call(url, body):
    req=urllib.request.Request(url,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+KEY,'Content-Type':'application/json','anthropic-version':'2023-06-01'})
    try: return json.load(urllib.request.urlopen(req,timeout=180))
    except urllib.error.HTTPError as e: return {'error':e.code,'body':e.read()[:400].decode()}
FILL=' '.join(f'Rule {i}: keep the workspace tidy.' for i in range(200))
def fn(n,defer=False):
    t={'name':n,'description':f'The {n} tool. '+('Does a thing carefully. '*20),'input_schema':{'type':'object','properties':{'path':{'type':'string'}},'required':['path']}}
    if defer: t['defer_loading']=True
    return t
names=['alpha','bravo','calendar_create_event','calendar_list_events','calendar_delete_event']
S={'type':'tool_search_tool_regex_20251119','name':'tool_search_tool_regex'}
for label,tools in [('full',[fn(n) for n in names]),('deferred',[fn(n) for n in names[:2]]+[fn(n,True) for n in names[2:]]+[S])]:
    n=uuid.uuid4().hex[:8]
    r=call('https://openrouter.ai/api/v1/messages',{'model':'anthropic/claude-sonnet-5','max_tokens':300,'system':f'[{n}] '+FILL,'tools':tools,
        'messages':[{'role':'user','content':'Create a calendar event at /tmp/cal.'}]})
    print(label, r.get('usage') or r, [c.get('type')+':'+str(c.get('name','')) for c in r.get('content',[])] if 'content' in r else '')
