import subprocess,sys,re,collections,hashlib
files=subprocess.check_output(['git','ls-files','crates/**/*.rs','xtask/**/*.rs'],text=True).split()
def is_test(f): return '/tests/' in f or re.search(r'_tests?\.rs$|/tests\.rs$',f)
W=8
for kind in ('test','prod'):
    seen=collections.Counter(); tot=0; perfile={}
    data=[]
    for f in files:
        if bool(is_test(f))!=(kind=='test'): continue
        ls=[l.strip() for l in open(f,errors='ignore')]
        ls=[l for l in ls if l and not l.startswith('//') and l not in ('}','{','});',')',');','},')]
        hs=[hash('\n'.join(ls[i:i+W])) for i in range(len(ls)-W+1)]
        data.append((f,len(ls),hs)); seen.update(hs)
    dupl=0; sl=0; worst=[]
    for f,n,hs in data:
        cov=[False]*n
        for i,h in enumerate(hs):
            if seen[h]>1:
                for j in range(i,i+W): cov[j]=True
        d=sum(cov); dupl+=d; sl+=n; worst.append((d,n,f))
    print(kind,'significant lines',sl,'in duplicated %d-line windows'%W,dupl,'%.1f%%'%(100*dupl/sl))
    for d,n,f in sorted(worst,reverse=True)[:8]: print('   %6d/%6d %3d%% %s'%(d,n,100*d//max(n,1),f))
