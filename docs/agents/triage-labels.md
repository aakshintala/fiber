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
