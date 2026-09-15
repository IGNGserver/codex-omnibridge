[CmdletBinding()]
param(
    [string]$InstallDir = "$(Join-Path $env:LOCALAPPDATA 'CodexMultiProvider\bin')",
    [string]$PackageDir = $PSScriptRoot,
    [string]$CliBinary = "",
    [switch]$InstallPatchedCodex,
    [switch]$InstallRouterService
)

$ErrorActionPreference = "Stop"

function Copy-OwnedFile([string]$Source, [string]$Destination) {
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "required package file is missing: $Source"
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Destination) | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

$PackageDir = (Resolve-Path -LiteralPath $PackageDir).Path
$Manifest = Join-Path $InstallDir ".codex-mp-install-manifest"
$InstallRoot = ([System.IO.Path]::GetFullPath($InstallDir)).TrimEnd([char[]]@('\', '/'))
$InstallRootPrefix = "$InstallRoot$([System.IO.Path]::DirectorySeparatorChar)"

function Test-OwnedInstallPath([string]$Path) {
    if (-not $Path) { return $false }
    try {
        $FullPath = [System.IO.Path]::GetFullPath($Path)
    } catch {
        return $false
    }
    return $FullPath.StartsWith($InstallRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)
}

$CliSource = if ($CliBinary) { $CliBinary } else { Join-Path $PackageDir "codex-mp.exe" }
$PatchedDir = if ($env:CODEX_MP_CODEX_ARTIFACT_DIR) {
    $env:CODEX_MP_CODEX_ARTIFACT_DIR
} else {
    Join-Path $PackageDir "dist\stock-codex"
}
$InstallPatched = $InstallPatchedCodex.IsPresent -or $env:CODEX_MP_BUILD_CODEX -eq "1" -or [bool]$env:CODEX_MP_CODEX_ARTIFACT_DIR

if (-not (Test-Path -LiteralPath $CliSource -PathType Leaf)) {
    $projectDir = Split-Path -Parent $PackageDir
    $manifestPath = Join-Path $projectDir "Cargo.toml"
    if ((Test-Path -LiteralPath $manifestPath) -and (Get-Command cargo -ErrorAction SilentlyContinue)) {
        & cargo build --release --locked --manifest-path $manifestPath --package codex-mp-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
        $CliSource = Join-Path $projectDir "target\release\codex-mp.exe"
    }
}
if (-not (Test-Path -LiteralPath $CliSource -PathType Leaf)) {
    throw "codex-mp.exe was not found: $CliSource"
}

if ($env:CODEX_MP_BUILD_CODEX -eq "1") {
    $buildScript = Join-Path $PackageDir "build-patched-codex.ps1"
    if (-not (Test-Path -LiteralPath $buildScript -PathType Leaf)) {
        $buildScript = Join-Path (Split-Path -Parent $PackageDir) "scripts\build-patched-codex.ps1"
    }
    if (-not (Test-Path -LiteralPath $buildScript -PathType Leaf)) {
        throw "build-patched-codex.ps1 is missing from the package"
    }
    & powershell -NoProfile -ExecutionPolicy Bypass -File $buildScript -Output $PatchedDir
    if ($LASTEXITCODE -ne 0) { throw "stock Codex build failed with exit code $LASTEXITCODE" }
}

if ($InstallPatched) {
    foreach ($name in @(
        "codex-mp-codex-bin.exe",
        "codex-mp-codex.cmd",
        "codex-mp-app-server-bin.exe",
        "codex-mp-app-server.cmd",
        "codex-mp-build.json"
    )) {
        if (-not (Test-Path -LiteralPath (Join-Path $PatchedDir $name) -PathType Leaf)) {
            throw "stock Codex artifact is missing: $(Join-Path $PatchedDir $name)"
        }
    }
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
if (Test-Path -LiteralPath $Manifest -PathType Leaf) {
    foreach ($ownedFile in Get-Content -LiteralPath $Manifest) {
        if ($ownedFile -and (Test-OwnedInstallPath $ownedFile) -and $ownedFile -ne $Manifest) {
            Remove-Item -LiteralPath $ownedFile -Force -ErrorAction SilentlyContinue
        }
    }
}

$installed = [System.Collections.Generic.List[string]]::new()
$cliDestination = Join-Path $InstallDir "codex-mp.exe"
Copy-OwnedFile $CliSource $cliDestination
$installed.Add($cliDestination)

$uninstallSource = Join-Path $PackageDir "uninstall-windows.ps1"
$uninstallDestination = Join-Path $InstallDir "codex-mp-uninstall.ps1"
Copy-OwnedFile $uninstallSource $uninstallDestination
$installed.Add($uninstallDestination)

$routerServiceInstaller = Join-Path $PackageDir "install-router-service-windows.ps1"
if (Test-Path -LiteralPath $routerServiceInstaller -PathType Leaf) {
    $routerServiceDestination = Join-Path $InstallDir "codex-mp-router-service-install.ps1"
    Copy-OwnedFile $routerServiceInstaller $routerServiceDestination
    $installed.Add($routerServiceDestination)
}

$routerServiceUninstaller = Join-Path $PackageDir "uninstall-router-service-windows.ps1"
if (Test-Path -LiteralPath $routerServiceUninstaller -PathType Leaf) {
    $routerServiceUninstallDest = Join-Path $InstallDir "codex-mp-router-service-uninstall.ps1"
    Copy-OwnedFile $routerServiceUninstaller $routerServiceUninstallDest
    $installed.Add($routerServiceUninstallDest)
}

if ($InstallRouterService.IsPresent -or $env:CODEX_MP_INSTALL_SERVICE -eq "1") {
    $serviceInstallerScript = Join-Path $InstallDir "codex-mp-router-service-install.ps1"
    if (Test-Path -LiteralPath $serviceInstallerScript -PathType Leaf) {
        & powershell -NoProfile -ExecutionPolicy Bypass -File $serviceInstallerScript -InstallDir $InstallDir -CliBinary $cliDestination
    }
}

if ($InstallPatched) {
    foreach ($name in @(
        "codex-mp-codex-bin.exe",
        "codex-mp-codex.cmd",
        "codex-mp-app-server-bin.exe",
        "codex-mp-app-server.cmd",
        "codex-mp-build.json"
    )) {
        $source = Join-Path $PatchedDir $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "stock Codex artifact is missing: $source"
        }
        $destination = Join-Path $InstallDir $name
        Copy-OwnedFile $source $destination
        $installed.Add($destination)
    }
}

if ($env:CODEX_MP_INSTALL_DESKTOP -eq "1") {
    if (-not $InstallPatched) {
        throw "CODEX_MP_INSTALL_DESKTOP=1 requires the explicit experimental Desktop runtime artifact"
    }
    & $cliDestination desktop install `
        --app-server-binary (Join-Path $InstallDir "codex-mp-app-server-bin.exe") `
        --codex-mp-binary $cliDestination
    if ($LASTEXITCODE -ne 0) { throw "Desktop runtime adapter installation failed with exit code $LASTEXITCODE" }
}

$manifestTmp = "$Manifest.tmp"
$installed.Add($Manifest)
$installed | Set-Content -Encoding utf8 -LiteralPath $manifestTmp
Move-Item -Force -LiteralPath $manifestTmp -Destination $Manifest

Write-Host "installed $cliDestination"
Write-Host "installed $uninstallDestination"
Write-Host "add $InstallDir to PATH before running codex-mp"
