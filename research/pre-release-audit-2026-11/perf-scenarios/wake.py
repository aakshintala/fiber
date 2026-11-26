import os,glob,time,collections
def snap():
    d={}
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        try:
            cmd=open(f'/proc/{p}/cmdline','rb').read().replace(b'\0',b' ').decode()
            if 'target/release/fiber' not in cmd or cmd.startswith('timeout'): continue
            kind=cmd.split()[1]
            for t in glob.glob(f'/proc/{p}/task/*'):
                name=open(t+'/comm').read().strip()
                sw=sum(int(l.split()[1]) for l in open(t+'/status') if 'ctxt_switches' in l)
                s=open(t+'/stat').read().rsplit(')',1)[1].split()
                d[(kind,p,t.split('/')[-1],name)]=(sw,int(s[11])+int(s[12]))
        except Exception: pass
    return d
a=snap(); time.sleep(10); b=snap()
agg=collections.defaultdict(lambda:[0,0,0])
for k in b:
    if k in a:
        e=agg[(k[0],k[3])]; e[0]+=1; e[1]+=b[k][0]-a[k][0]; e[2]+=b[k][1]-a[k][1]
print("kind     thread-name        threads  ctx-switches/10s  cpu-ticks/10s")
for (kind,name),(n,sw,cpu) in sorted(agg.items(),key=lambda x:(x[0][0],-x[1][1])): print(f"{kind:8} {name:18} {n:7} {sw:17} {cpu:14}")
