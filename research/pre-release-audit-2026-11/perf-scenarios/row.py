import os,sys,glob,time
root=int(sys.argv[1]); label=sys.argv[2]
def snap():
    pp={}
    for p in os.listdir('/proc'):
        if p.isdigit():
            try: pp[int(p)]=int(open(f'/proc/{p}/stat').read().rsplit(')',1)[1].split()[1])
            except: pass
    ds={root}; ch=True
    while ch:
        ch=False
        for p,q in pp.items():
            if q in ds and p not in ds: ds.add(p); ch=True
    r=dict(n=0,rss=0,pss=0,priv=0,thr=0,cpu=0,sw=0,kinds={})
    for p in ds:
        try:
            if os.path.basename(os.readlink(f'/proc/{p}/exe'))!='fiber': continue
            cmd=open(f'/proc/{p}/cmdline','rb').read().split(b'\0')[1].decode()
            sm=dict(l.split(':',1) for l in open(f'/proc/{p}/smaps_rollup') if ':' in l)
            k=lambda n:int(sm.get(n,'0 kB').split()[0])
            s=open(f'/proc/{p}/stat').read().rsplit(')',1)[1].split()
            r['n']+=1; r['rss']+=k('Rss'); r['pss']+=k('Pss'); r['priv']+=k('Private_Clean')+k('Private_Dirty'); r['thr']+=int(s[17]); r['cpu']+=int(s[11])+int(s[12])
            for t in glob.glob(f'/proc/{p}/task/*/status'): r['sw']+=sum(int(l.split()[1]) for l in open(t) if 'ctxt_switches' in l)
            r['kinds'][cmd]=r['kinds'].get(cmd,0)+1
        except Exception: pass
    return r
a=snap(); time.sleep(10); b=snap()
print(f"{label:22} procs={b['n']:3} {str(b['kinds']):44} sumRSS={b['rss']:7} PSS={b['pss']:6} private={b['priv']:6} threads={b['thr']:4} wakeups/s={(b['sw']-a['sw'])/10:6.1f} cpu_ms/s={(b['cpu']-a['cpu'])*10/10:5.1f}")
