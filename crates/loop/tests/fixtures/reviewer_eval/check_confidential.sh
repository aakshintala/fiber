#!/bin/sh
# Scans reviewer-eval set files for confidential values: the lane's home
# directory, owner name and email, macOS and Linux home literals,
# session-id shapes, the closed secret matchers, and lane-supplied host,
# profile and project literals. Prints `CONFIDENTIAL <class> id=<id>` per
# hit line and exits 1 on any hit, 0 when clean. Without lane-supplied
# values only the fixed classes (home literals, session ids, secrets) run.
#
#   check_confidential.sh [--home PATH] [--name NAME] [--email EMAIL]
#     [--host HOST]... [--profile PROFILE]... [--project PROJECT]... FILE...
#
# Every value the scan needs is a committed byte or an explicit flag: no
# environment reads, no network, no clock. Values hold no tabs or newlines.

set -eu
export LC_ALL=C

usage() {
    echo "usage: check_confidential.sh [--home PATH] [--name NAME] [--email EMAIL] [--host HOST]... [--profile PROFILE]... [--project PROJECT]... FILE..." >&2
    exit 2
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
NEEDLES="$TMP/needles"
FILES="$TMP/files"
: > "$NEEDLES"
: > "$FILES"

needle() {
    if [ -n "$2" ]; then
        printf '%s\t%s\n' "$1" "$2" >> "$NEEDLES"
    fi
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --home) needle "home" "$2"; shift 2 ;;
        --name) needle "owner" "$2"; shift 2 ;;
        --email) needle "owner" "$2"; shift 2 ;;
        --host) needle "host" "$2"; shift 2 ;;
        --profile) needle "profile" "$2"; shift 2 ;;
        --project) needle "project" "$2"; shift 2 ;;
        --) shift; while [ "$#" -gt 0 ]; do printf '%s\n' "$1" >> "$FILES"; shift; done ;;
        -*) usage ;;
        *) printf '%s\n' "$1" >> "$FILES"; shift ;;
    esac
done

if [ ! -s "$FILES" ]; then
    usage
fi

UUID='[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}'
AWS_KEY='AKIA[0-9A-Za-z]{16}'
PRIVATE_KEY='BEGIN.*PRIVATE KEY'
GITHUB_TOKEN='ghp_|gho_|github_pat_'
ANTHROPIC_KEY='sk-ant-'
SLACK_TOKEN='xox[baprs]-'
GOOGLE_KEY='AIza'

fail=0
report() {
    printf 'CONFIDENTIAL %s id=%s\n' "$1" "$2"
    fail=1
}

while IFS= read -r file; do
    while IFS= read -r line || [ -n "$line" ]; do
        id="$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
        case "$line" in
            *"/Users/"*) report "macos-home" "$id" ;;
        esac
        case "$line" in
            *"/home/"*) report "linux-home" "$id" ;;
        esac
        while IFS= read -r needle_line || [ -n "$needle_line" ]; do
            class="${needle_line%%	*}"
            literal="${needle_line#*	}"
            if printf '%s\n' "$line" | grep -F -q -- "$literal"; then
                report "$class" "$id"
            fi
        done < "$NEEDLES"
        if printf '%s\n' "$line" | grep -E -q -- "$UUID"; then
            report "session-id" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$AWS_KEY"; then
            report "aws-key" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$PRIVATE_KEY"; then
            report "private-key" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$GITHUB_TOKEN"; then
            report "github-token" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$ANTHROPIC_KEY"; then
            report "anthropic-key" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$SLACK_TOKEN"; then
            report "slack-token" "$id"
        fi
        if printf '%s\n' "$line" | grep -E -q -- "$GOOGLE_KEY"; then
            report "google-api-key" "$id"
        fi
    done < "$file"
done < "$FILES"

exit "$fail"
