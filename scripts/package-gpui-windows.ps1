[CmdletBinding()]
param(
    [string]$Version,
    [string]$BuildDirectory = "target/release",
    [string]$OutputDirectory = "release-artifacts/windows",
    [string]$OutputName,
    [string]$IsccPath
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot

if (-not $env:EXPLORIE_PLUGIN_CATALOG) {
    throw "Set EXPLORIE_PLUGIN_CATALOG to the catalog used when building the release application."
}
& node (Join-Path $PSScriptRoot "package-plugins.mjs") --verify-catalog $env:EXPLORIE_PLUGIN_CATALOG --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "Official plugin catalog verification failed." }

if (-not $Version) {
    $Version = (Get-Content -LiteralPath (Join-Path $repository "package.json") -Raw | ConvertFrom-Json).version
}
if (-not $OutputName) {
    $OutputName = "explorie-$Version-windows-x64-setup"
}

$build = [IO.Path]::GetFullPath((Join-Path $repository $BuildDirectory))
$output = [IO.Path]::GetFullPath((Join-Path $repository $OutputDirectory))
$resources = [IO.Path]::GetFullPath((Join-Path $repository "apps/desktop/native-assets/resources"))
$definition = [IO.Path]::GetFullPath((Join-Path $repository "apps/desktop/gpui/installer/windows/explorie.iss"))
$sevenZip = Join-Path $repository "apps/desktop/native-assets/binaries/7zip-x86_64-pc-windows-msvc"
& node (Join-Path $PSScriptRoot "prepare-7zip.mjs") --stage-source (Join-Path $build "licenses")
if ($LASTEXITCODE -ne 0) { throw "Bundled 7-Zip corresponding source verification failed." }
& node (Join-Path $PSScriptRoot "package-plugins.mjs") --verify-catalog $env:EXPLORIE_PLUGIN_CATALOG --target x86_64-pc-windows-msvc --stage-directory (Join-Path $build "plugins")
if ($LASTEXITCODE -ne 0) { throw "Bundled plugin staging failed." }

foreach ($required in @(
    (Join-Path $build "explorie-gpui.exe"),
    (Join-Path $build "rclone.exe"),
    (Join-Path $sevenZip "7z.exe"),
    (Join-Path $sevenZip "7z.dll"),
    (Join-Path $resources "7zip-LICENSE.txt"),
    (Join-Path $resources "7zip-COPYING.txt"),
    (Join-Path $resources "7zip-NOTICE.txt"),
    (Join-Path $resources "winfsp-2.1.25156.msi"),
    (Join-Path $resources "assimp-LICENSE.txt"),
    $definition
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required installer input was not found: $required"
    }
}

New-Item -ItemType Directory -Path (Join-Path $build "7zip") -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $sevenZip "7z.exe"), (Join-Path $sevenZip "7z.dll") -Destination (Join-Path $build "7zip") -Force
& node (Join-Path $PSScriptRoot "smoke-7zip.mjs") (Join-Path $build "7zip/7z.exe")
if ($LASTEXITCODE -ne 0) { throw "Bundled 7-Zip archive smoke failed." }

$icon = Get-ChildItem -LiteralPath (Join-Path $build "build") -Filter "explorie.ico" -File -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "explorie-gpui-[^\\]+\\out\\explorie\.ico$" } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if (-not $icon) {
    throw "The generated Explorie ICO was not found below $build/build. Build explorie-gpui first."
}

if (-not $IsccPath) {
    $candidates = @(
        (Get-Command iscc.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1),
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) }
    $IsccPath = $candidates | Select-Object -First 1
}
if (-not $IsccPath -or -not (Test-Path -LiteralPath $IsccPath -PathType Leaf)) {
    throw "Inno Setup 6 compiler (ISCC.exe) was not found."
}

New-Item -ItemType Directory -Path $output -Force | Out-Null
& $IsccPath "/DAppVersion=$Version" "/DBuildDir=$build" "/DResourceDir=$resources" "/DAppIcon=$($icon.FullName)" "/DOutputDir=$output" "/DOutputName=$OutputName" $definition
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE."
}

$installer = Join-Path $output "$OutputName.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "The Windows installer was not created: $installer"
}
Get-Item -LiteralPath $installer
