# Usage: bash --norc --noprofile bash-driver.bash <script> <case>...
# Sources the generated script, then completes each case the way bash
# does on a Tab: the words go in COMP_WORDS, the last one is the word being
# completed, and `_fiber` is called with the command, that word and the
# one before it. A case ending in a space completes an empty word.
# Prints `SOURCE-STDERR` and what sourcing wrote on stderr, `COMPLETE` and
# `complete -p fiber`, then per case `CASE <n>`, one `CAND:<word>` line per
# candidate, and `CASE-END`.
script=$1
shift
errors=$(source "$script" 2>&1 >/dev/null)
source "$script" 2>/dev/null
printf 'SOURCE-STDERR\n%s\nCOMPLETE\n%s\n' "$errors" "$(complete -p fiber)"
n=0
for case in "$@"; do
  n=$((n + 1))
  read -r -a COMP_WORDS <<<"$case"
  if [[ $case == *' ' ]]; then
    COMP_WORDS+=("")
  fi
  COMP_CWORD=$((${#COMP_WORDS[@]} - 1))
  COMP_LINE=$case
  COMP_POINT=${#case}
  COMPREPLY=()
  prev=''
  if [[ $COMP_CWORD -gt 0 ]]; then
    prev=${COMP_WORDS[COMP_CWORD - 1]}
  fi
  _fiber fiber "${COMP_WORDS[COMP_CWORD]}" "$prev" 2>/dev/null
  printf 'CASE %s\n' "$n"
  for word in "${COMPREPLY[@]}"; do
    printf 'CAND:%s\n' "$word"
  done
  printf 'CASE-END\n'
done
