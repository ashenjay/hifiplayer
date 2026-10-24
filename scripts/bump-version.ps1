# Sets the app version everywhere it lives, so releases stay consistent.
#   powershell -File scripts\bump-version.ps1 1.0.1
# Then add the release notes to CHANGELOG.md, commit, and tag:  git tag v1.0.1
# Version numbers follow https://semver.org: MAJOR.MINOR.PATCH
#   PATCH = bug fixes, MINOR = new features, MAJOR = breaking changes.

param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { throw "Not a valid version: $Version (expected e.g. 1.2.3)" }
$root = Split-Path -Parent $PSScriptRoot

function Set-InFile($path, $pattern, $replacement) {
    $full = Join-Path $root $path
    $text = [IO.File]::ReadAllText($full)
    if (-not [regex]::IsMatch($text, $pattern, 'Multiline')) { throw "Version not found in $path" }
    $new = [regex]::Replace($text, $pattern, $replacement, 'Multiline')
    [IO.File]::WriteAllText($full, $new)
    Write-Host "  $path"
}

Write-Host "Setting version $Version in:"
# The first `version = ` line in each Cargo.toml is the package version.
Set-InFile 'Cargo.toml' '(?<=^\[package\][\s\S]*?^version = ")[^"]+' $Version
Set-InFile 'app\src-tauri\Cargo.toml' '(?<=^\[package\][\s\S]*?^version = ")[^"]+' $Version
Set-InFile 'app\package.json' '(?<=^  "version": ")[^"]+' $Version
Set-InFile 'app\src-tauri\tauri.conf.json' '(?<=^  "version": ")[^"]+' $Version

Push-Location $root
cmd /c "cargo update --workspace --offline >nul 2>&1"
Pop-Location
Write-Host "Done. Remember to update CHANGELOG.md."
