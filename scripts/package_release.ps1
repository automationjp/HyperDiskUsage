Param(
  [switch]$SkipGui,
  [ValidateSet('generic','native')]
  [string]$CpuFlavor = 'generic',
  [switch]$Help,
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Rest
)

$ErrorActionPreference = 'Stop'

function Ensure-CargoWix {
  try {
    $exists = (Get-Command cargo-wix -ErrorAction SilentlyContinue)
    if (-not $exists) {
      Write-Host '==> Installing cargo-wix'
      cargo install cargo-wix | Out-Host
    }
  } catch {
    Write-Host '(warn) failed to install cargo-wix; MSI generation may fail'
  }
}

Ensure-CargoWix

function Show-Help {
  @'
Usage: scripts/package_release.ps1 [-SkipGui] [-CpuFlavor generic|native] [-Help]

Builds release binaries (CLI/GUI) for the current host and packages them into dist/*.zip
along with README.md. Also drops plain .exe copies on Windows hosts. If cargo-wix
is available, attempts MSI generation.

Options:
  -SkipGui         Skip building/packaging hyperdu-gui.
  -CpuFlavor       generic (portable) or native (use -C target-cpu=native).
  -Help            Show this help.

Examples:
  pwsh -File scripts/package_release.ps1
  pwsh -File scripts/package_release.ps1 -SkipGui -CpuFlavor native

Note: For cross packaging on POSIX hosts, use scripts/package_release.sh --targets ...
'@ | Write-Host
}

if ($Help -or $Rest -contains '/help' -or $Rest -contains '/?') { Show-Help; exit 0 }
$Root = Split-Path -Parent $PSCommandPath | Split-Path -Parent
Set-Location $Root

function Build-And-Capture([string]$Package, [string]$Rustflags) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = "cargo"
  $psi.Arguments = "build -p $Package --release --message-format=json"
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError  = $true
  $psi.UseShellExecute = $false
  if ($Rustflags) { $psi.EnvironmentVariables["RUSTFLAGS"] = $Rustflags }
  $p = [System.Diagnostics.Process]::Start($psi)
  $monitor = [System.Threading.Tasks.Task]::Run([System.Action]{
    while (-not $using:p.HasExited) {
      Start-Sleep -Seconds 5
      if (-not $using:p.HasExited) {
        $stamp = (Get-Date).ToString('HH:mm:ss')
        Write-Host "  ... cargo build still running ($stamp)"
      }
    }
  })
  $paths = New-Object System.Collections.Generic.HashSet[string]
  $lastHeartbeat = [DateTime]::UtcNow
  while (-not $p.StandardOutput.EndOfStream) {
    $line = $p.StandardOutput.ReadLine()
    try { $obj = $line | ConvertFrom-Json } catch { continue }
    if ($null -ne $obj -and $obj.reason -eq "compiler-artifact" -and $obj.executable) {
      [void]$paths.Add($obj.executable)
      # Show progress for binaries to avoid CI idle timeout
      if ($obj.target -and $obj.target.name) {
        Write-Host ("  built " + $obj.target.name)
      }
    } elseif ($null -ne $obj -and $obj.reason -eq "compiler-artifact") {
      # Heartbeat every ~10s for library artifacts to avoid idle cancellation
      $now = [DateTime]::UtcNow
      if (($now - $lastHeartbeat).TotalSeconds -ge 10) {
        if ($obj.target -and $obj.target.name) {
          Write-Host ("  ... building " + $obj.target.name)
        } else {
          Write-Host "  ... building (progress)"
        }
        $lastHeartbeat = $now
      }
    }
  }
  $p.WaitForExit()
  if ($monitor -and -not $monitor.IsCompleted) {
    $monitor.Wait()
  }
  if ($p.ExitCode -ne 0) {
    $err = $p.StandardError.ReadToEnd()
    throw "cargo build failed: $err"
  }
  $last = ($paths | Sort-Object | Select-Object -Last 1)
  if (-not $last) { throw "Failed to capture binary for $Package" }
  return $last
}

$osTag = 'windows'
$arch = $env:PROCESSOR_ARCHITECTURE
$Dist = Join-Path $Root 'dist'
if (Test-Path $Dist) { Remove-Item $Dist -Recurse -Force }
New-Item -ItemType Directory -Path $Dist | Out-Null

Write-Host "==> Building hyperdu-cli (release)"
$rustflags = if ($CpuFlavor -eq 'native') { '-C target-cpu=native' } else { '' }
$suffix = if ($CpuFlavor -eq 'native') { 'native' } else { 'generic' }
$cliBin = Build-And-Capture 'hyperdu-cli' $rustflags
Write-Host "  cli: $cliBin"

if (-not $SkipGui) {
  Write-Host "==> Building hyperdu-gui (release)"
  try { $guiBin = Build-And-Capture 'hyperdu-gui' $rustflags; Write-Host "  gui: $guiBin" }
  catch { Write-Host "  gui: not built (skipping)"; $guiBin = $null }
}

$cliName = "hyperdu-cli-$osTag-$arch-$suffix.zip"
$guiName = "hyperdu-gui-$osTag-$arch-$suffix.zip"

Add-Type -AssemblyName System.IO.Compression.FileSystem
function New-Zip($zipPath, $files) {
  if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
  $tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ([System.Guid]::NewGuid()))
  foreach ($f in $files) { Copy-Item $f $tmp }
  [System.IO.Compression.ZipFile]::CreateFromDirectory($tmp.FullName, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $false)
  Remove-Item $tmp -Recurse -Force
}

Write-Host "==> Packaging CLI -> $cliName"
New-Zip (Join-Path $Dist $cliName) @($cliBin, (Join-Path $Root 'README.md'))
# Also drop a plain .exe for easy run
Copy-Item $cliBin (Join-Path $Dist ("hyperdu-cli-windows-$arch-$suffix.exe")) -Force

if ($guiBin) {
  Write-Host "==> Packaging GUI -> $guiName"
  New-Zip (Join-Path $Dist $guiName) @($guiBin, (Join-Path $Root 'README.md'))
  Copy-Item $guiBin (Join-Path $Dist ("hyperdu-gui-windows-$arch-$suffix.exe")) -Force
}

Write-Host "OK -> $Dist"

# Optional: Build MSI installers with cargo-wix if available
try {
  if (Get-Command cargo-wix -ErrorAction SilentlyContinue) {
    Write-Host "==> Building MSI (cargo-wix) for hyperdu-cli"
    cargo wix -p hyperdu-cli | Out-Host
    $msiCli = Get-ChildItem -Path (Join-Path $Root 'target/wix') -Filter '*hyperdu-cli*.msi' -Recurse -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | Select-Object -Last 1
    if ($msiCli) { Copy-Item $msiCli.FullName (Join-Path $Dist 'hyperdu-cli-setup.msi') -Force }
    if (-not $SkipGui) {
      Write-Host "==> Building MSI (cargo-wix) for hyperdu-gui"
      cargo wix -p hyperdu-gui | Out-Host
      $msiGui = Get-ChildItem -Path (Join-Path $Root 'target/wix') -Filter '*hyperdu-gui*.msi' -Recurse -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | Select-Object -Last 1
      if ($msiGui) { Copy-Item $msiGui.FullName (Join-Path $Dist 'hyperdu-gui-setup.msi') -Force }
    }
  } else {
    Write-Host "info: cargo-wix not found; skipping MSI generation. Install with: cargo install cargo-wix"
  }
} catch {
  Write-Host "warn: MSI generation failed ($_)"
}
