<#
.SYNOPSIS
    Generate winget manifests for the HyperDU CLI.

.DESCRIPTION
    Writes the three-file manifest set that the Windows Package Manager
    Community Repository requires (version, installer, default locale) at
    ManifestVersion 1.12.0.

    The previous version of this script emitted a single `manifest.yaml` using
    the pre-1.0 preview schema (`Id`, `Version`, `Name`), which today's winget
    rejects outright. It also left `YourOrg.HyperDU` and `Your Org` in the
    output, because the release workflow only ever substituted the URL and hash.

    InstallerType is `portable`, not `exe`. The release asset is the bare
    binary, not an installer, and winget requires every `exe` entry to support a
    silent install. `portable` is the type meant for standalone executables:
    winget puts a shim on PATH and can uninstall it again.

.PARAMETER Version
    Package version. Defaults to `cargo pkgid -p hyperdu`; the crate
    inherits version.workspace, so the literal is not in its Cargo.toml.

.PARAMETER InstallerUrl
    Download URL for the x64 executable. Omitted in a local dry run.

.PARAMETER InstallerSha256
    SHA256 of that executable, lowercase hex.

.PARAMETER OutDir
    Where to write the manifests. Defaults to dist/winget.

.EXAMPLE
    pwsh -File scripts/package/winget.ps1 -Version 0.5.0-beta.1 `
        -InstallerUrl https://example.invalid/hyperdu.exe `
        -InstallerSha256 0000000000000000000000000000000000000000000000000000000000000000
#>
Param(
  [string]$Version,
  [string]$InstallerUrl,
  [string]$InstallerSha256,
  [string]$OutDir = (Join-Path 'dist' 'winget')
)

$ErrorActionPreference = 'Stop'

if (-not $Version) {
  # Ask cargo rather than regex the manifest: the crate inherits
  # `version.workspace = true`, so the literal is not in hyperdu/Cargo.toml.
  # pkgid prints `<url>#<version>` (or `#<name>@<version>`).
  $Version = (cargo pkgid -p hyperdu) -replace '^.*[#@]', ''
}
if (-not $Version) { throw 'could not determine hyperdu version from cargo pkgid' }

# Chosen deliberately: winget-pkgs keys packages by publisher folder, and that
# folder is awkward to rename once other versions live under it.
$PackageIdentifier = 'automationjp.HyperDU'
$Publisher = 'automationjp'
$RepoUrl = 'https://github.com/automationjp/HyperDiskUsage'
$ManifestVersion = '1.12.0'
$Locale = 'en-US'

# Placeholders only when this runs outside a release, so a local invocation
# still shows the shape of the output. A release always passes both.
if (-not $InstallerUrl) { $InstallerUrl = '__URL__' }
if (-not $InstallerSha256) { $InstallerSha256 = '__SHA256__' }

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

$versionYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.$ManifestVersion.schema.json
PackageIdentifier: $PackageIdentifier
PackageVersion: "$Version"
DefaultLocale: $Locale
ManifestType: version
ManifestVersion: $ManifestVersion
"@

$installerYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.$ManifestVersion.schema.json
PackageIdentifier: $PackageIdentifier
PackageVersion: "$Version"
Platform:
  - Windows.Desktop
MinimumOSVersion: 10.0.17763.0
InstallerType: portable
Commands:
  - hyperdu
ReleaseDate: $(Get-Date -Format 'yyyy-MM-dd')
Installers:
  - Architecture: x64
    InstallerUrl: $InstallerUrl
    InstallerSha256: $InstallerSha256
ManifestType: installer
ManifestVersion: $ManifestVersion
"@

# ShortDescription is capped at 256 characters by the schema, and the line
# length guidance is 100, so this is deliberately terse.
$localeYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.$ManifestVersion.schema.json
PackageIdentifier: $PackageIdentifier
PackageVersion: "$Version"
PackageLocale: $Locale
Publisher: $Publisher
PublisherUrl: https://github.com/automationjp
PublisherSupportUrl: $RepoUrl/issues
PackageName: HyperDU
PackageUrl: $RepoUrl
License: MIT
LicenseUrl: $RepoUrl/blob/main/LICENSE
ShortDescription: Fast cross-platform disk usage analyzer.
Description: >-
  HyperDU reports where disk space went. It uses each platform's bulk
  enumeration API and a work-stealing scanner, and can read the NTFS MFT
  directly when run elevated against a volume root.
Moniker: hyperdu
Tags:
  - cli
  - disk
  - disk-usage
  - du
  - filesystem
  - storage
ManifestType: defaultLocale
ManifestVersion: $ManifestVersion
"@

$files = [ordered]@{
  "$PackageIdentifier.yaml"                = $versionYaml
  "$PackageIdentifier.installer.yaml"      = $installerYaml
  "$PackageIdentifier.locale.$Locale.yaml" = $localeYaml
}

foreach ($name in $files.Keys) {
  $path = Join-Path $OutDir $name
  # winget-pkgs requires UTF-8, and a BOM makes its validation fail.
  [System.IO.File]::WriteAllText($path, $files[$name], (New-Object System.Text.UTF8Encoding($false)))
  Write-Host "Wrote $path"
}

Write-Host ''
Write-Host "Submit with:  wingetcreate submit --token <PAT> $OutDir"
Write-Host "Or copy into: winget-pkgs/manifests/a/$Publisher/HyperDU/$Version/"
