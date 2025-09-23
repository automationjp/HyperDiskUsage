#compdef hyperdu
_hyperdu() {
  local -a opts
  opts=(
    '--top[Show top N entries]' '--exclude[Exclude substrings]' '--exclude-from[Exclude file]'
    '--max-depth[Max depth]' '--min-file-size[Min file size]' '--follow-links'
    '--one-file-system' '--logical-only' '--approximate' '--compat[Compatibility mode]'
    '--apparent-size' '--perf[Performance profile]' '--time' '--time-kind' '--time-style'
    '--csv[Write CSV]' '--json[Write JSON]' '--progress'
  )
  _arguments '*:: :->args' ${opts}
}
_hyperdu "$@"
