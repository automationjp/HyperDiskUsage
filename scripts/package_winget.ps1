Param(
  [string]$Version
)

if (-not $Version) {
  $Version = (Select-String -Path 'hyperdu-cli/Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
}

$outDir = Join-Path 'dist' 'winget'
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

$id = 'YourOrg.HyperDU'
$installerUrl = '__URL__'
$sha256 = '__SHA256__'

$yaml = @"
Id: $id
Version: $Version
Name: HyperDU
Publisher: Your Org
License: MIT
InstallerType: exe
Installers:
  - Architecture: x64
    InstallerUrl: $installerUrl
    InstallerSha256: $sha256
"@

Set-Content -Path (Join-Path $outDir 'manifest.yaml') -Value $yaml -Encoding UTF8
Write-Host "Wrote winget manifest: $outDir/manifest.yaml"

# Optional: open PR using GitHub CLI (requires manual configuration)
# gh repo fork microsoft/winget-pkgs --clone=false
# gh pr create -R microsoft/winget-pkgs -t "Add HyperDU $Version" -b "Automated manifest" -B master
