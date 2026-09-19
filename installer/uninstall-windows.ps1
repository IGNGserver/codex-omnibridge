[CmdletBinding()]
param(
    [string]$InstallDir = "$(Join-Path $env:LOCALAPPDATA 'CodexMultiProvider\bin')",
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$RemainingArgs = @()
)

$ErrorActionPreference = "Stop"
$CliBinary = Join-Path $InstallDir "codex-mp.exe"
$Uninstaller = Join-Path $InstallDir "codex-mp-uninstall.ps1"
$RouterServiceUninstaller = Join-Path $InstallDir "codex-mp-router-service-uninstall.ps1"
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

# Stop and unregister the router service first so no running process keeps the
# binaries locked. This step is best-effort: it must never abort the cleanup.
if (Test-Path -LiteralPath $RouterServiceUninstaller -PathType Leaf) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $RouterServiceUninstaller
}

# A half-finished or partially removed install is exactly the case the manifest
# exists for, so never abort just because the CLI binary is gone: degrade to
# manifest-driven cleanup instead.
if (Test-Path -LiteralPath $CliBinary -PathType Leaf) {
    & $CliBinary uninstall @RemainingArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "codex-mp uninstall failed with exit code $LASTEXITCODE; continuing with file cleanup"
    }
} else {
    Write-Warning "$CliBinary is missing; skipping Codex integration restore"
    Write-Warning "re-install codex-mp and run 'codex-mp uninstall' to restore config.toml"
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
