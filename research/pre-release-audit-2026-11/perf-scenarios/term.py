import os,sys,pty,time,glob,threading,select,json,struct,fcntl,termios,subprocess
# usage: term.py <label> <n_terminals> <script.json> <settle_s> [prompt]
label,n,script,settle=sys.argv[1],int(sys.argv[2]),sys.argv[3],float(sys.argv[4])
prompt=sys.argv[5] if len(sys.argv)>5 else 'hi'
B='/home/user/fiber/target/release/fiber'
fds=[];pids=[];outb=[0]
for i in range(n):
    pid,fd=pty.fork()
    if pid==0:
        os.execv(B,[B,'--model','scripted/'+script])
    fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',50,160,0,0))
    fds.append(fd);pids.append(pid)
def drain():
    while True:
        r,_,_=select.select(fds,[],[],0.5)
        for f in r:
            try:
                d=os.read(f,65536); outb[0]+=len(d)
                # answer terminal queries minimally
                if b'\x1b[6n' in d: os.write(f,b'\x1b[1;1R')
                if b'\x1b[c' in d: os.write(f,b'\x1b[?62;c')
            except OSError: pass
threading.Thread(target=drain,daemon=True).start()
time.sleep(4)
for f in fds:
    os.write(f,b'\x1b'); time.sleep(1.0)
    os.write(f,prompt.encode()); time.sleep(0.2); os.write(f,b'\r')
time.sleep(settle)
def snap():
    r={}
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        try:
            if os.path.basename(os.readlink(f'/proc/{p}/exe'))!='fiber': continue
            c=open(f'/proc/{p}/cmdline','rb').read().split(b'\0')
            kind=c[1].decode() if len(c)>1 and c[1] and not c[1].startswith(b'--') else 'terminal'
            if kind=='hub' or kind=='serve': kind='hub'
            sm=dict(l.split(':',1) for l in open(f'/proc/{p}/smaps_rollup') if ':' in l)
            k=lambda x:int(sm.get(x,'0 kB').split()[0])
            s=open(f'/proc/{p}/stat').read().rsplit(')',1)[1].split()
            sw=0
            for t in glob.glob(f'/proc/{p}/task/*/status'): sw+=sum(int(l.split()[1]) for l in open(t) if 'ctxt_switches' in l)
            a=r.setdefault(kind,[0]*7)
            for i,v in enumerate([1,k('Rss'),k('Pss'),k('Private_Clean')+k('Private_Dirty'),int(s[17]),int(s[11])+int(s[12]),sw]): a[i]+=v
        except Exception: pass
    return r
W=10
a=snap(); o0=outb[0]; time.sleep(W); b=snap()
print(f"## {label}: terminal bytes drawn/s={(outb[0]-o0)/W:.0f}")
print(f"{'kind':10}{'n':>3}{'RSS':>9}{'PSS':>8}{'priv':>8}{'thr':>5}{'cpu_ms/s':>9}{'wake/s':>8}")
tot=[0]*7
for kd,v in sorted(b.items()):
    av=a.get(kd,[0]*7)
    row=[v[0],v[1],v[2],v[3],v[4],(v[5]-av[5])*10/W,(v[6]-av[6])/W]
    print(f"{kd:10}{row[0]:>3}{row[1]:>9}{row[2]:>8}{row[3]:>8}{row[4]:>5}{row[5]:>9.1f}{row[6]:>8.1f}")
    tot=[x+y for x,y in zip(tot,row)]
print(f"{'TOTAL':10}{tot[0]:>3}{tot[1]:>9}{tot[2]:>8}{tot[3]:>8}{tot[4]:>5}{tot[5]:>9.1f}{tot[6]:>8.1f}")
for f in fds:
    try: os.write(f,b'\x03\x03'); 
    except OSError: pass
time.sleep(0.5)
subprocess.run(['pkill','-x','fiber'])
