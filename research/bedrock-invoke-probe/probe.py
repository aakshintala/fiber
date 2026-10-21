"""Claude on AWS Bedrock's Invoke API, probed: the AWS event-stream framing
of a streamed reply, and the error shapes Bedrock sends. Standard library
only; run with `python3 -I probe.py <case>...`.

Credentials come from `aws configure export-credentials --profile $PROFILE
--format process` and are never printed or saved. Request headers are never
saved. Response headers keep only `content-type` and `x-amzn-errortype`. A
200 stream's bytes go to `raw/<case>.eventstream` exactly as they arrived;
the request body, status, kept headers, the decoded events and the cost go
to `raw/<case>.json`. The run stops before its estimated spend passes CAP.
"""

import base64
import datetime
import hashlib
import hmac
import json
import os
import struct
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
import zlib

# The AWS profile to read credentials from: required, so a run never
# signs with whatever profile happens to be the default.
PROFILE = os.environ.get("PROBE_PROFILE", "")
REGION = os.environ.get("PROBE_REGION", "ap-south-1")
MODEL = "global.anthropic.claude-haiku-4-5-20251001-v1:0"
# Haiku 4.5, per million tokens: input, output, cache read, cache write.
PRICE = {"input": 1.0, "output": 5.0, "cache_read": 0.10, "cache_write": 1.25}
CAP = 0.10
HERE = os.path.dirname(os.path.abspath(__file__))
SPENT = os.path.join(HERE, "raw", "spent.json")


def credentials():
    out = subprocess.run(
        ["aws", "configure", "export-credentials", "--profile", PROFILE, "--format", "process"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


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
    """Each event-stream message as (headers, payload), CRCs checked."""
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
    return out


def account_id():
    return subprocess.run(
        ["aws", "sts", "get-caller-identity", "--profile", PROFILE, "--query", "Account", "--output", "text"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def scrub(data, account):
    """`data` with the account id zeroed wherever a chunk carries it. A
    thinking block's signature holds the AWS account id in clear inside its
    base64, so each signature is decoded, zeroed and re-encoded at the same
    length, the chunk's `bytes` re-encoded at the same length, and both
    CRCs recomputed. Every other byte is kept."""
    secret = account.encode()
    zero = b"0" * len(secret)
    out = b""
    for headers, payload in frames(data):
        hbytes = b""
        for name, value in headers.items():
            hbytes += bytes([len(name)]) + name.encode() + b"\x07" + struct.pack(">H", len(value)) + value.encode()
        if headers.get(":event-type") == "chunk":
            wrapper = json.loads(payload)
            event = base64.b64decode(wrapper["bytes"])
            parsed = json.loads(event)
            sig = parsed.get("delta", {}).get("signature") or parsed.get("content_block", {}).get("signature")
            if sig:
                clean = base64.b64encode(base64.b64decode(sig).replace(secret, zero)).decode()
                assert len(clean) == len(sig)
                event = event.replace(sig.encode(), clean.encode())
            event = event.replace(secret, zero)
            encoded = base64.b64encode(event).decode()
            assert len(encoded) == len(wrapper["bytes"])
            payload = payload.replace(wrapper["bytes"].encode(), encoded.encode())
        total = 16 + len(hbytes) + len(payload)
        prelude = struct.pack(">II", total, len(hbytes))
        prelude += struct.pack(">I", zlib.crc32(prelude))
        body = prelude + hbytes + payload
        out += body + struct.pack(">I", zlib.crc32(body))
    assert len(out) == len(data)
    return out


def events(data):
    out = []
    for headers, payload in frames(data):
        if headers.get(":event-type") == "chunk":
            out.append(json.loads(base64.b64decode(json.loads(payload)["bytes"])))
        else:
            out.append({"frame_headers": headers, "payload": payload.decode(errors="replace")})
    return out


def cost(evs):
    usage = {}
    for e in evs:
        if e.get("type") == "message_start":
            usage.update(e["message"].get("usage", {}))
        if e.get("type") == "message_delta":
            usage.update(e.get("usage", {}))
    dollars = (
        usage.get("input_tokens", 0) * PRICE["input"]
        + usage.get("output_tokens", 0) * PRICE["output"]
        + usage.get("cache_read_input_tokens", 0) * PRICE["cache_read"]
        + usage.get("cache_creation_input_tokens", 0) * PRICE["cache_write"]
    ) / 1e6
    return usage, dollars


WEATHER = {
    "name": "get_weather",
    "description": "Weather for a city.",
    "input_schema": {
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"],
        "additionalProperties": False,
    },
}

# About 4,600 tokens of system text: past Haiku 4.5's 4,096-token minimum
# for a cache breakpoint.
LONG_SYSTEM = "You are a terse assistant. " + " ".join(
    f"Rule {n}: answer in as few words as the question allows." for n in range(1, 420)
)


def base(**extra):
    body = {"anthropic_version": "bedrock-2023-05-31", "max_tokens": 64}
    body.update(extra)
    return body


CASES = {
    "plain": lambda: base(messages=[{"role": "user", "content": "Say hello in five words."}]),
    "tool": lambda: base(
        max_tokens=256,
        tools=[WEATHER],
        messages=[
            {
                "role": "user",
                "content": "What is the weather in Paris and in Rome? Call the tool once per city, both in this reply.",
            }
        ],
    ),
    "thinking": lambda: base(
        max_tokens=2048,
        thinking={"type": "enabled", "budget_tokens": 1024},
        messages=[{"role": "user", "content": "What is 17 * 23? Answer with the number."}],
    ),
    "cache-write": lambda: base(
        max_tokens=16,
        system=[{"type": "text", "text": LONG_SYSTEM, "cache_control": {"type": "ephemeral"}}],
        messages=[{"role": "user", "content": "Say ok."}],
    ),
    "cache-read": lambda: base(
        max_tokens=16,
        system=[{"type": "text", "text": LONG_SYSTEM, "cache_control": {"type": "ephemeral"}}],
        messages=[{"role": "user", "content": "Say ok."}],
    ),
    "ttl-1h": lambda: base(
        max_tokens=16,
        system=[{"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
        messages=[{"role": "user", "content": [{"type": "text", "text": "Say ok.", "cache_control": {"type": "ephemeral", "ttl": "1h"}}]}],
    ),
    "adaptive": lambda: base(
        max_tokens=512,
        thinking={"type": "adaptive"},
        output_config={"effort": "low"},
        messages=[{"role": "user", "content": "What is 17 * 23? Answer with the number."}],
    ),
    "bad-model": lambda: base(messages=[{"role": "user", "content": "hi"}]),
    "validation": lambda: base(max_tokens=0, messages=[{"role": "user", "content": "hi"}]),
    "bad-signature": lambda: base(messages=[{"role": "user", "content": "hi"}]),
    "bad-key-id": lambda: base(messages=[{"role": "user", "content": "hi"}]),
    "bad-bearer": lambda: base(messages=[{"role": "user", "content": "hi"}]),
}


def run(case, creds, account, spent):
    model = {
        "bad-model": "global.anthropic.claude-nonexistent-v9:0",
        "adaptive": "global.anthropic.claude-haiku-5-5",
    }.get(case, MODEL)
    path = "/model/" + urllib.parse.quote(model, safe="") + "/invoke-with-response-stream"
    url = f"https://bedrock-runtime.{REGION}.amazonaws.com{path}"
    body = json.dumps(CASES[case]()).encode()
    headers = {"content-type": "application/json", "accept": "application/vnd.amazon.eventstream"}
    now = datetime.datetime.now(datetime.timezone.utc)
    used = dict(creds)
    if case == "bad-signature":
        used["SecretAccessKey"] = "x" * 40
    if case == "bad-key-id":
        used["AccessKeyId"] = "AKIA" + "Q" * 16
    if case == "bad-bearer":
        headers["authorization"] = "Bearer ABSKQmVkcm9ja0FQSUtleS1ub3RhcmVhbGtleQ=="
    else:
        headers.update(sign("POST", url, headers, body, used, now))
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            status, raw, got = response.status, response.read(), response.headers
    except urllib.error.HTTPError as e:
        status, raw, got = e.code, e.read(), e.headers
    kept = {k: v for k, v in got.items() if k.lower() in ("content-type", "x-amzn-errortype")}
    record = {
        "case": case,
        "model": model,
        "region": REGION,
        "url_path": path,
        "request": json.loads(body),
        "status": status,
        "headers": kept,
    }
    dollars = 0.0
    if status == 200:
        raw = scrub(raw, account)
        with open(os.path.join(HERE, "raw", f"{case}.eventstream"), "wb") as f:
            f.write(raw)
        evs = events(raw)
        usage, dollars = cost(evs)
        record["events"] = evs
        record["usage"] = usage
        record["cost_usd"] = round(dollars, 6)
    else:
        record["body"] = raw.decode(errors="replace").replace(account, "0" * len(account))
    with open(os.path.join(HERE, "raw", f"{case}.json"), "w") as f:
        json.dump(record, f, indent=2)
        f.write("\n")
    spent += dollars
    print(f"{case}: HTTP {status} {kept.get('x-amzn-errortype', '')} ${dollars:.6f} (total ${spent:.6f})")
    return spent


def rescrub():
    """Scrubs every saved stream and record again, in place."""
    account = account_id()
    for name in sorted(os.listdir(os.path.join(HERE, "raw"))):
        path = os.path.join(HERE, "raw", name)
        if name.endswith(".eventstream"):
            with open(path, "rb") as f:
                data = f.read()
            clean = scrub(data, account)
            for event in events(clean):
                sig = event.get("delta", {}).get("signature") or event.get("content_block", {}).get("signature")
                if sig:
                    assert account.encode() not in base64.b64decode(sig), name
                assert account not in json.dumps(event), name
            with open(path, "wb") as f:
                f.write(clean)
            with open(path[: -len(".eventstream")] + ".json") as f:
                record = json.load(f)
            record["events"] = events(clean)
            with open(path[: -len(".eventstream")] + ".json", "w") as f:
                json.dump(record, f, indent=2)
                f.write("\n")
    for name in os.listdir(os.path.join(HERE, "raw")):
        with open(os.path.join(HERE, "raw", name), "rb") as f:
            assert account.encode() not in f.read(), name


def main():
    cases = sys.argv[1:]
    if not PROFILE:
        print("probe.py: set PROBE_PROFILE to the AWS profile to sign with", file=sys.stderr)
        return 2
    if cases == ["rescrub"]:
        rescrub()
        return 0
    unknown = [c for c in cases if c not in CASES]
    if not cases or unknown:
        print("usage: probe.py rescrub | " + "|".join(CASES), file=sys.stderr)
        return 2
    spent = 0.0
    if os.path.exists(SPENT):
        with open(SPENT) as f:
            spent = json.load(f)["usd"]
    creds = credentials()
    account = account_id()
    for case in cases:
        if spent >= CAP:
            print(f"stopping: ${spent:.6f} spent, cap ${CAP}", file=sys.stderr)
            return 1
        spent = run(case, creds, account, spent)
        with open(SPENT, "w") as f:
            json.dump({"usd": round(spent, 6)}, f)
            f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
