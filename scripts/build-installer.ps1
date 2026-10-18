# Builds dist\downloader-setup-windows-x64.exe with Inno Setup.
# Expects all three target\release Downloader executables to exist already.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$candidates = @(
    (Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe')
)
$iscc = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $iscc -and $env:GITHUB_ACTIONS -eq 'true') {
    choco install innosetup -y --no-progress
    if ($LASTEXITCODE -ne 0) { throw "choco install innosetup failed" }
    $iscc = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $iscc -or -not (Test-Path -LiteralPath $iscc)) { throw 'Inno Setup 6 or 7 is required.' }

$isl = 'installer\ChineseTraditional.isl'
if (-not (Test-Path $isl)) {
    # Newer Inno Setup ships ChineseTraditional.isl officially; older ones need the
    # unofficial translation from the issrc repo (pinned to a 6.x tag).
    $bundled = Join-Path (Split-Path $iscc) 'Languages\ChineseTraditional.isl'
    if (Test-Path $bundled) {
        Copy-Item $bundled $isl
    } else {
        Invoke-WebRequest -UseBasicParsing -OutFile $isl `
            'https://raw.githubusercontent.com/jrsoftware/issrc/is-6_4_3/Files/Languages/Unofficial/ChineseTraditional.isl'
    }
}

$m = Select-String -Path 'gui\Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$version = $m.Matches[0].Groups[1].Value
Write-Host "Building installer for Downloader $version"

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc /Qp "/DAppVersion=$version" "/DSrcDir=$root\target\release" 'installer\downloader.iss'
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }
Get-Item 'dist\downloader-setup-windows-x64.exe' | Format-List Name, Length
