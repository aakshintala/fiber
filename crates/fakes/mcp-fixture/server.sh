#!/bin/bash
# A fake MCP stdio server for tests (`docs/testing.md`, "Fakes"). It works
# on bash 3.2 and BSD sed: no associative arrays, no GNU extensions.
#
# Usage: server.sh <dir>
#
# <dir> holds `tools.json`, a JSON array of tool objects, each with `name`
# and optionally `description`, `inputSchema` and `annotations`, and one
# `call-<tool>.json` file per tool holding that call's `result` object. A
# tool whose result file holds exactly `hang` is never answered, and one
# whose result file holds exactly `exit` makes the server exit without
# answering. If `release-<tool>` is a FIFO, the server holds that tool's
# answer: in the background subshell, if `held-<tool>` is also a FIFO it
# first writes one line to it (`printf 'held\n' > "$dir/held-<tool>"`),
# then blocks on `read -r _ < "$dir/release-<tool>"`, then answers. No
# sleep, no polling. A tool without a release FIFO answers at once. A
# `notify-<tool>`
# file makes the server send `notifications/tools/list_changed` before it
# answers that tool's call. A `fail-start` file makes the server exit 1
# before reading anything.
#
# It answers `initialize`, `tools/list`, `tools/call` and `ping`, appends
# every received line to `requests.log`, and writes its working directory
# to `cwd.txt` and its pid to `pid.txt`. Anything else shaped as a request
# gets JSON-RPC error -32601; notifications get no answer.
set -u

dir="$1"
pwd -P > "$dir/cwd.txt"
printf '%s\n' "$$" > "$dir/pid.txt"
if [ -f "$dir/fail-start" ]; then
    exit 1
fi
# A `noise` file's lines go to stdout first, so a test can prove a
# non-JSON line is ignored.
if [ -f "$dir/noise" ]; then
    cat "$dir/noise"
fi
# A `ping-on-start` file makes the server send one `ping` with a string id
# before reading: the client must answer `{"result":{}}` echoing that id,
# and any other method gets `-32601`. The answers land in `requests.log`.
if [ -f "$dir/ping-on-start" ]; then
    printf '%s\n' '{"jsonrpc":"2.0","id":"probe","method":"ping"}'
    printf '%s\n' '{"jsonrpc":"2.0","id":"bogus","method":"no-such-method"}'
fi

pick() {
    # $1: the line, $2: the key. Prints the first string value of the key.
    printf '%s' "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p"
}

id_of() {
    printf '%s' "$1" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'
}

answer() {
    # $1: the response line. Short lines write atomically, so background
    # answers never interleave.
    printf '%s\n' "$1"
}

while IFS= read -r line; do
    printf '%s\n' "$line" >> "$dir/requests.log"
    method="$(pick "$line" method)"
    id="$(id_of "$line")"
    case "$method" in
        initialize)
            answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fx\",\"version\":\"0.0.0\"}}}"
            ;;
        tools/list)
            tools="$(cat "$dir/tools.json")"
            answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":$tools}}"
            ;;
        tools/call)
            # The last `"name"` on the line: our client encodes params
            # with sorted keys, so the tool's top-level `name` sorts after
            # `arguments` and any `name` inside them.
            tool="$(printf '%s' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')"
            result="$dir/call-$tool.json"
            if [ -f "$result" ]; then
                if [ "$(cat "$result")" = "exit" ]; then
                    exit 0
                fi
                if [ -f "$dir/notify-$tool" ]; then
                    answer '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}'
                fi
                if [ "$(cat "$result")" != "hang" ]; then
                    body="$(cat "$result")"
                    # Each call is answered in the background, so a held
                    # call does not hold back a fast one behind it.
                    ( if [ -p "$dir/release-$tool" ]; then if [ -p "$dir/held-$tool" ]; then printf 'held\n' > "$dir/held-$tool"; fi; read -r _ < "$dir/release-$tool"; fi; answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$body}" ) &
                fi
            elif [ -n "$id" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32602,\"message\":\"Unknown tool: $tool\"}}"
            fi
            ;;
        ping)
            if [ -n "$id" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{}}"
            fi
            ;;
        "")
            # A notification, such as `notifications/initialized`: no answer.
            ;;
        *)
            if [ -n "$id" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32601,\"message\":\"Method not found: $method\"}}"
            fi
            ;;
    esac
done
