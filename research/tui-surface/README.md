# TUI surface: what the owner uses, and what toolkits provide

Evidence for [#15](https://github.com/aakshintala/fiber/issues/15) and [#147](https://github.com/aakshintala/fiber/issues/147), gathered on 2026-09-27.

## Owner usage

`usage.py` reads the owner's pi sessions (`~/.pi/agent/sessions`) and Claude Code sessions (`~/.claude/projects`), main sessions only, and counts what a terminal UI has to show. It counts only sessions with at least one prompt: 164 pi sessions and 208 Claude Code sessions.

| | pi | Claude Code |
|---|---|---|
| Prompts per session, median / p90 | 2 / 15 | 5 / 17 |
| Session wall time, median / p90 | 0.2 h / 6.3 h | 1.5 h / 11.8 h |
| Tool calls per prompt, median / p90 / p99 / max | 4 / 30 / 137 / 312 | 5 / 28 / 101 / 516 |
| Tool calls between two pieces of assistant text, median / p90 / max | 4 / 13 / 318 | 2 / 6 / 141 |
| Sessions with a delegate | 44 (27%) | 71 (34%) |
| Of those, 3 or more delegates running at once | 22 of 44 | 12 of 28 with transcripts |
| Sessions with a background launch | 26% | 41% |
| Sessions with an interrupt | 21% | 24% |
| Sessions with a message typed mid-turn | not measured | 31% (168 messages) |
| Sessions where the agent asked the person a question | 35% | 47% |
| Sessions with a permission denial | none (pi has no permissions) | 17% (60 denials) |

Most Claude Code delegates ran through `cursor_run` and leave no transcript, so their overlap is not measured. Instead, 27 Claude Code sessions launched two or more delegates in one message. Of the 689 Claude Code `queued_command` entries, 521 are background-task notifications, not typed messages. The logs do not record what the person opened in the UI, or approvals granted.

## Toolkits

`toolkits.md` surveys, from primary sources, which capabilities ratatui, Bubble Tea, Ink, OpenTUI and Textual provide. `probes.md` builds one app in four of them: full screen, mouse capture, 2,000 wrapped items, a 32-column side panel and an input box. It measures that app on macOS arm64.

| | Ships as | Peak footprint, idle | Idle wakeups | Threads |
|---|---|---|---|---|
| Rust, ratatui 0.30 | 493 KiB binary | 1.9 MiB | 0 | 1 |
| Go, Bubble Tea v2.0.10 | 3.9 MiB binary | 11.1 MiB | rising, from a 60 fps ticker (`tea.go` `startRenderer`, `renderer.go` `defaultFPS = 60`) | 9–10 |
| Bun, OpenTUI 0.5.12 | 71 MiB, `bun build --compile` | 88 MiB | a few, source not found | — |
| Python, Textual 8.2.8 | 7.2 MiB, PyInstaller `--onefile` | 88.9 MiB | a few | 3 |

- No toolkit provides search with highlighted matches, or selection across content scrolled off screen.
- OpenTUI provides a virtualised `ScrollBox`, drag-to-select with OSC 52 copy, and incremental Markdown (`parseMarkdownIncremental`).
- Textual provides `MarkdownStream`.
- Time to first frame was not measured cleanly. The method caught the pseudo-terminal's setup cost, not the app's own startup.

## Running it

```sh
python3 research/tui-surface/usage.py
```

The probe apps are not kept. `probes.md` lists how each was built and measured.
