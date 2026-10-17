# Retries, failed turns, crash recovery and rewind (#1635)

One run per case: `--failure-case CASE`.

- retry: the working line reading ↻ Retrying in 4s · rate_limited · attempt 2 of 4 with its attention mark, and the group line above counting 1 failed model call.
- failed: the ✗ line with the message and provider_unavailable in the error colour, the provider's words dim under it, then the dim ▣ failed line with its duration and tool calls.
- auth_failed: the ✗ line with the message and authentication_failed, and log in as its own target.
- nudge: the single dim ◔ Context at 268k line sitting between the group lines where it happened.
- connection_lost: the conversation stub above, the Connection lost · reconnecting (attempt 2)… banner where the working line was, and the draft row kept under it.
- crashed: the dim ▣ cut short line with Fiber stopped at 14:18, the ledger row reading ? may have run; not run again, the ↺ resumed band on the handoff tint, and the one line naming every orphaned job.
- rewind: the Rewind header, one before: row per turn, the chosen turn opened into its step boundaries, the files and calls since with none of it undone, the Jobs that stop row, and the Rewind row last.
- rewound: the ⟲ line naming the new session, what was not restored, and which jobs stopped.
