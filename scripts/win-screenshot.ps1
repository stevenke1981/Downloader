# Launch the GUI and capture the screen so rendering can be checked from CI artifacts.
param([string]$Exe = 'target\release\downloader.exe', [string]$Out = 'win-screenshot.png', [string[]]$Arguments = @())
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or !$env:RUNNER_TEMP) {
    throw 'This screenshot fixture runs only on an isolated GitHub Actions runner.'
}
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class WindowGeometry {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
}
'@
$env:DOWNLOADER_STATE_DIR = Join-Path $env:RUNNER_TEMP ('screenshot-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force $env:DOWNLOADER_STATE_DIR | Out-Null
Set-Content (Join-Path $env:DOWNLOADER_STATE_DIR 'settings.json') '{"language":"zh_TW","watch_clipboard":false}' -Encoding utf8
$p = $null
$bmp = $g = $null
$previousDpi = [WindowGeometry]::SetThreadDpiAwarenessContext([IntPtr](-4))
try {
    # Do not reposition, maximize, resize, or change display resolution here:
    # the screenshot must demonstrate the application's actual startup placement.
    $startOptions = @{ FilePath=$Exe; PassThru=$true }
    if ($Arguments.Count -gt 0) { $startOptions.ArgumentList = $Arguments }
    $p = Start-Process @startOptions
    Start-Sleep -Seconds 10
    $p.Refresh()
    if ($p.HasExited) { throw "GUI exited early with code $($p.ExitCode)" }
    $hwnd = $p.MainWindowHandle
    if ($hwnd -eq [IntPtr]::Zero) { throw 'No main window handle' }
    $screen = [System.Windows.Forms.Screen]::FromHandle($hwnd)
    $b = $screen.Bounds
    $work = $screen.WorkingArea
    $frame = New-Object WindowGeometry+Rect
    $client = New-Object WindowGeometry+Rect
    $clientOrigin = New-Object WindowGeometry+Point
    if (![WindowGeometry]::GetWindowRect($hwnd, [ref]$frame) -or
        ![WindowGeometry]::GetClientRect($hwnd, [ref]$client) -or
        ![WindowGeometry]::ClientToScreen($hwnd, [ref]$clientOrigin)) { throw 'Cannot read window geometry' }
    $contained = $frame.Left -ge $work.Left -and $frame.Top -ge $work.Top -and
        $frame.Right -le $work.Right -and $frame.Bottom -le $work.Bottom
    [ordered]@{
        screen = @{ x=$b.X; y=$b.Y; width=$b.Width; height=$b.Height }
        work = @{ left=$work.Left; top=$work.Top; right=$work.Right; bottom=$work.Bottom }
        frame = $frame
        client = @{ x=$clientOrigin.X; y=$clientOrigin.Y; width=$client.Right; height=$client.Bottom }
        dpi = [WindowGeometry]::GetDpiForWindow($hwnd)
        frame_inside_work_area = $contained
        source_sha = $env:GITHUB_SHA
        fixture = $(if ($env:DOWNLOADER_UI_FIXTURE) { $env:DOWNLOADER_UI_FIXTURE } else { 'empty-main' })
        native_mouse_interaction = 'not performed'
    } | ConvertTo-Json -Depth 4 | Set-Content ([IO.Path]::ChangeExtension($Out, '.json')) -Encoding utf8
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Host "saved $Out ($($b.Width)x$($b.Height)); client $($client.Right)x$($client.Bottom); DPI $([WindowGeometry]::GetDpiForWindow($hwnd))"
    if ($b.Width -ne 1024 -or $b.Height -ne 768) { throw 'Expected a real 1024x768 desktop; do not substitute a larger screenshot.' }
    if (!$contained) { throw 'Application frame extends outside the monitor work area.' }
} finally {
    if ($g) { $g.Dispose() }
    if ($bmp) { $bmp.Dispose() }
    if ($p -and !$p.HasExited) { Stop-Process -Id $p.Id -Force }
    if ($previousDpi -ne [IntPtr]::Zero) { [void][WindowGeometry]::SetThreadDpiAwarenessContext($previousDpi) }
}
