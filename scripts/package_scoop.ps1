Param(
  [string]$Version
)

if (-not $Version) {
  $Version = (Select-String -Path 'hyperdu-cli/Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
}

$outDir = Join-Path 'dist' 'scoop'
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

$manifest = [ordered]@{
  version = $Version
  description = 'Hyper-fast disk usage analyzer CLI'
  homepage = 'https://github.com/your-org/HyperDiskUsage'
  license = 'MIT'
  architecture = @{ x64 = @{ url = '__URL__'; hash = '__SHA256__' } }
  bin = 'hyperdu.exe'
}

$json = $manifest | ConvertTo-Json -Depth 5
Set-Content -Path (Join-Path $outDir 'hyperdu.json') -Value $json -Encoding UTF8
Write-Host "Wrote scoop manifest: $outDir/hyperdu.json"
