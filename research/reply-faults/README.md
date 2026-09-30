# Malformed model replies in the owner's sessions

Measured on macOS, 2026-09-29, for issue #190. Sources are the owner's pi sessions (`~/.pi/agent/sessions`) and Claude Code sessions (`~/.claude/projects`), with Claude Code main sessions and subagent transcripts counted separately. The Claude Code numbers include sessions that were still running when the script ran.

Run `python3 analyze.py` to regenerate `results.json` and the tables. Run `python3 analyze.py --selftest` to check the classifiers against fixed inputs.

## Units

- A pi turn is one assistant message. A Claude Code turn is one API response: Claude Code writes one line per content block, so lines are grouped by `message.id`.
- Rates use tool calls as the denominator for argument and name faults, and turns for everything else.
- Models with fewer than 50 turns are counted in "All" but get no row of their own.

## Totals

| Source | Sessions | Turns | Tool calls | Tool errors (any cause) |
|---|---|---|---|---|
| pi | 692 | 39,061 | 44,325 | 1,235 |
| Claude Code main | 293 | 19,346 | 19,205 | 501 |
| Claude Code subagents | 235 | 11,980 | 13,362 | 394 |

## What pi repairs before anyone sees it

pi never shows a raw argument fault to the log. Before the arguments are stored, `parseStreamingJson` escapes raw control characters and bad backslashes, falls back to a partial-JSON parser, and returns `{}` if all of that fails. At validation it then turns `null` into a deleted optional key and `"45"` into `45` or `"true"` into `true`. So the pi validation-failure count is what is left after that repair. JSON repair leaves no trace. Type coercion does: the stored arguments come before coercion, so the script checks them again against the tool schemas the session declared. Only 101 of 692 sessions declare their tools (3,480 calls). The other sessions are not checked, because schemas change between sessions.

| Model (pi) | Calls checked | Calls needing coercion | Kind |
|---|---|---|---|
| openai-codex/gpt-6-sol | 739 | 120 (16%) | `null` for an optional field, for example `{"path": "...", "offset": null, "limit": null}` |
| opencode-go/glm-5.3-flash | 334 | 30 (9%) | numbers and booleans as strings, for example `{"offset": "330", "limit": "45"}` |
| all others | 2,407 | 0 | |

Under a strict validator these 150 calls would have failed. The 120 gpt-6-sol calls follow OpenAI's strict-mode convention, where every property is present and absent ones are `null`.

## a. Arguments that failed validation

| Source | Failures | Rate per call | Sessions | By model |
|---|---|---|---|---|
| pi (after pi's repair) | 42 | 0.09% | 29 | muse-spark-1.3 19 of 27,819; glm-5.3-flash 16 of 7,513; gpt-5.6-sol 4 of 6,161; gpt-5.6-luna 2 of 1,051; gpt-6-sol 1 of 739 |
| Claude Code main | 6 | 0.03% | 6 | opus-5 2, opus-5-5 2, sonnet-5 2 |
| Claude Code subagents | 8 | 0.06% | 5 | sonnet-5 8 of 5,719 |

The 42 pi failures, sorted by reading each one:

| Kind | Count | Repairable without guessing? |
|---|---|---|
| Required field omitted on two extension tools (`ask_user` `tab`/`header`/`options` 16, `subagent_spawn` `isolation` 5) | 21 | No. The same field was missed over and over, so this is a tool-design fault as much as a model fault. |
| Array sent as a JSON string: `"edits": "[{\"newText\": ...}]"` (4), `"questions": "[...]"` (2), all muse-spark-1.3 | 6 | Yes: parse the string, check that it matches the schema |
| `notify.onSuccess` given a non-string | 5 | No |
| Required field missing inside `edits[]` | 2 | No |
| Empty arguments `{}` (probably a parse that fell back to `{}`) | 2 | No |
| Mangled or wrong key names: `{"questions sach": [...]}`, `{"codes": "3"}` for `taskId` (gpt-5.6-sol) | 2 | No |
| `header` longer than 12 characters | 2 | Could truncate, but that changes meaning |
| Unexpected key, wrong tool shape | 2 | No |

The 14 Claude Code failures:

| Kind | Count | Example |
|---|---|---|
| Invalid JSON | 7 (6 subagent, 1 main, all sonnet-5) | `{"file_path": "/tmp/monocode/src/features/sessions/model/session.ts", "offset": 185, 230}`, `{"file_path": "...cli.test.ts", "offset": 3440, }` |
| Array longer than allowed (`AskUserQuestion` with more than 4 questions) | 3 | `"code": "too_big", "maximum": 4` |
| Unexpected parameter on `Bash` (`prompt`, `query`) | 2 | `An unexpected parameter \`query\` was provided` |
| Invented parameter, every required one missing | 1 | `Edit` with `{"replace_name": "contract-inventory.md fix count"}` |
| Deferred tool called before its schema was loaded | 1 | harness fault, not a model fault |

Examples:

- `~/.pi/agent/sessions/--Users-aakshintala-.pi--/2026-09-08T04-22-52-641Z_01a07f41-2be0-721a-b5b3-8881b2c2b2dc.jsonl:130` (edits sent as a string)
- `~/.pi/agent/sessions/--Users-aakshintala-work-fiber--/2026-09-08T18-41-52-279Z_01a08253-9a97-7520-8df4-6843d6752087.jsonl:278` (`questions sach`)
- `~/.claude/projects/-Users-aakshintala-work-fiber/e4bf346b-abde-4fbe-a300-c2895c03c8a5/subagents/agent-a859e7be1c574dd0b.jsonl:37` (invalid JSON)
- `~/.claude/projects/-Users-aakshintala-work-fiber/8d5683cc-079e-48a5-a142-39fcdf3ad7ea.jsonl:808` (`replace_name`)

Recovery: both harnesses return the error as a tool result and the model retries. The table counts turns until a call to the same tool succeeds.

| Source | Failures | Fixed on the next turn | Within 3 turns | Never (session ended) |
|---|---|---|---|---|
| pi | 42 | 34 | 39 | 2 |
| Claude Code main | 6 | 5 | 5 | 1 |
| Claude Code subagents | 8 | 8 | 8 | 0 |

## b. Unknown tool names

| Source | Count | Detail |
|---|---|---|
| pi | 1 of 44,325 calls | muse-spark-1.3 called `process` after it had been removed. Among the 101 sessions that declare tools, no call used a tool that was not offered, and none used another harness's name. |
| Claude Code main | 0 | |
| Claude Code subagents | 4 of 565 sonnet-5-5 calls | `bash` for `Bash`. Claude Code replies "Tool names are case-sensitive: call Bash instead"; 3 fixed on the next turn, 1 within 3. Example: `~/.claude/projects/-Users-aakshintala-work-fiber/110817a7-9fed-4cea-b950-bc3587410089/subagents/agent-a23a1916baae56b22.jsonl:44` |

Did not occur: no `apply_patch`, `shell`, `str_replace_editor`, `Grep`/`Glob`/`Read` in pi, and no `paths` sent as a delimited string.

## c. Tool calls written into text

None in either source. Visible text and pi thinking were searched for `<tool_call>`, `<function_calls>`, `<invoke`, `<parameter name=`, `to=functions.`, `<|tool`, `[TOOL_CALLS]`, fenced JSON naming a tool, and bare `{"name": ..., "arguments": ...}`. A looser search also found nothing, apart from the brief that started this measurement. The one hit from the strict pattern (Claude Code main, opus-5) is a false positive: a JSON test-corpus entry with a `"name"` key, quoted in prose. False-positive rate: 1 of 1.

## d. Runaway repetition

None. The detector flags a line of 40 or more characters repeated 8 or more times, or a 40–400 character run repeated 5 or more times in the last 6,000 characters. Loosening it to 4 repeats of 30 characters gave 4 hits (a repeated markdown heading, and code lines in glm-5.3-flash thinking), all of them false positives. No turn in either source stopped on length.

## e. Stop reasons and empty replies

| Source | Stop reasons |
|---|---|
| pi | toolUse 36,427; stop 1,812; error 751; aborted 71; length 0 |
| Claude Code main | tool_use 16,906; end_turn 2,389; stop_sequence 39 (all Claude Code's own synthetic messages); not recorded 12; max_tokens 0; refusal 0 |
| Claude Code subagents | not recorded in 87% of turns (subagent transcripts do not keep it); tool_use 1,189; end_turn 345 |

pi has no refusal or content-filter stop at all. Its OpenAI-compatible adapter maps `content_filter` to an error, and none of the error messages mention it.

Empty replies:

- pi: 13 replies with no text, thinking or tool call, all with stop reason `stop`, all with 0 output tokens (muse-spark-1.3 5, claude-opus-5 4, glm-5.3-flash 3, kimi-k3 1). Add 1 muse-spark-1.3 reply with only redacted thinking. pi treats each one as a finished turn and waits for the user. Example: `~/.pi/agent/sessions/--Users-aakshintala-work-ClaudeBar--/2026-08-30T04-54-08-429Z_01a05104-8f2d-748b-9b6b-b02273c1e523.jsonl:19`.
- pi: 1 claude-opus-5 reply with a `bash` call whose arguments were `{}` and whose stop reason was `stop`, after 5 output tokens (`...2026-08-30T05-14-35-445Z_01a05117-4835-72e7-8b27-b85b16731818.jsonl:37`). pi ran it anyway.
- Claude Code main: 3 replies with thinking only, all followed by "[Request interrupted by user]". These are user interruptions, not model faults.

## f. Other faults the harness had to cope with

pi error turns (751), sorted by the error message:

| Kind | Count | Main model | Fault lies with |
|---|---|---|---|
| Rate limit or quota (429, usage limit) | 546 | muse-spark-1.3 via opencode-go, 537 | provider |
| User abort | 97 | spread | user |
| Server error or overload (5xx) | 66 | muse-spark-1.3, 65 | provider |
| Network (connection error, WebSocket, timeout) | 64 | muse-spark-1.3, 52 | transport |
| Stream ended early ("stream ended before a terminal response event", "terminated", "Stream ended without finish_reason") | 22 | muse-spark-1.3 18, glm-5.3-flash 4 | provider or transport |
| Bad request 400 (thinking level the model does not support, reasoning `encrypted_content` not issued to this caller) | 22 | several | harness configuration |
| Auth | 4 | | configuration |

Claude Code wrote 44 synthetic assistant messages: 20 "Not logged in", 15 "No response requested.", 5 spend limit, 3 "Connection lost mid-response. The response above may be incomplete." and 1 "Connection dropped (ECONNRESET)". None of them are model output.

## Limits

- pi JSON repair and partial-JSON fallback leave no trace, so the rate of malformed JSON from non-Anthropic models in pi is unknown. Only the 2 empty-argument cases hint at it. Claude Code does no repair, and there 7 of 32,567 calls (all sonnet-5) had invalid JSON.
- The pi schema check covers only the 101 sessions that record their tool declarations: 8% of calls, and 739 of those calls are gpt-6-sol.
- A pi session file can hold branches. The script reads entries in file order, so recovery counts can span branches.
- Recovery counts a later success of the same tool, not of the same call. A model that gave up on the tool shows as "never".
