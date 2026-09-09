#compdef hyperdu
_hyperdu() {
  local -a opts
  local help
  # Read this binary's options so platform/build-only switches stay in sync.
  help=$(command "${words[1]}" --help 2>/dev/null) || return
  opts=("${(@f)$(printf '%s\n' "$help" | awk '/^[[:space:]]+(-[^,[:space:]]+,[[:space:]]+)?--/ { for (i = 1; i <= NF; i++) if ($i ~ /^--/) { print $i; break } }')}")
  _arguments '*:: :->args' ${opts}
}
_hyperdu "$@"
