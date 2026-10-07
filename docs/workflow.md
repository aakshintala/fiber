# Implementation workflow

How a ticket becomes a merged pull request. This is what is true now, not a
plan. It is settled by
[Implementation workflow: how a ticket becomes a merged PR](https://github.com/aakshintala/fiber/issues/66);
that ticket's resolution holds the rationale and the rejected alternatives.

It starts from an agent-ready ticket: one that states its outcome, cites
the `docs/<area>.md` pages it implements, names its blockers as GitHub
blocking links, and carries the `ready-for-agent` label. Turning a design
into tickets is not part of it. What must pass to merge is set in `docs/ci.md`,
`docs/testing.md`, `docs/code-quality.md` and `docs/dependencies.md`.

No harness is assumed. Any agent harness can fill any role below.

## Roles

- The orchestrator owns a ticket from start to merge. It writes the brief,
  opens the pull request, answers the review, waits for `CI` and merges.
- An implementer writes the code. It may be the orchestrator's own session
  or a separate one the orchestrator briefs; that is the harness's choice.
- The reviewer reads the finished diff, read-only, and reports findings.

The owner reads no code. A pull request merges on a green `CI` check and a
resolved review, and the orchestrator merges it without asking.

The orchestrator judges an implementer's work from the diff and the gate's
output, never from the implementer's account of it.

An orchestrator may start other orchestrators, one per ticket. The one the
owner started is the main orchestrator; the ones it starts are
sub-orchestrators. A sub-orchestrator comments on tickets and opens
`needs-owner` and follow-up issues itself. Before it opens an issue, it
searches the tracker for an existing one. Its report to the orchestrator
that started it lists every issue it opened or commented on. The main
orchestrator keeps the conversation with the owner: what to ask, when, and
in what order.

## Choosing models

An implementer is chosen for the capability its ticket needs: a mechanical
edit does not need the model a subtle concurrency change does.

The reviewer is from a different model family than the implementer, so the
two do not share blind spots.

## The brief

An implementer is given:

- the ticket
- the `docs/<area>.md` pages the ticket cites
- `GLOSSARY.md`
- the gate: `scripts/check` passing in CI (`docs/workflow.md`, "The gate")

`AGENTS.md` at the repository root points every harness at these files.

## The gate

`scripts/check` passing in CI on the exact head gates the merge. Before a
push, the implementer runs the checks for the crates they changed:
`cargo clippy -p <crate> --all-targets -- -D warnings` and
`cargo nextest run -p <crate>`.

`scripts/check` runs what CI runs, for the crates `docs/ci.md`, "Selection",
chooses. The list of checks is `docs/ci.md`, "On every pull request that
changes code" and "The docs check".

Mutation testing runs in CI only.

Driving the binary is not part of the gate. A behaviour the tests do not
reach gets a test: a binary-level test for the JSON lines, a screen test for
what the terminal shows (`docs/testing.md`).

## Tools, not handwork

The same edit in three or more places is made by a script that rewrites the
code, not by hand. Edits are the same when one rewrite rule makes all of
them; a sweep where each site needs its own edit is handwork, and the body
says so. A check that will run more than once is a script. Three
is picked, not measured.

The pull request body gives the command that ran the script and what it
printed, so anyone can run the script from the pull request's head against a
scratch checkout of the base commit and compare. A script later work will
use is committed under `scripts/`. A one-off script goes in the pull request
body.

A question about what one layer does with a given input is answered with that
layer's jig, not a throwaway program (`docs/testing.md`, "Jigs").

## When fixes keep failing

When two fixes that rest on one assumption have failed the same check, the
implementer does not write a third. It writes down, in one sentence, the
assumption both fixes made, and tests that assumption directly. The pull
request body names the two failed fixes and gives the assumption, the test's
command and its result. If the assumption is false, the next fix starts from
what the test showed. If it holds, the cause is elsewhere.

## Size

A pull request carries one ticket. There is no cap on changed lines.

A ticket too large for one implementer session is split by the orchestrator
into pull requests that each leave `main` green. The ticket closes with the
last one.

A pull request whose change takes a source file over 800 lines files a
`split` ticket for that file (`docs/code-quality.md`, "Size"). An
orchestrator choosing its next ticket takes an open `split` ticket whose
blockers are closed before any other.

## The pull request

The body says `Resolves #<ticket>`, and either `Doc friction: none` or a
link to each `needs-owner` issue the work raised ("When a doc should
change").

A pull request that resolves an issue labelled `bug` proves its fix with a
red commit, then green. Its first commit, the red commit, holds the test that
reproduces the bug and any new signature or test seam that test needs, but
not the fix. The red commit builds, and at least one new or changed test
fails there. The head passes. CI runs the new and changed tests at both
commits (`docs/testing.md`, "Proving a test bites"). The squash merge still
lands one commit. A pull request whose every changed file is a docs file, as
"Selection" defines it, passes the check with a message saying so: the doc
was wrong and the code was right, so no test can show the bug.

A ticket whose defect is in test code, such as a flaky test, carries the
`test-only` label, never `bug`. Its pull request states the root cause and
the evidence that the fix holds, such as repeated runs under load, in place
of a red commit. Once a `test-only` ticket's cause is confirmed to be in
production code, it is a bug: the implementer relabels it `bug` and drops
`test-only`.

The orchestrator waits on `CI` with `gh-ci`. A failed check is fixed in a new
commit. A failed run is never re-run until it passes; the one exception is
CI's own retry of a binary-level test (`docs/testing.md`, "Flaky tests").

## Review

Every pull request gets one review, after the gate passes. The reviewer is
given the diff, the ticket, the docs it cites, `GLOSSARY.md` and
`docs/code-quality.md`, and checks two things:

- Spec: the diff does what the ticket and its `docs/<area>.md` pages say.
- Standards: the items in `docs/code-quality.md`, "What a reviewer checks".

A pull request that changes only docs, in wording taken from an owner ruling
recorded on its ticket, has no separate reviewer. The orchestrator checks the
diff against the ruling and says so in the pull request body.

The review is posted on the pull request as a comment. Each finding is
either fixed, and the fix's diff reviewed again, or answered in a reply that
cites evidence: a test, a doc line, a command's output. The pull request
merges when every finding has one or the other.

A finding that a review of an earlier pull request also raised becomes a
type, a lint or a check in `scripts/check`, in its own pull request that
links both findings, when a tool can catch it
(`docs/code-quality.md`, "Tools enforce the rules"). Otherwise it is added to
`docs/code-quality.md`, "What a reviewer checks".

## When a doc should change

The doc wins, and the code changes to match it. A code pull request may
correct a doc's wording, such as a misnamed type or a broken link. What a
doc decides, the owner changes.

Two findings send a doc to the owner:

- It cannot be met as written.
- It can be met, but building it shows a problem or a simpler design. The
  signs are the same workaround in several places, special-case branches for
  unrelated edge cases, a type that needs an escape hatch to compile, a lock
  where the doc says nothing is shared, callers that must know a module's
  internals, or a mechanism the doc requires that nothing uses.

For the second, the implementer pauses and tells the orchestrator what it
found. The orchestrator reads the ticket's resolution first. An alternative
the resolution already rejected is answered with the resolution's reason,
the implementer carries on, and nothing is filed.

Every other finding becomes an issue labelled `needs-owner`, filed by the
orchestrator. It quotes the doc rule, names the code it affects, and gives
the evidence: what fails, for a doc that cannot be met, or a sketch or a
measurement, not a rewrite, for a better design.

While the owner decides:

- A doc that cannot be met, or an alternative that would replace what the
  ticket builds: the ticket is marked blocked by the issue, and the
  orchestrator takes the next ticket whose blockers are closed.
- Any other alternative: the implementer finishes as the doc says, and the
  pull request merges.

The owner's ruling closes the issue, which unblocks the ticket. Accepted,
the doc changes, a blocked ticket's brief is rewritten against the new doc,
and code already merged follows in a later pull request. Rejected, the
ticket resumes as the doc says. A doc reported as unmeetable is ruled on
differently: the ruling shows how it is met or changes the doc, and a ticket
that no longer makes sense is closed and its work replanned.

## Merging

A pull request opens as a draft and stays a draft until its review loop is
done; marking it ready starts CI's full run (`docs/ci.md`). When `CI` is
green, the full run passed on the exact head, and every review finding is
resolved, the orchestrator squash-merges the pull request, deletes its
branch and checks the ticket closed.
