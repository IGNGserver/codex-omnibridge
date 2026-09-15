[CmdletBinding()]
param(
    [string]$InstallDir = "$(Join-Path $env:LOCALAPPDATA 'CodexMultiProvider\bin')"
)

$ErrorActionPreference = "Stop"
$CliBinary = Join-Path $InstallDir "codex-mp.exe"
$Uninstaller = Join-Path $InstallDir "codex-mp-uninstall.ps1"
$RouterServiceUninstaller = Join-Path $InstallDir "codex-mp-router-service-uninstall.ps1"
$Manifest = Join-Path $InstallDir ".codex-mp-install-manifest"

if (Test-Path -LiteralPath $RouterServiceUninstaller -PathType Leaf) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $RouterServiceUninstaller -ErrorAction SilentlyContinue
}

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

if (-not (Test-Path -LiteralPath $CliBinary -PathType Leaf)) {
    throw "$CliBinary is missing; refusing to remove Codex integration"
}

& $CliBinary uninstall @args
if ($LASTEXITCODE -ne 0) {
    throw "codex-mp uninstall failed with exit code $LASTEXITCODE"
}

if (Test-Path -LiteralPath $Manifest -PathType Leaf) {
    foreach ($ownedFile in Get-Content -LiteralPath $Manifest) {
        if ($ownedFile -and (Test-OwnedInstallPath $ownedFile)) {
            Remove-Item -LiteralPath $ownedFile -Force -ErrorAction SilentlyContinue
        }
    }
} else {
    Remove-Item -LiteralPath $CliBinary, $Uninstaller -Force -ErrorAction SilentlyContinue
}

Write-Host "removed Codex MultiProvider binaries and launchers"
