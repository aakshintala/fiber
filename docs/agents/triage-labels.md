# Triage Labels

The skills speak in terms of canonical triage roles: three categories and five states. This file maps those roles to the label strings this repo uses.

| Role              | Label in fiber    | Meaning                                            |
| ----------------- | ----------------- | -------------------------------------------------- |
| `bug`             | `bug`             | Something is broken in the product                 |
| `enhancement`     | `enhancement`     | New feature or improvement                         |
| `test-only`       | `test-only`       | Defect in test code or CI configuration, such as a flaky test; never also `bug` |
| `needs-triage`    | `needs-triage`    | Owner needs to evaluate this issue                 |
| `needs-info`      | `needs-owner`     | Waiting on the owner to decide or supply something |
| `ready-for-agent` | `ready-for-agent` | Fully specified, ready for an agent                |
| `ready-for-human` | `needs-owner`     | Needs the owner to test by hand or act             |
| `wontfix`         | `wontfix`         | Will not be actioned                               |

When a skill names a role, apply the label in the second column.

More labels sit outside the roles:

- `blocked`: the ticket is specified but waits on another ticket, or on a wave not yet ticketed. Its body names what it waits for. When that lands, swap `blocked` for `ready-for-agent`.
- `future-candidate`: closed as not planned for now, to revisit if the need comes up. The closing comment says what would bring it back.
- `needs-plan`: Leaves design choices open; gets a Verifier-checked plan. The label is about the work, not the crate: a few-line change in a heavily used crate does not need it, and a large refactor in a leaf crate does. Triage applies `needs-plan` (keeping the category label, never alongside `ready-for-agent`) when a ticket leaves design choices open. When its Verifier-checked plan lands, swap `needs-plan` back for `ready-for-agent`; the plan phase swaps it as its last step, when the plan is posted on the ticket. Once the backlog is relabelled, a `ready-for-agent` query returns a much shorter list, so read both labels to see all available work.
