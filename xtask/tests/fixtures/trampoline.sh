#!/bin/sh
# The kernel execs this checked-in file; the freshly written body runs through /bin/sh.
exec /bin/sh "$0.sh" "$@"
