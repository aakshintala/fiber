"""Google Vertex, probed: Claude through `streamRawPredict` and Gemini
through `streamGenerateContent`, their error shapes, and global against
regional locations. Standard library only; run with
`python3 -I probe.py <case>...`, or `python3 -I probe.py rescrub`.

The access token comes from `gcloud auth print-access-token`, as the
`vertex` package's `credential()` reads it, and is never printed or saved.
Request headers are never saved. Response headers keep only their names,
and the values of `content-type`, `retry-after` and `x-should-retry`. A 200
stream's bytes go to `raw/<case>.sse`; the request body, URL path, status,
kept headers, the decoded events, usage and cost go to `raw/<case>.json`.

Every saved file is scrubbed before it is written: the project id becomes
`example-project`, the project number is zeroed, the gcloud account becomes
`person@example.com`, and each message, tool-call and response id is
replaced by a stand-in of the same length. The run stops before its
estimated spend passes CAP.
"""

import base64
import copy
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

# The project to bill: required, so a run never bills whatever project
# gcloud happens to name.
PROJECT = os.environ.get("PROBE_PROJECT", "")
HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")
SPENT = os.path.join(RAW, "spent.json")
CAP = 0.12

CLAUDE = "claude-haiku-4-5@20251001"
CLAUDE_ADAPTIVE = "claude-haiku-5-5"
GEMINI = "gemini-3.1-flash-lite"
GEMINI_2 = "gemini-2.5-flash-lite"

# US dollars per million tokens, from Google's Vertex AI pricing page
# (October 20, 2026): input, output, cache read, cache write (5 minutes),
# cache write (1 hour). Thinking tokens bill as output. A regional
# endpoint adds 10% for Claude.
PRICE = {
    CLAUDE: (1.0, 5.0, 0.10, 1.25, 2.0),
    CLAUDE_ADAPTIVE: (0.10, 0.50, 0.01, 0.125, 0.20),
    GEMINI: (0.25, 1.50, 0.025, 0, 0),
    GEMINI_2: (0.10, 0.40, 0.01, 0, 0),
}

KEPT_HEADERS = ("content-type", "retry-after", "x-should-retry")


def host(location):
    return "aiplatform.googleapis.com" if location == "global" else f"{location}-aiplatform.googleapis.com"


def base(location, publisher, project=None, host_name=None):
    return (
        f"https://{host_name or host(location)}/v1/projects/{project or PROJECT}"
        f"/locations/{location}/publishers/{publisher}/models"
    )


def gcloud(*args):
    ran = subprocess.run(["gcloud", *args], capture_output=True, text=True)
    if ran.returncode != 0:
        sys.exit(f"gcloud {args[0]} {args[1]} failed (exit {ran.returncode})")
    return ran.stdout.strip()


_TOKEN = None
_SECRETS = None


def token():
    global _TOKEN
    if _TOKEN is None:
        _TOKEN = gcloud("auth", "print-access-token")
    return _TOKEN


def secrets():
    """The values every saved file is scrubbed of, with their stand-ins."""
    global _SECRETS
    if _SECRETS is None:
        number = gcloud("projects", "describe", PROJECT, "--format=value(projectNumber)")
        account = gcloud("config", "get-value", "account")
        _SECRETS = [(PROJECT, "example-project"), (number, "0" * len(number))]
        if account:
            _SECRETS.append((account, "person@example.com"))
    return _SECRETS


# Ids a reply names: Claude's message and tool-use ids, Gemini's response
# id, and anything that looks like a request id.
ID_PATTERNS = [
    re.compile(r"msg_vrtx_[A-Za-z0-9]+"),
    re.compile(r"toolu_vrtx_[A-Za-z0-9]+"),
    re.compile(r"req_vrtx_[A-Za-z0-9]+"),
    re.compile(r'("responseId": ?")([^"]+)(")'),
]
_IDS = {}


def stand_in(found):
    # A stand-in an earlier run wrote is kept, so a rescrub changes nothing.
    if re.fullmatch(r"(?:[a-z]+_vrtx_)?0{5,}[0-9]{1,6}", found):
        return found
    if found not in _IDS:
        n = str(len(_IDS) + 1)
        prefix = re.match(r"[a-z]+_vrtx_", found)
        head = prefix.group(0) if prefix else ""
        _IDS[found] = head + "0" * (len(found) - len(head) - len(n)) + n
    return _IDS[found]


def scrub_signature(sig):
    """A base64 signature with any secret inside its bytes zeroed, kept at
    its length; returned unchanged when it holds none."""
    try:
        raw = base64.b64decode(sig + "=" * (-len(sig) % 4))
    except Exception:
        return sig
    changed = raw
    for value, _ in secrets():
        changed = changed.replace(value.encode(), b"0" * len(value.encode()))
    if changed == raw:
        return sig
    return base64.b64encode(changed).decode().rstrip("=")[: len(sig)].ljust(len(sig), "=")


def scrub(text):
    for value, replacement in secrets():
        text = text.replace(value, replacement)
    for pattern in ID_PATTERNS[:3]:
        text = pattern.sub(lambda m: stand_in(m.group(0)), text)
    text = ID_PATTERNS[3].sub(lambda m: m.group(1) + stand_in(m.group(2)) + m.group(3), text)
    text = re.sub(
        r'("(?:signature|thoughtSignature)": ?")([A-Za-z0-9+/=_-]+)(")',
        lambda m: m.group(1) + scrub_signature(m.group(2)) + m.group(3),
        text,
    )
    return text


def spent():
    try:
        with open(SPENT) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def charge(case, cost):
    ledger = spent()
    ledger[case] = round(ledger.get(case, 0.0) + cost, 6)
    with open(SPENT, "w") as f:
        json.dump(ledger, f, indent=2, sort_keys=True)
        f.write("\n")


def guard():
    total = sum(spent().values())
    if total >= CAP:
        sys.exit(f"spend ${total:.4f} reached the cap ${CAP}")


def claude_cost(model, usage, regional=False):
    pin, pout, pread, pw5, pw1 = PRICE[model]
    creation = usage.get("cache_creation") or {}
    w1 = creation.get("ephemeral_1h_input_tokens", 0)
    w5 = usage.get("cache_creation_input_tokens", 0) - w1
    cost = (
        usage.get("input_tokens", 0) * pin
        + usage.get("output_tokens", 0) * pout
        + usage.get("cache_read_input_tokens", 0) * pread
        + w5 * pw5
        + w1 * pw1
    ) / 1e6
    return cost * (1.1 if regional else 1.0)


def gemini_cost(model, meta):
    pin, pout, pread, _, _ = PRICE[model]
    cached = meta.get("cachedContentTokenCount", 0)
    prompt = meta.get("promptTokenCount", 0) - cached
    out = meta.get("candidatesTokenCount", 0) + meta.get("thoughtsTokenCount", 0)
    return (prompt * pin + out * pout + cached * pread) / 1e6


def send(url, body, auth="token", extra=None):
    headers = {"content-type": "application/json", "accept": "text/event-stream"}
    if auth == "token":
        headers["authorization"] = f"Bearer {token()}"
    elif auth is not None:
        headers["authorization"] = f"Bearer {auth}"
    headers.update(extra or {})
    data = json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, headers=headers, method="POST")
    try:
        response = urllib.request.urlopen(request, timeout=120)
        status = response.status
    except urllib.error.HTTPError as error:
        response = error
        status = error.code
    raw = response.read()
    kept = {}
    for name, value in response.headers.items():
        lower = name.lower()
        kept[lower] = value if lower in KEPT_HEADERS else None
    return status, kept, raw


def sse_events(raw):
    """Each `data:` line's JSON, with the `event:` name before it."""
    events = []
    name = None
    for line in raw.decode().splitlines():
        if line.startswith("event:"):
            name = line[6:].strip()
        elif line.startswith("data:"):
            payload = line[5:].strip()
            try:
                events.append((name, json.loads(payload)))
            except json.JSONDecodeError:
                events.append((name, payload))
    return events


def path_of(url):
    return url.split("://", 1)[1].split("/", 1)[1]


def save(case, url, body, status, headers, raw, extra=None):
    record = {"path": "/" + path_of(url), "request": body, "status": status, "headers": headers}
    if status == 200 and url.endswith("alt=sse") or status == 200 and "streamRawPredict" in url:
        with open(os.path.join(RAW, f"{case}.sse"), "w") as f:
            f.write(scrub(raw.decode()))
        record["events"] = [
            {"event": name, "data": data} for name, data in sse_events(raw)
        ]
    else:
        try:
            record["body"] = json.loads(raw)
        except json.JSONDecodeError:
            record["body"] = raw.decode(errors="replace")
    record.update(extra or {})
    text = json.dumps(record, indent=2, sort_keys=True) + "\n"
    with open(os.path.join(RAW, f"{case}.json"), "w") as f:
        f.write(scrub(text))
    return record


# --- Claude -------------------------------------------------------------

WEATHER = {
    "name": "get_weather",
    "description": "Weather for a city.",
    "input_schema": {
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"],
        "additionalProperties": False,
    },
    "strict": True,
}
TIME = {
    "name": "get_time",
    "description": "The local time in a city.",
    "input_schema": {
        "type": "object",
        "properties": {"city": {"type": "string"}, "format": {"type": "string"}},
        "required": ["city"],
    },
    "strict": False,
}


def claude_body(text, max_tokens=16, **extra):
    """A body as Fiber builds it, with `anthropic_version` in place of
    `model`."""
    body = {
        "anthropic_version": "vertex-2023-10-16",
        "system": [{"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral"}}],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]}
        ],
        "tool_choice": {"type": "auto"},
        "stream": True,
        "max_tokens": max_tokens,
    }
    body.update(extra)
    return body


def claude(case, body, model=CLAUDE, location="global", auth="token", host_name=None):
    guard()
    url = f"{base(location, 'anthropic', host_name=host_name)}/{model}:streamRawPredict"
    # Fiber's `anthropic-messages` sends this header on every request.
    status, headers, raw = send(url, body, auth, {"anthropic-version": "2023-06-01"})
    usage = {}
    blocks = []
    if status == 200:
        for name, data in sse_events(raw):
            if not isinstance(data, dict):
                continue
            if data.get("type") == "message_start":
                usage.update(data["message"].get("usage", {}))
            elif data.get("type") == "message_delta":
                usage.update({k: v for k, v in data.get("usage", {}).items() if v is not None})
            elif data.get("type") == "content_block_start":
                blocks.append(copy.deepcopy(data["content_block"]))
            elif data.get("type") == "content_block_delta":
                delta = data["delta"]
                block = blocks[data["index"]]
                kind = delta.get("type")
                if kind == "text_delta":
                    block["text"] = block.get("text", "") + delta["text"]
                elif kind == "thinking_delta":
                    block["thinking"] = block.get("thinking", "") + delta["thinking"]
                elif kind == "signature_delta":
                    block["signature"] = block.get("signature", "") + delta["signature"]
                elif kind == "input_json_delta":
                    block["_json"] = block.get("_json", "") + delta["partial_json"]
        for block in blocks:
            if "_json" in block:
                block["input"] = json.loads(block.pop("_json") or "{}")
    cost = claude_cost(model, usage, regional=location != "global") if status == 200 else 0.0
    charge(case, cost)
    record = save(case, url, body, status, headers, raw, {"usage": usage, "cost": round(cost, 6)})
    print(f"{case}: {status} cost ${cost:.6f} usage {usage}")
    if status != 200:
        print("  " + scrub(json.dumps(record.get("body")))[:400])
    return status, blocks


def case_claude_plain():
    claude("claude-plain", claude_body("Reply with the single word: hello"))


def case_claude_model_in_body():
    body = claude_body("Reply with the single word: hello", max_tokens=4)
    body["model"] = CLAUDE
    claude("claude-model-in-body", body)


def case_claude_no_version():
    body = claude_body("Reply with the single word: hello", max_tokens=4)
    del body["anthropic_version"]
    claude("claude-no-version", body)


def case_claude_api_version():
    claude("claude-api-version", claude_body("Reply with the single word: hello", max_tokens=4, anthropic_version="2023-06-01"))


def case_claude_tool():
    body = claude_body("What is the weather in Paris? Use the tool.", max_tokens=200, tools=[TIME, WEATHER])
    status, blocks = claude("claude-tool", body)
    if status != 200:
        return
    use = next(b for b in blocks if b["type"] == "tool_use")
    body = copy.deepcopy(body)
    body["messages"][0]["content"][0].pop("cache_control")
    body["messages"] += [
        {"role": "assistant", "content": [b for b in blocks]},
        {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": use["id"], "content": "Sunny, 21 C.", "cache_control": {"type": "ephemeral"}}],
        },
    ]
    claude("claude-tool-result", body)


def case_claude_thinking():
    body = claude_body(
        "Is it warmer in Paris or in Rome right now? Use the tool for each city.",
        max_tokens=2048,
        tools=[WEATHER],
        thinking={"type": "enabled", "budget_tokens": 1024},
    )
    status, blocks = claude("claude-thinking", body)
    if status != 200:
        return
    use = next(b for b in blocks if b["type"] == "tool_use")
    replay = copy.deepcopy(body)
    replay["messages"][0]["content"][0].pop("cache_control")
    replay["messages"] += [
        {"role": "assistant", "content": blocks},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": use["id"], "content": "Sunny, 21 C."}]},
    ]
    # A tool use in a reply that thought must carry the thinking block back.
    calls = [b for b in blocks if b["type"] == "tool_use"]
    if len(calls) > 1:
        replay["messages"][-1]["content"] += [
            {"type": "tool_result", "tool_use_id": c["id"], "content": "Cloudy, 17 C."} for c in calls[1:]
        ]
    claude("claude-thinking-replay", replay)
    tampered = copy.deepcopy(replay)
    for block in tampered["messages"][1]["content"]:
        if block["type"] == "thinking":
            sig = block["signature"]
            block["signature"] = sig[:-8] + ("A" if sig[-8] != "A" else "B") + sig[-7:]
    claude("claude-thinking-tampered", tampered)


def long_system():
    lines = []
    for n in range(1, 400):
        lines.append(
            f"Rule {n}: when the file named module_{n}.rs changes, rerun the tests for "
            f"crate number {n % 17} and record the result in the ledger under entry {n * 7}."
        )
    return "\n".join(lines)


def case_claude_cache():
    system = [{"type": "text", "text": long_system(), "cache_control": {"type": "ephemeral"}}]
    for case in ("claude-cache-write", "claude-cache-read"):
        claude(case, claude_body("Reply with the single word: ok", max_tokens=4, system=system))


def case_claude_ttl_1h():
    system = [{"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral", "ttl": "1h"}}]
    claude("claude-ttl-1h", claude_body("Reply with the single word: ok", max_tokens=4, system=system))


def case_claude_adaptive():
    claude(
        "claude-adaptive",
        claude_body("What is 17 * 23? Reply with the number.", max_tokens=256, thinking={"type": "adaptive"}, output_config={"effort": "low"}),
        model=CLAUDE_ADAPTIVE,
    )


def case_claude_adaptive_tool():
    claude(
        "claude-adaptive-tool",
        claude_body("What is the weather in Paris? Use the tool.", max_tokens=300, tools=[WEATHER], thinking={"type": "adaptive"}, output_config={"effort": "low"}),
        model=CLAUDE_ADAPTIVE,
    )


def case_claude_regional():
    location = os.environ.get("PROBE_CLAUDE_REGION", "us-east5")
    claude("claude-regional", claude_body("Reply with the single word: hello", max_tokens=4), location=location)


def case_claude_errors():
    claude("claude-unknown-model", claude_body("hi", max_tokens=4), model="claude-no-such-model@20990101")
    claude("claude-bad-request", claude_body("hi", max_tokens=0))
    claude("claude-bad-token", claude_body("hi", max_tokens=4), auth="ya29.not-a-real-token")
    claude("claude-no-auth", claude_body("hi", max_tokens=4), auth=None)
    claude("claude-bad-location", claude_body("hi", max_tokens=4), location="nowhere1")


def case_claude_data_sharing():
    """A model Google gates behind data sharing with its publisher."""
    claude("claude-data-sharing", claude_body("hi", max_tokens=4), model="claude-fable-5")


def case_expired():
    """A token gcloud printed more than an hour ago, read from the file
    PROBE_EXPIRED_TOKEN names; the file is never saved."""
    with open(os.environ["PROBE_EXPIRED_TOKEN"]) as f:
        old = f.read().strip()
    secrets().append((old, "[expired token]"))
    claude("claude-expired-token", claude_body("hi", max_tokens=4), auth=old)
    gemini("gemini-expired-token", gemini_body("hi"), auth=old)


# --- Gemini -------------------------------------------------------------

def gemini_body(text, system="You are terse.", role=None, **extra):
    """A body as Fiber builds it."""
    body = {
        "contents": [{"role": "user", "parts": [{"text": text}]}],
        "generationConfig": {"thinkingConfig": {"includeThoughts": True}, "maxOutputTokens": 256},
    }
    if system is not None:
        body["systemInstruction"] = {"parts": [{"text": system}]}
        if role is not None:
            body["systemInstruction"]["role"] = role
    body.update(extra)
    return body


def gemini(case, body, model=GEMINI, location="global", auth="token", extra_headers=None, method="streamGenerateContent?alt=sse"):
    guard()
    url = f"{base(location, 'google')}/{model}:{method}"
    status, headers, raw = send(url, body, auth, extra_headers)
    meta = {}
    parts = []
    if status == 200:
        for _, data in sse_events(raw):
            if not isinstance(data, dict):
                continue
            meta = data.get("usageMetadata", meta)
            for candidate in data.get("candidates", []):
                parts += candidate.get("content", {}).get("parts", [])
    cost = gemini_cost(model, meta) if status == 200 else 0.0
    charge(case, cost)
    record = save(case, url, body, status, headers, raw, {"usage": meta, "cost": round(cost, 6)})
    text = "".join(p.get("text", "") for p in parts if not p.get("thought"))
    print(f"{case}: {status} cost ${cost:.6f} text " + scrub(repr(text[:80])))
    if status != 200:
        print("  " + scrub(json.dumps(record.get("body")))[:400])
    return status, parts


def case_gemini_sysrole():
    for role in ("user", "model", None):
        gemini(
            f"gemini-sysrole-{role or 'none'}",
            gemini_body("Say hello.", system="Whatever the user says, reply with the single word PINEAPPLE.", role=role),
        )


SCHEMA = {
    "type": "object",
    "$defs": {"unit": {"type": "string", "enum": ["c", "f"]}},
    "properties": {
        "city": {"type": "string"},
        "unit": {"$ref": "#/$defs/unit"},
        "days": {"anyOf": [{"type": "integer"}, {"type": "null"}]},
    },
    "required": ["city", "unit", "days"],
    "additionalProperties": False,
}


def gemini_tools(key="parametersJsonSchema", schema=SCHEMA):
    return [{"functionDeclarations": [{"name": "get_weather", "description": "Weather for a city.", key: schema}]}]


def case_gemini_schema():
    gemini(
        "gemini-schema-parameters-ref",
        gemini_body("Weather in Paris in celsius, today only?", tools=gemini_tools("parameters")),
    )


def case_gemini_tool():
    """A call, then its replay with the signature, without it, and with the
    ids #136 asks about."""
    body = gemini_body(
        "What is the weather in Paris in celsius for today? Use the tool.",
        tools=gemini_tools(),
        toolConfig={"functionCallingConfig": {"mode": "VALIDATED"}},
    )
    status, parts = gemini("gemini-tool", body)
    if status != 200:
        return
    calls = [p for p in parts if "functionCall" in p]
    if not calls:
        return
    call = calls[0]

    def replay(case, call_part, response_id=None):
        sent = copy.deepcopy(body)
        response = {"name": call_part["functionCall"]["name"], "response": {"output": "Sunny, 21 C."}}
        if response_id is not None:
            response["id"] = response_id
        elif "id" in call_part["functionCall"]:
            response["id"] = call_part["functionCall"]["id"]
        sent["contents"] += [
            {"role": "model", "parts": [call_part]},
            {"role": "user", "parts": [{"functionResponse": response}]},
        ]
        return gemini(case, sent)

    replay("gemini-replay-signed", call)
    unsigned = {k: v for k, v in call.items() if k != "thoughtSignature"}
    replay("gemini-replay-unsigned", unsigned)
    for label, new_id in (("odd", "call abc/123!"), ("long65", "c" * 65)):
        changed = copy.deepcopy(call)
        changed["functionCall"]["id"] = new_id
        replay(f"gemini-replay-id-{label}", changed, response_id=new_id)
    replay("gemini-replay-id-mismatch", call, response_id="call_other")


def case_gemini_thinking():
    """A thinking level, as Fiber sends one: thought parts and their
    signatures, then the reply replayed with every part as it came."""
    body = gemini_body(
        "Is 391 a prime number? Think it through, then answer yes or no.",
        generationConfig={"thinkingConfig": {"includeThoughts": True, "thinkingLevel": "MEDIUM"}, "maxOutputTokens": 1024},
    )
    status, parts = gemini("gemini-thinking", body)
    if status != 200:
        return
    replay = copy.deepcopy(body)
    replay["contents"] += [{"role": "model", "parts": parts}, {"role": "user", "parts": [{"text": "And 393?"}]}]
    gemini("gemini-thinking-replay", replay)


GEMINI_3_IDS = [
    "gemini-3-flash-preview",
    "gemini-3.1-flash-lite",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.7-flash",
    "gemini-3.8-flash",
    "gemini-4-argon",
]

# Vertex AI pricing (October 20, 2026), US dollars per million tokens:
# input, output, cache read.
GEMINI_3_PRICE = {
    "gemini-3-flash-preview": (0.5, 3.0, 0.05),
    "gemini-3.1-flash-lite": (0.25, 1.5, 0.025),
    "gemini-3.1-pro-preview": (2.0, 12.0, 0.2),
    "gemini-3.1-pro-preview-customtools": (2.0, 12.0, 0.2),
    "gemini-3.5-flash": (1.5, 9.0, 0.15),
    "gemini-3.5-flash-lite": (0.3, 2.5, 0.03),
    "gemini-3.7-flash": (0.75, 3.75, 0.075),
    "gemini-3.8-flash": (0.75, 3.75, 0.075),
    "gemini-4-argon": (2.0, 10.0, 0.1),
}


def case_gemini_models():
    """One tool call per Gemini 3 and later model: does it answer, with a
    `functionCall`, at `global`?"""
    path = os.path.join(RAW, "gemini-models.json")
    found = {}
    if os.path.exists(path):
        with open(path) as f:
            found = json.load(f)
    only = os.environ.get("PROBE_GEMINI_MODELS")
    for model in only.split(",") if only else GEMINI_3_IDS:
        PRICE.setdefault(model, GEMINI_3_PRICE[model] + (0, 0))
        body = gemini_body(
            "What is the weather in Paris in celsius for today? Use the tool.",
            tools=gemini_tools(),
            generationConfig={"thinkingConfig": {"includeThoughts": True, "thinkingLevel": "LOW"}, "maxOutputTokens": 1024},
        )
        status, parts = gemini(f"gemini-models-{model}", body, model=model)
        calls = [p["functionCall"] for p in parts if "functionCall" in p]
        found[model] = {
            "status": status,
            "calls": [{"name": c.get("name"), "has_id": "id" in c} for c in calls],
            "signed": [("thoughtSignature" in p) for p in parts if "functionCall" in p],
        }
    with open(path, "w") as f:
        f.write(scrub(json.dumps(found, indent=2, sort_keys=True) + "\n"))


def case_gemini_search():
    """The hosted `google_search` beside a function tool, as Fiber sends it
    when a model declares `web_search`: without the flag, then with it.
    Google bills grounding per query past a monthly free allowance."""
    tools = gemini_tools() + [{"google_search": {}}]
    text = "Search the web: what is the latest stable release of the Rust programming language? Answer in one line."
    gemini("gemini-search-no-flag", gemini_body(text, tools=tools, toolConfig={"functionCallingConfig": {"mode": "AUTO"}}))
    gemini(
        "gemini-search",
        gemini_body(text, tools=tools, toolConfig={"functionCallingConfig": {"mode": "AUTO"}, "includeServerSideToolInvocations": True}),
    )


def case_gemini_argon_levels():
    """The thinking levels `gemini-4-argon` takes: the `gemini` package has
    no row for it, since the Gemini API does not list it."""
    model = "gemini-4-argon"
    PRICE.setdefault(model, GEMINI_3_PRICE[model] + (0, 0))
    for level in ("MINIMAL", "MEDIUM", "HIGH"):
        body = gemini_body(
            "Reply with the single word: ok",
            generationConfig={"thinkingConfig": {"includeThoughts": True, "thinkingLevel": level}, "maxOutputTokens": 512},
        )
        gemini(f"gemini-argon-{level.lower()}", body, model=model)


# A 16x16 PNG of solid blue, built with zlib.
BLUE_PNG = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR42mNgYPhPIhrVMKph2GoAAJLb/wHQPqPSAAAAAElFTkSuQmCC"


def image_reply(case, model):
    """A function response carrying a blue PNG in `parts`: does the model
    read it?"""
    body = gemini_body("Look at the image the tool returns, then name its colour in one word.", tools=gemini_tools(schema={"type": "object", "properties": {}}))
    body["tools"][0]["functionDeclarations"][0]["name"] = "get_image"
    if model == GEMINI_2:
        # 2.5 Flash-Lite refuses `includeThoughts` while thinking is off
        # (`raw/gemini2-thoughts.json`).
        del body["generationConfig"]["thinkingConfig"]
    status, parts = gemini(f"{case}-call", copy.deepcopy(body) | {"toolConfig": {"functionCallingConfig": {"mode": "ANY"}}}, model=model)
    calls = [p for p in parts if "functionCall" in p]
    if status != 200 or not calls:
        return None
    call = calls[0]
    response = {
        "name": "get_image",
        "response": {"output": "The image is attached."},
        "parts": [{"inlineData": {"mimeType": "image/png", "data": BLUE_PNG}}],
    }
    if "id" in call["functionCall"]:
        response["id"] = call["functionCall"]["id"]
    body["contents"] += [{"role": "model", "parts": [call]}, {"role": "user", "parts": [{"functionResponse": response}]}]
    gemini(case, body, model=model)
    return call


def case_gemini_image():
    image_reply("gemini-imgparts", GEMINI)


def case_gemini2():
    """#136's 2.x rows: ids on emitted calls, a replayed made-up id, and
    images in `functionResponse.parts`."""
    body = gemini_body("What is the weather in Paris in celsius for today? Use the tool.", tools=gemini_tools())
    del body["generationConfig"]["thinkingConfig"]
    status, parts = gemini("gemini2-tool", body, model=GEMINI_2)
    calls = [p for p in parts if "functionCall" in p]
    if status == 200 and calls:
        call = copy.deepcopy(calls[0])
        call["functionCall"]["id"] = "call abc/123!"
        body["contents"] += [
            {"role": "model", "parts": [call]},
            {"role": "user", "parts": [{"functionResponse": {"id": "call abc/123!", "name": "get_weather", "response": {"output": "Sunny, 21 C."}}}]},
        ]
        gemini("gemini2-replay-id", body, model=GEMINI_2)
    image_reply("gemini2-imgparts", GEMINI_2)


def case_gemini2_image():
    image_reply("gemini2-imgparts", GEMINI_2)
    # The same PNG as a user part, to show the image itself is one the
    # model takes.
    body = gemini_body("Name the colour of this image in one word.")
    del body["generationConfig"]["thinkingConfig"]
    body["contents"][0]["parts"].append({"inlineData": {"mimeType": "image/png", "data": BLUE_PNG}})
    gemini("gemini2-image-user-part", body, model=GEMINI_2)


def case_gemini_regional():
    location = os.environ.get("PROBE_GEMINI_REGION", "us-central1")
    body = gemini_body("Reply with the single word: hello")
    gemini("gemini-regional", body, location=location)
    body = gemini_body("Reply with the single word: hello")
    del body["generationConfig"]["thinkingConfig"]
    gemini("gemini2-regional", body, model=GEMINI_2, location=location)


def case_gemini_errors():
    gemini("gemini-unknown-model", gemini_body("hi"), model="gemini-no-such-model")
    bad = gemini_body("hi")
    bad["contents"][0]["role"] = "robot"
    gemini("gemini-bad-request", bad)
    gemini("gemini-bad-token", gemini_body("hi"), auth="ya29.not-a-real-token")
    gemini("gemini-api-key-header", gemini_body("hi"), auth=None, extra_headers={"x-goog-api-key": "not-a-real-key"})


# --- Discovery (count tokens, which is free) ------------------------------

CLAUDE_IDS = [
    "claude-haiku-4-5@20251001",
    "claude-haiku-5-5",
    "claude-haiku-5-5@default",
    "claude-sonnet-4-5@20250929",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "claude-sonnet-5-5",
    "claude-opus-4-5@20251101",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-opus-5-5",
    "claude-fable-5",
    "claude-fable-5-1",
]
GEMINI_IDS = [
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.5-pro",
    "gemini-3-flash-preview",
    "gemini-3.1-flash-lite",
    "gemini-3.1-flash-lite-image",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.6-flash",
    "gemini-3.7-flash",
    "gemini-3.8-flash",
    "gemini-4-argon",
]


def case_claude_access():
    """Which Claude models answer `streamRawPredict` for this project: one
    output token each, at `global` and at two regions."""
    found = {}
    tries = [(model, "global") for model in CLAUDE_IDS]
    tries += [(CLAUDE, "us-east5"), (CLAUDE, "europe-west1")]
    for model, location in tries:
        guard()
        url = f"{base(location, 'anthropic')}/{model}:streamRawPredict"
        body = claude_body("hi", max_tokens=1)
        status, _, raw = send(url, body, "token", {"anthropic-version": "2023-06-01"})
        usage = {}
        if status == 200:
            for _, data in sse_events(raw):
                if isinstance(data, dict) and data.get("type") == "message_start":
                    usage.update(data["message"].get("usage", {}))
                if isinstance(data, dict) and data.get("type") == "message_delta":
                    usage.update({k: v for k, v in data.get("usage", {}).items() if v is not None})
            price = PRICE.get(model, (15.0, 75.0, 1.5, 18.75, 30.0))
            cost = (usage.get("input_tokens", 0) * price[0] + usage.get("output_tokens", 0) * price[1]) / 1e6
            cost *= 1.1 if location != "global" else 1.0
            charge("claude-access", cost)
            found[f"{model} {location}"] = {"status": status, "usage": usage}
        else:
            found[f"{model} {location}"] = summary(status, raw)
        print(model, location, status)
    with open(os.path.join(RAW, "claude-access.json"), "w") as f:
        f.write(scrub(json.dumps(found, indent=2, sort_keys=True) + "\n"))


def count_claude(model, location="global", host_name=None):
    url = f"{base(location, 'anthropic', host_name=host_name)}/count-tokens:rawPredict"
    body = {"model": model, "messages": [{"role": "user", "content": "hi"}], "anthropic_version": "vertex-2023-10-16"}
    status, _, raw = send(url, body)
    return status, raw


def count_gemini(model, location="global", host_name=None):
    url = f"{base(location, 'google', host_name=host_name)}/{model}:countTokens"
    status, _, raw = send(url, {"contents": [{"role": "user", "parts": [{"text": "hi"}]}]})
    return status, raw


def summary(status, raw):
    try:
        body = json.loads(raw)
    except json.JSONDecodeError:
        return {"status": status}
    if status == 200:
        return {"status": status, "body": body}
    # `streamRawPredict` wraps its error object in a one-element list.
    shape = "list" if isinstance(body, list) else "object"
    if isinstance(body, list) and len(body) == 1:
        body = body[0]
    error = body.get("error", body) if isinstance(body, dict) else body
    if isinstance(error, dict):
        return {"status": status, "shape": shape, "error": {k: error.get(k) for k in ("code", "status", "type", "message") if k in error}}
    if isinstance(error, dict):
        return {"status": status, "error": {k: error.get(k) for k in ("code", "status", "type", "message") if k in error}}
    return {"status": status, "error": error}


def case_discover():
    found = {"claude_global": {}, "gemini_global": {}, "locations": {}}
    for model in CLAUDE_IDS:
        found["claude_global"][model] = summary(*count_claude(model))
        print("claude", model, found["claude_global"][model]["status"])
    for model in GEMINI_IDS:
        found["gemini_global"][model] = summary(*count_gemini(model))
        print("gemini", model, found["gemini_global"][model]["status"])
    for label, call in (
        ("claude us-east5", lambda: count_claude(CLAUDE, "us-east5")),
        ("claude asia-south1", lambda: count_claude(CLAUDE, "asia-south1")),
        ("claude europe-west1", lambda: count_claude(CLAUDE, "europe-west1")),
        ("claude global on global-aiplatform host", lambda: count_claude(CLAUDE, "global", "global-aiplatform.googleapis.com")),
        ("claude us-east5 on the global host", lambda: count_claude(CLAUDE, "us-east5", "aiplatform.googleapis.com")),
        ("gemini us-central1", lambda: count_gemini(GEMINI, "us-central1")),
        ("gemini asia-south1", lambda: count_gemini(GEMINI, "asia-south1")),
        ("gemini 2.5 us-central1", lambda: count_gemini(GEMINI_2, "us-central1")),
        ("gemini global on global-aiplatform host", lambda: count_gemini(GEMINI, "global", "global-aiplatform.googleapis.com")),
        ("gemini us-central1 on the global host", lambda: count_gemini(GEMINI, "us-central1", "aiplatform.googleapis.com")),
    ):
        found["locations"][label] = summary(*call())
        print(label, found["locations"][label]["status"])
    with open(os.path.join(RAW, "discover.json"), "w") as f:
        f.write(scrub(json.dumps(found, indent=2, sort_keys=True) + "\n"))


def rescrub():
    """Scrubs every saved file again and checks none holds a secret."""
    for name in sorted(os.listdir(RAW)):
        path = os.path.join(RAW, name)
        with open(path) as f:
            text = f.read()
        clean = scrub(text)
        if clean != text:
            with open(path, "w") as f:
                f.write(clean)
            print("rescrubbed", name)
        for value, _ in secrets():
            assert value not in clean, f"{name} still holds a secret"
        assert "ya29." not in clean.replace("ya29.not-a-real-token", ""), f"{name} holds a token"


CASES = {name[5:].replace("_", "-"): fn for name, fn in globals().items() if name.startswith("case_")}

if __name__ == "__main__":
    if not PROJECT:
        sys.exit("set PROBE_PROJECT to the Google Cloud project to bill")
    os.makedirs(RAW, exist_ok=True)
    for arg in sys.argv[1:]:
        if arg == "rescrub":
            rescrub()
        elif arg in CASES:
            CASES[arg]()
        else:
            sys.exit(f"unknown case {arg}; cases: {', '.join(sorted(CASES))}")
    print(f"spend so far: ${sum(spent().values()):.6f}")
