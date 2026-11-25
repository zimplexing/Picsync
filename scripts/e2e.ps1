param([string]$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PicsyncUi {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, IntPtr e);
  public struct RECT { public int Left, Top, Right, Bottom; }
  public struct POINT { public int X, Y; }
}
'@
$script:clickedScan = $false
$script:clickedCopy = $false
function ClickClient([IntPtr]$h, [int]$cx, [int]$cy, [string]$button) {
    $p = New-Object PicsyncUi+POINT; $p.X = $cx; $p.Y = $cy
    [void][PicsyncUi]::ClientToScreen($h, [ref]$p)
    [void][PicsyncUi]::SetCursorPos($p.X, $p.Y); Start-Sleep -Milliseconds 350
    [PicsyncUi]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 140
    [PicsyncUi]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 250
    if ($button -eq 'scan') { $script:clickedScan = $true } else { $script:clickedCopy = $true }
    Write-Output "click $button at client $cx,$cy -> screen $($p.X),$($p.Y)"
}
function SaveWindow([IntPtr]$h, [string]$path) {
    $r = New-Object PicsyncUi+RECT; [void][PicsyncUi]::GetClientRect($h, [ref]$r)
    $origin = New-Object PicsyncUi+POINT
    [void][PicsyncUi]::ClientToScreen($h, [ref]$origin)
    $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($origin.X, $origin.Y, 0, 0, $bmp.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}
function Wait-Window([System.Diagnostics.Process]$process) {
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 300
        $process.Refresh()
        $handle = $process.MainWindowHandle
        if ($handle -ne [IntPtr]::Zero) {
            $client = New-Object PicsyncUi+RECT
            if ([PicsyncUi]::GetClientRect($handle, [ref]$client) -and $client.Right -gt 100) { return $handle }
        }
    }
    throw 'main window missing'
}
function Invoke-Run([string]$Tag) {
    $process = Start-Process -FilePath (Join-Path $Root 'target\release\picsync.exe') -PassThru
    $handle = Wait-Window $process
    [void][PicsyncUi]::SetForegroundWindow($handle); Start-Sleep -Milliseconds 800
    $client = New-Object PicsyncUi+RECT; [void][PicsyncUi]::GetClientRect($handle, [ref]$client)
    Write-Output "client=$($client.Right)x$($client.Bottom)"
    $buttonY = $client.Bottom - 33
    ClickClient $handle 74 $buttonY 'scan'
    Start-Sleep -Seconds 3
    SaveWindow $handle (Join-Path $Root "ui-$Tag-preview.png")
    ClickClient $handle 204 $buttonY 'copy'
    Start-Sleep -Seconds 4
    SaveWindow $handle (Join-Path $Root "ui-$Tag-copied.png")
    $process.CloseMainWindow() | Out-Null
    Start-Sleep -Seconds 2
    if (-not $process.HasExited) { $process.Kill() }
}

$today = (Get-Date).Date
$todayText = $today.ToString('yyyy-MM-dd')
$olderDay = $today.AddDays(-2)
$olderText = $olderDay.ToString('yyyy-MM-dd')

$e2e = Join-Path $Root '.e2e'
Remove-Item $e2e -Recurse -Force -ErrorAction SilentlyContinue
$source = Join-Path $e2e 'source'; $destination = Join-Path $e2e 'dest'
$configDir = Join-Path $e2e 'config'
New-Item -ItemType Directory -Force (Join-Path $source 'sub') | Out-Null
New-Item -ItemType Directory -Force $configDir | Out-Null
foreach ($spec in @(@('a.jpg', 'sub'), @('b.PNG', ''), @('old.jpg', ''), @('notes.txt', ''))) {
    $file = Join-Path (Join-Path $source $spec[1]) $spec[0]
    [System.IO.File]::WriteAllBytes($file, [System.Text.Encoding]::UTF8.GetBytes("content-$($spec[0])"))
    (Get-Item $file).LastWriteTime = if ($spec[0] -eq 'old.jpg') { $olderDay.AddHours(9) } else { $today.AddHours(10) }
}
[ordered]@{ version = 1; source = $source; destination = $destination; recursive = $true; basis = 'Modified' } |
    ConvertTo-Json | Set-Content (Join-Path $configDir 'settings.json') -Encoding utf8
$env:PICSYNC_DATA_DIR = $configDir

Invoke-Run 'first'
$day = Join-Path $destination $todayText
$first = @(Get-ChildItem $day -File | Sort-Object Name)
Write-Output "first_run_files=$($first.Name -join ',')"
Write-Output "first_hash_a=$((Get-FileHash (Join-Path $day 'a.jpg')).Hash)"
Write-Output "source_unmodified=$((Get-FileHash (Join-Path $source 'sub\a.jpg')).Hash -eq (Get-FileHash (Join-Path $day 'a.jpg')).Hash)"
Write-Output "old_excluded=$(-not (Test-Path (Join-Path $destination $olderText)))"
Write-Output "no_subfolder=$(-not (Test-Path (Join-Path $day 'sub')))"

Invoke-Run 'second'
$second = @(Get-ChildItem $day -File | Sort-Object Name)
Write-Output "second_run_files=$($second.Name -join ',')"
Write-Output "idempotent=$($second.Count -eq 2)"
Write-Output "settings_kept=$((Get-Content (Join-Path $configDir 'settings.json') -Raw | ConvertFrom-Json).source -eq $source)"
