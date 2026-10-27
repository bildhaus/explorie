[CmdletBinding()]
param(
    [string]$Executable,
    [int]$TimeoutSeconds = 25,
    [switch]$KeepProfile
)

$ErrorActionPreference = 'Stop'
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($Executable)) {
    $Executable = Join-Path $workspace 'target\release\explorie-gpui.exe'
}
$Executable = [IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "GPUI release executable is missing: $Executable"
}

$fixture = Join-Path $workspace 'apps\desktop\native-assets\icons\icon.png'
if (-not (Test-Path -LiteralPath $fixture -PathType Leaf)) {
    throw "Preview source fixture is missing: $fixture"
}
if ($null -eq (Get-Command magick -ErrorAction SilentlyContinue)) {
    throw 'ImageMagick is required for the helper-generated preview cache smoke.'
}

Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class ExploriePreviewCacheSmokeNative {
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PostMessage(IntPtr window, uint message, UIntPtr key, IntPtr data);

    [DllImport("user32.dll")]
    public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extraInfo);
}
'@

function Send-NativeKey {
    param(
        [IntPtr]$WindowHandle,
        [uint32]$VirtualKey
    )
    if (-not [ExploriePreviewCacheSmokeNative]::PostMessage(
        $WindowHandle,
        0x0100,
        [UIntPtr]::new($VirtualKey),
        [IntPtr]::Zero
    )) {
        throw "Unable to send key-down for virtual key $VirtualKey."
    }
    if (-not [ExploriePreviewCacheSmokeNative]::PostMessage(
        $WindowHandle,
        0x0101,
        [UIntPtr]::new($VirtualKey),
        [IntPtr]::Zero
    )) {
        throw "Unable to send key-up for virtual key $VirtualKey."
    }
}

function Send-NativePreviewCacheChord {
    param([IntPtr]$WindowHandle)

    foreach ($key in @(0x11, 0x12, 0x10)) {
        [ExploriePreviewCacheSmokeNative]::keybd_event($key, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 25
    }
    Send-NativeKey -WindowHandle $WindowHandle -VirtualKey 0x50
    Start-Sleep -Milliseconds 250
    foreach ($key in @(0x10, 0x12, 0x11)) {
        [ExploriePreviewCacheSmokeNative]::keybd_event($key, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 25
    }
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$smokeRoot = [IO.Path]::GetFullPath(
    (Join-Path $tempRoot ("explorie-gpui-cache-smoke-{0}" -f [guid]::NewGuid()))
)
$tempPrefix = $tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $smokeRoot.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Smoke profile escaped the temporary directory: $smokeRoot"
}

$browsePath = Join-Path $smokeRoot 'browse'
$profile = Join-Path $smokeRoot 'profile'
New-Item -ItemType Directory -Path $browsePath, $profile -Force | Out-Null
$source = Join-Path $browsePath 'helper-preview.psd'
Copy-Item -LiteralPath $fixture -Destination $source
$sourceHash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash

$process = $null
try {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.WorkingDirectory = Split-Path $Executable -Parent
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    if ($null -ne $startInfo.ArgumentList) {
        $startInfo.ArgumentList.Add($browsePath)
    } else {
        $startInfo.Arguments = '"' + $browsePath.Replace('"', '\"') + '"'
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
        throw "GPUI release process exited before cache smoke (code $($process.ExitCode))."
    }
    if ($process.MainWindowHandle -eq 0) {
        throw "GPUI release process did not create a native window within $TimeoutSeconds seconds."
    }

    $shell = New-Object -ComObject WScript.Shell
    if (-not $shell.AppActivate($process.Id)) {
        throw 'Unable to activate the GPUI release window.'
    }
    $previewDir = Join-Path $profile 'cache\preview'
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 250
        $listingReady = $null -ne (
            Get-ChildItem -LiteralPath $previewDir -Filter '*-icon-*.png' -File -ErrorAction SilentlyContinue |
                Select-Object -First 1
        )
    } while (-not $listingReady -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline)
    if (-not $listingReady) {
        throw 'The native listing did not materialize its file row.'
    }
    Send-NativeKey -WindowHandle $process.MainWindowHandle -VirtualKey 0x28
    Start-Sleep -Milliseconds 250
    Send-NativeKey -WindowHandle $process.MainWindowHandle -VirtualKey 0x20

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $artifact = $null
    do {
        Start-Sleep -Milliseconds 250
        $artifact = Get-ChildItem -LiteralPath $previewDir -Filter '*-image.png' -File -ErrorAction SilentlyContinue |
            Select-Object -First 1
    } while ($null -eq $artifact -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline)
    if ($null -eq $artifact) {
        throw 'The helper-generated preview artifact was not created.'
    }

    $firstWrite = $artifact.LastWriteTimeUtc
    $sentinel = Join-Path $previewDir 'stale-cache-sentinel.bin'
    Copy-Item -LiteralPath $source -Destination $sentinel
    Start-Sleep -Milliseconds 1100
    if (-not $shell.AppActivate($process.Id)) {
        throw 'Unable to reactivate the GPUI release window.'
    }
    Send-NativePreviewCacheChord -WindowHandle $process.MainWindowHandle

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $refreshed = $null
    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        $refreshed = Get-ChildItem -LiteralPath $previewDir -Filter '*-image.png' -File -ErrorAction SilentlyContinue |
            Select-Object -First 1
        $waiting = (Test-Path -LiteralPath $sentinel) -or
            $null -eq $refreshed -or
            $refreshed.LastWriteTimeUtc -le $firstWrite
    } while ($waiting -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline)

    if ($process.HasExited) {
        throw "GPUI release process exited during cache smoke (code $($process.ExitCode))."
    }
    if (Test-Path -LiteralPath $sentinel) {
        throw 'Preview cache clear left stale cache data behind.'
    }
    if ($null -eq $refreshed -or $refreshed.LastWriteTimeUtc -le $firstWrite) {
        throw 'The open helper preview was not regenerated after cache clearing.'
    }

    $process.Refresh()
    $proof = [ordered]@{
        executable = $Executable
        bytes = (Get-Item -LiteralPath $Executable).Length
        processId = $process.Id
        responding = $process.Responding
        mainWindowHandle = $process.MainWindowHandle.ToInt64()
        mainWindowTitle = $process.MainWindowTitle
        isolatedProfile = $profile
        sourceHashPreserved = $sourceHash -eq (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        generatedArtifact = $refreshed.FullName
        artifactRegenerated = $refreshed.LastWriteTimeUtc -gt $firstWrite
        staleCacheRemoved = -not (Test-Path -LiteralPath $sentinel)
    }

    if (-not $process.CloseMainWindow()) {
        throw 'GPUI release window refused a normal close request.'
    }
    if (-not $process.WaitForExit(15000)) {
        throw 'GPUI release process did not close cleanly within 15 seconds.'
    }

    $proof['exitCode'] = $process.ExitCode
    $proof['recoveryMarkerPresent'] = Test-Path -LiteralPath (Join-Path $profile 'runtime-dirty-v1.json')
    if ($process.ExitCode -ne 0) {
        throw "GPUI release process returned exit code $($process.ExitCode)."
    }
    if ($proof['recoveryMarkerPresent']) {
        throw 'GPUI release process left the dirty-session marker after a normal close.'
    }
    if (-not $proof['sourceHashPreserved']) {
        throw 'Preview cache clearing changed the source fixture.'
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
                'explorie-gpui-cache-smoke-',
                [StringComparison]::Ordinal
            )
        ) {
            Remove-Item -LiteralPath $resolvedSmokeRoot -Recurse -Force
        }
    }
}
