import os,sys,re,time,json
home=sys.argv[1]
def procs():
    out=[]
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        try:
            env=open(f'/proc/{p}/environ','rb').read()
            if home.encode() not in env: continue
            cmd=open(f'/proc/{p}/cmdline','rb').read().replace(b'\0',b' ').decode()[:70]
            exe=os.path.basename(os.readlink(f'/proc/{p}/exe'))
            st=dict(l.split(':',1) for l in open(f'/proc/{p}/status') if ':' in l)
            sm=dict(l.split(':',1) for l in open(f'/proc/{p}/smaps_rollup') if ':' in l)
            k=lambda d,n:int(d.get(n,'0 kB').split()[0])
            s=open(f'/proc/{p}/stat').read().rsplit(')',1)[1].split()
            out.append(dict(pid=int(p),ppid=int(s[1]),exe=exe,cmd=cmd,rss=k(st,'VmRSS'),hwm=k(st,'VmHWM'),anon=k(st,'RssAnon'),file=k(st,'RssFile'),pss=k(sm,'Pss'),priv=k(sm,'Private_Clean')+k(sm,'Private_Dirty'),thr=int(st['Threads']),cpu=(int(s[11])+int(s[12]))/os.sysconf('SC_CLK_TCK')))
        except Exception as e: pass
    return out
ps=procs()
def kind(p):
    c=p['cmd']
    if p['exe']!='fiber' and 'fiber' not in p['exe'] and p['exe'] not in('claude','node'): return 'other:'+p['exe']
    for k in ('keeper','hub','session','ask','resume'):
        if re.search(r'\b'+k+r'\b',c): return k
    return p['exe']
agg={}
for p in ps:
    a=agg.setdefault(kind(p),dict(n=0,rss=0,pss=0,anon=0,priv=0,thr=0,cpu=0,hwm=0))
    a['n']+=1
    for f in ('rss','pss','anon','priv','thr','cpu','hwm'): a[f]+=p[f]
print(f"{'kind':14}{'n':>3}{'RSS':>9}{'peakRSS':>9}{'PSS':>9}{'Private':>9}{'Anon':>9}{'thr':>5}{'cpu_s':>7}")
tot=dict(n=0,rss=0,pss=0,anon=0,priv=0,thr=0,cpu=0,hwm=0)
for k,a in sorted(agg.items(),key=lambda x:-x[1]['pss']):
    print(f"{k:14}{a['n']:>3}{a['rss']:>9}{a['hwm']:>9}{a['pss']:>9}{a['priv']:>9}{a['anon']:>9}{a['thr']:>5}{a['cpu']:>7.2f}")
    for f in tot: tot[f]+=a[f]
print(f"{'TOTAL':14}{tot['n']:>3}{tot['rss']:>9}{tot['hwm']:>9}{tot['pss']:>9}{tot['priv']:>9}{tot['anon']:>9}{tot['thr']:>5}{tot['cpu']:>7.2f}   (KiB)")
if '-v' in sys.argv:
    for p in sorted(ps,key=lambda p:p['pid']): print(p['pid'],p['ppid'],p['rss'],p['pss'],p['thr'],p['cmd'])
