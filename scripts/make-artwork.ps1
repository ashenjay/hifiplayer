# Draws the app icon source (1024 px PNG) and the NSIS installer artwork (BMP).
# Then run: cd app; npx tauri icon ..\assets\icon-source.png

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root 'assets'
New-Item -ItemType Directory -Force $assets | Out-Null

$purple = [System.Drawing.Color]::FromArgb(255, 139, 92, 246)
$pink = [System.Drawing.Color]::FromArgb(255, 236, 72, 153)
$bg = [System.Drawing.Color]::FromArgb(255, 18, 18, 20)

function New-Canvas($w, $h) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    return $bmp, $g
}

# Logo: gradient disc with a white waveform, drawn into a square of `size` at (x, y).
function Draw-Logo($g, $x, $y, $size) {
    $rect = New-Object System.Drawing.RectangleF $x, $y, $size, $size
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $purple, $pink, 45
    $g.FillEllipse($brush, $rect)
    $bars = @(0.30, 0.55, 0.85, 0.60, 0.95, 0.50, 0.30)
    $barW = $size * 0.075
    $gap = $size * 0.035
    $total = $bars.Count * $barW + ($bars.Count - 1) * $gap
    $left = $x + ($size - $total) / 2
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    for ($i = 0; $i -lt $bars.Count; $i++) {
        $h = $size * 0.52 * $bars[$i]
        $bx = $left + $i * ($barW + $gap)
        $by = $y + ($size - $h) / 2
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $r = $barW
        $path.AddArc($bx, $by, $r, $r, 180, 180)
        $path.AddArc($bx, $by + $h - $r, $r, $r, 0, 180)
        $path.CloseFigure()
        $g.FillPath($white, $path)
    }
}

# 1. Icon source: logo on a dark rounded square.
$bmp, $g = New-Canvas 1024 1024
$g.Clear([System.Drawing.Color]::Transparent)
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 220
$path.AddArc(0, 0, $r, $r, 180, 90); $path.AddArc(1024 - $r, 0, $r, $r, 270, 90)
$path.AddArc(1024 - $r, 1024 - $r, $r, $r, 0, 90); $path.AddArc(0, 1024 - $r, $r, $r, 90, 90)
$path.CloseFigure()
$g.FillPath((New-Object System.Drawing.SolidBrush $bg), $path)
Draw-Logo $g 132 132 760
$bmp.Save((Join-Path $assets 'icon-source.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()

# 2. NSIS header (150x57) and welcome/finish sidebar (164x314), 24-bit BMP.
$bmp, $g = New-Canvas 150 57
$g.Clear([System.Drawing.Color]::White)
Draw-Logo $g 95 6 45
$bmp.Save((Join-Path $assets 'installer-header.bmp'), [System.Drawing.Imaging.ImageFormat]::Bmp)
$g.Dispose(); $bmp.Dispose()

$bmp, $g = New-Canvas 164 314
$rect = New-Object System.Drawing.Rectangle 0, 0, 164, 314
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $bg, ([System.Drawing.Color]::FromArgb(255, 45, 22, 80)), 90
$g.FillRectangle($grad, $rect)
Draw-Logo $g 32 52 100
$font = New-Object System.Drawing.Font 'Segoe UI Semibold', 15
$small = New-Object System.Drawing.Font 'Segoe UI', 8.5
$white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
$grey = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 170, 170, 185))
$fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = 'Center'
$g.DrawString('HiFi Player', $font, $white, (New-Object System.Drawing.RectangleF 0, 172, 164, 30), $fmt)
$g.DrawString("Bit-perfect playback`nASIO · WASAPI · DSD", $small, $grey, (New-Object System.Drawing.RectangleF 0, 205, 164, 40), $fmt)
$bmp.Save((Join-Path $assets 'installer-sidebar.bmp'), [System.Drawing.Imaging.ImageFormat]::Bmp)
$g.Dispose(); $bmp.Dispose()

Write-Host "Artwork written to $assets"
