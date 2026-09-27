# macOS: sandbox-exec probe

Run on September 26, 2026, on macOS 26.6.2 (build 25G83), Apple silicon.

`man sandbox-exec` names it "(DEPRECATED)" and says "The sandbox-exec
command is DEPRECATED." It is present and enforcing.

## Writes and network

Policy:

```
(version 1)
(allow default)
(deny file-write*)
(allow file-write* (subpath "<workspace>") (subpath "/private/tmp")
                   (subpath "<resolved $TMPDIR>") (literal "/dev/null"))
(deny network-outbound (remote ip))
```

Results, run as `sandbox-exec -f p.sb /bin/bash -c '...'`:

| Action | Result |
|---|---|
| write inside the workspace | allowed |
| `echo x > $HOME/...` | `Operation not permitted` |
| write to `/tmp` | allowed |
| a grandchild `bash -c` writing to `$HOME` | `Operation not permitted` |
| `python3 -c 'open("$HOME/...","w")'` | `PermissionError: [Errno 1] Operation not permitted` |
| `curl https://example.com` | `curl: (7) Failed to connect to example.com port 443 after 61 ms: Couldn't connect to server` |
| read `/etc/hosts` | allowed |

The policy applies to every process the command starts. Paths must be
resolved (`/private/var/...`, not `/var/...`).

## Denying reads of one directory

Policy: `(allow default)` plus `(deny file-read* file-write* (subpath "<dir>"))`.

| Action | Result |
|---|---|
| `cat <dir>/tok` | `Operation not permitted` |
| `python3 -c 'open("<dir>/tok").read()'` | `PermissionError: [Errno 1] Operation not permitted` |
| `ls <dir>` | `Operation not permitted` |

This is the case the shell's credential deny cannot see: a command that
declares no paths.

## Cost per call

Mean of 50 runs from one Python process, macOS only:

| Command | Time |
|---|---:|
| `/bin/bash -c true` | 2.0 ms |
| `sandbox-exec -f r.sb /bin/bash -c true` | 7.7 ms |

About 6 ms added per shell call on macOS.
