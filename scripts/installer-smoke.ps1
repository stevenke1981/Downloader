# Silent per-user install / verify / uninstall of the Inno Setup installer.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$setup = Join-Path $root 'dist\downloader-setup-windows-x64.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Downloader'
function Check($cond, $msg) { if (-not $cond) { throw "FAILED: $msg" } ; Write-Host "ok: $msg" }

$p = Start-Process $setup -Wait -PassThru -ArgumentList @(
    '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER',
    '/TASKS=desktopicon,addtopath,browser', '/LOG=install.log')
Check ($p.ExitCode -eq 0) "installer exit code 0 (got $($p.ExitCode))"
foreach ($f in 'downloader.exe', 'dlx.exe', 'downloader-host.exe', 'extension\manifest.json', 'README.md', 'LICENSE.txt', 'unins000.exe') {
    Check (Test-Path (Join-Path $dir $f)) "installed $f"
}
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
Check ($userPath -like "*$dir*") "install dir on user PATH"
Check (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Downloader.lnk')) "Start menu shortcut"

$hk = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\io.github.stevenke1981.downloader'
Check (Test-Path $hk) "Chrome native host registered"
Check (Test-Path 'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\io.github.stevenke1981.downloader') "Edge native host registered"
$mf = (Get-ItemProperty $hk).'(default)'
$m = Get-Content $mf -Raw | ConvertFrom-Json
Check (Test-Path $m.path) "host manifest points to $($m.path)"
$ver = & (Join-Path $dir 'dlx.exe') --version
Check ($LASTEXITCODE -eq 0) "dlx --version runs: $ver"
$tmp = Join-Path $env:RUNNER_TEMP 'smoke'
New-Item -ItemType Directory -Force $tmp | Out-Null
& (Join-Path $dir 'dlx.exe') -q -c 4 -o $tmp 'https://raw.githubusercontent.com/stevenke1981/Downloader/main/LICENSE'
Check ($LASTEXITCODE -eq 0) "installed dlx downloads a file"
Check ((Get-Content (Join-Path $tmp 'LICENSE') -Raw) -like '*MIT License*') "downloaded content ok"

$p = Start-Process (Join-Path $dir 'unins000.exe') -Wait -PassThru -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
Check ($p.ExitCode -eq 0) "uninstaller exit code 0"
Start-Sleep -Seconds 3
Check (-not (Test-Path (Join-Path $dir 'downloader.exe'))) "downloader.exe removed"
Check (-not (Test-Path $hk)) "native host unregistered"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
Check (-not ($userPath -like "*$dir*")) "removed from user PATH"
Write-Host "Installer smoke test passed"
