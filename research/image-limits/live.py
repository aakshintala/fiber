#!/usr/bin/env python3
"""Oversized-image requests, one matrix per vendor. Usage: live.py anthropic|openai|openai-chat|google
Reads fixtures/ (made by gen) and writes raw/<vendor>-<case>.json without the image bytes or any key."""
import base64, json, os, struct, sys, urllib.request, urllib.error, zlib
D = os.path.dirname(os.path.abspath(__file__))
F = D + "/fixtures/"
PRICE = {  # USD per million tokens (input, output), from each vendor's pricing page
    "anthropic": (2.0, 10.0),   # claude-sonnet-5-5, platform.claude.com/docs/en/about-claude/pricing
    "openai": (0.10, 0.50),     # gpt-6-luna short context, developers.openai.com/api/docs/pricing
    "openai-chat": (0.10, 0.50),
    "google": (0.25, 1.50),     # gemini-3.1-flash-lite, ai.google.dev/gemini-api/docs/pricing
}
CAP = 0.50
PROMPT = "State the width and height of this image in pixels if you can tell, otherwise say unknown. Answer in under 15 words."

def noise_png(n):
    """n x n RGB PNG of random bytes: incompressible, ~3n^2 bytes."""
    raw = b"".join(b"\0" + os.urandom(3 * n) for _ in range(n))
    def chunk(t, d): return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 0)) + chunk(b"IEND", b"")

CASES = {
    "flat-8000x6000": (F + "flat-8000x6000.png", "image/png"),
    "flat-9000x9000": (F + "flat-9000x9000.png", "image/png"),
    "small-gif": (F + "small.gif", "image/gif"),
    "noise-2400": (None, "image/png"),   # ~17 MB PNG, ~23 MB base64
}
vendor = sys.argv[1]
only = sys.argv[2:]
spent = 0.0

def post(url, headers, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), headers={**headers, "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()

def req(b64, mime):
    if vendor == "anthropic":
        key = os.environ["ANTHROPIC_API_KEY"]
        return ("https://api.anthropic.com/v1/messages", {"x-api-key": key, "anthropic-version": "2023-06-01"},
            {"model": "claude-sonnet-5-5", "max_tokens": 60, "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": mime, "data": b64}}, {"type": "text", "text": PROMPT}]}]})
    if vendor == "openai-chat":
        key = os.environ["OPENAI_API_KEY"]
        return ("https://api.openai.com/v1/chat/completions", {"authorization": "Bearer " + key},
            {"model": "gpt-6-luna", "max_completion_tokens": 300, "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}}, {"type": "text", "text": PROMPT}]}]})
    if vendor == "openai":
        key = os.environ["OPENAI_API_KEY"]
        return ("https://api.openai.com/v1/responses", {"authorization": "Bearer " + key},
            {"model": "gpt-6-luna", "max_output_tokens": 300, "input": [{"role": "user", "content": [
                {"type": "input_image", "image_url": f"data:{mime};base64,{b64}"}, {"type": "input_text", "text": PROMPT}]}]})
    key = os.environ["GEMINI_API_KEY"]
    return ("https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent", {"x-goog-api-key": key},
        {"contents": [{"parts": [{"inline_data": {"mime_type": mime, "data": b64}}, {"text": PROMPT}]}], "generationConfig": {"maxOutputTokens": 300}})

for name, (path, mime) in CASES.items():
    if only and name not in only: continue
    data = noise_png(2400) if path is None else open(path, "rb").read()
    b64 = base64.b64encode(data).decode()
    url, hdr, body = req(b64, mime)
    st, txt = post(url, hdr, body)
    try: j = json.loads(txt)
    except Exception: j = {"raw": txt[:2000]}
    usage = j.get("usage") or j.get("usageMetadata") or {}
    inp = usage.get("input_tokens") or usage.get("prompt_tokens") or usage.get("promptTokenCount") or 0
    out = usage.get("output_tokens") or usage.get("completion_tokens") or usage.get("candidatesTokenCount") or 0
    p = PRICE[vendor]; spent += (inp * p[0] + out * p[1]) / 1e6
    rec = {"vendor": vendor, "case": name, "file_bytes": len(data), "base64_bytes": len(b64), "status": st, "usage": usage,
           "error": j.get("error"), "text": txt[:1500] if st != 200 else None,
           "answer": j.get("content") or j.get("output") or j.get("choices") or j.get("candidates")}
    json.dump(rec, open(f"{D}/raw/{vendor}-{name}.json", "w"), indent=1)
    print(name, st, "bytes", len(data), "b64", len(b64), "in", inp, "out", out, "est_usd %.5f" % spent, (j.get("error") or ""))
    if spent > CAP: print("cap reached"); break
