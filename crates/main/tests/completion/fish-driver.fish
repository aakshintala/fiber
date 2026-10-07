# Usage: fish --no-config fish-driver.fish <script> <case>...
# Sources the generated script, then asks fish for each case's completions
# with `complete -C`, as a Tab at the end of that command line would.
# Prints `SOURCE-STDERR` and what sourcing wrote on stderr, then per case
# `CASE <n>`, one `CAND:<word>` line per candidate, and `CASE-END`.
set -l script $argv[1]
set -l errors (source $script 2>&1 >/dev/null)
source $script 2>/dev/null
printf 'SOURCE-STDERR\n%s\n' "$errors"
set -l n 0
for case in $argv[2..-1]
    set n (math $n + 1)
    printf 'CASE %s\n' $n
    for candidate in (complete -C "$case")
        printf 'CAND:%s\n' (string split -f 1 \t -- $candidate)
    end
    printf 'CASE-END\n'
end
