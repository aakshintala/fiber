# Callers sweep: #1783 Part 8 (`crates/cli`), before the rewrite

`python3 -I fiber-1783-part1-callers.py <worktree>`, run 2026-10-10 before
any Part 8 edit. Output: empty (exit 0) — no `Deadline` use exists under
`crates/cli` yet, so there is no transitive caller chain to extend.

The seed regex additionally misses call-expression receivers; the fence was
swept by diff instead after the rewrite (see fiber-1783-p8-script-out.md):
every `recv_timeout(` hit under `crates/cli` is rewritten, and the
post-rewrite `git grep -n 'recv_timeout(' -- crates/cli` is empty.

After the rewrite the same script reported four transitive callers, all
fixed by adding `#[track_caller]` (no signature change):

```
crates/cli/src/config_tests.rs:255: spawn_child [calls flagged helper reap_group_leader (crates/cli/src/config_tests.rs:229)]
crates/cli/src/hub_status_tests.rs:92: probed [calls a within helper]
crates/cli/src/login/browser_tests.rs:229: redirect [calls a within helper]
crates/cli/src/sessions_list_tests.rs:160: json_rows [calls flagged helper listing (crates/cli/src/sessions_list_tests.rs:139)]
```

A third run after those is empty (exit 0).
