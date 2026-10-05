"""OpenCode Go and Zen probe. Reads the key from OPENCODE_API_KEY and never prints it.

Usage: python3 probe.py <step>   (steps below; each appends to raw/<step>.json)
"""
import os
import json, sys, time, urllib.request, urllib.error, uuid

KEY = os.environ['OPENCODE_API_KEY']
GO = 'https://opencode.ai/zen/go'
ZEN = 'https://opencode.ai/zen'
SESSION = 'fiber-probe-' + uuid.uuid4().hex[:8]

TOOL_RESP = {"type": "function", "name": "get_weather", "description": "Weather for a city.",
             "parameters": {"type": "object", "properties": {"city": {"type": "string"}},
                            "required": ["city"], "additionalProperties": False}, "strict": True}


def call(method, url, body=None, headers=None, stream=False):
    h = {'Authorization': 'Bearer ' + KEY, 'Content-Type': 'application/json',
         'x-opencode-session': SESSION,
         # Cloudflare answers 403 "error code: 1010" to Python's default User-Agent.
         'User-Agent': 'fiber-probe/0.1'}
    h.update(headers or {})
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers=h)
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            raw = r.read()
            status, rh = r.status, dict(r.headers)
    except urllib.error.HTTPError as e:
        raw, status, rh = e.read(), e.code, dict(e.headers)
    rec = {'method': method, 'url': url, 'request': body, 'status': status,
           'headers': {k: v for k, v in rh.items() if k.lower() not in ('set-cookie',)},
           'elapsed_s': round(time.time() - t, 2), 'body': raw.decode('utf-8', 'replace')}
    return rec


def save(step, recs):
    s = json.dumps(recs, indent=1)
    assert KEY not in s
    open(f'raw/{step}.json', 'w').write(s)
    for r in recs:
        print(r['method'], r['url'], r['status'], r['body'][:300].replace('\n', ' '))


def models():
    save('models', [call('GET', GO + '/v1/models'), call('GET', ZEN + '/v1/models')])


def go_protocols():
    m = 'muse-spark-1.3-contributor'
    recs = [
        call('POST', GO + '/v1/responses', {"model": m, "input": "Say hi in one word.", "max_output_tokens": 64}),
        call('POST', GO + '/v1/chat/completions', {"model": m, "messages": [{"role": "user", "content": "Say hi in one word."}], "max_tokens": 64}),
        call('POST', GO + '/v1/messages', {"model": m, "messages": [{"role": "user", "content": "Say hi in one word."}], "max_tokens": 64},
             {'anthropic-version': '2023-06-01', 'x-api-key': KEY}),
        # A Go model pi lists under openai-completions, for cost/usage comparison.
        call('POST', GO + '/v1/chat/completions', {"model": "glm-5.3-flash", "messages": [{"role": "user", "content": "Say hi in one word."}], "max_tokens": 64}),
        # A Go model pi lists under anthropic-messages.
        call('POST', GO + '/v1/messages', {"model": "qwen3.8-flash", "messages": [{"role": "user", "content": "Say hi in one word."}], "max_tokens": 64},
             {'anthropic-version': '2023-06-01', 'x-api-key': KEY}),
    ]
    save('go_protocols', recs)


def go_stream_tools():
    m = 'muse-spark-1.3-contributor'
    body = {"model": m, "stream": True, "tools": [TOOL_RESP],
            "input": [{"role": "user", "content": "What is the weather in Paris? Use the tool."}]}
    first = call('POST', GO + '/v1/responses', body)
    recs = [first]
    # Find the function call in the SSE stream and send its result back.
    fc = None
    for line in first['body'].splitlines():
        if line.startswith('data: ') and '"function_call"' in line:
            try:
                ev = json.loads(line[6:])
            except ValueError:
                continue
            item = ev.get('item') or {}
            if item.get('type') == 'function_call' and ev.get('type') == 'response.output_item.done':
                fc = item
    if fc:
        body2 = {"model": m, "stream": True, "tools": [TOOL_RESP],
                 "input": [{"role": "user", "content": "What is the weather in Paris? Use the tool."},
                           {"type": "function_call", "call_id": fc['call_id'], "name": fc['name'], "arguments": fc['arguments']},
                           {"type": "function_call_output", "call_id": fc['call_id'], "output": "18 C, clear"}]}
        recs.append(call('POST', GO + '/v1/responses', body2))
    save('go_stream_tools', recs)
    # Raw SSE bytes for the provider tests.
    for i, r in enumerate(recs):
        open(f'raw/go_stream_tools_{i}.sse', 'w').write(r['body'])


def zen():
    # Cheapest priced model in the live Zen list; one tiny request on the shared key.
    recs = [call('POST', ZEN + '/v1/responses', {"model": "gpt-6-luna", "input": "Say hi in one word.", "max_output_tokens": 16}),
            # muse-spark-1.3-contributor on the Zen URL: is the Go model reachable there?
            call('POST', ZEN + '/v1/responses', {"model": "muse-spark-1.3-contributor", "input": "Say hi in one word.", "max_output_tokens": 16})]
    save('zen', recs)


def usage():
    save('usage', [call('GET', GO + '/v1/usage')])


if __name__ == '__main__':
    globals()[sys.argv[1]]()
