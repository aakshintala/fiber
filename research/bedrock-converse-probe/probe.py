"""AWS Bedrock's Converse API, probed: the AWS event-stream framing of a
ConverseStream reply, its request shapes and its error shapes, on Bedrock's
models other than Claude. Standard library only; run with
`PROBE_PROFILE=<profile> python3 -I probe.py <case>...`.

PROBE_PROFILE is required: without it the probe exits before any AWS call.
Credentials come from `aws configure export-credentials --profile $PROFILE
--format process` and are never printed or saved. The account id, which a
reasoning signature can carry, is read from the ARNs `aws bedrock
list-inference-profiles` returns, held in memory only, and zeroed in every
saved byte. Request headers are never saved. Response headers keep only
`content-type` and `x-amzn-errortype`. A 200 stream's bytes go to
`raw/<name>.eventstream`, scrubbed, with both CRCs recomputed; the request
body, status, kept headers, the decoded events, the usage and the cost go to
`raw/<name>.json`. The run stops before its recorded spend passes CAP.
"""

import base64
import datetime
import hashlib
import hmac
import json
import os
import re
import struct
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
import zlib

# The AWS profile to read credentials from: required, so a run never signs
# with whatever profile happens to be the default.
PROFILE = os.environ.get("PROBE_PROFILE", "")
REGION = os.environ.get("PROBE_REGION", "ap-south-1")
CAP = 0.20
HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")
SPENT = os.path.join(RAW, "spent.json")

MINISTRAL = "mistral.ministral-3-8b-instruct"
OSS = "openai.gpt-oss-20b-1:0"
OSS120 = "openai.gpt-oss-120b-1:0"
QWEN = "qwen.qwen3-32b-v1:0"
DEEPSEEK = "deepseek.v3.2"
NOVA_MICRO = "apac.amazon.nova-micro-v1:0"
NOVA2 = "global.amazon.nova-2-lite-v1:0"
HAIKU = "global.anthropic.claude-haiku-4-5-20251001-v1:0"
QWEN_VL = "qwen.qwen3-vl-235b-a22b"

# Per million tokens, from models.dev's `amazon-bedrock` entry (fetched
# 2026-10-20): input, output, cache read, cache write.
PRICE = {
    MINISTRAL: (0.15, 0.15, 0, 0),
    OSS: (0.07, 0.30, 0, 0),
    OSS120: (0.15, 0.60, 0, 0),
    QWEN: (0.15, 0.60, 0, 0),
    DEEPSEEK: (0.62, 1.85, 0, 0),
    NOVA_MICRO: (0.037, 0.148, 0.00925, 0.037),
    NOVA2: (0.30, 2.50, 0.075, 0.30),
    HAIKU: (1.0, 5.0, 0.10, 1.25),
    QWEN_VL: (0.53, 2.66, 0, 0),
}


def credentials():
    out = subprocess.run(
        ["aws", "configure", "export-credentials", "--profile", PROFILE, "--format", "process"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def account_id():
    """The account id, from an inference profile's ARN: a free, read-only
    listing. Never printed."""
    out = subprocess.run(
        [
            "aws", "bedrock", "list-inference-profiles",
            "--profile", PROFILE, "--region", REGION, "--output", "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    for profile in json.loads(out)["inferenceProfileSummaries"]:
        parts = profile["inferenceProfileArn"].split(":")
        if len(parts) > 4 and parts[4].isdigit():
            return parts[4]
    raise SystemExit("probe.py: no inference profile ARN names an account")


def sign(method, url, headers, body, creds, now):
    """AWS SigV4 for service `bedrock`: the headers to add."""
    parts = urllib.parse.urlsplit(url)
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    day = now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(body).hexdigest()
    signed = dict((k.lower(), v.strip()) for k, v in headers.items())
    signed["host"] = parts.netloc
    signed["x-amz-date"] = amz_date
    signed["x-amz-content-sha256"] = payload_hash
    if creds.get("SessionToken"):
        signed["x-amz-security-token"] = creds["SessionToken"]
    names = sorted(signed)
    canonical_headers = "".join(f"{n}:{signed[n]}\n" for n in names)
    signed_headers = ";".join(names)
    # Non-S3 services encode the already-encoded path once more.
    canonical_uri = urllib.parse.quote(parts.path, safe="/~")
    canonical = "\n".join(
        [method, canonical_uri, parts.query, canonical_headers, signed_headers, payload_hash]
    )
    scope = f"{day}/{REGION}/bedrock/aws4_request"
    to_sign = "\n".join(
        ["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()]
    )
    key = ("AWS4" + creds["SecretAccessKey"]).encode()
    for part in (day, REGION, "bedrock", "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    added = {
        "x-amz-date": amz_date,
        "x-amz-content-sha256": payload_hash,
        "authorization": f"AWS4-HMAC-SHA256 Credential={creds['AccessKeyId']}/{scope}, "
        f"SignedHeaders={signed_headers}, Signature={signature}",
    }
    if creds.get("SessionToken"):
        added["x-amz-security-token"] = creds["SessionToken"]
    return added


def frames(data):
    """Each event-stream message as (headers, payload), CRCs checked, and
    the bytes left over after the last whole message."""
    out = []
    at = 0
    while at + 12 <= len(data):
        total, hlen, pcrc = struct.unpack(">III", data[at : at + 12])
        assert zlib.crc32(data[at : at + 8]) == pcrc, "prelude crc"
        msg = data[at : at + total]
        assert zlib.crc32(msg[:-4]) == struct.unpack(">I", msg[-4:])[0], "message crc"
        headers = {}
        h = msg[12 : 12 + hlen]
        i = 0
        while i < len(h):
            nlen = h[i]
            name = h[i + 1 : i + 1 + nlen].decode()
            kind = h[i + 1 + nlen]
            i += 2 + nlen
            assert kind == 7, f"header type {kind}"
            vlen = struct.unpack(">H", h[i : i + 2])[0]
            headers[name] = h[i + 2 : i + 2 + vlen].decode()
            i += 2 + vlen
        out.append((headers, msg[12 + hlen : -4]))
        at += total
    assert at == len(data), "trailing bytes"
    return out


def frame(headers, payload):
    hbytes = b""
    for name, value in headers.items():
        hbytes += bytes([len(name)]) + name.encode() + b"\x07" + struct.pack(">H", len(value)) + value.encode()
    total = 16 + len(hbytes) + len(payload)
    prelude = struct.pack(">II", total, len(hbytes))
    prelude += struct.pack(">I", zlib.crc32(prelude))
    body = prelude + hbytes + payload
    return body + struct.pack(">I", zlib.crc32(body))


UUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")


def scrub_text(text, account):
    """`text` with the account id zeroed, in clear and inside any base64 run
    whose decoded bytes carry it, each run re-encoded at its own length, and
    every request id (a UUID) zeroed at its own length."""
    zero = "0" * len(account)
    text = text.replace(account, zero)
    text = UUID.sub("00000000-0000-0000-0000-000000000000", text)
    out = []
    for piece in text.split('"'):
        candidate = piece
        if len(piece) >= 16 and len(piece) % 4 == 0:
            try:
                raw = base64.b64decode(piece, validate=True)
            except ValueError:
                raw = None
            if raw is not None and account.encode() in raw:
                candidate = base64.b64encode(raw.replace(account.encode(), zero.encode())).decode()
                assert len(candidate) == len(piece)
        out.append(candidate)
    return '"'.join(out)


def scrub(data, account):
    """Every message re-framed with its payload scrubbed at the same length,
    both CRCs recomputed."""
    out = b""
    for headers, payload in frames(data):
        clean = scrub_text(payload.decode(), account).encode()
        assert len(clean) == len(payload)
        out += frame(headers, clean)
    assert len(out) == len(data)
    return out


def events(data):
    out = []
    for headers, payload in frames(data):
        body = json.loads(payload) if payload else {}
        if isinstance(body, dict):
            body.pop("p", None)
        out.append({"headers": headers, "payload": body})
    return out


def usage_of(evs):
    for e in evs:
        if e["headers"].get(":event-type") == "metadata":
            return e["payload"].get("usage", {})
    return {}


def cost(model, usage):
    i, o, r, w = PRICE[model]
    return (
        usage.get("inputTokens", 0) * i
        + usage.get("outputTokens", 0) * o
        + usage.get("cacheReadInputTokens", 0) * r
        + usage.get("cacheWriteInputTokens", 0) * w
    ) / 1e6


class Run:
    """One probe run: the credentials, the account id and the spend."""

    def __init__(self):
        self.creds = credentials()
        self.account = account_id()
        self.spent = 0.0
        if os.path.exists(SPENT):
            with open(SPENT) as f:
                self.spent = json.load(f)["usd"]

    def call(self, name, model, body, note=None):
        """Sends `body` to `model`'s ConverseStream, saves what came back
        under `name`, and returns (status, the unscrubbed events or the
        error body)."""
        if self.spent >= CAP:
            raise SystemExit(f"stopping: ${self.spent:.6f} spent, cap ${CAP}")
        path = "/model/" + urllib.parse.quote(model, safe="") + "/converse-stream"
        url = f"https://bedrock-runtime.{REGION}.amazonaws.com{path}"
        raw_body = json.dumps(body).encode()
        headers = {"content-type": "application/json", "accept": "application/vnd.amazon.eventstream"}
        now = datetime.datetime.now(datetime.timezone.utc)
        headers.update(sign("POST", url, headers, raw_body, self.creds, now))
        request = urllib.request.Request(url, data=raw_body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                status, raw, got = response.status, response.read(), response.headers
        except urllib.error.HTTPError as e:
            status, raw, got = e.code, e.read(), e.headers
        except TimeoutError:
            # No response head within 300 seconds: nothing came back to
            # price, so the record holds the timeout alone.
            status, raw, got = "timeout", b"", {}
        kept = {k.lower(): v for k, v in got.items() if k.lower() in ("content-type", "x-amzn-errortype")}
        # The request id is kept only as a synthetic value and its shape,
        # never as it came.
        request_id = next((v for k, v in got.items() if k.lower() == "x-amzn-requestid"), None)
        if request_id is not None:
            kept["x-amzn-requestid"] = "00000000-0000-0000-0000-000000000000"
            kept["x-amzn-requestid-shape"] = "uuid" if UUID.fullmatch(request_id.strip()) else f"{len(request_id)} characters"
        record = {
            "case": name,
            "model": model,
            "region": REGION,
            "url_path": path,
            "request": json.loads(scrub_text(json.dumps(body), self.account)),
            "status": status,
            "headers": kept,
        }
        if note:
            record["note"] = note
        dollars = 0.0
        result = None
        if status == 200:
            result = events(raw)
            clean = scrub(raw, self.account)
            # A classify check keeps its decoded events only.
            if not name.startswith("classify/"):
                with open(os.path.join(RAW, f"{name}.eventstream"), "wb") as f:
                    f.write(clean)
            record["events"] = events(clean)
            usage = usage_of(result)
            dollars = cost(model, usage)
            record["usage"] = usage
            record["cost_usd"] = round(dollars, 6)
        else:
            text = raw.decode(errors="replace")
            result = text
            record["body"] = scrub_text(text, self.account)
        with open(os.path.join(RAW, f"{name}.json"), "w") as f:
            f.write(scrub_text(json.dumps(record, indent=2), self.account))
            f.write("\n")
        self.spent += dollars
        with open(SPENT, "w") as f:
            json.dump({"usd": round(self.spent, 6)}, f)
            f.write("\n")
        print(f"{name}: {model} HTTP {status} {kept.get('x-amzn-errortype', '').split(':')[0]} "
              f"${dollars:.6f} (total ${self.spent:.6f})")
        return status, result


def text_of(evs):
    return "".join(
        e["payload"].get("delta", {}).get("text", "")
        for e in evs
        if e["headers"].get(":event-type") == "contentBlockDelta"
    )


def reasoning_blocks(evs):
    """Each streamed reasoning block as Converse takes it back:
    `reasoningText` with its text and signature, or `redactedContent`."""
    blocks = {}
    for e in evs:
        if e["headers"].get(":event-type") != "contentBlockDelta":
            continue
        index = e["payload"]["contentBlockIndex"]
        reasoning = e["payload"]["delta"].get("reasoningContent")
        if reasoning is None:
            continue
        block = blocks.setdefault(index, {"text": "", "signature": "", "redacted": ""})
        block["text"] += reasoning.get("text", "")
        block["signature"] += reasoning.get("signature", "")
        block["redacted"] += reasoning.get("redactedContent", "")
    out = []
    for index in sorted(blocks):
        block = blocks[index]
        if block["redacted"]:
            out.append({"reasoningContent": {"redactedContent": block["redacted"]}})
        else:
            text = {"text": block["text"]}
            if block["signature"]:
                text["signature"] = block["signature"]
            out.append({"reasoningContent": {"reasoningText": text}})
    return out


def png(width=8, height=8, rgb=(200, 30, 30)):
    """A solid-colour PNG, built by hand."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    row = b"\x00" + bytes(rgb) * width
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(row * height))
        + chunk(b"IEND", b"")
    )


WEATHER = {
    "toolSpec": {
        "name": "get_weather",
        "description": "Weather for a city.",
        "inputSchema": {
            "json": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
                "additionalProperties": False,
            }
        },
    }
}

TWO_CITIES = "What is the weather in Paris and in Rome? Call the tool once per city, both in this reply."

# About 1,500 tokens of system text: past Nova's 1,000-token minimum for a
# cache checkpoint, far below its 20,000-token ceiling.
LONG_SYSTEM = "You are a terse assistant. " + " ".join(
    f"Rule {n}: answer in as few words as the question allows." for n in range(1, 140)
)
# About 21,000 tokens: past Nova's 20,000-token ceiling for caching.
HUGE_SYSTEM = "You are a terse assistant. " + " ".join(
    f"Rule {n}: answer in as few words as the question allows." for n in range(1, 1900)
)


def user(*content):
    return {"role": "user", "content": list(content)}


def assistant(*content):
    return {"role": "assistant", "content": list(content)}


def text(t):
    return {"text": t}


def weather_history(status=None, result="Sunny, 22 C.", **extra):
    """A finished tool exchange: the call, then its result, then a question."""
    tool_result = {"toolUseId": "tooluse_probe0001", "content": [{"text": result}]}
    if status:
        tool_result["status"] = status
    tool_result.update(extra)
    return [
        user(text("What is the weather in Paris?")),
        assistant({"toolUse": {"toolUseId": "tooluse_probe0001", "name": "get_weather", "input": {"city": "Paris"}}}),
        user({"toolResult": tool_result}),
    ]


def body(messages, system="You are terse.", max_tokens=64, tools=None, **extra):
    out = {"messages": messages, "inferenceConfig": {"maxTokens": max_tokens}}
    if system:
        out["system"] = [text(system)]
    if tools is not None:
        out["toolConfig"] = {"tools": tools}
    out.update(extra)
    return out


def case_plain(run):
    run.call("plain", MINISTRAL, body([user(text("Say hello in five words."))], max_tokens=32))


def case_tool(run):
    for suffix, model in [("nova", NOVA_MICRO), ("qwen", QWEN), ("oss", OSS), ("ministral", MINISTRAL)]:
        run.call(f"tool-{suffix}", model, body([user(text(TWO_CITIES))], max_tokens=512, tools=[WEATHER]))


def case_reasoning(run):
    q = [user(text("What is 17 * 23? Answer with the number."))]
    run.call("reasoning-oss", OSS, body(q, max_tokens=512))
    for effort in ("low", "high"):
        run.call(f"reasoning-oss-{effort}", OSS,
                 body(q, max_tokens=2048, additionalModelRequestFields={"reasoning_effort": effort}))
    run.call("reasoning-oss120-low", OSS120,
             body(q, max_tokens=1024, additionalModelRequestFields={"reasoning_effort": "low"}))
    for effort in ("low", "medium"):
        run.call(f"reasoning-nova2-{effort}", NOVA2,
                 body(q, max_tokens=2048,
                      additionalModelRequestFields={"reasoningConfig": {"type": "enabled", "maxReasoningEffort": effort}}))
    run.call("reasoning-nova2-high", NOVA2,
             body(q, max_tokens=2048,
                  additionalModelRequestFields={"reasoningConfig": {"type": "enabled", "maxReasoningEffort": "high"}}),
             note="high effort with maxTokens set")
    run.call("reasoning-deepseek", DEEPSEEK, body(q, max_tokens=512))


def case_nova_high(run):
    """Nova 2 at `high`, with `maxTokens` left out as its 400 asks."""
    q = [user(text("What is 17 * 23? Answer with the number."))]
    run.call("reasoning-nova2-high-unset", NOVA2,
             {"system": [text("You are terse.")], "messages": q,
              "additionalModelRequestFields": {"reasoningConfig": {"type": "enabled", "maxReasoningEffort": "high"}}})


def case_replay(run):
    """A reply's reasoning sent back in the next request."""
    q = user(text("What is 17 * 23? Answer with the number."))
    then = user(text("And 18 * 23?"))
    for suffix, model, extra in [
        ("oss", OSS, {"additionalModelRequestFields": {"reasoning_effort": "low"}}),
        ("nova2", NOVA2, {"additionalModelRequestFields": {"reasoningConfig": {"type": "enabled", "maxReasoningEffort": "low"}}}),
    ]:
        status, evs = run.call(f"replay-{suffix}-first", model, body([q], max_tokens=1024, **extra))
        if status != 200:
            continue
        content = reasoning_blocks(evs) + [text(text_of(evs) or "391")]
        run.call(f"replay-{suffix}", model, body([q, assistant(*content), then], max_tokens=1024, **extra))


def case_signature(run):
    """Claude through Converse: the one model family known to sign its
    reasoning, so the decoder's signature path has a recording."""
    q = user(text("What is 17 * 23? Answer with the number."))
    thinking = {"thinking": {"type": "enabled", "budget_tokens": 1024}}
    status, evs = run.call("signature", HAIKU, body([q], max_tokens=2048, additionalModelRequestFields=thinking))
    if status != 200:
        return
    content = reasoning_blocks(evs) + [text(text_of(evs) or "391")]
    run.call("signature-replay", HAIKU,
             body([q, assistant(*content), user(text("And 18 * 23?"))], max_tokens=2048,
                  additionalModelRequestFields=thinking))


def case_cache(run):
    q = [user(text("Say ok."))]
    system = [text(LONG_SYSTEM), {"cachePoint": {"type": "default"}}]
    run.call("cache-write", NOVA_MICRO, {"system": system, "messages": q, "inferenceConfig": {"maxTokens": 16}})
    run.call("cache-read", NOVA_MICRO, {"system": system, "messages": q, "inferenceConfig": {"maxTokens": 16}})
    ttl = [text(LONG_SYSTEM), {"cachePoint": {"type": "default", "ttl": "1h"}}]
    run.call("cache-ttl-1h", NOVA_MICRO, {"system": ttl, "messages": q, "inferenceConfig": {"maxTokens": 16}})
    # Fiber's three markers: the system's end, the previous end and the new
    # end, one in a message after a tool result.
    history = weather_history()
    history[0]["content"].append({"cachePoint": {"type": "default"}})
    history[2]["content"].append({"cachePoint": {"type": "default"}})
    run.call("cache-messages", NOVA_MICRO,
             {"system": system, "messages": history, "inferenceConfig": {"maxTokens": 32},
              "toolConfig": {"tools": [WEATHER]}})


def case_cache_turns(run):
    """Markers after text in messages: one turn written, the next read."""
    system = [text(LONG_SYSTEM), {"cachePoint": {"type": "default"}}]
    point = {"cachePoint": {"type": "default"}}
    first = [user(text("Say ok."), point)]
    run.call("cache-turn-1", NOVA_MICRO, {"system": system, "messages": first, "inferenceConfig": {"maxTokens": 16}})
    second = [user(text("Say ok."), point), assistant(text("ok")), user(text("Say yes."), point)]
    run.call("cache-turn-2", NOVA_MICRO, {"system": system, "messages": second, "inferenceConfig": {"maxTokens": 16}})
    # A marker after the assistant's tool call, then one after a text
    # following a tool result.
    history = weather_history()
    history[1]["content"].append(point)
    history[2]["content"].append(text("Answer in one word."))
    history[2]["content"].append(point)
    run.call("cache-after-tool-use", NOVA_MICRO,
             {"system": system, "messages": history, "inferenceConfig": {"maxTokens": 32},
              "toolConfig": {"tools": [WEATHER]}})


def case_cache_nova(run):
    """A cache write on each other Nova model ap-south-1 serves."""
    q = [user(text("Say ok."))]
    system = [text(LONG_SYSTEM), {"cachePoint": {"type": "default"}}]
    for model, price in [
        ("apac.amazon.nova-lite-v1:0", (0.063, 0.252, 0.01575, 0.063)),
        ("apac.amazon.nova-pro-v1:0", (0.84, 3.36, 0.21, 0.84)),
        (NOVA2, PRICE[NOVA2]),
    ]:
        PRICE.setdefault(model, price)
        run.call(f"cache-write-{model.split('.')[2].split('-v')[0]}", model,
                 {"system": system, "messages": q, "inferenceConfig": {"maxTokens": 16}})


def case_redacted(run):
    """Grok's encrypted reasoning, `redactedContent`, sent back."""
    grok = "global.xai.grok-4.7"
    PRICE.setdefault(grok, (2, 6, 0.5, 0))
    q = user(text("What is 17 * 23? Answer with the number."))
    status, evs = run.call("redacted-first", grok, body([q], max_tokens=200))
    if status != 200:
        return
    content = reasoning_blocks(evs) + [text(text_of(evs) or "391")]
    run.call("redacted-replay", grok, body([q, assistant(*content), user(text("And 18 * 23?"))], max_tokens=200))


def case_cache_huge(run):
    q = [user(text("Say ok."))]
    system = [text(HUGE_SYSTEM), {"cachePoint": {"type": "default"}}]
    run.call("cache-over-20k", NOVA_MICRO, {"system": system, "messages": q, "inferenceConfig": {"maxTokens": 16}})


def case_cache_after_result(run):
    """A marker after a text that follows a tool result, in the same
    message."""
    system = [text(LONG_SYSTEM), {"cachePoint": {"type": "default"}}]
    history = weather_history()
    history[2]["content"].append(text("Answer in one word."))
    history[2]["content"].append({"cachePoint": {"type": "default"}})
    run.call("cache-text-after-result", NOVA_MICRO,
             {"system": system, "messages": history, "inferenceConfig": {"maxTokens": 32},
              "toolConfig": {"tools": [WEATHER]}})


def case_tool_results(run):
    for suffix, model in [("oss", OSS), ("ministral", MINISTRAL), ("qwen", QWEN), ("nova", NOVA_MICRO), ("deepseek", DEEPSEEK)]:
        run.call(f"tool-error-{suffix}", model,
                 body(weather_history(status="error", result="The weather service is down."), tools=[WEATHER]))
    for suffix, model in [("oss", OSS), ("ministral", MINISTRAL)]:
        run.call(f"tool-empty-{suffix}", model, body(weather_history(result=""), tools=[WEATHER]))
    run.call("no-toolconfig", MINISTRAL, body(weather_history()))


def case_images(run):
    image = {"image": {"format": "png", "source": {"bytes": base64.b64encode(png()).decode()}}}
    run.call("image", MINISTRAL, body([user(image, text("What colour is this image? One word."))], max_tokens=16))
    ask = [assistant(text("Here is the map.")), user(text("What colour was the map image? One word."))]
    for suffix, model in [("ministral", MINISTRAL), ("nova2", NOVA2), ("qwen-vl", QWEN_VL)]:
        # Inside the result, as Converse's ToolResultContentBlock allows.
        inside = weather_history()
        inside[2]["content"][0]["toolResult"]["content"].append(image)
        run.call(f"image-in-result-{suffix}", model, body(inside + ask, max_tokens=16, tools=[WEATHER]))
        # Beside the result, in the same user message.
        beside = weather_history()
        beside[2]["content"].append(image)
        run.call(f"image-beside-result-{suffix}", model, body(beside + ask, max_tokens=16, tools=[WEATHER]))


def case_shapes(run):
    q = [user(text(TWO_CITIES))]
    for suffix, model in [("oss", OSS), ("ministral", MINISTRAL), ("nova", NOVA_MICRO)]:
        for strict in (False, True):
            spec = json.loads(json.dumps(WEATHER))
            spec["toolSpec"]["strict"] = strict
            run.call(f"strict-{str(strict).lower()}-{suffix}", model, body(q, max_tokens=256, tools=[spec]))
    run.call("toolchoice-auto-oss", OSS,
             {"messages": q, "inferenceConfig": {"maxTokens": 256},
              "toolConfig": {"tools": [WEATHER], "toolChoice": {"auto": {}}}})
    run.call("max-tokens-over-oss", OSS, body([user(text("Say ok."))], max_tokens=128000))


# Each models.dev `amazon-bedrock` model the package might list that
# ap-south-1 serves, by on-demand id or inference profile, with its price
# per million input and output tokens and the output cap its check sends:
# a reply under a tool config, so a model that cannot stream tool use
# fails before it generates.
CLASSIFY = [
    ("apac.amazon.nova-lite-v1:0", 0.063, 0.252, 200),
    ("apac.amazon.nova-micro-v1:0", 0.037, 0.148, 200),
    ("apac.amazon.nova-pro-v1:0", 0.84, 3.36, 200),
    ("global.amazon.nova-2-lite-v1:0", 0.3, 2.5, 200),
    ("deepseek.v3-v1:0", 0.58, 1.68, 200),
    ("deepseek.v3.2", 0.62, 1.85, 200),
    ("minimax.minimax-m2", 0.3, 1.2, 200),
    ("minimax.minimax-m2.1", 0.3, 1.2, 200),
    ("minimax.minimax-m2.5", 0.3, 1.2, 200),
    ("mistral.devstral-2-123b", 0.4, 2, 200),
    ("mistral.magistral-small-2509", 0.5, 1.5, 200),
    ("mistral.ministral-3-14b-instruct", 0.2, 0.2, 200),
    ("mistral.ministral-3-3b-instruct", 0.1, 0.1, 200),
    ("mistral.ministral-3-8b-instruct", 0.15, 0.15, 200),
    ("mistral.mistral-large-3-675b-instruct", 0.5, 1.5, 200),
    ("mistral.voxtral-mini-3b-2507", 0.04, 0.04, 200),
    ("mistral.voxtral-small-24b-2507", 0.1, 0.3, 200),
    ("moonshot.kimi-k2-thinking", 0.6, 2.5, 200),
    ("moonshotai.kimi-k2.5", 0.6, 3, 200),
    ("global.moonshotai.kimi-k3", 3, 15, 48),
    ("nvidia.nemotron-nano-12b-v2", 0.2, 0.6, 200),
    ("nvidia.nemotron-nano-3-30b", 0.06, 0.24, 200),
    ("nvidia.nemotron-nano-9b-v2", 0.06, 0.23, 200),
    ("nvidia.nemotron-super-3-120b", 0.15, 0.65, 200),
    ("openai.gpt-oss-120b-1:0", 0.15, 0.6, 200),
    ("openai.gpt-oss-20b-1:0", 0.07, 0.3, 200),
    ("openai.gpt-oss-safeguard-120b", 0.15, 0.6, 200),
    ("openai.gpt-oss-safeguard-20b", 0.07, 0.2, 200),
    ("global.openai.gpt-5.6-luna", 0.2, 1.2, 200),
    ("global.openai.gpt-5.6-sol", 4, 20, 48),
    ("global.openai.gpt-5.6-terra", 2, 12, 48),
    ("global.openai.gpt-6-astra", 10, 50, 48),
    ("global.openai.gpt-6-luna", 0.1, 0.5, 200),
    ("global.openai.gpt-6-sol", 2, 10, 48),
    ("global.openai.gpt-6.1-sol", 2, 10, 48),
    ("in.openai.gpt-5.6-luna", 0.22, 1.32, 200),
    ("in.openai.gpt-5.6-terra", 2.2, 13.2, 48),
    ("qwen.qwen3-235b-a22b-2507-v1:0", 0.22, 0.88, 200),
    ("qwen.qwen3-32b-v1:0", 0.15, 0.6, 200),
    ("qwen.qwen3-coder-30b-a3b-v1:0", 0.15, 0.6, 200),
    ("qwen.qwen3-coder-480b-a35b-v1:0", 0.45, 1.8, 200),
    ("qwen.qwen3-next-80b-a3b", 0.15, 1.2, 200),
    ("qwen.qwen3-vl-235b-a22b", 0.53, 2.66, 200),
    ("global.xai.grok-4.6", 2, 6, 48),
    ("global.xai.grok-4.7", 2, 6, 48),
    ("zai.glm-4.7", 0.6, 2.2, 200),
    ("zai.glm-4.7-flash", 0.07, 0.4, 200),
    ("zai.glm-5", 1, 3.2, 200),
    ("global.zai.glm-5.3", 1.68, 5.28, 48),
]


def case_classify(run):
    os.makedirs(os.path.join(RAW, "classify"), exist_ok=True)
    for model, i, o, cap in CLASSIFY:
        PRICE.setdefault(model, (i, o, 0, 0))
        if os.path.exists(os.path.join(RAW, "classify", model.replace(":", "_") + ".json")):
            continue
        run.call(f"classify/{model.replace(':', '_')}", model,
                 body([user(text("Call get_weather for Paris."))], max_tokens=cap, tools=[WEATHER]))


def case_request_id(run):
    """Whether a reply names its request in `x-amzn-RequestId`, on a 200
    stream and on an error."""
    run.call("request-id", MINISTRAL, body([user(text("Say ok."))], max_tokens=8))
    run.call("request-id-error", "mistral.nonexistent-v9:0", body([user(text("hi"))]))


def case_errors(run):
    run.call("bad-model", "mistral.nonexistent-v9:0", body([user(text("hi"))]))
    run.call("validation", MINISTRAL, body([user(text("hi"))], max_tokens=0))


CASES = {
    "plain": case_plain,
    "tool": case_tool,
    "reasoning": case_reasoning,
    "nova-high": case_nova_high,
    "replay": case_replay,
    "signature": case_signature,
    "cache": case_cache,
    "cache-turns": case_cache_turns,
    "cache-huge": case_cache_huge,
    "cache-nova": case_cache_nova,
    "redacted": case_redacted,
    "cache-after-result": case_cache_after_result,
    "tool-results": case_tool_results,
    "images": case_images,
    "shapes": case_shapes,
    "errors": case_errors,
    "classify": case_classify,
    "request-id": case_request_id,
}


def leaks(data, account):
    """Whether `data` holds the account id in clear, or inside any run of
    standard or URL-safe base64 between quotes."""
    if account.encode() in data:
        return True
    for piece in data.decode(errors="replace").replace("\\/", "/").split('"'):
        if len(piece) < 16:
            continue
        for decode in (base64.b64decode, base64.urlsafe_b64decode):
            try:
                if account.encode() in decode(piece + "=" * (-len(piece) % 4)):
                    return True
            except ValueError:
                pass
    return False


def rescrub(account):
    """Scrubs every saved stream and record again, in place, and checks no
    saved byte holds the account id."""
    names = [os.path.relpath(os.path.join(d, f), RAW) for d, _, fs in os.walk(RAW) for f in fs]
    for name in sorted(names):
        path = os.path.join(RAW, name)
        if name.endswith(".eventstream"):
            with open(path, "rb") as f:
                data = f.read()
            with open(path, "wb") as f:
                f.write(scrub(data, account))
    for name in names:
        with open(os.path.join(RAW, name), "rb") as f:
            data = f.read()
        assert not leaks(data, account), name
        found = UUID.findall(data.decode(errors="replace"))
        assert all(set(u) <= set("0-") for u in found), name


def main():
    cases = sys.argv[1:]
    if not PROFILE:
        print("probe.py: set PROBE_PROFILE to the AWS profile to sign with", file=sys.stderr)
        return 2
    unknown = [c for c in cases if c not in CASES and c != "rescrub"]
    if not cases or unknown:
        print("usage: probe.py rescrub | " + "|".join(CASES), file=sys.stderr)
        return 2
    os.makedirs(RAW, exist_ok=True)
    if cases == ["rescrub"]:
        rescrub(account_id())
        return 0
    run = Run()
    for case in cases:
        CASES[case](run)
    rescrub(run.account)
    return 0


if __name__ == "__main__":
    sys.exit(main())
