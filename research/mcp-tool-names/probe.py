"""Tool-name length probe (#588). Finds the longest accepted function-tool name per protocol.
Usage: python3 probe.py anthropic|codex|zen-responses|go-completions
One tiny request per length tested (max 16 output tokens); keys are read here and never printed or saved."""
import json, sys, os, time, urllib.request, urllib.error
D = os.path.dirname(os.path.abspath(__file__))
UA = "fiber-probe/0.1"
def post(url, headers, body):
    h = {"Content-Type": "application/json", "User-Agent": UA, **headers}
    r = urllib.request.Request(url, json.dumps(body).encode(), h)
    try:
        resp = urllib.request.urlopen(r, timeout=120); return resp.status, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()
def name(n): return "a" * n
def schema(): return {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}
def anthropic():
    key = open("/tmp/anthropic-key-2nd-ws").read().strip()
    def send(n):
        return post("https://api.anthropic.com/v1/messages", {"x-api-key": key, "anthropic-version": "2023-06-01"},
            {"model": "claude-sonnet-5-5", "max_tokens": 16, "messages": [{"role": "user", "content": "Say hi."}],
             "tools": [{"name": name(n), "description": "d", "input_schema": schema()}], "tool_choice": {"type": "none"}})
    return send
def codex():
    t = json.load(open(os.path.expanduser("~/.codex/auth.json")))["tokens"]
    def send(n):
        st, raw = post("https://chatgpt.com/backend-api/codex/responses",
            {"Authorization": "Bearer " + t["access_token"], "chatgpt-account-id": t["account_id"], "originator": "fiber-probe", "OpenAI-Beta": "responses=experimental", "Accept": "text/event-stream"},
            {"model": "gpt-6-luna", "instructions": "Reply briefly.", "store": False, "stream": True,
             "input": [{"role": "user", "content": [{"type": "input_text", "text": "Say hi."}]}],
             "tools": [{"type": "function", "name": name(n), "description": "d", "parameters": schema()}], "tool_choice": "none"})
        return st, raw[:600]
    return send
def zen_responses():
    key = open("/tmp/opencode-key").read().strip()
    def send(n):
        return post("https://opencode.ai/zen/v1/responses", {"Authorization": "Bearer " + key},
            {"model": "gpt-6-luna", "input": "Say hi.", "max_output_tokens": 16, "tool_choice": "none",
             "tools": [{"type": "function", "name": name(n), "description": "d", "parameters": schema()}]})
    return send
def go_completions():
    key = open("/tmp/opencode-key").read().strip()
    def send(n):
        return post("https://opencode.ai/zen/go/v1/chat/completions", {"Authorization": "Bearer " + key, "x-opencode-session": "fiber-probe-names"},
            {"model": "glm-5.3-flash", "max_tokens": 16, "messages": [{"role": "user", "content": "Say hi."}], "tool_choice": "none",
             "tools": [{"type": "function", "function": {"name": name(n), "description": "d", "parameters": schema()}}]})
    return send
which = sys.argv[1]
send = {"anthropic": anthropic, "codex": codex, "zen-responses": zen_responses, "go-completions": go_completions}[which]()
log = []
def ok(n):
    time.sleep(1); st, raw = send(n)
    log.append({"length": n, "status": st, "body": raw[:500]}); print(which, n, st, flush=True)
    return st == 200
lo, hi = 0, None
n = 64
while hi is None and n <= 1024:
    if ok(n): lo = n; n *= 2
    else: hi = n
if hi is None: hi = lo + 1
if lo == 0:
    n = 32
    while n >= 1 and not ok(n): hi = n; n //= 2
    lo = n if n else 0
while hi - lo > 1:
    mid = (lo + hi) // 2
    if ok(mid): lo = mid
    else: hi = mid
print(which, "max accepted", lo)
json.dump({"protocol": which, "max_accepted": lo, "requests": log}, open(f"{D}/raw/{which}.json", "w"), indent=1)
