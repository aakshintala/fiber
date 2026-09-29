"""PDF inside a tool result, per protocol. Usage: python3 probe.py anthropic|responses|completions|gemini|extras
Fake history (user ask, assistant read call, tool result carrying the PDF), then a question.
Each accepted case is followed by a second request that extends the first, to read cached tokens."""
import base64, json, os, random, sys, time, urllib.request, urllib.error, hashlib
HERE = os.path.dirname(os.path.abspath(__file__)); RAW = HERE + "/raw"
key = lambda n: open(f"/tmp/{n}-key").read().strip()
PDFS = {n: base64.b64encode(open(f"{HERE}/{n}.pdf", "rb").read()).decode() for n in ("text", "scanned")}
Q = {"text": "Quote the sentence in the PDF that contains the launch code, and give the code.",
     "scanned": "Describe the figure on the page: the shape, its colour, and the number drawn in it."}
FOLLOW = "In one short sentence, say again what the file was about."
FILLER = "Build log notes, ignore them.\n" + "\n".join(f"Step {i}: the compiler checked module {i} and the linker joined object files for target {i * 7}, with no warnings." for i in range(260))
# $ per token: (in, out, cache_read, cache_write)
PRICE = {"anthropic": (2e-6, 10e-6, 0.2e-6, 2.5e-6), "openai": (0.1e-6, 0.5e-6, 0.01e-6, 0.125e-6), "gemini": (0.25e-6, 1.5e-6, 0.025e-6, 0)}
CAP = {"anthropic": 1.0, "openai": 1.0, "gemini": 1.0}
SPEND = {}
def spend_file(v): return f"{RAW}/.spend-{v}"
def load(v):
    try: return float(open(spend_file(v)).read())
    except (OSError, ValueError): return 0.0
def scrub(o):
    if isinstance(o, dict): return {k: scrub(x) for k, x in o.items()}
    if isinstance(o, list): return [scrub(x) for x in o]
    if isinstance(o, str):
        if o == FILLER: return "<filler 260 lines>"
        if len(o) > 2000: return f"<{len(o)} chars sha256 {hashlib.sha256(o.encode()).hexdigest()[:12]}>"
    return o
def post(vendor, name, url, headers, body):
    if load(vendor) > CAP[vendor] * 0.9: sys.exit("cap reached " + vendor)
    req = urllib.request.Request(url, json.dumps(body).encode(), {"content-type": "application/json", **headers})
    try:
        r = urllib.request.urlopen(req, timeout=120); status, text, hdr = r.status, r.read().decode(), r.headers
    except urllib.error.HTTPError as e:
        status, text, hdr = e.code, e.read().decode(), e.headers
    try: js = json.loads(text)
    except ValueError: js = text
    keep = {k: v for k, v in hdr.items() if k.lower() in ("content-type", "retry-after", "request-id", "x-request-id") } if False else {k: v for k, v in hdr.items() if k.lower() == "content-type"}
    json.dump({"status": status, "headers": keep, "request": scrub(body), "response": scrub(js)}, open(f"{RAW}/{name}.json", "w"), indent=1)
    return status, js
def cost(vendor, i, o, cr, cw):
    p = PRICE[vendor]; c = (i - cr - cw) * p[0] + o * p[1] + cr * p[2] + cw * p[3]
    t = load(vendor) + c
    open(spend_file(vendor), "w").write(str(t)); return c
def show(name, status, txt, usage):
    print(f"{name}: {status} {usage} :: {(txt or '')[:230]!r}", flush=True)

# ---------- anthropic ----------
def anth(name, messages):
    st, js = post("anthropic", name, "https://api.anthropic.com/v1/messages",
        {"x-api-key": key("anthropic"), "anthropic-version": "2023-06-01"},
        {"model": "claude-sonnet-5-5", "max_tokens": 300, "system": FILLER, "messages": messages,
         "tools": [{"name": "read", "description": "Read a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}}]})
    if st != 200: show(name, st, json.dumps(js)[:300], ""); return None, None
    u = js["usage"]; cr, cw = u.get("cache_read_input_tokens", 0), u.get("cache_creation_input_tokens", 0)
    cost("anthropic", u["input_tokens"] + cr + cw, u["output_tokens"], cr, cw)
    t = "".join(b.get("text", "") for b in js["content"])
    show(name, st, t, f"in={u['input_tokens']} cache_read={cr} cache_write={cw} out={u['output_tokens']}"); return t, js
def run_anthropic():
    doc = lambda k: {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": PDFS[k]}}
    head = lambda k: [{"role": "user", "content": f"Read {k}.pdf with the read tool, then answer: {Q[k]}"},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_01probe", "name": "read", "input": {"path": k + ".pdf"}}]}]
    for shape in ("in-result", "after-result"):
        for k in ("text", "scanned"):
            if shape == "in-result":
                last = [{"type": "tool_result", "tool_use_id": "toolu_01probe", "content": [doc(k)], "cache_control": {"type": "ephemeral"}}]
            else:
                last = [{"type": "tool_result", "tool_use_id": "toolu_01probe", "content": "The file follows as a document."}, {**doc(k), "cache_control": {"type": "ephemeral"}}]
            msgs = head(k) + [{"role": "user", "content": last}]
            t, _ = anth(f"anthropic.{shape}.{k}.1", msgs)
            if t is None: continue
            time.sleep(3)
            anth(f"anthropic.{shape}.{k}.2", msgs + [{"role": "assistant", "content": t}, {"role": "user", "content": FOLLOW}])

# ---------- openai ----------
def oai(name, path, body):
    st, js = post("openai", name, "https://api.openai.com" + path, {"authorization": "Bearer " + key("openai")}, body)
    if st != 200: show(name, st, json.dumps(js)[:300], ""); return None
    u = js["usage"]
    if "output" in js:  # responses
        cr = u.get("input_tokens_details", {}).get("cached_tokens", 0); i, o = u["input_tokens"], u["output_tokens"]
        t = "".join(c.get("text", "") for it in js["output"] if it["type"] == "message" for c in it["content"])
    else:
        cr = u.get("prompt_tokens_details", {}).get("cached_tokens", 0); i, o = u["prompt_tokens"], u["completion_tokens"]
        t = js["choices"][0]["message"]["content"]
    cost("openai", i, o, cr, 0); show(name, st, t, f"in={i} cached={cr} out={o}"); return t
def run_responses():
    def ent(k, how, fid):
        if how == "file_id": f = {"type": "input_file", "file_id": fid}
        else: f = {"type": "input_file", "filename": k + ".pdf", "file_data": "data:application/pdf;base64," + PDFS[k]}
        return f
    fids = {}
    def upload(k):
        import subprocess
        r = subprocess.run(["curl", "-s", "https://api.openai.com/v1/files", "-H", "Authorization: Bearer " + key("openai"), "-F", "purpose=user_data", "-F", f"file=@{HERE}/{k}.pdf"], capture_output=True, text=True)
        return json.loads(r.stdout)["id"]
    for shape in ("in-result-file_data", "in-result-file_id", "after-result"):
        for k in ("text", "scanned"):
            fid = None
            if shape.endswith("file_id"): fid = fids.setdefault(k, upload(k))
            base = [{"role": "developer", "content": FILLER}, {"role": "user", "content": f"Read {k}.pdf with the read tool, then answer: {Q[k]}"},
                {"type": "function_call", "call_id": "call_probe", "name": "read", "arguments": json.dumps({"path": k + ".pdf"})}]
            if shape == "after-result":
                items = base + [{"type": "function_call_output", "call_id": "call_probe", "output": "The file follows as a user file."},
                                {"role": "user", "content": [ent(k, "file_data", None)]}]
            else:
                items = base + [{"type": "function_call_output", "call_id": "call_probe", "output": [ent(k, shape.split("-")[-1], fid)]}]
            b = lambda inp: {"model": "gpt-6-luna", "input": inp, "store": False, "max_output_tokens": 600, "prompt_cache_key": "probe121-" + shape + k,
                             "reasoning": {"effort": "low"}, "tools": [{"type": "function", "name": "read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]}
            t = oai(f"responses.{shape}.{k}.1", "/v1/responses", b(items))
            if t is None: continue
            time.sleep(3)
            oai(f"responses.{shape}.{k}.2", "/v1/responses", b(items + [{"role": "assistant", "content": t}, {"role": "user", "content": FOLLOW}]))
    for fid in fids.values():
        urllib.request.urlopen(urllib.request.Request("https://api.openai.com/v1/files/" + fid, method="DELETE", headers={"authorization": "Bearer " + key("openai")}))
def run_completions():
    part = lambda k: {"type": "file", "file": {"filename": k + ".pdf", "file_data": "data:application/pdf;base64," + PDFS[k]}}
    for shape in ("in-result", "after-result"):
        for k in ("text", "scanned"):
            msgs = [{"role": "system", "content": FILLER}, {"role": "user", "content": f"Read {k}.pdf with the read tool, then answer: {Q[k]}"},
                {"role": "assistant", "content": None, "tool_calls": [{"id": "call_probe", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": k + ".pdf"})}}]}]
            if shape == "in-result": msgs.append({"role": "tool", "tool_call_id": "call_probe", "content": [part(k)]})
            else: msgs += [{"role": "tool", "tool_call_id": "call_probe", "content": "The file follows as a user file."}, {"role": "user", "content": [part(k)]}]
            b = lambda m: {"model": "gpt-6-luna", "messages": m, "max_completion_tokens": 600, "reasoning_effort": "none", "prompt_cache_key": "probe121c-" + shape + k,
                           "tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}]}
            t = oai(f"completions.{shape}.{k}.1", "/v1/chat/completions", b(msgs))
            if t is None: continue
            time.sleep(3)
            oai(f"completions.{shape}.{k}.2", "/v1/chat/completions", b(msgs + [{"role": "assistant", "content": t}, {"role": "user", "content": FOLLOW}]))

# ---------- gemini ----------
GMODEL = os.environ.get("GMODEL", "gemini-3.1-flash-lite")
def gem(name, contents):
    st, js = post("gemini", name, "https://generativelanguage.googleapis.com/v1beta/models/" + GMODEL + ":generateContent",
        {"x-goog-api-key": key("gemini")},
        {"systemInstruction": {"parts": [{"text": FILLER}]}, "contents": contents, "generationConfig": {"maxOutputTokens": 400},
         "tools": [{"functionDeclarations": [{"name": "read", "description": "Read a file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]}]})
    if st != 200: show(name, st, json.dumps(js)[:400], ""); return None
    u = js["usageMetadata"]; cr = u.get("cachedContentTokenCount", 0)
    cost("gemini", u["promptTokenCount"], u.get("candidatesTokenCount", 0) + u.get("thoughtsTokenCount", 0), cr, 0)
    try: t = "".join(p.get("text", "") for p in js["candidates"][0]["content"]["parts"])
    except (KeyError, IndexError): t = json.dumps(js["candidates"][0])[:200]
    show(name, st, t, f"in={u['promptTokenCount']} cached={cr} out={u.get('candidatesTokenCount')}"); return t
def run_gemini():
    inl = lambda k: {"inlineData": {"mimeType": "application/pdf", "data": PDFS[k]}}
    for shape in ("in-result-parts", "in-result-response-field", "after-result"):
        for k in ("text", "scanned"):
            c = [{"role": "user", "parts": [{"text": f"Read {k}.pdf with the read tool, then answer: {Q[k]}"}]},
                 {"role": "model", "parts": [{"functionCall": {"name": "read", "args": {"path": k + ".pdf"}}, "thoughtSignature": "skip_thought_signature_validator"}]}]
            if shape == "in-result-parts": last = [{"functionResponse": {"name": "read", "response": {"output": "file attached"}, "parts": [inl(k)]}}]
            elif shape == "in-result-response-field": last = [{"functionResponse": {"name": "read", "response": {"file": inl(k)}}}]
            else: last = [{"functionResponse": {"name": "read", "response": {"output": "The file follows as a user part."}}}, inl(k)]
            c.append({"role": "user", "parts": last})
            t = gem(f"gemini.{shape}.{k}.1", c)
            if t is None: continue
            time.sleep(3)
            gem(f"gemini.{shape}.{k}.2", c + [{"role": "model", "parts": [{"text": t}]}, {"role": "user", "parts": [{"text": FOLLOW}]}])

def run_extras():
    """Requests outside the shape loops: the 404, three 400s, the Gemini repeat, the completions image cases."""
    global GMODEL
    keep, GMODEL = GMODEL, "gemini-2.5-flash-lite"
    gem("gemini.404-2.5-flash-lite", [{"role": "user", "parts": [{"text": "hi"}]}])
    GMODEL = keep
    inl = {"inlineData": {"mimeType": "application/pdf", "data": PDFS["text"]}}
    fc = {"functionCall": {"name": "read", "args": {"path": "text.pdf"}}}
    c = [{"role": "user", "parts": [{"text": "Read text.pdf with the read tool, then answer: " + Q["text"]}]},
         {"role": "model", "parts": [{**fc, "thoughtSignature": "skip_thought_signature_validator"}]},
         {"role": "user", "parts": [{"functionResponse": {"name": "read", "response": {"output": "file attached"}, "parts": [inl]}}]},
         {"role": "model", "parts": [{"text": "The launch code is PELICAN-4471."}]}, {"role": "user", "parts": [{"text": FOLLOW}]}]
    for i in range(3):
        gem(f"gemini.cache-repeat.in-result-parts.text.{i+1}", c); time.sleep(8)
    gem("gemini.no-thought-signature.text.1", [c[0], {"role": "model", "parts": [fc]}, c[2]])
    T = [{"type": "function", "function": {"name": "read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}]
    hist = [{"role": "system", "content": FILLER}, {"role": "user", "content": "Read text.pdf with the read tool."},
            {"role": "assistant", "content": None, "tool_calls": [{"id": "call_probe", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": "text.pdf"})}}]}]
    oai("completions.reasoning-effort-with-tools.1", "/v1/chat/completions", {"model": "gpt-6-luna", "messages": hist + [{"role": "tool", "tool_call_id": "call_probe", "content": "ok"}], "max_completion_tokens": 100, "reasoning_effort": "low", "tools": T})
    import subprocess
    r = subprocess.run(["curl", "-s", "https://api.openai.com/v1/files", "-H", "Authorization: Bearer " + key("openai"), "-F", "purpose=user_data", "-F", f"file=@{HERE}/text.pdf"], capture_output=True, text=True)
    fid = json.loads(r.stdout)["id"]
    oai("responses.file_id-with-filename.1", "/v1/responses", {"model": "gpt-6-luna", "store": False, "max_output_tokens": 100, "input": [
        {"role": "user", "content": "Read text.pdf."}, {"type": "function_call", "call_id": "call_probe", "name": "read", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "call_probe", "output": [{"type": "input_file", "file_id": fid, "filename": "text.pdf"}]}]})
    urllib.request.urlopen(urllib.request.Request("https://api.openai.com/v1/files/" + fid, method="DELETE", headers={"authorization": "Bearer " + key("openai")}))
    # image cases need a PNG of the scan: sips -s format png scanned.pdf --out /tmp/p121.png
    png = base64.b64encode(open("/tmp/p121.png", "rb").read()).decode()
    img = {"type": "image_url", "image_url": {"url": "data:image/png;base64," + png}}
    sc_call = {"role": "assistant", "content": None, "tool_calls": [{"id": "call_probe", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": "scanned.pdf"})}}]}
    base = hist[:1] + [{"role": "user", "content": "Read scanned.pdf with the read tool, then answer: " + Q["scanned"]}, sc_call]
    b = lambda m: {"model": "gpt-6-luna", "messages": m, "max_completion_tokens": 600, "reasoning_effort": "none", "tools": T}
    oai("completions.image-in-result.scanned.1", "/v1/chat/completions", b(base + [{"role": "tool", "tool_call_id": "call_probe", "content": [img]}]))
    oai("completions.image-in-result.scanned.2", "/v1/chat/completions", b(base + [{"role": "tool", "tool_call_id": "call_probe", "content": [img]}]))
    oai("completions.image-after-result.scanned.1", "/v1/chat/completions", b(base + [{"role": "tool", "tool_call_id": "call_probe", "content": "The page image follows."}, {"role": "user", "content": [img]}]))

if __name__ == "__main__":
    {"anthropic": run_anthropic, "responses": run_responses, "completions": run_completions, "gemini": run_gemini, "extras": run_extras}[sys.argv[1]]()
    print("spend so far:", {v: round(load(v), 4) for v in PRICE})
