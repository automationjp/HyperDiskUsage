Param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Rest
)

$ErrorActionPreference = 'Stop'

function Show-Help {
  @'
Usage: scripts/lint.ps1

Runs formatting and lint checks for the workspace:
  - cargo fmt --all -- --check
  - cargo clippy --workspace -- -D warnings
  - cargo deny check   (if cargo-deny is installed)

Options:
  -h, --help, /help, /?   Show this help
'@ | Write-Host
}

if ($Rest -contains '--help' -or $Rest -contains '-h' -or $Rest -contains '/help' -or $Rest -contains '/?') {
  Show-Help
  exit 0
}

Write-Host '==> rustfmt (check)'
cargo fmt --all -- --check

Write-Host '==> clippy (workspace, deny warnings)'
cargo clippy --workspace -- -D warnings

if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
  Write-Host '==> cargo-deny'
  cargo deny check
} else {
  Write-Host '(info) cargo-deny not found; skipping dependency audit'
}

Write-Host 'OK'
