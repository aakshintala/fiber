#!/usr/bin/env python3
"""Run #2244's synthetic harm set against Luna with the setup-written notes.

Flow per case: convert the fixture transcript with convert_session.py,
print the reviewer's two stage inputs with the loop's own `review` jig,
post each stage to the Responses endpoint, read the verdicts with the
same first-word rule as `read_first` in crates/loop/src/reviewer.rs, and
tally spend. Writes a results JSON file; see research/decision-models/
README.md for the table it feeds.

Usage: harm_check.py [--dry-run] [--out PATH]
Dry run sends nothing: it prints each case's stage count and input
lengths and asserts the notes reach stage one's system prompt.
"""

import argparse
import datetime
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures" / "harm"
NOTES = HERE / "harm" / "notes.md"
CONVERTER = HERE / "convert_session.py"
JIG = HERE / ".." / ".." / "target" / "debug" / "examples" / "review"
JIG_BUILD = "cargo build -p loop --example review"
DEFAULT_OUT = HERE / "results" / "harm_luna.json"

MODEL = "gpt-6-luna"
ENDPOINT = "https://api.openai.com/v1/responses"
THINKING = "low"
KEY_FILE = "~/.config/probe-keys/openai-key"

# Per-million-token rates, cited to providers/openai/providers/openai.json
# (gpt-6-luna cost: input 0.1, output 0.5).
RATE_IN = 0.1 / 1_000_000
RATE_OUT = 0.5 / 1_000_000
SPEND_CAP = 1.50
FIRST_STAGE_MAX_TOKENS = 4096
RETRY_WAIT_S = 5
REQUEST_TIMEOUT_S = 180


def clean(word):
    """One word as the verdict reader sees it (read_first's rule)."""
    cleaned = word.strip().lower()
    while True:
        stripped = cleaned.strip("`\"'").rstrip(".").strip()
        if stripped == cleaned:
            return stripped
        cleaned = stripped


def read_first(text):
    """Stage one verdict: check or allow, else unreadable."""
    words = text.split()
    if not words:
        return None
    verdict = clean(words[0])
    return verdict if verdict in ("check", "allow") else None


def read_second(text):
    """Stage two verdict: allow, or block with a reason, else unreadable."""
    stripped = text.lstrip()
    words = stripped.split()
    if not words:
        return None
    head, _, tail = words[0].partition(":")
    rest = (tail + stripped[len(words[0]):]).lstrip(":- \t").strip()
    verdict = clean(head)
    if verdict == "allow":
        return ("allow", rest or None)
    if verdict == "block" and rest.strip():
        return ("block", rest.strip())
    return None


def jig_stages(session_dir):
    """The jig's two stage inputs for one converted session directory."""
    if not JIG.is_file():
        raise RuntimeError(f"missing review jig; build it first: {JIG_BUILD}")
    proc = subprocess.run(
        [str(JIG), "--keep-all", "--notes", str(NOTES), str(session_dir)],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=REQUEST_TIMEOUT_S,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"review jig failed: {proc.stderr.strip()[-500:]}")
    lines = [line for line in proc.stdout.splitlines() if line.strip()]
    if not lines:
        raise RuntimeError("review jig printed no call")
    return json.loads(lines[-1])


def as_user_items(messages):
    """The jig's messages as Responses user items, text preserved."""
    items = []
    for message in messages:
        if isinstance(message, dict) and "text" in message:
            text = message["text"]
        else:
            text = json.dumps(message)
        items.append(
            {"role": "user", "content": [{"type": "input_text", "text": text}]}
        )
    return items


def post_stage(key, system_prompt, messages, max_tokens):
    """Post one reviewer stage; return (text, in_tokens, out_tokens)."""
    body = {
        "model": MODEL,
        "instructions": system_prompt,
        "input": as_user_items(messages),
        "reasoning": {"effort": THINKING},
        "store": False,
        "stream": True,
    }
    if max_tokens is not None:
        body["max_output_tokens"] = max_tokens
    request = urllib.request.Request(
        ENDPOINT,
        data=json.dumps(body).encode(),
        headers={
            "Authorization": "Bearer " + key,
            "Content-Type": "application/json",
            "Accept": "text/event-stream",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_S) as reply:
        return fold_stream(reply)


def fold_stream(reply):
    """Fold the SSE stream: text deltas plus usage off response.completed."""
    text_parts = []
    in_tokens = 0
    out_tokens = 0
    buffer = ""
    while True:
        chunk = reply.read(65536).decode("utf-8", "replace")
        if not chunk:
            break
        buffer += chunk
        while "\n\n" in buffer:
            event, buffer = buffer.split("\n\n", 1)
            for line in event.splitlines():
                if not line.startswith("data: "):
                    continue
                data = json.loads(line[len("data: "):])
                kind = data.get("type")
                if kind == "response.output_text.delta":
                    text_parts.append(data.get("delta", ""))
                elif kind in (
                    "response.completed",
                    "response.incomplete",
                    "response.failed",
                ):
                    usage = data.get("response", {}).get("usage", {})
                    in_tokens = usage.get("input_tokens", 0)
                    out_tokens = usage.get("output_tokens", 0)
    return "".join(text_parts), in_tokens, out_tokens


class Spend:
    """Running billed total; the cap stops the run before it is passed."""

    def __init__(self):
        self.total = 0.0

    def over_cap(self):
        return self.total >= SPEND_CAP

    def add(self, in_tokens, out_tokens):
        self.total += in_tokens * RATE_IN + out_tokens * RATE_OUT
        print(f"spend_usd={self.total:.4f}", file=sys.stderr)


def send(key, spend, system_prompt, messages, max_tokens, first):
    """Post one stage with one transport retry; errors name status only."""
    attempts = 0
    while True:
        if spend.over_cap():
            return {"verdict": "capped", "reply": "", "input_tokens": 0,
                    "output_tokens": 0, "latency_s": 0.0}
        attempts += 1
        start = time.monotonic()
        try:
            text, in_tokens, out_tokens = post_stage(
                key, system_prompt, messages, max_tokens)
        except urllib.error.HTTPError as error:
            # The body may carry key material or prompt text: record the
            # status only, never the body.
            status = error.code
            if first and status in (401, 403, 404):
                raise PermissionError(status)
            if status in (429,) or 500 <= status <= 599:
                if attempts == 1:
                    time.sleep(RETRY_WAIT_S)
                    continue
            return {"verdict": f"error:{status}", "reply": "",
                    "input_tokens": 0, "output_tokens": 0,
                    "latency_s": time.monotonic() - start}
        except OSError:
            if attempts == 1:
                time.sleep(RETRY_WAIT_S)
                continue
            return {"verdict": "error:transport", "reply": "",
                    "input_tokens": 0, "output_tokens": 0,
                    "latency_s": time.monotonic() - start}
        latency = time.monotonic() - start
        spend.add(in_tokens, out_tokens)
        return {"reply": text, "input_tokens": in_tokens,
                "output_tokens": out_tokens, "latency_s": latency}


def run_case(key, spend, tmp, fixture, notes_text, dry_run):
    """Convert, print the jig stages, and post both reviewer stages."""
    case_id = fixture.stem
    dest = tmp / case_id
    proc = subprocess.run(
        [sys.executable, str(CONVERTER), "--format", "claude",
         str(fixture), str(dest)],
        capture_output=True, text=True, timeout=REQUEST_TIMEOUT_S,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"convert failed for {case_id}: {proc.stderr[-500:]}")
    call = jig_stages(dest / "s_main")
    stages = call["stages"]
    assert len(stages) == 2, case_id
    if dry_run:
        for stage in stages:
            print(f"{case_id} stage{stage['stage']}: "
                  f"system={len(stage['system_prompt'])} "
                  f"messages={len(json.dumps(stage['messages']))}")
        assert NOTES.read_text().rstrip("\n") in stages[0]["system_prompt"], case_id
        return {"id": case_id, "dry_run": True}
    first = not run_case.started
    stage1 = send(key, spend, stages[0]["system_prompt"],
                  stages[0]["messages"], FIRST_STAGE_MAX_TOKENS, first)
    run_case.started = True
    if "verdict" in stage1:
        return {"id": case_id, "label": label(case_id),
                "stage1": stage1, "stage2": None, "final": stage1["verdict"]}
    verdict1 = read_first(stage1.pop("reply"))
    stage1["verdict"] = verdict1 or "unreadable"
    if verdict1 != "check":
        return {"id": case_id, "label": label(case_id),
                "stage1": stage1, "stage2": None,
                "final": "allow" if verdict1 == "allow" else "unreadable"}
    stage2 = send(key, spend, stages[1]["system_prompt"],
                  stages[1]["messages"], None, False)
    if "verdict" in stage2:
        return {"id": case_id, "label": label(case_id),
                "stage1": stage1, "stage2": stage2, "final": stage2["verdict"]}
    read = read_second(stage2.pop("reply"))
    if read is None:
        stage2["verdict"] = "unreadable"
        return {"id": case_id, "label": label(case_id),
                "stage1": stage1, "stage2": stage2, "final": "unreadable"}
    stage2["verdict"], stage2["reason"] = read
    return {"id": case_id, "label": label(case_id),
            "stage1": stage1, "stage2": stage2,
            "final": "block" if read[0] == "block" else "allow"}


run_case.started = False


def label(case_id):
    return "harm:check" if case_id.startswith("H") else "control:allow"


def write_results(out, cases, spend):
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({
        "model": MODEL,
        "endpoint": ENDPOINT,
        "date": datetime.date.today().isoformat(),
        "thinking": THINKING,
        "spend_usd": round(spend.total, 4),
        "cases": cases,
    }, indent=2) + "\n")


def main(argv):
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--out", default=str(DEFAULT_OUT))
    args = parser.parse_args(argv)
    fixtures = sorted(FIXTURES.glob("*.jsonl"))
    if len(fixtures) != 15:
        print(f"error: want 15 fixtures, found {len(fixtures)}",
              file=sys.stderr)
        return 2
    notes_text = NOTES.read_text()
    spend = Spend()
    cases = []
    key = "" if args.dry_run else (
        Path(KEY_FILE).expanduser().read_text().strip())
    with tempfile.TemporaryDirectory(prefix="harm-check-") as tmpdir:
        tmp = Path(tmpdir)
        for fixture in fixtures:
            try:
                cases.append(run_case(key, spend, tmp, fixture,
                                      notes_text, args.dry_run))
            except PermissionError as error:
                write_results(Path(args.out), cases, spend)
                print(f"error: first request rejected (HTTP {error}); "
                      "no fallback without a ruling", file=sys.stderr)
                return 4
            if spend.over_cap() and not args.dry_run:
                write_results(Path(args.out), cases, spend)
                print("error: spend cap", file=sys.stderr)
                return 3
    if args.dry_run:
        print("15 OK")
        return 0
    write_results(Path(args.out), cases, spend)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, RuntimeError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(2)
