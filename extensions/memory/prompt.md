# Memory

Keep what you learn from one session to the next as linked Markdown pages.
Paths below are relative to Fiber home. There is no memory tool: save and
edit pages with `write` and `edit`.

Where pages live:

- `data/github.com-aakshintala-fiber-extensions-memory/<name>.md`: one page
  per file; the file name is the page's name. Every page sits in this one
  flat folder, whichever project wrote it, so a page written in one
  repository is found from another.
- `data/github.com-aakshintala-fiber-extensions-memory/index.md`: the machine
  index. List here what every session should see: notes about the person,
  and knowledge that helps in any repository.
- `projects/<key>/data/github.com-aakshintala-fiber-extensions-memory/index.md`:
  the project index, for the repository you are working in. List here what
  matters in that repository.
- `data/github.com-aakshintala-fiber-extensions-memory/sources/`: raw
  material a page summarises, such as a clipped web page, so the summary can
  be checked against it. No index lists a source, and a source is never
  edited after saving it.

Pages and indexes are plain Markdown:

- A page's frontmatter is one `description` line, so a `grep` result shows
  whether the page is worth opening.
- A page links another page as `[[name]]`.
- An index is a list of pages, one line each: `- [[name]] — hook`. Indexes
  are written by hand, ordered and grouped by what matters most. A page no
  index lists is still found by links and by `grep` over the folder.

Where pages disagree, a project page wins over a general one.

A page can go stale: before acting on a page that names a file or a flag,
check that the file or flag still says what the page claims.

Both indexes together have a budget of 25,000 bytes. When told they are
over budget, prune them: drop what matters least until they fit.
