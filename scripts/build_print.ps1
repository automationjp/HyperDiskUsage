Param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Rest
)

# Build with Cargo, printing produced executables after a successful build.
# This is a convenience wrapper that forwards all args to `cargo build`.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/build_print.ps1 [CARGO_BUILD_ARGS...]
#
# Examples:
#   powershell -ExecutionPolicy Bypass -File scripts/build_print.ps1 -p hyperdu-cli --release
#   powershell -ExecutionPolicy Bypass -File scripts/build_print.ps1 -p hyperdu-gui
#
# Environment:
#   HYPERDU_TIMINGS=1      -> pass `--timings` to cargo (report in target/cargo-timings)
#   HYPERDU_SELF_PROFILE=1 -> add `-Z self-profile` to RUSTFLAGS (nightly required)
#   HYPERDU_NIGHTLY=1      -> prefer `cargo +nightly`
#   HYPERDU_LOG=1          -> tee verbose logs to dist/build_*.log (POSIX wrapper)

function Show-Help {
  $lines = Get-Content -Path $PSCommandPath -TotalCount 40
  $lines | Where-Object { $_ -match '^#' } | ForEach-Object { $_ -replace '^#\s?', '' }
  Write-Host "\nPrints built executables (unique paths) to stdout." -ForegroundColor DarkGray
}

if ($Rest -contains '--help' -or $Rest -contains '-h' -or $Rest -contains '/help' -or $Rest -contains '/?') {
  Show-Help
  exit 0
}

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "cargo"
$psi.Arguments = "build --message-format=json " + ($Rest -join ' ')
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.UseShellExecute = $false

$proc = [System.Diagnostics.Process]::Start($psi)
$paths = New-Object System.Collections.Generic.HashSet[string]

while (-not $proc.StandardOutput.EndOfStream) {
  $line = $proc.StandardOutput.ReadLine()
  try {
    $obj = $line | ConvertFrom-Json
  } catch {
    continue
  }
  if ($null -ne $obj -and $obj.reason -eq "compiler-artifact" -and $obj.executable) {
    [void]$paths.Add($obj.executable)
  }
}

$proc.WaitForExit()

$paths | Sort-Object

if ($proc.ExitCode -ne 0) {
  exit $proc.ExitCode
}
