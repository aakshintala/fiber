"""Does OpenRouter pass cache_control through to Anthropic? Probe for #133.

Each case sends the same request twice to anthropic/claude-haiku-4.5 pinned to
the Anthropic upstream, and records usage from both replies. A pass-through
shows a cache write on the first send and a cache read on the second.
"""
import json, os, sys, time, urllib.request, uuid

KEY = open("/tmp/openrouter-key").read().strip()
MODEL = "anthropic/claude-haiku-4.5"
OUT = sys.argv[1]
# ~6000 tokens: over Haiku 4.5's minimum cacheable prompt. A nonce per case
# keeps one case's cache from serving another.
FILLER = " ".join(f"Rule {i}: keep answers short and cite the rule number." for i in range(700))

def body(case, nonce):
    sys_text = f"[{nonce}] You are a terse assistant. " + FILLER
    sysmsg = {"role": "system", "content": [{"type": "text", "text": sys_text}]}
    user = {"role": "user", "content": [{"type": "text", "text": "Reply with the word ok."}]}
    tools = None
    if case == "system":
        sysmsg["content"][0]["cache_control"] = {"type": "ephemeral"}
    elif case == "system_ttl_1h":
        sysmsg["content"][0]["cache_control"] = {"type": "ephemeral", "ttl": "1h"}
    elif case == "last_message":
        user = {"role": "user", "content": [{"type": "text", "text": sys_text + "\n\nReply with the word ok.", "cache_control": {"type": "ephemeral"}}]}
        sysmsg = {"role": "system", "content": "You are a terse assistant."}
    elif case == "tool":
        sysmsg = {"role": "system", "content": "You are a terse assistant."}
        tools = [{"type": "function", "function": {"name": "lookup", "description": f"[{nonce}] " + FILLER,
                  "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}},
                  "cache_control": {"type": "ephemeral"}}]
    elif case == "none":
        pass
    b = {"model": MODEL, "max_tokens": 16, "messages": [sysmsg, user],
         "provider": {"only": ["anthropic"], "allow_fallbacks": False}, "usage": {"include": True}}
    if tools: b["tools"] = tools
    return b

def send(b):
    req = urllib.request.Request("https://openrouter.ai/api/v1/chat/completions", data=json.dumps(b).encode(),
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")

results = {}
for case in ["none", "system", "system_ttl_1h", "last_message", "tool"]:
    nonce = uuid.uuid4().hex[:8]
    b = body(case, nonce)
    runs = []
    for i in range(2):
        st, resp = send(b)
        runs.append({"status": st, "provider": resp.get("provider"), "usage": resp.get("usage"), "error": resp.get("error")})
        time.sleep(2)
    results[case] = runs
    json.dump({"request": b, "runs": runs}, open(os.path.join(OUT, f"{case}.json"), "w"), indent=1)
    print(case, json.dumps([{"s": r["status"], "p": r["provider"], "u": {k: r["usage"].get(k) for k in ("prompt_tokens", "prompt_tokens_details", "cost")} if r["usage"] else r["error"]} for r in runs]))
