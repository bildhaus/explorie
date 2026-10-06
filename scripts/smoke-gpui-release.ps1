[CmdletBinding()]
param(
    [string]$Executable,
    [string]$BrowsePath,
    [string]$ProfilePath,
    [int]$TimeoutSeconds = 25,
    [switch]$KeepProfile
)

$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ExplorieSmokeWindow {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [StructLayout(LayoutKind.Sequential)]
  public struct POINT { public int X; public int Y; }
  [DllImport("user32.dll")]
  public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")]
  public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
  [DllImport("user32.dll")]
  public static extern uint GetDpiForWindow(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern bool IsZoomed(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int width, int height, uint flags);
  [DllImport("user32.dll")]
  public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
}
'@

function Invoke-TitleBarControl([IntPtr]$Handle, [double]$LogicalOffsetFromRight) {
    $rect = New-Object ExplorieSmokeWindow+RECT
    if (-not [ExplorieSmokeWindow]::GetClientRect($Handle, [ref]$rect)) {
        throw 'Unable to read the GPUI client bounds for title-bar smoke.'
    }
    $origin = New-Object ExplorieSmokeWindow+POINT
    if (-not [ExplorieSmokeWindow]::ClientToScreen($Handle, [ref]$origin)) {
        throw 'Unable to resolve the GPUI client origin for title-bar smoke.'
    }
    $dpi = [ExplorieSmokeWindow]::GetDpiForWindow($Handle)
    $scale = if ($dpi -gt 0) { $dpi / 96.0 } else { 1.0 }
    $x = $origin.X + $rect.Right - [Math]::Round($LogicalOffsetFromRight * $scale)
    $y = $origin.Y + [Math]::Round(19.0 * $scale)
    [ExplorieSmokeWindow]::SetForegroundWindow($Handle) | Out-Null
    [ExplorieSmokeWindow]::SetCursorPos($x, $y) | Out-Null
    [ExplorieSmokeWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    [ExplorieSmokeWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
}

function Wait-ForZoomState([IntPtr]$Handle, [bool]$Expected, [string]$Action) {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        Start-Sleep -Milliseconds 100
        if ([ExplorieSmokeWindow]::IsZoomed($Handle) -eq $Expected) { return }
    }
    throw "GPUI custom title-bar $Action control did not reach the expected window state."
}

$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($Executable)) {
    $Executable = Join-Path $workspace 'target\release\explorie-gpui.exe'
}
$Executable = [IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "GPUI release executable is missing: $Executable"
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$smokeRoot = [IO.Path]::GetFullPath(
    (Join-Path $tempRoot ("explorie-gpui-release-smoke-{0}" -f [guid]::NewGuid()))
)
$tempPrefix = $tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $smokeRoot.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Smoke profile escaped the temporary directory: $smokeRoot"
}

$profile = if ([string]::IsNullOrWhiteSpace($ProfilePath)) {
    Join-Path $smokeRoot 'profile'
} else {
    [IO.Path]::GetFullPath($ProfilePath)
}
if (-not $profile.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Smoke profile escaped the temporary directory: $profile"
}
if ([string]::IsNullOrWhiteSpace($BrowsePath)) {
    $BrowsePath = Join-Path $smokeRoot 'browse'
    New-Item -ItemType Directory -Path $BrowsePath -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $BrowsePath 'release-smoke.txt') -Value 'GPUI release smoke'
} else {
    $BrowsePath = [IO.Path]::GetFullPath($BrowsePath)
    if (-not (Test-Path -LiteralPath $BrowsePath -PathType Container)) {
        throw "Smoke browse path is not a directory: $BrowsePath"
    }
}
New-Item -ItemType Directory -Path $profile -Force | Out-Null

$process = $null
try {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.WorkingDirectory = Split-Path $Executable -Parent
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    if ($null -ne $startInfo.ArgumentList) {
        $startInfo.ArgumentList.Add($BrowsePath)
    } else {
        $startInfo.Arguments = '"' + $BrowsePath.Replace('"', '\"') + '"'
    }
    $startInfo.EnvironmentVariables['EXPLORIE_TEST_CONFIG_DIR'] = $profile

    $process = [Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw 'Unable to start the GPUI release executable.'
    }

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
    } while (-not $process.HasExited -and $process.MainWindowHandle -eq 0 -and [DateTime]::UtcNow -lt $deadline)

    if ($process.HasExited) {
        throw "GPUI release process exited before smoke proof (code $($process.ExitCode))."
    }
    if ($process.MainWindowHandle -eq 0) {
        throw "GPUI release process did not create a native window within $TimeoutSeconds seconds."
    }

    $proof = [ordered]@{
        executable = $Executable
        bytes = (Get-Item -LiteralPath $Executable).Length
        processId = $process.Id
        responding = $process.Responding
        mainWindowHandle = $process.MainWindowHandle.ToInt64()
        mainWindowTitle = $process.MainWindowTitle
        isolatedProfile = $profile
        customTitleBarMaximize = $false
        customTitleBarRestore = $false
        customTitleBarClose = $false
    }

    if (-not [ExplorieSmokeWindow]::SetWindowPos(
        $process.MainWindowHandle,
        [IntPtr](-1),
        0,
        0,
        0,
        0,
        0x0043
    )) {
        throw 'Unable to bring the GPUI window to the front for title-bar smoke.'
    }
    Start-Sleep -Milliseconds 500
    Invoke-TitleBarControl $process.MainWindowHandle 69
    Wait-ForZoomState $process.MainWindowHandle $true 'maximize'
    $proof['customTitleBarMaximize'] = $true
    Invoke-TitleBarControl $process.MainWindowHandle 69
    Wait-ForZoomState $process.MainWindowHandle $false 'restore'
    $proof['customTitleBarRestore'] = $true
    Invoke-TitleBarControl $process.MainWindowHandle 23
    if (-not $process.WaitForExit(15000)) {
        throw 'GPUI custom title-bar close control did not close cleanly within 15 seconds.'
    }
    $proof['customTitleBarClose'] = $true

    $proof['exitCode'] = $process.ExitCode
    $proof['recoveryMarkerPresent'] = Test-Path -LiteralPath (Join-Path $profile 'runtime-dirty-v1.json')
    if ($process.ExitCode -ne 0) {
        throw "GPUI release process returned exit code $($process.ExitCode)."
    }
    if ($proof['recoveryMarkerPresent']) {
        throw 'GPUI release process left the dirty-session marker after a normal close.'
    }

    $proof | ConvertTo-Json
} finally {
    if ($null -ne $process -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    if (-not $KeepProfile -and (Test-Path -LiteralPath $smokeRoot)) {
        $resolvedSmokeRoot = [IO.Path]::GetFullPath($smokeRoot)
        if (
            $resolvedSmokeRoot.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedSmokeRoot).StartsWith(
                'explorie-gpui-release-smoke-',
                [StringComparison]::Ordinal
            )
        ) {
            Remove-Item -LiteralPath $resolvedSmokeRoot -Recurse -Force
        }
    }
}
