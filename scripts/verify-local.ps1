# Local equivalent of the Windows code checks; never dispatches GitHub Actions.
param([switch]$BuildInstaller)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot

function Invoke-Check([string]$Program, [string[]]$CheckArgs) {
    Write-Host "Checking: $Program $($CheckArgs -join ' ')"
    Get-Command $Program -ErrorAction Stop | Out-Null
    # Route native stdout/stderr through the host so Start-Transcript records
    # diagnostics as well as check names. Native exit status decides failure.
    $ErrorActionPreference = 'Continue'
    & $Program @CheckArgs 2>&1 | ForEach-Object { Write-Host $_.ToString() }
    if ($LASTEXITCODE -ne 0) {
        throw "$Program failed with exit code $LASTEXITCODE"
    }
}

Push-Location $projectRoot
try {
    Get-Command cargo, node -ErrorAction Stop | Out-Null
    Invoke-Check cargo @('fmt', '-p', 'downloader', '--check')
    Invoke-Check cargo @('clippy', '--release', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-Check cargo @('build', '--release', '--locked')
    Invoke-Check cargo @('test', '--release', '--workspace', '--locked')
    Invoke-Check node @('--test', 'scripts/tests/extension-background.test.cjs')
    Invoke-Check node @('--check', 'extension/options.js')
    Invoke-Check node @('--check', 'extension/alt.js')
    Invoke-Check 'target\release\dlx.exe' @('--version')
    if ($BuildInstaller) {
        & (Join-Path $PSScriptRoot 'build-installer.ps1')
    }
    Write-Host 'PASS: local Windows checks completed. CI and other OS checks were not run.'
} finally {
    Pop-Location
}
