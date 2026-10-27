param(
    [string]$Executable = "target/release/explorie-gpui.exe",
    [string]$OutputDirectory = "target"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ExplorieVisualWindow {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [DllImport("user32.dll")]
  public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")]
  public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int width, int height, uint flags);
  [DllImport("user32.dll")]
  public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
  [DllImport("user32.dll")]
  public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extraInfo);
}
'@

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$resolvedOutput = (Resolve-Path -LiteralPath $OutputDirectory).Path
$proofRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("explorie-gpui-visual-" + [guid]::NewGuid())
$fixture = Join-Path $proofRoot "fixture"
$historyRoot = Join-Path $proofRoot "history"
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $fixture "Native Services") -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $historyRoot "History One"),(Join-Path $historyRoot "History Two"),(Join-Path $historyRoot "History Three") -Force | Out-Null
Set-Content -LiteralPath (Join-Path $fixture "migration-notes.md") -Value "# GPUI cutover"
Set-Content -LiteralPath (Join-Path $fixture "palette.json") -Value '{"theme":"native"}'
$conflictSource = Join-Path $fixture "Conflict source"
$conflictDestination = Join-Path $fixture "Conflict destination"
New-Item -ItemType Directory -Path $conflictSource,$conflictDestination -Force | Out-Null
Set-Content -LiteralPath (Join-Path $conflictSource "duplicate.txt") -Value "incoming"
Set-Content -LiteralPath (Join-Path $conflictDestination "duplicate.txt") -Value "existing"

function Start-Explorie([string]$profile, [AllowNull()][string]$browsePath = $fixture) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $resolvedExecutable
    if (-not [string]::IsNullOrWhiteSpace($browsePath)) {
        if ($null -ne $startInfo.ArgumentList) {
            $startInfo.ArgumentList.Add($browsePath)
        } else {
            $startInfo.Arguments = '"' + $browsePath.Replace('"', '\"') + '"'
        }
    }
    $startInfo.UseShellExecute = $false
    $startInfo.EnvironmentVariables["EXPLORIE_TEST_CONFIG_DIR"] = $profile
    return [System.Diagnostics.Process]::Start($startInfo)
}

function Send-KeyChord([byte]$key, [byte]$modifier = 0, [byte]$modifier2 = 0) {
    if ($modifier -ne 0) {
        [ExplorieVisualWindow]::keybd_event($modifier, 0, 0, [UIntPtr]::Zero)
    }
    if ($modifier2 -ne 0) {
        [ExplorieVisualWindow]::keybd_event($modifier2, 0, 0, [UIntPtr]::Zero)
    }
    [ExplorieVisualWindow]::keybd_event($key, 0, 0, [UIntPtr]::Zero)
    [ExplorieVisualWindow]::keybd_event($key, 0, 0x0002, [UIntPtr]::Zero)
    if ($modifier2 -ne 0) {
        [ExplorieVisualWindow]::keybd_event($modifier2, 0, 0x0002, [UIntPtr]::Zero)
    }
    if ($modifier -ne 0) {
        [ExplorieVisualWindow]::keybd_event($modifier, 0, 0x0002, [UIntPtr]::Zero)
    }
}

function Wait-ForWindow([System.Diagnostics.Process]$process) {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        Start-Sleep -Milliseconds 100
        $process.Refresh()
        if ($process.HasExited) { throw "Explorie exited before its window appeared." }
        if ($process.MainWindowHandle -ne 0) { return [IntPtr]$process.MainWindowHandle }
    }
    throw "Explorie did not create a window within ten seconds."
}

function Close-Explorie([System.Diagnostics.Process]$process) {
    if ($process.HasExited) { return }
    $null = $process.CloseMainWindow()
    if (-not $process.WaitForExit(10000)) {
        $process.Kill()
        $process.WaitForExit()
        throw "Explorie did not close normally after visual capture."
    }
    if ($process.ExitCode -ne 0) { throw "Explorie exited with code $($process.ExitCode)." }
}

function Wait-ForProfileFile([string]$path) {
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        if (Test-Path -LiteralPath $path -PathType Leaf) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Explorie did not persist the expected profile file: $path"
}

function Initialize-Profile([string]$profile) {
    New-Item -ItemType Directory -Path $profile -Force | Out-Null
    $process = Start-Explorie $profile
    try {
        $null = Wait-ForWindow $process
        Wait-ForProfileFile (Join-Path $profile "settings-v1.json")
        Wait-ForProfileFile (Join-Path $profile "session-v1.json")
    } finally {
        Close-Explorie $process
    }
}

function Set-Appearance([string]$profile, [string]$theme, [bool]$highContrast) {
    $settingsPath = Join-Path $profile "settings-v1.json"
    $settings = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
    $settings.appearance.theme = $theme
    $settings.appearance.highContrast = $highContrast
    $settings | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $settingsPath
}

function Set-PinnedPreview([string]$profile) {
    $settingsPath = Join-Path $profile "settings-v1.json"
    $settings = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
    $settings.view.showPreviewPanel = $true
    $settings | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $settingsPath
}

function Set-SessionPath([string]$profile, [string]$path) {
    $sessionPath = Join-Path $profile "session-v1.json"
    $session = Get-Content -LiteralPath $sessionPath -Raw | ConvertFrom-Json
    foreach ($tab in $session.tabs) {
        $tab.path = $path
    }
    $session.recents = @($path)
    $session | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $sessionPath
}

function Set-SessionHistory([string]$profile) {
    $sessionPath = Join-Path $profile "session-v1.json"
    $session = Get-Content -LiteralPath $sessionPath -Raw | ConvertFrom-Json
    $session.tabs[0] | Add-Member -NotePropertyName back -NotePropertyValue @(
        (Join-Path $historyRoot "History One"),
        (Join-Path $historyRoot "History Two"),
        (Join-Path $historyRoot "History Three")
    ) -Force
    $session.tabs[0] | Add-Member -NotePropertyName forward -NotePropertyValue @() -Force
    $session | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $sessionPath
}

function Capture-Appearance(
    [string]$name,
    [string]$theme,
    [bool]$highContrast,
    [bool]$openMore = $false,
    [bool]$openPreview = $false,
    [int]$windowWidth = 1040,
    [int]$windowHeight = 784,
    [bool]$openSettings = $false,
    [bool]$openConflict = $false,
    [bool]$openCommands = $false,
    [bool]$openWorkspaces = $false,
    [bool]$openNewFolder = $false,
    [bool]$openArchive = $false,
    [bool]$openDelete = $false,
    [bool]$openQuickLook = $false,
    [bool]$openOperations = $false,
    [bool]$openGoToFolder = $false,
    [bool]$openLoadError = $false,
    [bool]$openHistory = $false,
    [bool]$openGridView = $false,
    [bool]$selectFile = $false,
    [bool]$showSyncthing = $false
) {
    $profile = Join-Path $proofRoot $name
    Initialize-Profile $profile
    Set-Appearance $profile $theme $highContrast
    if ($openLoadError) {
        Set-SessionPath $profile (Join-Path $proofRoot "missing-folder")
    }
    if ($openHistory) {
        Set-SessionHistory $profile
    }
    if ($openPreview) {
        Set-PinnedPreview $profile
    }
    $syncthingMarker = Join-Path $fixture ".stfolder"
    $syncthingConflict = Join-Path $fixture "migration-notes.sync-conflict-20260825-120000.md"
    if ($showSyncthing) {
        New-Item -ItemType Directory -Path $syncthingMarker -Force | Out-Null
        Set-Content -LiteralPath $syncthingConflict -Value "# Resolve this Syncthing conflict"
    }
    $browsePath = if ($openLoadError) {
        $null
    } elseif ($openConflict) {
        $conflictSource
    } else {
        $fixture
    }
    $process = Start-Explorie $profile $browsePath
    try {
        $handle = Wait-ForWindow $process
        [ExplorieVisualWindow]::SetWindowPos(
            $handle,
            [IntPtr](-1),
            80,
            80,
            $windowWidth,
            $windowHeight,
            0x0040
        ) | Out-Null
        Start-Sleep -Milliseconds 800
        $rect = New-Object ExplorieVisualWindow+RECT
        if (-not [ExplorieVisualWindow]::GetWindowRect($handle, [ref]$rect)) {
            throw "Unable to read Explorie window bounds."
        }
        if ($openMore -or $openSettings) {
            [ExplorieVisualWindow]::SetCursorPos($rect.Right - 25, $rect.Top + 108) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 300
        }
        if ($openSettings) {
            [ExplorieVisualWindow]::SetCursorPos($rect.Right - 118, $rect.Top + 256) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 800
        }
        if ($openPreview -or $openQuickLook) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            $previewRowY = if ($windowWidth -lt 1100) { 322 } else { 292 }
            [ExplorieVisualWindow]::SetCursorPos($rect.Left + 390, $rect.Top + $previewRowY) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 150
            if ($openQuickLook) {
                [ExplorieVisualWindow]::keybd_event(0x20, 0, 0, [UIntPtr]::Zero)
                [ExplorieVisualWindow]::keybd_event(0x20, 0, 0x0002, [UIntPtr]::Zero)
            }
            Start-Sleep -Milliseconds 800
        }
        if ($openConflict) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x28
            Send-KeyChord 0x43 0x11
            Start-Sleep -Milliseconds 150
            Send-KeyChord 0x26 0x12
            Start-Sleep -Milliseconds 500
            Send-KeyChord 0x28
            Send-KeyChord 0x0D
            Start-Sleep -Milliseconds 500
            Send-KeyChord 0x56 0x11
            Start-Sleep -Milliseconds 1000
        }
        if ($openCommands) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x50 0x11 0x10
            Start-Sleep -Milliseconds 800
        }
        if ($openWorkspaces) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x57 0x11 0x10
            Start-Sleep -Milliseconds 800
        }
        if ($openNewFolder) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x4E 0x11 0x10
            Start-Sleep -Milliseconds 800
        }
        if ($openArchive -or $openDelete) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x28
            Start-Sleep -Milliseconds 200
        }
        if ($openArchive) {
            Send-KeyChord 0x41 0x11 0x12
            Start-Sleep -Milliseconds 800
        }
        if ($openDelete) {
            Send-KeyChord 0x2E 0x10
            Start-Sleep -Milliseconds 800
        }
        if ($openOperations) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x28
            Send-KeyChord 0x28
            Send-KeyChord 0x28
            Send-KeyChord 0x28
            Send-KeyChord 0x43 0x11
            Send-KeyChord 0x26
            Send-KeyChord 0x0D
            Start-Sleep -Milliseconds 300
            Send-KeyChord 0x56 0x11
            Start-Sleep -Milliseconds 1200
        }
        if ($openGoToFolder) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x47 0x11
            Start-Sleep -Milliseconds 800
        }
        if ($openHistory) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            [ExplorieVisualWindow]::SetCursorPos($rect.Left + 260, $rect.Top + 62) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 800
        }
        if ($openGridView) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            Send-KeyChord 0x32 0x11
            Start-Sleep -Milliseconds 300
            [ExplorieVisualWindow]::SetCursorPos($rect.Right - 133, $rect.Top + 108) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 800
        }
        if ($selectFile) {
            [ExplorieVisualWindow]::SetForegroundWindow($handle) | Out-Null
            [ExplorieVisualWindow]::SetCursorPos($rect.Left + 390, $rect.Top + 185) | Out-Null
            [ExplorieVisualWindow]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [ExplorieVisualWindow]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 500
        }
        [ExplorieVisualWindow]::SetCursorPos(
            [Math]::Max(0, $rect.Left - 16),
            [Math]::Max(0, $rect.Top - 16)
        ) | Out-Null
        Start-Sleep -Milliseconds 100
        $width = $rect.Right - $rect.Left
        $height = $rect.Bottom - $rect.Top
        $bitmap = New-Object System.Drawing.Bitmap($width, $height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
            $capture = Join-Path $resolvedOutput "gpui-visual-$name.png"
            $bitmap.Save($capture, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    } finally {
        if (($openConflict -or $openCommands -or $openWorkspaces -or $openNewFolder -or $openArchive -or $openDelete -or $openQuickLook -or $openGoToFolder -or $openHistory -or $openGridView) -and -not $process.HasExited) {
            [ExplorieVisualWindow]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
            Send-KeyChord 0x1B
            Start-Sleep -Milliseconds 300
        }
        Close-Explorie $process
        if ($showSyncthing) {
            if (Test-Path -LiteralPath $syncthingConflict -PathType Leaf) {
                Remove-Item -LiteralPath $syncthingConflict -Force
            }
            if (Test-Path -LiteralPath $syncthingMarker -PathType Container) {
                Remove-Item -LiteralPath $syncthingMarker -Force
            }
        }
    }
    return [pscustomobject]@{ Name = $name; Capture = $capture; Width = $width; Height = $height }
}

try {
    $captures = @(
        Capture-Appearance "dark" "dark" $false
        Capture-Appearance -name "dark-status-wide" -theme "dark" -highContrast $false -windowWidth 1216 -windowHeight 736 -selectFile $true
        Capture-Appearance "dark-more" "dark" $false $true
        Capture-Appearance "dark-settings" "dark" $false $false $false 1040 784 $true
        Capture-Appearance "dark-settings-narrow" "dark" $false $false $false 816 616 $true
        Capture-Appearance "dark-conflict" "dark" $false $false $false 1040 784 $false $true
        Capture-Appearance "dark-commands" "dark" $false $false $false 1040 784 $false $false $true
        Capture-Appearance "dark-workspaces" "dark" $false $false $false 1040 784 $false $false $false $true
        Capture-Appearance -name "dark-new-folder" -theme "dark" -highContrast $false -openNewFolder $true
        Capture-Appearance -name "dark-archive" -theme "dark" -highContrast $false -openArchive $true
        Capture-Appearance -name "dark-trash" -theme "dark" -highContrast $false -openDelete $true
        Capture-Appearance "light" "light" $false
        Capture-Appearance "high-contrast-light" "light" $true
        Set-Content -LiteralPath (Join-Path $fixture ".explorie.json") -Value @'
{
  "migration-notes.md": {
    "status": "Done",
    "priority": "High",
    "type": "Document",
    "category": "Project"
  },
  "palette.json": {
    "status": "In Progress",
    "priority": "Medium",
    "type": "Data",
    "category": "Reference"
  }
}
'@
        Capture-Appearance "dark-custom-columns" "dark" $false
        Capture-Appearance "dark-preview-narrow" "dark" $false $false $true 816 616
        Capture-Appearance "dark-preview-wide" "dark" $false $false $true 1216 736
        Capture-Appearance -name "dark-quick-look" -theme "dark" -highContrast $false -windowWidth 1216 -windowHeight 736 -openQuickLook $true
        Capture-Appearance -name "dark-operations" -theme "dark" -highContrast $false -openOperations $true
        Capture-Appearance -name "dark-go-to-folder" -theme "dark" -highContrast $false -openGoToFolder $true
        Capture-Appearance -name "dark-load-error" -theme "dark" -highContrast $false -openLoadError $true
        Capture-Appearance -name "dark-navigation-history" -theme "dark" -highContrast $false -openHistory $true
        Capture-Appearance -name "dark-grid-view-options" -theme "dark" -highContrast $false -openGridView $true
        Capture-Appearance -name "dark-syncthing" -theme "dark" -highContrast $false -windowWidth 1216 -windowHeight 736 -showSyncthing $true
    )
    $captures | ConvertTo-Json
} finally {
    $resolvedProof = [System.IO.Path]::GetFullPath($proofRoot)
    $resolvedTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    if (-not $resolvedProof.StartsWith($resolvedTemp, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a visual-proof directory outside the system temp root."
    }
    if (Test-Path -LiteralPath $resolvedProof) {
        Remove-Item -LiteralPath $resolvedProof -Recurse -Force
    }
}
