"""One billed Zen request to a Gemini model over google-generative-ai (#1642).
Key read from ~/.config/probe-keys/opencode-key, never written out. One request, no retry."""
import json, os, urllib.request, urllib.error
KEY = open(os.path.expanduser('~/.config/probe-keys/opencode-key')).read().strip()
MODEL = 'gemini-3.5-flash-lite'
URL = f'https://opencode.ai/zen/v1/models/{MODEL}:generateContent'
body = {'contents': [{'role': 'user', 'parts': [{'text': 'Reply with the single word: ok'}]}],
        'generationConfig': {'maxOutputTokens': 16}}
h = {'content-type': 'application/json', 'user-agent': 'fiber-probe/0.1',
     'x-goog-api-key': KEY, 'authorization': 'Bearer ' + KEY, 'x-opencode-session': 'fiber-probe-1642'}
req = urllib.request.Request(URL, json.dumps(body).encode(), h)
try:
    r = urllib.request.urlopen(req, timeout=60); status = r.status; hd = dict(r.headers); txt = r.read().decode()
except urllib.error.HTTPError as e:
    status = e.code; hd = dict(e.headers); txt = e.read().decode()
assert KEY not in txt
hd = {k: v for k, v in hd.items() if k.lower() in ('content-type', 'retry-after', 'x-should-retry')}
out = {'url': URL, 'status': status, 'request_headers': sorted(h), 'request': body, 'response_headers': hd, 'body': json.loads(txt) if txt.startswith(('{', '[')) else txt}
s = json.dumps(out, indent=1)
assert KEY not in s
open('research/opencode-zen-gemini-probe/raw/generate.json', 'w').write(s)
print(status)
