"""Live probe of chatgpt.com/backend-api/codex/responses. Usage: probe.py <group>
Hard cap: 60 requests total, counted in raw/count.txt. Credentials are read here and never printed or saved."""
import json, sys, time, uuid, urllib.request, urllib.error, os, random
D = os.path.dirname(os.path.abspath(__file__)); RAW = D + "/raw"
T = json.load(open(os.path.expanduser("~/.codex/auth.json")))["tokens"]
URL = "https://chatgpt.com/backend-api/codex/responses"; MODEL = "gpt-6-luna"; CAP = 60
def count():
    p = RAW + "/count.txt"; n = int(open(p).read()) if os.path.exists(p) else 0
    if n >= CAP: sys.exit("cap reached")
    open(p, "w").write(str(n + 1)); return n + 1
def body(inp="Say hi in three words.", **kw):
    b = {"model": MODEL, "instructions": "You are a helpful assistant.", "store": False, "stream": True,
         "input": [{"role": "user", "content": [{"type": "input_text", "text": inp}]}],
         "include": ["reasoning.encrypted_content"]}
    b.update(kw); return b
def send(name, b, beta="responses=experimental", sid=None, xreq=None, sid_header="session_id"):
    n = count(); time.sleep(3)
    h = {"Authorization": "Bearer " + T["access_token"], "chatgpt-account-id": T["account_id"], "originator": "fiber-probe",
         "User-Agent": "fiber-probe/0", "Accept": "text/event-stream", "Content-Type": "application/json"}
    if beta: h["OpenAI-Beta"] = beta
    if sid: h[sid_header] = sid
    if xreq: h["x-client-request-id"] = xreq
    r = urllib.request.Request(URL, json.dumps(b).encode(), h)
    out = {"name": name, "n": n, "sent_headers": [k for k in h if k not in ("Authorization", "chatgpt-account-id")], "request": b}
    try:
        resp = urllib.request.urlopen(r, timeout=120); out["status"] = resp.status; hd = resp.headers; raw = resp.read().decode()
    except urllib.error.HTTPError as e:
        out["status"] = e.code; hd = e.headers; raw = e.read().decode()
    except Exception as e:
        out["status"] = "exc"; hd = {}; raw = repr(e)
    out["resp_headers"] = {k: v for k, v in hd.items() if k.lower() not in ("set-cookie", "chatgpt-account-id")}
    ev = []; usage = None; text = ""
    for blk in raw.split("\n\n"):
        t = d = None
        for l in blk.split("\n"):
            if l.startswith("event:"): t = l[6:].strip()
            if l.startswith("data:"): d = l[5:].strip()
        if d is None: continue
        try: j = json.loads(d)
        except Exception: j = d
        ev.append({"event": t, "data": j})
        if isinstance(j, dict):
            if j.get("type") == "response.output_text.delta": text += j["delta"]
            r_ = j.get("response")
            if isinstance(r_, dict) and r_.get("usage"): usage = r_["usage"]
    out["events"] = ev; out["text"] = text; out["usage"] = usage
    if not ev: out["body"] = raw[:2000]
    s = json.dumps(out, indent=1)
    for secret in (T["access_token"], T["account_id"], T["refresh_token"], T["id_token"]): s = s.replace(secret, "REDACTED")
    open(f"{RAW}/{name}.json", "w").write(s)
    types = [e["event"] for e in ev]
    print(name, out["status"], "events:", len(ev), "last:", types[-1] if types else None, "usage:", json.dumps({k: (usage or {}).get(k) for k in ("input_tokens","output_tokens")}), "cached:", ((usage or {}).get("input_tokens_details") or {}).get("cached_tokens"), "text:", text[:80].replace("\n", " "), "" if ev else raw[:300], flush=True)
    return out
TOOL = [{"type": "function", "name": "get_weather", "description": "Get weather", "strict": False,
         "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}]
def cache_prompt(tag):
    random.seed(tag); words = [f"w{random.randint(0,99999)}" for _ in range(3000)]
    return f"Reference text {tag}: " + " ".join(words) + "\nReply with the single word OK."
g = sys.argv[1]
if g == "events":
    send("ev-text", body("Write two short sentences about rivers."), sid=str(uuid.uuid4()))
    send("ev-text2", body("Say hi in three words."), sid=str(uuid.uuid4()))
    send("ev-tool", body("What is the weather in Paris? Use the tool.", tools=TOOL, tool_choice="auto"), sid=str(uuid.uuid4()))
    send("ev-tool2", body("Weather in Rome and Oslo? Call the tool for each.", tools=TOOL, tool_choice="auto", parallel_tool_calls=True), sid=str(uuid.uuid4()))
elif g == "beta":
    send("beta-with", body(), sid=str(uuid.uuid4()))
    send("beta-without", body(), beta=None, sid=str(uuid.uuid4()))
    send("beta-without-nosid", body(), beta=None)
    send("beta-other", body(), beta="responses_websockets=2026-02-06", sid=str(uuid.uuid4()))
elif g == "fields":
    send("f-baseline", body())
    send("f-parallel-true", body("Weather in Rome and Oslo? Call the tool for each.", tools=TOOL, parallel_tool_calls=True))
    send("f-parallel-false", body("Weather in Rome and Oslo? Call the tool for each.", tools=TOOL, parallel_tool_calls=False))
    send("f-parallel-omitted", body("Weather in Rome and Oslo? Call the tool for each.", tools=TOOL))
    send("f-toolchoice-auto", body("Say hi.", tools=TOOL, tool_choice="auto"))
    send("f-toolchoice-required", body("Say hi.", tools=TOOL, tool_choice="required"))
    send("f-verb-low", body("Explain how a bicycle stays upright.", text={"verbosity": "low"}))
    send("f-verb-high", body("Explain how a bicycle stays upright.", text={"verbosity": "high"}))
    send("f-verb-omitted", body("Explain how a bicycle stays upright."))
    send("f-temp", body(temperature=0.5))
    send("f-temp0", body(temperature=0))
    send("f-tier-priority", body(service_tier="priority"))
    send("f-tier-flex", body(service_tier="flex"))
    send("f-tier-auto", body(service_tier="auto"))
    send("f-tier-default", body(service_tier="default"))
elif g == "cache":
    rnd = sys.argv[2]
    def run(label, fresh, split=False):
        p = cache_prompt(label + rnd); s = str(uuid.uuid4())
        for i in (1, 2):
            if i == 2: time.sleep(20)
            if fresh: s = str(uuid.uuid4())
            b = body(p, prompt_cache_key=s)
            send(f"cache-{rnd}-{label}-{i}", b, sid=s, xreq=s)
    run("stable", False); run("fresh", True)
elif g == "cachesplit":
    # stable prompt_cache_key, fresh session_id header each request (rig-like)
    p = cache_prompt("split"); k = str(uuid.uuid4())
    for i in (1, 2):
        if i == 2: time.sleep(20)
        send(f"cachesplit-{i}", body(p, prompt_cache_key=k), sid=str(uuid.uuid4()))
    # no prompt_cache_key and no session headers at all
    p = cache_prompt("none")
    for i in (1, 2):
        if i == 2: time.sleep(20)
        send(f"cachenone-{i}", body(p))
elif g == "cachevar":
    p = cache_prompt("hdronly"); s = str(uuid.uuid4())
    for i in (1, 2):
        if i == 2: time.sleep(20)
        send(f"cachehdronly-{i}", body(p), sid=s, xreq=s)
    p = cache_prompt("keyonly"); k = str(uuid.uuid4())
    for i in (1, 2):
        if i == 2: time.sleep(20)
        send(f"cachekeyonly-{i}", body(p, prompt_cache_key=k))
elif g == "cachehdr":
    for label, kw in (("sessionid-underscore", dict(sid_header="session_id")), ("session-id-hyphen", dict(sid_header="session-id")), ("xreq-only", dict(xreq="X"))):
        p = cache_prompt(label); s = str(uuid.uuid4())
        for i in (1, 2):
            if i == 2: time.sleep(20)
            if "xreq" in kw: send(f"cachehdr-{label}-{i}", body(p), xreq=s)
            else: send(f"cachehdr-{label}-{i}", body(p), sid=s, **kw)
