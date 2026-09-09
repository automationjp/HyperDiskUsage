<#
.SYNOPSIS
    Install the HyperDU binaries this skill needs, and register the MCP server.

.DESCRIPTION
    This builds from source with `cargo`, so a Rust toolchain is required. That
    is worth stating plainly rather than leaving someone to discover it after a
    failed `winget install`.

    Prebuilt archives and published crates both exist -- the repository README
    lists them -- but this script installs two binaries on whichever platform it
    lands on, and choosing the right archive for each, then verifying it, is
    more than a setup script should decide on its owner's behalf.

    Registering an MCP server rewrites an agent's configuration, so this prints
    the command by default and only runs it when asked with -Register.
    Installing a binary is reversible with `cargo uninstall`; silently editing
    someone's agent config is the kind of surprise that makes a setup script
    untrustworthy.

.PARAMETER Check
    Report what is installed and change nothing.

.PARAMETER Register
    After installing, register the MCP server with any detected client.

.EXAMPLE
    .\setup-hyperdu.ps1
    Install what is missing and print how to register the MCP server.

.EXAMPLE
    .\setup-hyperdu.ps1 -Register
    Install, then register with the detected clients.
#>
[CmdletBinding()]
param(
    [switch]$Check,
    [switch]$Register
)

$ErrorActionPreference = 'Stop'

$RepoUrl = 'https://github.com/automationjp/HyperDiskUsage'
# The crate is `hyperdu-cli`; the command it installs is `hyperdu`, declared
# by a [[bin]] section. That matches what the deb and rpm packages install.
$CliBin = 'hyperdu'
$McpBin = 'hyperdu-mcp'
$McpName = 'hyperdu'

function Test-Installed {
    param([string]$Name)
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Get-InstalledPath {
    param([string]$Name)
    (Get-Command $Name -ErrorAction SilentlyContinue).Source
}

# Resolve the repository root when this script is run from inside a checkout, so
# a contributor testing local changes installs those rather than whatever is on
# the default branch. Layout: <root>\plugin\skills\<skill>\scripts\<this file>.
function Get-RepoRoot {
    $candidate = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..') -ErrorAction SilentlyContinue
    if (-not $candidate) { return $null }
    $manifest = Join-Path $candidate 'Cargo.toml'
    if (-not (Test-Path $manifest)) { return $null }
    if (-not (Select-String -Path $manifest -Pattern 'hyperdu-core' -Quiet)) { return $null }
    return $candidate.Path
}

function Write-BinaryReport {
    foreach ($bin in @($CliBin, $McpBin)) {
        if (Test-Installed $bin) {
            Write-Host "  present: $bin ($(Get-InstalledPath $bin))"
        }
        else {
            Write-Host "  MISSING: $bin"
        }
    }
}

Write-Host 'HyperDU setup'
Write-Host ''
Write-Host 'Binaries:'
Write-BinaryReport
Write-Host ''

if ($Check) {
    if ((Test-Installed $CliBin) -and (Test-Installed $McpBin)) { exit 0 }
    Write-Host 'Run this script without -Check to install what is missing.'
    exit 1
}

$missing = @($CliBin, $McpBin) | Where-Object { -not (Test-Installed $_) }

if ($missing.Count -gt 0) {
    if (-not (Test-Installed 'cargo')) {
        Write-Error @"
cargo not found, and this script installs by building from source.

Install a Rust toolchain first:
  https://rustup.rs

Then run this script again.

Or install the two binaries yourself and re-run with -Check: prebuilt archives
are attached to each release, and the crates are published.
  $RepoUrl/releases
"@
        exit 1
    }

    $root = Get-RepoRoot
    if ($root) {
        Write-Host "Installing from this checkout: $root"
    }
    else {
        Write-Host "Installing from $RepoUrl"
    }

    foreach ($bin in $missing) {
        Write-Host ''
        if ($root) {
            $crateDir = Join-Path $root $bin
            Write-Host "==> cargo install --path $crateDir"
            & cargo install --path $crateDir
        }
        else {
            Write-Host "==> cargo install --git $RepoUrl $bin"
            & cargo install --git $RepoUrl $bin
        }
        if ($LASTEXITCODE -ne 0) {
            Write-Error "cargo install failed for $bin"
            exit 1
        }
    }

    # cargo appends ~/.cargo/bin to the user PATH on first install, but this
    # process inherited its environment before that happened. Without the
    # refresh the freshly installed binaries report as missing and the next
    # step looks broken.
    $userPath = [System.Environment]::GetEnvironmentVariable('PATH', 'User')
    if ($userPath) { $env:PATH = "$userPath;$env:PATH" }

    Write-Host ''
    Write-Host 'Binaries after install:'
    Write-BinaryReport
    Write-Host ''

    if (-not (Test-Installed $CliBin) -or -not (Test-Installed $McpBin)) {
        Write-Warning @'
cargo finished but the binaries are not on PATH.
They are in %USERPROFILE%\.cargo\bin. Add it to PATH, for example:

  $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

'@
    }
}

# --- MCP registration -------------------------------------------------------
#
# Installing the server binary is not enough: a client only sees it once it has
# been registered.

Write-Host 'MCP server registration:'

function Invoke-ClientRegistration {
    param(
        [string]$Client,
        [string[]]$Arguments
    )
    if (-not (Test-Installed $Client)) { return $false }

    $display = "$Client $($Arguments -join ' ')"
    if ($Register) {
        Write-Host "  ==> $display"
        & $Client @Arguments
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  registered with $Client"
        }
        else {
            Write-Warning "  $Client registration failed; run the command above by hand"
        }
    }
    else {
        Write-Host "  $Client detected. To register:"
        Write-Host "    $display"
    }
    return $true
}

$claudeFound = Invoke-ClientRegistration -Client 'claude' `
    -Arguments @('mcp', 'add', '--transport', 'stdio', $McpName, '--', $McpBin)
$codexFound = Invoke-ClientRegistration -Client 'codex' `
    -Arguments @('mcp', 'add', $McpName, '--', $McpBin)

if (-not ($claudeFound -or $codexFound)) {
    Write-Host @"
  No claude or codex CLI found on PATH.

  For any other MCP client, add a stdio server that runs:
    $McpBin

  The bundled plugin/mcp.json already declares exactly that.
"@
}
elseif (-not $Register) {
    Write-Host ''
    Write-Host 'Re-run with -Register to have this script issue those commands.'
}

Write-Host ''
Write-Host 'Done.'
