#!/bin/bash
# Probe: Gemini VALIDATED mode against ask_user's schema. Key is read from file into env, never printed.
GEMINI_API_KEY="$(cat "$HOME/.config/probe-keys/gemini-key")"
export GEMINI_API_KEY
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1214-probe-out
mkdir -p "$OUT"
MODEL=gemini-3-flash-preview
SCHEMA='{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"question":{"type":"string"},"header":{"type":"string","maxLength":12},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","properties":{"label":{"type":"string"},"description":{"type":"string"}},"required":["label"],"additionalProperties":false}},"multiSelect":{"type":"boolean"}},"required":["question","header"],"additionalProperties":false}}},"required":["questions"]}'
TOOLS=$(printf '{"functionDeclarations":[{"name":"ask_user","description":"Asks whoever drives this session one to four questions.","parametersJsonSchema":%s}]}' "$SCHEMA")
run() {
  local name="$1" prompt="$2"
  local body
  body=$(jq -n --arg p "$prompt" --argjson t "$TOOLS" '{contents:[{role:"user",parts:[{text:$p}]}],tools:[$t],toolConfig:{functionCallingConfig:{mode:"VALIDATED"}}}')
  curl -sS -o "$OUT/$name.json" -w "%{http_code}\n" \
    -H "x-goog-api-key: $GEMINI_API_KEY" -H "Content-Type: application/json" \
    -X POST "https://generativelanguage.googleapis.com/v1beta/models/$MODEL:generateContent" -d "$body" | sed "s/^/$name HTTP /"
}
run A_baseline "Use the ask_user tool to ask one question: which database should the project use, Postgres or SQLite? Give two options."
run B_six_questions "Use the ask_user tool once. Put six separate questions in it, one per entry, about: colour, font, shell, editor, theme, region. Give each two options."
run C_long_header "Use the ask_user tool with one question about the deploy target. Its header must be the full sentence: Which deployment environment should this service target today. Keep the whole header."
run D_one_option "Use the ask_user tool with one question about log level. Give it exactly one option, labelled debug, and no other options."
run E_seven_options "Use the ask_user tool with one question about log level. Give it seven options: debug, info, warn, error, trace, off, fatal."
run F_empty_list "Call the ask_user tool with an empty questions list."
