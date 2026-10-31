# Builds the Windows release artifacts locally, mirroring .github/workflows/release.yml:
#   dist\downloader-windows-x64.zip          portable build
#   dist\downloader-browser-extension.zip    unpacked browser extension
#   dist\downloader-setup-windows-x64.exe    Inno Setup installer (unless -NoInstaller)
# Usage: ./scripts/package-windows.ps1 [-SkipBuild] [-NoInstaller]
param(
    [switch]$SkipBuild,
    [switch]$NoInstaller
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $SkipBuild) {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}
$exes = 'downloader.exe', 'dlx.exe', 'dlxctl.exe', 'downloader-host.exe'
foreach ($exe in $exes) {
    if (-not (Test-Path -LiteralPath "target\release\$exe")) { throw "Missing target\release\$exe" }
}

$name = 'downloader-windows-x64'
$stage = Join-Path 'dist' $name
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'docs') | Out-Null

Copy-Item README.md, LICENSE, THIRD_PARTY.md $stage
Copy-Item licenses (Join-Path $stage 'licenses') -Recurse
Copy-Item docs\fdm-*.md, docs\native-rust-plan.md (Join-Path $stage 'docs')
Copy-Item docs\desktop-controls.zh-TW.md, docs\browser-setup.zh-TW.md, docs\usability.zh-TW.md $stage
Copy-Item docs\fdm-tools.zh-TW.md, docs\fdm-feature-matrix.zh-TW.md $stage
Copy-Item docs\ui-0.5.2-settings-*.png, docs\dispatch-plan.md $stage
Copy-Item docs\validation-0.6.0.md, docs\small-window-validation.zh-TW.md $stage
Copy-Item extension (Join-Path $stage 'extension') -Recurse
foreach ($exe in $exes) { Copy-Item "target\release\$exe" $stage }

$zip = Join-Path 'dist' "$name.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path $stage -DestinationPath $zip

$extZip = 'dist\downloader-browser-extension.zip'
if (Test-Path -LiteralPath $extZip) { Remove-Item -LiteralPath $extZip -Force }
$extFiles = Get-ChildItem extension -Force | Where-Object { $_.Name -ne 'EXTENSION_ID' }
Compress-Archive -Path $extFiles.FullName -DestinationPath $extZip

if (-not $NoInstaller) {
    & (Join-Path $PSScriptRoot 'build-installer.ps1')
}

Get-ChildItem dist -File | ForEach-Object {
    [pscustomobject]@{
        Name   = $_.Name
        Bytes  = $_.Length
        SHA256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    }
} | Format-Table -AutoSize
