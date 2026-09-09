# bash completion for hyperdu (placeholder)
_hyperdu()
{
  local cur opts help
  COMPREPLY=()
  cur="${COMP_WORDS[COMP_CWORD]}"
  # Read this binary's options so platform/build-only switches stay in sync.
  help=$(command "${COMP_WORDS[0]}" --help 2>/dev/null) || return
  opts=$(printf '%s\n' "$help" | awk '/^[[:space:]]+(-[^,[:space:]]+,[[:space:]]+)?--/ { for (i = 1; i <= NF; i++) if ($i ~ /^--/) { print $i; break } }')
  if [[ ${cur} == -* ]] ; then
    COMPREPLY=( $(compgen -W "${opts}" -- ${cur}) )
    return 0
  fi
}
complete -F _hyperdu hyperdu
