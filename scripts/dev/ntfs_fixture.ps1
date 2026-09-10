# CI-only integration harness. It creates and formats only its own new VHDX;
# no physical disk number is selected and no existing disk is cleaned/formatted.
# https://learn.microsoft.com/windows-server/administration/windows-commands/attach-vdisk
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
    throw 'This harness is restricted to GitHub Actions. Use mft_parity directly for a live volume.'
}
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'The NTFS integration fixture requires an elevated runner.'
}
$owned = Join-Path $env:RUNNER_TEMP ('hyperdu-ntfs-' + [Guid]::NewGuid().ToString('N'))
if ($owned -match '["\r\n]' -or (Test-Path -LiteralPath $owned)) { throw 'Unsafe fixture path.' }
$null = New-Item -ItemType Directory -Path $owned
$vhd = Join-Path $owned 'fixture.vhdx'
$letter = @('Z','Y','X','W','V','U','T','S','R') | Where-Object {
    -not (Get-PSDrive -Name $_ -ErrorAction SilentlyContinue) -and
    -not (Test-Path -LiteralPath "${_}:\")
} | Select-Object -First 1
if (-not $letter) { throw 'No unused fixture drive letter.' }
$root = "${letter}:\"
$previous = @{}
foreach ($key in @('HYPERDU_MFT_PARITY_ROOT','HYPERDU_MFT_PARITY_FIXTURE','HYPERDU_MFT_DIAG')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
function Invoke-OwnedDiskPart([string[]]$Commands) {
    $script = Join-Path $owned ([Guid]::NewGuid().ToString('N') + '.txt')
    ($Commands + 'exit') | Set-Content -LiteralPath $script -Encoding ascii
    & diskpart /s $script
    if ($LASTEXITCODE -ne 0) { throw "DiskPart failed ($LASTEXITCODE)." }
}
function Assert-OwnedVolume {
    $imageDisk = Get-DiskImage -ImagePath $vhd | Get-Disk
    $partition = Get-Partition -DriveLetter $letter
    $volume = Get-Volume -DriveLetter $letter
    if ($partition.DiskNumber -ne $imageDisk.Number -or
        $volume.FileSystem -ne 'NTFS' -or $volume.FileSystemLabel -ne 'HyperDUFixture') {
        throw 'Drive letter does not belong to the newly created NTFS image.'
    }
}
try {
    Invoke-OwnedDiskPart @(
        "create vdisk file=`"$vhd`" maximum=256 type=expandable",
        'attach vdisk', 'create partition primary',
        'format fs=ntfs quick label=HyperDUFixture', "assign letter=$letter"
    )
    Assert-OwnedVolume
    $fixture = Join-Path $root 'hyperdu-fixture'
    foreach ($dir in @('plain','sparse','empty')) {
        $null = New-Item -ItemType Directory -Path (Join-Path $fixture $dir) -Force
    }
    $payload = [byte[]]::new(65536)
    for ($i = 0; $i -lt $payload.Length; $i++) { $payload[$i] = [byte]($i % 251) }
    for ($i = 0; $i -lt 128; $i++) {
        [IO.File]::WriteAllBytes((Join-Path $fixture "plain\file-$i.bin"), $payload)
    }
    [IO.File]::WriteAllBytes((Join-Path $root 'root-file.bin'), $payload)
    # An alternate stream is diagnostic data, not extra unnamed-file bytes.
    [IO.File]::WriteAllBytes((Join-Path $fixture 'plain\file-0.bin:fixture-ads'), [byte[]]::new(8192))
    & fsutil hardlink create (Join-Path $fixture 'plain\alias.bin') (Join-Path $fixture 'plain\file-0.bin')
    if ($LASTEXITCODE -ne 0) { throw 'Could not create fixture hardlink.' }
    foreach ($item in @(@('hole.bin',1048576), @('empty.bin',0))) {
        $path = Join-Path $fixture ('sparse\' + $item[0])
        [IO.File]::WriteAllBytes($path, [byte[]]::new(0))
        & fsutil sparse setflag $path
        if ($LASTEXITCODE -ne 0) { throw 'Could not mark fixture sparse.' }
        $file = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $file.SetLength([long]$item[1]); $file.Flush($true) } finally { $file.Dispose() }
    }
    # Flush NTFS metadata by detaching, then forbid mutations during comparison.
    Invoke-OwnedDiskPart @("select vdisk file=`"$vhd`"", 'detach vdisk', 'attach vdisk readonly')
    if (-not (Test-Path -LiteralPath $root)) {
        Invoke-OwnedDiskPart @("select vdisk file=`"$vhd`"", 'select partition 1', "assign letter=$letter")
    }
    Assert-OwnedVolume
    $env:HYPERDU_MFT_PARITY_ROOT = $root
    $env:HYPERDU_MFT_PARITY_FIXTURE = '1'
    $env:HYPERDU_MFT_DIAG = '1'
    Write-Host "Testing actual MFT and enumeration on read-only fixture $root"
    & cargo test -p hyperdu-core -p hyperdu -p hyperdu-gui -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Core/CLI tests failed ($LASTEXITCODE)." }
} finally {
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
    if (Test-Path -LiteralPath $vhd) {
        Invoke-OwnedDiskPart @("select vdisk file=`"$vhd`"", 'detach vdisk')
    }
    # This path is a new GUID-named directory owned by this invocation only.
    Remove-Item -LiteralPath $owned -Recurse -Force
}
