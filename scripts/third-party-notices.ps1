# Regenerates THIRD_PARTY_NOTICES.md from the Rust and npm dependency metadata.
# Run from anywhere: powershell -File scripts\third-party-notices.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$meta = cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json
$ours = @('hifiplayer', 'hifiplayer-app')
$rust = $meta.packages |
    Where-Object { $ours -notcontains $_.name } |
    Sort-Object name, version |
    ForEach-Object {
        $lic = if ($_.license) { $_.license } elseif ($_.license_file) { "see $($_.license_file)" } else { 'unknown' }
        $repo = if ($_.repository) { $_.repository } else { '' }
        "| $($_.name) | $($_.version) | $lic | $repo |"
    }

$pkg = Get-Content app\package.json | ConvertFrom-Json
$npm = $pkg.dependencies.PSObject.Properties | Sort-Object Name | ForEach-Object {
    $p = Get-Content "app\node_modules\$($_.Name)\package.json" | ConvertFrom-Json
    "| $($p.name) | $($p.version) | $($p.license) |"
}

$out = @"
# Third-party notices

HiFi Player is licensed under the GNU General Public License v3.0 (see LICENSE).
It includes the open-source components listed below, each under its own licence.
Their licence texts are available from the linked repositories.

Trademarks: ASIO is a trademark of Steinberg Media Technologies GmbH. Spotify, Qobuz and
TIDAL are trademarks of their respective owners. HiFi Player is not affiliated with or
endorsed by any of them. Spotify playback uses the unofficial librespot library.

## Rust crates ($($rust.Count))

| Crate | Version | Licence | Repository |
|---|---|---|---|
$($rust -join "`n")

## JavaScript packages (bundled in the app)

| Package | Version | Licence |
|---|---|---|
$($npm -join "`n")
"@

Set-Content -Path THIRD_PARTY_NOTICES.md -Value $out -Encoding utf8
Write-Host "THIRD_PARTY_NOTICES.md: $($rust.Count) crates, $($npm.Count) npm packages"
