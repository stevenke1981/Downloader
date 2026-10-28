# Full local acceptance plus real loopback optional-engine fixtures.
param(
    [string]$PythonExe = 'python',
    [string]$Aria2Path,
    [string]$YtDlpPath,
    [string]$PythonLib,
    [string]$EvidenceDir
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (!$EvidenceDir) {
    $EvidenceDir = Join-Path $projectRoot ('target\fdm-evidence\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
if (!$Aria2Path -or !$YtDlpPath) {
    throw 'Supply official aria2c and yt-dlp executable paths for protocol acceptance.'
}
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

function Invoke-FdmCheck([string]$Program, [string[]]$CheckArgs) {
    Write-Host "Checking: $Program $($CheckArgs -join ' ')"
    & $Program @CheckArgs
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

Push-Location $projectRoot
Start-Transcript -Path (Join-Path $EvidenceDir 'verification.log') -Force | Out-Null
try {
    & (Join-Path $PSScriptRoot 'verify-local.ps1')
    Invoke-FdmCheck cargo @('test', '--release', '--locked', '-p', 'downloader-core', '--test', 'external', 'real_ffmpeg', '--', '--ignored', '--test-threads=1')
    Invoke-FdmCheck (Join-Path $projectRoot 'target\release\dlxctl.exe') @('--help')
    $runtimeArgs = @((Join-Path $PSScriptRoot 'verify-fdm-runtime.py'), '--dlx', (Join-Path $projectRoot 'target\release\dlx.exe'), '--aria2', $Aria2Path, '--ytdlp', $YtDlpPath, '--evidence', (Join-Path $EvidenceDir 'runtime'))
    if ($PythonLib) { $runtimeArgs += @('--python-lib', $PythonLib) }
    Invoke-FdmCheck $PythonExe $runtimeArgs
    Get-FileHash -Algorithm SHA256 -LiteralPath 'target\release\downloader.exe','target\release\dlx.exe','target\release\dlxctl.exe' | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $EvidenceDir 'artifacts.json')
    Write-Host 'PASS: local release, loopback protocol, and management acceptance. GUI interaction, Internet tunnel, live MMS, other OS and CI remain separate evidence.'
} finally {
    Stop-Transcript | Out-Null
    Pop-Location
}
