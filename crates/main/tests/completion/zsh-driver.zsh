# Usage: zsh -f zsh-driver.zsh <script> <case>...
# Runs an interactive zsh on a pseudo-terminal through zsh's zpty module,
# with the generated script as its $1, and sources zsh-capture.zsh in it.
# Then types each case and a Tab, reads up to that case's `CASE-END`, and
# clears the line. Prints per case `CASE <n>`, then one `CAND:<word>` line
# per candidate, then `CASE-END`.
zmodload zsh/zpty || exit 3
script=$1
shift
capture=${0:A:h}/zsh-capture.zsh
zpty z zsh -f -i -s "$script" || exit 4
zpty -w z "source ${(q)capture}"
out=''
until [[ $out == *CAPTURE-READY$'\r'* ]]; do
  zpty -r z line || exit 5
  out+=$line
done
n=0
for case in "$@"; do
  n=$((n + 1))
  zpty -w -n z "$case"$'\t'
  out=''
  until [[ $out == *CASE-END$'\r'* ]]; do
    zpty -r z line || exit 6
    out+=$line
  done
  print -r -- "CASE $n"
  print -r -- "${out//$'\r'/}" | grep '^CAND:'
  print -r -- "CASE-END"
  zpty -w -n z $'\C-u'
done
zpty -d z
