import os, sys, glob

root = int(sys.argv[1])
pp = {}
for p in os.listdir('/proc'):
    if p.isdigit():
        try:
            pp[int(p)] = int(open(f'/proc/{p}/stat').read().rsplit(')', 1)[1].split()[1])
        except Exception:
            pass
ds = {root}
ch = True
while ch:
    ch = False
    for p, q in pp.items():
        if q in ds and p not in ds:
            ds.add(p)
            ch = True
agg = {}
for p in ds:
    try:
        exe = os.path.basename(os.readlink(f'/proc/{p}/exe'))
        st = dict(l.split(':', 1) for l in open(f'/proc/{p}/status') if ':' in l)
        sm = dict(l.split(':', 1) for l in open(f'/proc/{p}/smaps_rollup') if ':' in l)
        k = lambda d, n: int(d.get(n, '0 kB').split()[0])
        s = open(f'/proc/{p}/stat').read().rsplit(')', 1)[1].split()
        sw = 0
        for t in glob.glob(f'/proc/{p}/task/*/status'):
            sw += sum(int(l.split()[1]) for l in open(t) if 'ctxt_switches' in l)
        a = agg.setdefault(exe, [0] * 8)
        vals = [1, k(st, 'VmRSS'), k(sm, 'Pss'), k(sm, 'Private_Clean') + k(sm, 'Private_Dirty'),
                k(st, 'RssAnon'), int(st['Threads']), (int(s[11]) + int(s[12])) / 100, sw]
        for i, v in enumerate(vals):
            a[i] += v
    except Exception:
        pass
print(f"{'exe':14}{'n':>3}{'RSS':>9}{'PSS':>9}{'Private':>9}{'Anon':>9}{'thr':>5}{'cpu_s':>8}{'ctxsw':>9}")
tot = [0] * 8
for e, a in sorted(agg.items(), key=lambda x: -x[1][2]):
    print(f"{e:14}{a[0]:>3}{a[1]:>9}{a[2]:>9}{a[3]:>9}{a[4]:>9}{a[5]:>5}{a[6]:>8.2f}{a[7]:>9}")
    tot = [x + y for x, y in zip(tot, a)]
print(f"{'TOTAL':14}{tot[0]:>3}{tot[1]:>9}{tot[2]:>9}{tot[3]:>9}{tot[4]:>9}{tot[5]:>5}{tot[6]:>8.2f}{tot[7]:>9}")
