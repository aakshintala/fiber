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
# `notify-<tool>` file makes the server send `notifications/tools/list_changed`
# before it answers that tool's call. A `tools.json` file holds the `tools/list` answer, and makes
# `initialize` advertise the tools capability; one holding exactly
# `error` makes the server a prompt-only one: no tools capability, and
# `tools/list` answers JSON-RPC error -32601, as a server without tools
# support would. A `fail-start` file makes the server exit 1 before
# reading anything.
#
# A `prompts.json` file, a JSON array of prompt objects, each with `name`
# and optionally `description` and `arguments`, makes `initialize`
# advertise the prompts capability (`"prompts":{}` among its
# `capabilities`) and `prompts/list` answer `{"prompts":<file>}`. A
# `prompts.json` holding exactly `error` makes `prompts/list` answer
# JSON-RPC error -32603 instead. `prompts/get` reads the last `"name"` on the line (params encode with sorted keys,
# so the top-level `name` follows `arguments`, as for `tools/call`) and
# answers `prompt-<name>.json` as the `result`; a result file holding
# exactly `hang` is never answered, one holding exactly `exit` makes the
# server exit without answering, and a missing file answers -32602
# "Unknown prompt". A `cursor-forever` file makes every `tools/list`
# and `prompts/list` answer carry `"nextCursor":"again"`, at once. When
# the file holds a method name (`tools/list` or `prompts/list`), only
# that method's list pages forever: one handshake pages tools before
# prompts, so an endless prompt list needs tools pages to end. A
# `grandchild` file makes the server start `(trap '' TERM; exec sleep
# 3600) &` with stdout still on the pipe, before it reads, and write its
# pid to `grandchild.txt`: the grandchild ignores SIGTERM and holds
# stdout open past the server's exit, so only a signal to the server's
# process group takes it with the server. Its argv[0] is
# `<dir>/grandchild`, so a watchdog matching the directory finds it after
# the server exits.
#
# It answers `initialize`, `tools/list`, `tools/call`, `prompts/list`,
# `prompts/get` and `ping`, appends
# every received line to `requests.log`, and writes its working directory
# to `cwd.txt` and its pid to `pid.txt`. Anything else shaped as a request
# gets JSON-RPC error -32601; notifications get no answer.
set -u

dir="$1"
pwd -P > "$dir/cwd.txt"
printf '%s\n' "$$" > "$dir/pid.txt"
# A `grandchild` file starts a child that ignores SIGTERM and holds
# stdout open, before anything is read: stopping the server must take it
# with the server, through the server's process group.
if [ -f "$dir/grandchild" ]; then
    (trap '' TERM; exec -a "$dir/grandchild" sleep 3600) &
    printf '%s\n' "$!" > "$dir/grandchild.txt"
fi
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

pages_again() {
    # $1: the list method. Whether its answers carry another page: an
    # empty `cursor-forever` file pages every list, one holding a method
    # name only that method's list.
    [ -f "$dir/cursor-forever" ] || return 1
    [ -z "$(cat "$dir/cursor-forever")" ] && return 0
    [ "$(cat "$dir/cursor-forever")" = "$1" ]
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
            caps=""
            if [ -f "$dir/tools.json" ] && [ "$(cat "$dir/tools.json")" != "error" ]; then
                caps='"tools":{}'
            fi
            if [ -f "$dir/prompts.json" ]; then
                if [ -n "$caps" ]; then
                    caps="$caps,"
                fi
                caps="$caps\"prompts\":{}"
            fi
            answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{$caps},\"serverInfo\":{\"name\":\"fx\",\"version\":\"0.0.0\"}}}"
            ;;
        tools/list)
            if [ -f "$dir/tools.json" ] && [ "$(cat "$dir/tools.json")" = "error" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32601,\"message\":\"Method not found: tools/list\"}}"
            else
                if [ -f "$dir/tools.json" ]; then
                    tools="$(cat "$dir/tools.json")"
                else
                    tools="[]"
                fi
                if pages_again "tools/list"; then
                    answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":$tools,\"nextCursor\":\"again\"}}"
                else
                    answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":$tools}}"
                fi
            fi
            ;;
        prompts/list)
            if [ "$(cat "$dir/prompts.json")" = "error" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32603,\"message\":\"Internal error\"}}"
            else
                prompts="$(cat "$dir/prompts.json")"
                if pages_again "prompts/list"; then
                    answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"prompts\":$prompts,\"nextCursor\":\"again\"}}"
                else
                    answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"prompts\":$prompts}}"
                fi
            fi
            ;;
        prompts/get)
            # The last `"name"` on the line: our client encodes params
            # with sorted keys, so the prompt's top-level `name` sorts
            # after `arguments` and any `name` inside them.
            name="$(printf '%s' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')"
            result="$dir/prompt-$name.json"
            if [ -f "$result" ]; then
                if [ "$(cat "$result")" = "exit" ]; then
                    exit 0
                fi
                if [ "$(cat "$result")" != "hang" ]; then
                    body="$(cat "$result")"
                    answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$body}"
                fi
            elif [ -n "$id" ]; then
                answer "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32602,\"message\":\"Unknown prompt: $name\"}}"
            fi
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
