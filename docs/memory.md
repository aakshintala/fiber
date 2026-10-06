# Memory

How an agent keeps what it learns from one session to the next. This is what
is true now. It is settled by
[Spec: agent memory](https://github.com/aakshintala/fiber/issues/776) and the
closed tickets of [Map: agent memory](https://github.com/aakshintala/fiber/issues/476),
which hold the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`: memory, memory page, memory index, data directory,
opening message, instruction file.

## What memory is

Memory is a store of linked Markdown pages, one store per machine, kept by the
first-party `memory` extension. A hand-written index for the machine and one
for each project reach the model in the opening message. The model reads a
page only when it needs it, and saves or edits pages with its ordinary file
tools. Pages are plain files a person can read, edit and delete.

Memory is on while the `memory` extension is installed and enabled. A fresh
install has it ("Distribution"). There is no other switch.

## The store

The store lives in the extension's two data directories (`docs/state.md`,
"What each part holds"):

| Path in Fiber home | Holds |
|---|---|
| `data/memory/<name>.md` | one memory page; the file name is the page's name |
| `data/memory/index.md` | the machine index |
| `data/memory/sources/` | raw material that pages cite, such as a clipped web page |
| `projects/<key>/data/memory/index.md` | the project's index |

Every page sits in one flat folder, whichever project wrote it, so a page
written in one repository can be found from another. Only the indexes are
per machine or per project.

### Pages

A page is plain Markdown:

- Its frontmatter is one `description` line, so a `grep` result shows whether
  the page is worth opening. Any other frontmatter is ignored.
- It links another page as `[[name]]`.

### Indexes

An index is a Markdown list of pages, one line each: `- [[name]] — hook`.
Indexes are written by hand, by the model or the person, ordered and grouped
by what matters most. Fiber never generates one. An index may link another
page that lists more pages, such as a list of wiki pages, to stay short.

Which index lists a page:

- The machine index lists what every session should see: notes about the
  person, and knowledge that helps in any repository.
- A project index lists what matters in that repository.

A page that no index lists is still found by links and by `grep` over the
folder. There is no search index.

### Sources

`sources/` holds raw material a page summarises, so the summary can be checked
against it. No index lists a source, and the model does not edit a source
after saving it. Both are conventions in the extension's prompt text; Fiber
does not enforce them.

## What the model sees

The `memory` extension's manifest names `index.md` in each data directory as
its opening-message section (`docs/configuration.md`, "An extension's
manifest"). Fiber reads them as it reads instruction files and sends the
machine index first, then the project index, after the instruction files
(`docs/system-prompt.md`, "Extension sections"). A missing index gives nothing,
so an empty store adds nothing to the opening message.

An index changed during a session reaches the model as instruction files do
(`docs/system-prompt.md`, "When something changes"): the session's own edit
sends nothing, another session's or the person's edit sends a diff at the next
turn start, and a handoff sends both indexes again in full. Nothing about
memory enters the preamble, so a memory write never causes a prompt-cache miss
(`docs/prompt-cache.md`).

The extension's prompt text (`docs/system-prompt.md`, "Extension texts") tells
the model:

- where pages live, and which index to list a page in;
- the `description` line and `[[name]]` links;
- the `sources/` convention;
- that a project page wins over a general one where they disagree;
- to check a page that names a file or flag before acting on it;
- to prune the indexes when told they are over budget.

## Writing

The model saves and edits pages with `write` and `edit`. There is no `memory`
tool. A call whose paths are all Markdown files in data directories takes the
permission fast path, so saving a page costs no reviewer call
(`docs/permissions.md`, "Fast paths"). A non-Markdown file, such as a source
saved as HTML, is reviewed like any other write.

Two sessions writing the same page are kept apart by the ordinary checks: an
`edit` must still match the file, and a `write` that would replace a page this
session has not seen in its current form is refused with `stale_file`
(`docs/tools.md`, "Stale files").

Nothing runs in the background. A model that finds a page wrong fixes it.

## The budget

Both indexes together have a budget of 25,000 bytes. They are never cut. The
person cannot change the budget.

When they are over it, the model is told to prune them in two places:

- At each opening-message build, Fiber ends the memory section with one line
  giving their size against the budget (`docs/system-prompt.md`, "Extension
  sections"). The extension declares the budget in its manifest.
- After a `write` or `edit` that touches an index while the two are over
  budget, Fiber appends the same line to the call's result (the same
  section).

An index edited through the shell is not seen when it happens; the next build
reports it. The indexes also count as instruction text for the 10%-of-window
notice (`docs/system-prompt.md`, "Size").

## Who loads memory

Every Fiber session loads memory and may write it: interactive and headless
sessions alike, delegates, forks and rewinds. A delegate writes its own
opening message, so it gets the indexes as they are when it starts
(`docs/delegates.md`). A fork or a rewind inherits the opening message from
the log. A delegate that runs another harness loads that harness's memory,
not Fiber's.

## Finding and removing a bad entry

Every memory write is a logged tool call, shown in the person's client like any
other. Which session wrote a page, and what it had read before, is found by
searching session logs. Nothing else records provenance.

A person removes an entry by deleting its file and its index line.

## Distribution

`memory` is a first-party extension in the release's extensions archive, so a
fresh install has it (`docs/extensions.md`, "A fresh install").
`fiber extension remove memory` removes it and its data directories, asking
first, which turns memory off and deletes the store.

The extension has no code: it is a manifest and its prompt text. It starts no
Lua VM, so it costs nothing in a session beyond its opening-message section,
which adds nothing while the store is empty (`docs/extensions.md`, "Loading,
and cost when nothing is loaded").

## Testing

The opening-message section, its budget line and the prune line on writes are
core, tested like instruction files (`docs/testing.md`). The extension has no
code, so it is tested at the binary level with its shipped manifest: a fresh
install has it enabled and starts no VM for it, a page write takes the fast
path, an over-budget store ends the section with the prune line, and after
`fiber extension remove memory` the opening message has no memory section.
