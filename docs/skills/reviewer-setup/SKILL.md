---
name: reviewer-setup
description: Set up the reviewer's notes with the person: gather the evidence, draft one line per fact, confirm each topic with them, and save their confirmed lines.
---

# Setting up the reviewer's notes

The reviewer's notes are the standing rules the reviewer holds for this
person: which accounts and repositories are theirs, which hosts and tools
are inside their organisation, what routine actions are allowed, what must
never happen, and which shell commands are read-only. Two layers hold them:
the global notes in Fiber home and the per-project notes in the project.
You draft the lines and the person confirms them, one topic at a time.
The only write path is a `reviewer_notes_save` call carrying their kept
lines: Fiber shows them the diff for each file, with the file named, and
writes only on a confirm they answer. No agent edit reaches the notes or
the config files any other way, so never write those files yourself.

## 1. Gather the evidence

Run `fiber reviewer evidence --json` in the project. It prints one object
keyed by topic, in the setup's topic order, each value the topic's evidence
array as the panel used to gather it: file items from the project's git
remotes, CI configuration and package registries; session tallies counting
escalations a person answered allow per prefix; reviewer-allow tallies
counting reviewer allows per shell subject for the read-only topic.
Credentials are already removed from URLs. Draft from this evidence plus
what the person tells you in this session.

## 2. Draft lines, one topic at a time, in topic order

The six topics, in order: source control, internal hosts, internal tools,
routine actions, what must never happen, and read-only commands. Draft one
fact or rule a line, each line standing alone. Read-only lines are entries
in rendered form, one per counted subject:

```
shell.read_only."acme-cli query".flags = ["--json"]
```

with the flags the counted calls used.

## 3. Confirm each topic with the person

Confirm one topic at a time with `ask_user`, keeping only what they
confirm:

- 2-4 drafted lines become one multi-select question: each line is an
  option, selected lines are kept, and free text adds a line.
- 0-1 lines become one free-text question naming the drafted line in the
  question body.
- More than 4 lines spill into follow-up questions, four lines at most per
  question.

## 4. Save their lines

Call `reviewer_notes_save` with `{"lines": [{"topic", "layer", "text"}]}`
carrying the kept lines only: `topic` is the topic's snake_case name,
`layer` is `global` or `project` per line, `text` is the confirmed line.
Fiber shows the diff for each file, with the file named, and writes only
on a confirm the person answers; a call that changes nothing reports
unchanged and writes nothing. Send a topic's lines only when the topic has
at least one kept line: omission preserves, so topics you leave out and
entries you drop stand as they are. A changed entry is refused before any
confirm: re-draft it instead of editing around the refusal.

Set each line's layer by where the line comes from: a line from the
project's files goes to the project notes; the person's answer about
accounts, hosts or tools goes to the global notes; a session line goes to
the project notes when every escalation behind it came from this project,
and to the global notes otherwise; a read-only entry goes to the project
config when every allow behind it came from this project, and to the
global config otherwise.
