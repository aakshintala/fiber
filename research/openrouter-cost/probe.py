# Probe: OpenRouter inline usage.cost vs GET /generation, stream vs not. Model z-ai/glm-5.3-flash.
import json, urllib.request, urllib.error, time, threading, http.client
KEY=open('/tmp/openrouter-key').read().strip()
H={'Authorization':'Bearer '+KEY,'Content-Type':'application/json'}
M='z-ai/glm-5.3-flash'
DELAYS=[0,1,2,5,10,30,60,120]
FILLER=' '.join(f'Rule {i}: keep the workspace tidy and report each file you change.' for i in range(150))
TOOL=[{'type':'function','function':{'name':'read_file','description':'Read a file.','parameters':{'type':'object','properties':{'path':{'type':'string'}},'required':['path']}}}]
def body(kind, stream, nonce):
    sysm=(f'[{nonce}] ' if kind!='cache' else '[fixed-prefix-97] ')+'You are a test harness.\n'+FILLER
    b={'model':M,'max_tokens':64,'stream':stream,'messages':[{'role':'system','content':sysm},{'role':'user','content':'Say ok.' if kind!='tool' else 'Read the file a.txt.'}]}
    if kind=='tool': b['tools']=TOOL; b['tool_choice']='required'
    if kind=='long': b['messages'][1]['content']='Count from 1 to 200, separated by spaces.'; b['max_tokens']=600
    return b
def post(b):
    req=urllib.request.Request('https://openrouter.ai/api/v1/chat/completions',data=json.dumps(b).encode(),headers=H)
    return urllib.request.urlopen(req,timeout=180)
def lookup(gid):
    req=urllib.request.Request('https://openrouter.ai/api/v1/generation?id='+gid,headers=H)
    try: r=urllib.request.urlopen(req,timeout=60); return r.status,json.load(r)
    except urllib.error.HTTPError as e: return e.code,e.read()[:300].decode()
results=[]
def run(i,kind,stream,abort=False):
    rec={'n':i,'kind':kind,'stream':stream,'abort':abort}
    t0=time.time()
    try:
        r=post(body(kind,stream,f'n{i}-{int(t0)}'))
        if not stream:
            d=json.load(r); rec['response']=d; rec['gid']=d.get('id'); rec['inline_usage']=d.get('usage')
        else:
            chunks=[]; 
            for line in r:
                line=line.decode().strip()
                if not line.startswith('data:'): continue
                p=line[5:].strip()
                if p=='[DONE]': chunks.append('[DONE]'); break
                c=json.loads(p); chunks.append(c)
                rec.setdefault('gid',c.get('id'))
                if abort and len([x for x in chunks if isinstance(x,dict) and x.get('choices') and x['choices'][0].get('delta',{}).get('content')])>=3:
                    r.close(); rec['aborted_after_chunks']=len(chunks); break
            rec['chunks']=chunks
            u=[c for c in chunks if isinstance(c,dict) and c.get('usage')]
            rec['inline_usage']=u[-1]['usage'] if u else None
            rec['final_chunk_has_usage']=bool(u) and u[-1] is chunks[-2 if chunks[-1]=='[DONE]' else -1]
    except Exception as e: rec['error']=repr(e)[:300]
    rec['done_at']=time.time(); t1=time.time(); rec['polls']=[]
    if rec.get('gid'):
        for d in DELAYS:
            w=t1+d-time.time()
            if w>0: time.sleep(w)
            s,b=lookup(rec['gid']); rec['polls'].append({'t_after_response':round(time.time()-t1,2),'status':s,'body':b})
    results.append(rec)
jobs=[]
plan=[]
for i in range(20):
    stream=i%2==1
    kind=['plain','tool','cache','plain','long'][i%5] if i%5!=2 else 'cache'
    plan.append((i,kind,stream))
plan.append((20,'long',True,True))  # aborted stream
ths=[]
for p in plan:
    t=threading.Thread(target=run,args=p); t.start(); ths.append(t); time.sleep(1.5)
    if p[2] is False and p[1]=='cache': time.sleep(4)
for t in ths: t.join()
results.sort(key=lambda r:r['n'])
json.dump(results,open('research/openrouter-cost/raw/results.json','w'),indent=1)
