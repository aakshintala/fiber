# Condenses a raw run into one line per case and profile, timings removed.
import re, sys
case = None
rows = {}
for l in open(sys.argv[1]):
    l = l.rstrip()
    m = re.match(r'CASE (\S+) profile=(\S+) exit_code=(\S+) signal=(\S+) timed_out=(\S+)', l)
    if m:
        case, p, ec, sig, to = m.groups()
        rows[(case, p)] = [f"exit={ec} sig={sig}" + (" TIMEOUT" if to == 'true' else "")]
    elif case and 'out:' in l and 'BEFORE' not in l:
        t = re.sub(r'(after_ms|ms)=\d+', '', l.split('out: ', 1)[1].replace('RESULT ' + case, '')).strip()
        t = re.sub(r'\bpcall=Ok\(\(false, "[^"]*"\)\)', 'pcall=(false,msg)', t)
        rows[(case, p)].append(t[:150])
for (c, p), v in rows.items():
    print(f"{p:15} {c:32} " + " | ".join(v))
