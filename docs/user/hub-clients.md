# Talking to the hub

Any program that can hold a socket can drive Fiber: a script, a phone app, or anything else its person writes. It starts sessions, subscribes to their event streams, and sends prompts. The hub speaks the same lines as a session's socket, one JSON object per line ([What the hub speaks](../invocation.md#what-the-hub-speaks)).

## Reaching the hub

On the hub's own machine, open the local socket `run/hub` in Fiber home ([What each part holds](../state.md#what-each-part-holds)). No token travels there.

From another machine, either forward that socket over SSH and add it as a `unix:` address, or connect to the hub's port over a websocket and [`pair`](../invocation.md#remote-clients) first ([Remote clients](../invocation.md#remote-clients)).

## The first exchange

Every connection opens with a [`hub_hello`](../events.md#the-envelope) line carrying the hub's [`schema_version`](../events.md#the-envelope) and Fiber version ([The envelope](../events.md#the-envelope)). On the port, the connection's first command is [`authenticate`](../invocation.md#remote-clients) with its device token, or [`pair`](../invocation.md#remote-clients) with a pairing code ([Remote clients](../invocation.md#remote-clients)). Anything else there is answered [`unauthenticated`](../invocation.md#remote-clients) and the connection closes; a wrong, used, or old pairing code is rejected [`pairing_failed`](../invocation.md#remote-clients).

## Worked example

The script below opens `run/hub`, starts a session with [`start`](../invocation.md#what-the-hub-speaks), subscribes [`full`](../invocation.md#driver-commands) to it, and reads events until the turn completes. [`start`](../invocation.md#what-the-hub-speaks) is a [hub command](../invocation.md#what-the-hub-speaks); [`subscribe`](../invocation.md#driver-commands) is a [driver command](../invocation.md#driver-commands), and every answer is [`command_accepted` or `command_rejected`](../events.md#command-acknowledgements). A command for a session the connection has not subscribed to is rejected [`not_subscribed`](../invocation.md#driver-commands). The example names the scripted provider so it runs with no vendor credential; swap in any configured model ([The scripted provider](../model-routing.md#the-scripted-provider)).

```python
#!/usr/bin/env python3
"""Start a session through the hub and read one turn."""
import json, socket, sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(sys.argv[1])  # $FIBER_HOME/run/hub
stream = sock.makefile("r")

def send(obj):
    sock.sendall((json.dumps(obj) + "\n").encode())

print(json.loads(next(stream))["kind"])  # hub_hello
send({"id": "c_start", "command": "start", "args": {
    "workspace": sys.argv[2], "model": "scripted/hello.json",
    "content": [{"type": "text", "text": "Say hello."}]}})
for raw in stream:
    line = json.loads(raw)
    if line.get("kind") == "command_accepted" \
            and line["payload"].get("command_id") == "c_start":
        session = line["payload"]["result"]["session_id"]
        break
send({"id": "c_sub", "session_id": session,
      "command": "subscribe", "args": {"level": "full"}})
for raw in stream:
    line = json.loads(raw)
    print(line["kind"])
    if line.get("kind") == "turn_completed":
        break
```

Against a running hub the script prints one kind per line, in this order, exactly as the hub sent them: [`hub_hello`](../events.md#the-envelope), [`command_accepted`](../events.md#command-acknowledgements), [`command_accepted`](../events.md#command-acknowledgements), …, [`turn_completed`](../events.md#turn_completed).

The second [`command_accepted`](../events.md#command-acknowledgements) opens the replay: the lines up to its [`last_seq`](../events.md#command-acknowledgements) follow it, then the live stream runs to [`turn_completed`](../events.md#turn_completed).

Every line but the hub's own carries its session, with the envelope of [The envelope](../events.md#the-envelope).
