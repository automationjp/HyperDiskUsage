<#
.SYNOPSIS
    Generate a Scoop manifest for the HyperDU CLI.

.DESCRIPTION
    Two things in an earlier version made the resulting manifest unusable:

      * `bin` did not match what actually shipped, so Scoop installed the
        package and left no working command behind. The crate now declares its
        binary as `hyperdu`, and that is what the archive contains.
      * `homepage` pointed at `github.com/your-org/HyperDiskUsage`, which does
        not exist. Only the URL and hash were ever substituted at release time.

    The download is the zip, not the bare exe: Scoop extracts the archive and
    then resolves `bin` inside it, so the shim keeps a stable name regardless of
    what the release asset is called.

.PARAMETER Version
    Package version. Defaults to the version in hyperdu/Cargo.toml.

.PARAMETER Url
    Download URL for the x64 zip. Omitted in a local dry run.

.PARAMETER Sha256
    SHA256 of that zip, lowercase hex.

.PARAMETER OutDir
    Where to write the manifest. Defaults to dist/scoop.
#>
Param(
  [string]$Version,
  [string]$Url,
  [string]$Sha256,
  [string]$OutDir = (Join-Path 'dist' 'scoop')
)

$ErrorActionPreference = 'Stop'

if (-not $Version) {
  # Ask cargo rather than regex the manifest: the crate inherits
  # `version.workspace = true`, so the literal is not in hyperdu/Cargo.toml.
  # pkgid prints `<url>#<version>` (or `#<name>@<version>`).
  $Version = (cargo pkgid -p hyperdu) -replace '^.*[#@]', ''
}
if (-not $Version) { throw 'could not determine hyperdu version from cargo pkgid' }

# Placeholders only for a local dry run; a release passes both.
if (-not $Url) { $Url = '__URL__' }
if (-not $Sha256) { $Sha256 = '__SHA256__' }

$repo = 'https://github.com/automationjp/HyperDiskUsage'

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

$manifest = [ordered]@{
  version      = $Version
  description  = 'Fast cross-platform disk usage analyzer'
  homepage     = $repo
  license      = 'MIT'
  architecture = [ordered]@{
    '64bit' = [ordered]@{
      url  = $Url
      hash = $Sha256
    }
  }
  # Matches what cargo actually builds. See the note above.
  bin          = 'hyperdu.exe'
  checkver     = [ordered]@{
    github = $repo
  }
  autoupdate   = [ordered]@{
    architecture = [ordered]@{
      '64bit' = [ordered]@{
        url = "$repo/releases/download/v`$version/hyperdu-windows-x86_64-generic.zip"
      }
    }
  }
}

$json = $manifest | ConvertTo-Json -Depth 6
$path = Join-Path $OutDir 'hyperdu.json'
# Scoop reads these as UTF-8; a BOM trips up some tooling that consumes buckets.
[System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Wrote scoop manifest: $path"
