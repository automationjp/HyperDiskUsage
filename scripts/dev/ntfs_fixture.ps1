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
foreach ($key in @('HYPERDU_MFT_PARITY_ROOT','HYPERDU_MFT_PARITY_FIXTURE','HYPERDU_MFT_DIAG','HYPERDU_TEST_USN_ROOT','HYPERDU_MFT_IO','HYPERDU_SIMD')) {
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
        "create vdisk file=`"$vhd`" maximum=512 type=expandable",
        'attach vdisk', 'create partition primary',
        'format fs=ntfs quick label=HyperDUFixture', "assign letter=$letter"
    )
    Assert-OwnedVolume
    $fixture = Join-Path $root 'hyperdu-fixture'
    foreach ($dir in @('plain','sparse','empty','compressed','populated-sparse','attribute-list','encrypted')) {
        $null = New-Item -ItemType Directory -Path (Join-Path $fixture $dir) -Force
    }
    $payload = [byte[]]::new(65536)
    for ($i = 0; $i -lt $payload.Length; $i++) { $payload[$i] = [byte]($i % 251) }
    for ($i = 0; $i -lt 2048; $i++) {
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
    # Partially allocated sparse ranges exercise actual run allocation, not just holes.
    $populated = Join-Path $fixture 'populated-sparse\partial.bin'
    [IO.File]::WriteAllBytes($populated, [byte[]]::new(0))
    & fsutil sparse setflag $populated
    if ($LASTEXITCODE -ne 0) { throw 'Could not mark populated fixture sparse.' }
    $file = [IO.File]::OpenWrite($populated)
    try {
        $file.SetLength(8MB)
        $file.Write($payload, 0, 4096)
        $null = $file.Seek(4MB, [IO.SeekOrigin]::Begin)
        $file.Write($payload, 0, $payload.Length)
        $file.Flush($true)
    } finally { $file.Dispose() }

    $compressed = Join-Path $fixture 'compressed\mixed.bin'
    $compressible = [byte[]]::new(1MB + 313)
    [Array]::Copy($payload, $compressible, $payload.Length)
    [IO.File]::WriteAllBytes($compressed, $compressible)
    & compact /c /i /q /f $compressed
    if ($LASTEXITCODE -ne 0 -or -not ((Get-Item -LiteralPath $compressed).Attributes -band [IO.FileAttributes]::Compressed)) {
        throw 'NTFS compression was not exercised.'
    }

    # Many long named attributes force extension records/ATTRIBUTE_LIST while
    # only the unnamed stream contributes to the public file totals.
    $attributes = Join-Path $fixture 'attribute-list\payload.bin'
    [IO.File]::WriteAllBytes($attributes, $payload)
    for ($i = 0; $i -lt 64; $i++) {
        $stream = 'stream-' + $i + '-' + ('x' * 80)
        [IO.File]::WriteAllBytes(($attributes + ':' + $stream), [byte[]]::new(48))
    }
    for ($i = 0; $i -lt 32; $i++) {
        & fsutil hardlink create (Join-Path $fixture ('attribute-list\alias-' + $i + '.bin')) $attributes
        if ($LASTEXITCODE -ne 0) { throw 'Could not create attribute-list fixture hardlink.' }
    }

    # CI-only: EFS may create a certificate in this disposable runner account.
    $encrypted = Join-Path $fixture 'encrypted\efs.bin'
    [IO.File]::WriteAllBytes($encrypted, $payload)
    & cipher /e /a $encrypted
    if ($LASTEXITCODE -ne 0 -or -not ((Get-Item -LiteralPath $encrypted).Attributes -band [IO.FileAttributes]::Encrypted)) {
        Write-Host 'NOT RUN: EFS is unavailable on this runner.'
        Remove-Item -LiteralPath $encrypted
    } else { Write-Host 'EFS native fixture enabled.' }

    # Only this new image receives a journal. Never create or change one on a
    # user volume. Mutable replay tests precede the immutable MFT comparison.
    Assert-OwnedVolume
    # fsutil createjournal expects a drive designator (Z:), not a root path (Z:\).
    # https://learn.microsoft.com/windows-server/administration/windows-commands/fsutil-usn
    & fsutil usn createjournal m=8388608 a=1048576 "${letter}:"
    if ($LASTEXITCODE -ne 0) { throw 'Could not create journal on the owned fixture.' }
    $usn = Join-Path $root 'hyperdu-usn'
    $null = New-Item -ItemType Directory -Path $usn
    $env:HYPERDU_TEST_USN_ROOT = $usn
    & cargo test --locked -p hyperdu-core index:: --lib -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Native USN/index tests failed.' }
    [Environment]::SetEnvironmentVariable('HYPERDU_TEST_USN_ROOT', $null, 'Process')
    if (@(Get-ChildItem -LiteralPath $usn -Force).Count -ne 0) { throw 'USN tests left fixture children.' }
    Remove-Item -LiteralPath $usn
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
    & cargo test --locked -p hyperdu-core -p hyperdu -p hyperdu-gui -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Core/CLI tests failed ($LASTEXITCODE)." }
    foreach ($mode in @('sync','overlapped','unbuffered')) {
        $env:HYPERDU_MFT_IO = $mode
        Write-Host "Testing immutable raw MFT mode: $mode"
        & cargo test --locked -p hyperdu-core --test mft_parity -- --nocapture
        if ($LASTEXITCODE -ne 0) { throw "MFT $mode parity failed." }
    }
    # All measurements use the same read-only corpus and optimized test binary.
    # The test checks every mode's complete records and aggregates for equality.
    $buildRows = & cargo test --locked --release -p hyperdu-core --lib --no-run --message-format=json
    if ($LASTEXITCODE -ne 0) { throw 'Raw MFT benchmark build failed.' }
    $binary = @($buildRows | ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'hyperdu_core' -and $_.executable } |
        ForEach-Object { $_.executable })
    if ($binary.Count -ne 1) { throw 'Expected exactly one optimized core test binary.' }
    $record = @{
        head = (& git rev-parse HEAD).Trim()
        binary_sha256 = (Get-FileHash -LiteralPath $binary[0] -Algorithm SHA256).Hash
        build_command = 'cargo test --locked --release -p hyperdu-core --lib --no-run'
        lto = $env:CARGO_PROFILE_RELEASE_LTO
        codegen_units = $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS
        rustc = (& rustc -Vv) -join [Environment]::NewLine
        cache = 'warm after one untimed round per mode'
        corpus = 'owned read-only 512MiB NTFS VHD; 2048 plain files and complex allocation fixtures'
        limitations = 'internal MFT reader plus aggregation; not whole CLI throughput or cold-storage latency'
    }
    $record | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:RUNNER_TEMP 'hyperdu-mft-build.json') -Encoding utf8
    foreach ($simd in @('scalar','auto')) {
        $env:HYPERDU_SIMD = $simd
        & $binary[0] 'platform::windows_impl::mft_reader::overlapped::raw_volume_tests::benchmark_owned_raw_pipeline' --exact --ignored --nocapture --test-threads=1 2>&1 |
            Tee-Object -FilePath (Join-Path $env:RUNNER_TEMP "hyperdu-mft-$simd.log")
        if ($LASTEXITCODE -ne 0) { throw "Raw MFT benchmark $simd failed." }
    }
} finally {
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
    if (Test-Path -LiteralPath $vhd) {
        Invoke-OwnedDiskPart @("select vdisk file=`"$vhd`"", 'detach vdisk')
    }
    # This path is a new GUID-named directory owned by this invocation only.
    # Resolve the exact invocation-owned directory before recursive cleanup.
    $resolvedParent = (Resolve-Path -LiteralPath $env:RUNNER_TEMP).Path.TrimEnd('\')
    $resolvedOwned = (Resolve-Path -LiteralPath $owned).Path
    if (-not $resolvedOwned.StartsWith($resolvedParent + '\', [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolvedOwned) -notmatch '^hyperdu-ntfs-[0-9a-f]{32}$') {
        throw 'Refusing cleanup outside the invocation-owned fixture directory.'
    }
    Remove-Item -LiteralPath $resolvedOwned -Recurse -Force
}
