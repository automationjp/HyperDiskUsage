<#
.SYNOPSIS
Install the unified HyperDU command (CLI + MCP).
.DESCRIPTION
Builds from this checkout when available, otherwise from GitHub. Client
registration is opt-in with -Register. -Check changes nothing.
#>
[CmdletBinding()]
param([switch]$Check, [switch]$Register)
$ErrorActionPreference = 'Stop'
# Audited source revision; update together with the POSIX installer.
$SourceRev = '5f36fca6f955c727e4ffa2dbd7d4784568f2690c'
$RepoUrl = 'https://github.com/automationjp/HyperDiskUsage'

function Test-UnifiedHyperdu {
    if (-not (Get-Command hyperdu -ErrorAction SilentlyContinue)) { return $false }
    $helpText = & hyperdu mcp --help 2>&1 | Out-String
    return $LASTEXITCODE -eq 0 -and $helpText -match 'Usage:\s+hyperdu(?:\.exe)? mcp'
}

if ($Check) {
    if (Test-UnifiedHyperdu) { Write-Host 'Ready: hyperdu (CLI + MCP)'; exit 0 }
    Write-Host 'Missing unified hyperdu. Run this script without -Check to install.'
    exit 1
}

if (-not (Test-UnifiedHyperdu)) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'Rust 1.88+ is needed to build from source. See https://rustup.rs'
    }
    $sourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../../..'))
    $crateDir = Join-Path $sourceRoot 'hyperdu'
    if (Test-Path -LiteralPath (Join-Path $crateDir 'Cargo.toml')) {
        & cargo install --locked --force --path $crateDir
    } else {
        & cargo install --locked --force --git $RepoUrl --rev $SourceRev hyperdu
    }
    if ($LASTEXITCODE -ne 0) { throw 'HyperDU installation failed.' }
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $env:PATH = "$(Join-Path $cargoHome 'bin');$env:PATH"
    if (-not (Test-UnifiedHyperdu)) { throw 'Installed command is unavailable or lacks MCP. Check PATH.' }
}

Write-Host 'Ready: hyperdu (CLI + MCP)'
foreach ($client in @('claude', 'codex')) {
    $clientArgs = if ($client -eq 'claude') {
        @('mcp', 'add', '--transport', 'stdio', 'hyperdu', '--', 'hyperdu', 'mcp')
    } else { @('mcp', 'add', 'hyperdu', '--', 'hyperdu', 'mcp') }
    Write-Host "$client $($clientArgs -join ' ')"
    if ($Register -and (Get-Command $client -ErrorAction SilentlyContinue)) {
        & $client @clientArgs
        if ($LASTEXITCODE -ne 0) { throw "$client MCP registration failed." }
    }
}
Write-Host 'Other MCP clients: command="hyperdu", args=["mcp"].'
