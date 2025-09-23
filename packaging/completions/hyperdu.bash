# bash completion for hyperdu (placeholder)
_hyperdu()
{
  local cur prev opts
  COMPREPLY=()
  cur="${COMP_WORDS[COMP_CWORD]}"
  opts="--top --exclude --exclude-from --max-depth --min-file-size --follow-links \
        --one-file-system --logical-only --approximate --compat --apparent-size \
        --perf --time --time-kind --time-style --csv --json --progress"
  if [[ ${cur} == -* ]] ; then
    COMPREPLY=( $(compgen -W "${opts}" -- ${cur}) )
    return 0
  fi
}
complete -F _hyperdu hyperdu
