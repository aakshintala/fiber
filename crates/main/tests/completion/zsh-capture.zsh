# Sourced by zsh-driver.zsh in an interactive `zsh -f -i -s <script>`, where
# $1 is the generated script. It loads the completion system and the
# script, prints each candidate as `CAND:<word>` instead of offering it,
# binds Tab to a completion that prints `CASE-END` when it finishes, and
# prints `CAPTURE-READY`.
PROMPT='' RPROMPT=''
autoload -Uz compinit
compinit -u -D
source "$1"
compdef _fiber fiber
compadd() {
  # A call that queries or fills an array is the completion system's own.
  if [[ ${@[1,(i)(-|--)]} == *-(O|A|D)\ * ]]; then
    builtin compadd "$@"
    return $?
  fi
  local -a hits
  builtin compadd -A hits "$@"
  local hit
  for hit in $hits; do
    print -r -- $'\n'"CAND:$hit"
  done
}
_capture_tab() {
  zle complete-word
  print -r -- $'\n'"CASE-END"
}
zle -N _capture_tab
bindkey '^I' _capture_tab
print -r -- "CAPTURE-READY"
