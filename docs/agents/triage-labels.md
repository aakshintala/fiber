# Triage Labels

The skills speak in terms of canonical triage roles: three categories and five states. This file maps those roles to the label strings this repo uses.

| Role              | Label in fiber    | Meaning                                            |
| ----------------- | ----------------- | -------------------------------------------------- |
| `bug`             | `bug`             | Something is broken in the product                 |
| `enhancement`     | `enhancement`     | New feature or improvement                         |
| `test-only`       | `test-only`       | Defect in test code, such as a flaky test; never also `bug` |
| `needs-triage`    | `needs-triage`    | Owner needs to evaluate this issue                 |
| `needs-info`      | `needs-owner`     | Waiting on the owner to decide or supply something |
| `ready-for-agent` | `ready-for-agent` | Fully specified, ready for an agent                |
| `ready-for-human` | `needs-owner`     | Needs the owner to test by hand or act             |
| `wontfix`         | `wontfix`         | Will not be actioned                               |

When a skill names a role, apply the label in the second column.

Two more labels sit outside the roles:

- `blocked`: the ticket is specified but waits on another ticket, or on a wave not yet ticketed. Its body names what it waits for. When that lands, swap `blocked` for `ready-for-agent`.
- `future-candidate`: closed as not planned for now, to revisit if the need comes up. The closing comment says what would bring it back.
