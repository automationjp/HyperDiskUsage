Param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Rest
)

$ErrorActionPreference = 'Stop'

function Find-GitBash {
  $cands = @(
    'C:\Program Files\Git\bin\bash.exe',
    'C:\Program Files\Git\usr\bin\bash.exe',
    'C:\Program Files (x86)\Git\bin\bash.exe'
  )
  foreach ($p in $cands) { if (Test-Path $p) { return $p } }
  # Fallback to PATH
  $bash = (Get-Command bash -ErrorAction SilentlyContinue)?.Source
  if ($bash) { return $bash }
  throw "Git Bash not found. Install Git for Windows or ensure bash.exe is in PATH."
}

$bashExe = Find-GitBash

# Preserve current working dir and run the POSIX script under Git Bash so that
# Windows Git Credential Manager is used (same environment as manual git push).
$wd = (Get-Location).Path

# Build the argument string to forward to the shell script
$forward = $Rest -join ' '

# Use -lc to run a login shell that executes our commands, then exits
$cmd = "cd `"$wd`"; bash scripts/release_tag.sh $forward"

Write-Host "==> Invoking Git Bash: $bashExe"
Write-Host "    -> $cmd"

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $bashExe
$psi.Arguments = "-lc `"$cmd`""
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true

$p = [System.Diagnostics.Process]::Start($psi)
$p.WaitForExit()
Write-Output $p.StandardOutput.ReadToEnd()
Write-Error $p.StandardError.ReadToEnd() -ErrorAction SilentlyContinue
if ($p.ExitCode -ne 0) { exit $p.ExitCode }

