"""Thinking-level probes for OpenCode Go and Zen gateway models (#2335).

Usage: python3 probe.py <route> <protocol> <model> [<model> ...]
  route:    go | zen
  protocol: completions | responses | messages | google

Sends one tiny request per thinking level (off, minimal, low, medium, high,
xhigh, max), shaped the way Fiber sends that level on the protocol, and
records every status under raw/<route>-<model>.json. One request, no retry.
The key is read from ~/.config/probe-keys/opencode-key into the request
header; it is never printed, never on a command line, and every record is
checked for it before writing.
"""
import json
import os
import sys
import time
import urllib.request
import urllib.error

KEY = open(os.path.expanduser("~/.config/probe-keys/opencode-key")).read().strip()
GO = "https://opencode.ai/zen/go"
ZEN = "https://opencode.ai/zen"
HERE = os.path.dirname(os.path.abspath(__file__))

LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
PROMPT = "Reply with the single word: ok"


def headers(extra=None):
    h = {
        "Authorization": "Bearer " + KEY,
        "Content-Type": "application/json",
        "x-opencode-session": "fiber-probe-2335",
        # Cloudflare answers 403 "error code: 1010" to Python's default User-Agent.
        "User-Agent": "fiber-probe/0.1",
    }
    h.update(extra or {})
    return h


def bodies(protocol, model, level):
    """The URL, body and extra headers for Fiber's wire form of `level`."""
    if protocol == "completions":
        effort = "none" if level == "off" else level
        return (
            "/v1/chat/completions",
            {
                "model": model,
                "messages": [{"role": "user", "content": PROMPT}],
                "max_tokens": 64,
                "reasoning_effort": effort,
            },
            {},
        )
    if protocol == "responses":
        effort = "none" if level == "off" else level
        return (
            "/v1/responses",
            {
                "model": model,
                "input": PROMPT,
                "max_output_tokens": 16,
                "reasoning": {"effort": effort},
                "include": ["reasoning.encrypted_content"],
            },
            {},
        )
    if protocol == "messages":
        body = {
            "model": model,
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 64,
        }
        if level != "off":
            # Without compat.thinking_budget Fiber sends adaptive thinking
            # plus an effort; off sends no thinking key at all.
            body["thinking"] = {"type": "adaptive"}
            body["output_config"] = {"effort": level}
        return (
            "/v1/messages",
            body,
            {"anthropic-version": "2023-06-01", "x-api-key": KEY},
        )
    if protocol == "google":
        thinking = (
            {"thinkingBudget": 0}
            if level == "off"
            else {"thinkingLevel": level.upper()}
        )
        thinking["includeThoughts"] = True
        return (
            f"/v1/models/{model}:generateContent",
            {
                "contents": [{"role": "user", "parts": [{"text": PROMPT}]}],
                "generationConfig": {
                    "maxOutputTokens": 16,
                    "thinkingConfig": thinking,
                },
            },
            {"x-goog-api-key": KEY},
        )
    raise ValueError(protocol)


def call(base, path, body, extra_headers):
    h = headers(extra_headers)
    url = base + path
    data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=h)
    started = time.time()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            status, raw = r.status, r.read()
        text = raw.decode("utf-8", "replace")
        try:
            parsed = json.loads(text)
        except ValueError:
            parsed = {"_raw": text[:2000]}
    except urllib.error.HTTPError as e:
        status = e.code
        try:
            parsed = json.loads(e.read().decode("utf-8", "replace"))
        except ValueError:
            parsed = {"_raw": "non-JSON error body"}
    except Exception as e:
        # A dropped connection or timeout: recorded, not retried.
        status, parsed = -1, {"_transport": f"{type(e).__name__}: {e}"}
    return {
        "level": None,
        "status": status,
        "elapsed_s": round(time.time() - started, 2),
        "request": body,
        "response": parsed,
    }


def probe(route, protocol, model):
    base = GO if route == "go" else ZEN
    attempts = []
    for level in LEVELS:
        path, body, extra = bodies(protocol, model, level)
        rec = call(base, path, body, extra)
        rec["level"] = level
        attempts.append(rec)
        print(f"{route}/{model} {level}: {rec['status']} ({rec['elapsed_s']}s)", flush=True)
    out = {
        "route": route,
        "protocol": protocol,
        "model": model,
        "base": base,
        "levels": attempts,
    }
    s = json.dumps(out, indent=1)
    assert KEY not in s, "key leaked into record"
    path = os.path.join(HERE, "raw", f"{route}-{model}.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w").write(s + "\n")
    accepted = [a["level"] for a in attempts if a["status"] == 200]
    print(f"{route}/{model} accepts: {accepted}", flush=True)


if __name__ == "__main__":
    _, route, protocol, *models = sys.argv
    assert route in ("go", "zen"), route
    assert protocol in ("completions", "responses", "messages", "google"), protocol
    for model in models:
        probe(route, protocol, model)
