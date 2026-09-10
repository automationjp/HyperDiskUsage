# Offline installer argument regression. Mocks cargo/hyperdu; never builds or registers.
$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot '../../plugin/skills/disk-space-triage/scripts/setup-hyperdu.ps1'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('hyperdu-installer-' + [guid]::NewGuid())
$originalPath = $env:PATH
try {
    $scripts = Join-Path $fixture 'plugin/skills/disk-space-triage/scripts'
    New-Item -ItemType Directory -Path $scripts -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $scripts
    function global:hyperdu {
        $global:LASTEXITCODE = if ($global:mockInstalled) { 0 } else { 1 }
        if ($global:mockInstalled) { 'Usage: hyperdu mcp' }
    }
    function global:cargo {
        $global:installArgs = @($args)
        $global:mockInstalled = $true
        $global:LASTEXITCODE = 0
    }
    foreach ($local in @($false, $true)) {
        $global:mockInstalled = $false
        if ($local) {
            New-Item -ItemType Directory -Path (Join-Path $fixture 'hyperdu') | Out-Null
            New-Item -ItemType File -Path (Join-Path $fixture 'hyperdu/Cargo.toml') | Out-Null
        }
        & (Join-Path $scripts 'setup-hyperdu.ps1')
        $expected = @('install', '--locked', '--force')
        if ($local) { $expected += @('--path', (Join-Path $fixture 'hyperdu')) }
        else {
            $expected += @('--git', 'https://github.com/automationjp/HyperDiskUsage', '--rev',
                'c535dd8b34e877ac93170ab941dccf020f6b3c2d', 'hyperdu')
        }
        if (($global:installArgs -join "`n") -cne ($expected -join "`n")) {
            throw 'Installer arguments differed from expected locked local/pinned remote install.'
        }
    }
    'PASS: PowerShell local and remote installer arguments (mocked)'
} finally {
    $env:PATH = $originalPath
    Remove-Item Function:/cargo, Function:/hyperdu -ErrorAction SilentlyContinue
    Remove-Variable mockInstalled, installArgs -Scope Global -ErrorAction SilentlyContinue
    $resolved = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing cleanup outside the temporary root.'
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
