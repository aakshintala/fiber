# Escape classes

The kinds of bug Fiber has shipped past its tests and reviews. It is part of
`docs/testing.md`. A behaviour review of a pull request checks the change
against every class here, after its general checks. Each class names the
question to ask and one bug that shows it.

The list comes from a reading of 85 product bugs filed as issues labelled
`bug`. It grows: when a bug ships past a review, its fix adds the bug here as
an example of its class, or adds a class when it fits none.

## The classes

- **Built but never installed.** A piece works under its own tests and the
  session never connects it. Ask: where does the running session install
  this, and which test goes through that place? Process extensions' host
  services failed in every real session because the session never installed
  them, while tests used a stand-in (#4664).
- **A second path that skipped the rule.** Two paths reach the same effect
  and one of them carries the check. Ask: what else reaches this effect, and
  does it take the same check? The Lua path kept a program inside its
  package and the process path did not (#4677); the relay checked three
  commands where the socket refused more (#4327).
- **A second consumer left behind.** A value changes and one of its readers
  is updated. Ask: who else reads this event, notice or field? A before-tool
  hook rewrote a call's arguments, and the approval card still showed the
  model's (#4400); a fix to grouped notices reached the terminal and not
  `doctor` (#4701).
- **Live and resumed disagree.** State built while a session runs is rebuilt
  differently from the log. Ask: what does this look like after a resume, a
  reload or a restart, and is that the same? Resume reset the count of
  consecutive blocks (#4849); a resumed session never idle-exited again
  (#4355); a resumed session skipped the compaction trigger (#4684).
- **Two clients, one session.** Ask: what does a second terminal or a
  second command see? A delete or archive from one client left stale rows in
  another (#4494); two sessions resuming at once crossed their history waits
  (#4262).
- **A guard on some entry points.** Several inputs lead to one state and
  only some check it. Ask: which other keys, commands or pointer inputs
  reach this state? Search and copy waited for the opening pass; paging,
  the wheel and the scroll bar did not (#4444).
- **One of many treated as all.** A list is tested empty and full, and the
  code branches on "any" where the rule says "all". Ask: what happens with
  exactly one, and with a mix? One logged-out provider opened sign-in
  although others were signed in (#4576).
- **A dropped failure.** A write, a spawn or a sync fails and the result is
  discarded or turned into a default. Ask: for each fallible call in the
  change, who is told when it fails? Shell output and prompt-history write
  failures were dropped (#4670).
- **A fake kinder than the real thing.** The scripted provider, the fake
  terminal or the fake clock cannot produce the condition. Ask: what does a
  real provider, a real terminal under tmux, or a dead socket do here? A
  half-open connection hung a session for 27 minutes (#4968); the terminal
  exited silently under tmux (#4700); one provider rejected a replayed
  malformed argument on every later request (#3017).
- **A boundary wider than the doc.** A grant, a path rule or a refusal is
  looser in code than in its doc. Ask: what is the narrowest thing the doc
  allows, and what else gets through? The Linux sandbox granted read on
  every device under `/dev` (#4681); a manifest's program path escaped its
  package with `../` (#4853).
- **A promise with no decision behind it.** The doc or ticket does not say
  what should happen. Ask: what did nobody decide? Report it for a ruling;
  do not pick an answer in code. Twelve of the 85 bugs were of this kind.

## What this list is not

It is not the whole review. A change can fail in ways none of these bugs
did, so the behaviour review takes its general checks first and must name
one case outside this list on every run.
