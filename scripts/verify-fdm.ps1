# Native Rust protocol acceptance. No external download/media executable is required.
param([string]$EvidenceDir)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (!$EvidenceDir) {
    $EvidenceDir = Join-Path $projectRoot ('target\native-evidence\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null
Push-Location $projectRoot
Start-Transcript -Path (Join-Path $EvidenceDir 'verification.log') -Force | Out-Null
try {
    & (Join-Path $PSScriptRoot 'verify-local.ps1')
    & 'target\release\dlx.exe' --tools | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Native capabilities failed' }
    & 'target\release\dlxctl.exe' --help | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Remote CLI help failed' }
    Get-FileHash -Algorithm SHA256 -LiteralPath 'target\release\downloader.exe','target\release\dlx.exe','target\release\dlxctl.exe','target\release\downloader-host.exe' | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $EvidenceDir 'artifacts.json')
    Write-Host 'PASS: native release and loopback protocol acceptance. GUI interaction, public services, Internet tunnel, installer, other OS and hosted CI are separate gates.'
} finally {
    Stop-Transcript | Out-Null
    Pop-Location
}
