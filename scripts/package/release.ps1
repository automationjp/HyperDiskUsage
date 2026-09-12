Param(
  [switch]$SkipGui,
  [switch]$SkipMsi,
  [ValidateSet('generic','native')]
  [string]$CpuFlavor = 'generic',
  [switch]$Help,
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Rest
)

$ErrorActionPreference = 'Stop'

function Show-Help {
  @'
Usage: scripts/package/release.ps1 [-SkipGui] [-SkipMsi] [-CpuFlavor generic|native] [-Help]

Builds release binaries (CLI/GUI) for the current host and packages them into dist/*.zip
along with README.md. Also drops plain .exe copies on Windows hosts and builds the
MSI installers with cargo-wix + WiX Toolset 3.x (required unless -SkipMsi).

Options:
  -SkipGui         Skip building/packaging hyperdu-gui.
  -SkipMsi         Skip the MSI installers (local runs without WiX Toolset 3.x).
  -CpuFlavor       generic (portable) or native (use -C target-cpu=native).
  -Help            Show this help.

Examples:
  pwsh -File scripts/package/release.ps1
  pwsh -File scripts/package/release.ps1 -SkipGui -CpuFlavor native

Note: For cross packaging on POSIX hosts, use scripts/package/release.sh --targets ...
'@ | Write-Host
}

if ($Help -or $Rest -contains '/help' -or $Rest -contains '/?') { Show-Help; exit 0 }
$Root = Split-Path -Parent $PSCommandPath | Split-Path -Parent | Split-Path -Parent
Set-Location $Root

function Build-And-Capture([string]$Package, [string]$Rustflags) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = "cargo"
  $psi.Arguments = "build -p $Package --release --message-format=json"
  $psi.RedirectStandardOutput = $true
  # stderr is deliberately NOT redirected. This loop only drains stdout, and it
  # reads to EOF before WaitForExit; cargo writes its "Compiling ..." progress to
  # stderr, so on a cold CI build that pipe fills its buffer, cargo blocks on the
  # write, stdout stops, and both sides wait forever -- the windows job would sit
  # there until the 6-hour Actions ceiling. Letting stderr inherit the console
  # also puts cargo's progress straight into the Actions log.
  $psi.RedirectStandardError  = $false
  $psi.UseShellExecute = $false
  if ($Rustflags) { $psi.EnvironmentVariables["RUSTFLAGS"] = $Rustflags }
  $p = [System.Diagnostics.Process]::Start($psi)
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
  if ($p.ExitCode -ne 0) {
    # No captured stderr to quote any more: it went to the console, where the
    # Actions log already shows cargo's own diagnostics above this line.
    throw "cargo build failed for $Package (exit $($p.ExitCode))"
  }
  $last = ($paths | Sort-Object | Select-Object -Last 1)
  if (-not $last) { throw "Failed to capture binary for $Package" }
  return $last
}

$osTag = 'windows'
# PROCESSOR_ARCHITECTURE says "AMD64"; every other part of the project says
# "x86_64". Left raw, this produced hyperdu-windows-AMD64-generic.zip while
# scripts/package/scoop.ps1 built an autoupdate URL pointing at
# hyperdu-windows-x86_64-generic.zip, and the Linux artifacts from
# release.sh used x86_64 too. Normalize once, here, before any name is built.
$hostArchitecture = $env:PROCESSOR_ARCHITECTURE
if (-not $hostArchitecture) {
  $hostArchitecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
}
$arch = switch ($hostArchitecture.ToUpperInvariant()) {
  'X64' { 'x86_64' }
  'AMD64' { 'x86_64' }
  'ARM64' { 'aarch64' }
  default { throw "Unsupported host architecture: $hostArchitecture" }
}
$Dist = Join-Path $Root 'dist'
if (Test-Path $Dist) { Remove-Item $Dist -Recurse -Force }
New-Item -ItemType Directory -Path $Dist | Out-Null

Write-Host "==> Building hyperdu (release)"
$rustflags = if ($CpuFlavor -eq 'native') { '-C target-cpu=native' } else { '' }
$suffix = if ($CpuFlavor -eq 'native') { 'native' } else { 'generic' }
$cliBin = Build-And-Capture 'hyperdu' $rustflags
Write-Host "  cli: $cliBin"

if (-not $SkipGui) {
  Write-Host "==> Building hyperdu-gui (release)"
  $guiBin = Build-And-Capture 'hyperdu-gui' $rustflags
  Write-Host "  gui: $guiBin"
}

$cliName = "hyperdu-$osTag-$arch-$suffix.zip"
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
Copy-Item $cliBin (Join-Path $Dist ("hyperdu-windows-$arch-$suffix.exe")) -Force

if ($guiBin) {
  Write-Host "==> Packaging GUI -> $guiName"
  New-Zip (Join-Path $Dist $guiName) @($guiBin, (Join-Path $Root 'README.md'))
  Copy-Item $guiBin (Join-Path $Dist ("hyperdu-gui-windows-$arch-$suffix.exe")) -Force
}

Write-Host "OK -> $Dist"

# MSI installers. Required, not best-effort: v0.5.0-beta.2 (run 34180925269)
# printed "There are no WXS files to create an installer", copied nothing and
# stayed green, because cargo-wix is a native command whose exit code never
# reaches a PowerShell catch block. Fail on every way this can go wrong:
# tool missing, WiX missing, non-zero exit, no or empty output.
function Build-Msi([string]$Package, [string]$OutName) {
  # The member manifest is the INPUT argument on purpose. cargo-wix 0.3.9 runs
  # light.exe with `-b <directory of INPUT>` and never changes directory, so
  # `wix\License.rtf` and `assets\hyperdu.ico` in <crate>/wix/main.wxs only
  # resolve when INPUT is <crate>/Cargo.toml; from the workspace root they
  # point at a wix/ and assets/ that do not exist. -p is still required:
  # cargo metadata on a member manifest still lists all workspace members.
  $manifest = Join-Path $Root "$Package\Cargo.toml"
  $out = Join-Path $Dist $OutName
  Write-Host "==> Building MSI (cargo-wix) for $Package -> $OutName"
  # --no-build: the exe was built above with $RUSTFLAGS; a rebuild by cargo-wix
  # without them would silently replace a -native binary with a generic one.
  # -o with a file path is used verbatim; without it the MSI lands in
  # <cargo target dir>\wix\hyperdu-0.5.0.3-x86_64.msi, which is not under
  # $Root\target when a target-dir is configured (F:/cargo-target locally).
  cargo wix -p $Package --no-build --nocapture -o $out $manifest | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "cargo wix failed for $Package (exit $LASTEXITCODE)" }
  if (-not (Test-Path $out)) { throw "cargo wix exited 0 but $out was not written" }
  $bytes = (Get-Item $out).Length
  if ($bytes -lt 100KB) { throw "$OutName is $bytes bytes; the executable is not inside it" }
  Write-Host "  msi: $out ($bytes bytes)"
}

if ($SkipMsi) {
  Write-Host "info: -SkipMsi given; MSI installers not built"
} else {
  if (-not (Get-Command cargo-wix -ErrorAction SilentlyContinue)) {
    throw 'cargo-wix is not installed; run: cargo install cargo-wix@0.3.9 --locked'
  }
  if (-not (Get-Command candle.exe -ErrorAction SilentlyContinue)) {
    throw 'WiX Toolset 3.x (candle.exe) is not on PATH; cargo-wix 0.3.9 cannot use the dotnet wix tool (v4+)'
  }
  # Same stem as the zip/exe beside it: hyperdu-windows-x86_64-generic.msi.
  Build-Msi 'hyperdu' "hyperdu-$osTag-$arch-$suffix.msi"
  if (-not $SkipGui) { Build-Msi 'hyperdu-gui' "hyperdu-gui-$osTag-$arch-$suffix.msi" }
}
