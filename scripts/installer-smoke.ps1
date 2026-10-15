# Silent per-user install / verify / uninstall of the Inno Setup installer.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') {
    throw 'This installer test changes per-user integration and is intended only for a disposable CI runner.'
}
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
$fixtureDir = Join-Path $env:RUNNER_TEMP ('dl-fixture-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureDir | Out-Null
$expected = 'Downloader installer fixture: self-created test content.'
[IO.File]::WriteAllText((Join-Path $fixtureDir 'fixture.txt'), $expected)
$ready = Join-Path $fixtureDir 'port.txt'
$fixtureScript = Join-Path $PSScriptRoot 'installer-fixture.py'
$server = Start-Process python -WindowStyle Hidden -PassThru -ArgumentList @(
    ('"' + $fixtureScript + '"'), '--directory', ('"' + $fixtureDir + '"'), '--ready-file', ('"' + $ready + '"'))
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $ready)) {
        if ($server.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'Fixture server did not start' }
        Start-Sleep -Milliseconds 100
    }
    $port = [int](Get-Content -LiteralPath $ready -Raw)
    & (Join-Path $dir 'dlx.exe') -q -c 4 -o $tmp "http://127.0.0.1:$port/fixture.txt"
    Check ($LASTEXITCODE -eq 0) 'installed dlx downloads a local fixture'
    Check (([IO.File]::ReadAllText((Join-Path $tmp 'fixture.txt'))) -eq $expected) 'downloaded content matches fixture'
} finally {
    if (-not $server.HasExited) { Stop-Process -Id $server.Id }
}

$p = Start-Process (Join-Path $dir 'unins000.exe') -Wait -PassThru -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
Check ($p.ExitCode -eq 0) "uninstaller exit code 0"
Start-Sleep -Seconds 3
Check (-not (Test-Path (Join-Path $dir 'downloader.exe'))) "downloader.exe removed"
Check (-not (Test-Path $hk)) "native host unregistered"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
Check (-not ($userPath -like "*$dir*")) "removed from user PATH"
Write-Host "Installer smoke test passed"
