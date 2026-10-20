# Getting help

## `fiber doctor`

`fiber doctor` says whether a session can start, and how to fix it when it cannot. It prints one line per check and a fix on any line that stops a session starting. It exits non-zero when a session cannot start, and it never touches the network, so it does not check that a key is valid. The full list of lines is (`docs/invocation.md`, "Commands and flags").

It also prints where the diagnostic logs are, the newest line at `error` level among them, and the newest crash file with its age. When `docs/` is missing from Fiber home, it says `user guide not installed: run fiber update`.

## Logs and crash reports

Where each part of Fiber home lives, including the diagnostic logs and the crash reports, is (`docs/state.md`, "What each part holds"). Quote the newest error line and the crash file's age when you ask for help: they say what the last runs left behind.

## Filing an issue

When the guide and the doctor do not get you unstuck, file an issue at [https://github.com/aakshintala/fiber/issues](https://github.com/aakshintala/fiber/issues). Say what you ran, what you expected, and paste the `fiber doctor` output with the newest error line.
