[CmdletBinding()]
param(
    [string]$Source = "",
    [string]$Output = "",
    [string]$TargetDir = "",
    [switch]$SkipFetch,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$StockCodexCommit = if ($env:CODEX_MP_CODEX_COMMIT) { $env:CODEX_MP_CODEX_COMMIT } else { "73a1148c9c775c2a4616ce5096291740a00ed68a" }
$ProjectDir = Split-Path -Parent $PSScriptRoot
$Repository = if ($env:CODEX_MP_CODEX_REPOSITORY) { $env:CODEX_MP_CODEX_REPOSITORY } else { "https://github.com/openai/codex.git" }
$CacheRoot = if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA "codex-multiprovider" } else { Join-Path $env:TEMP "codex-multiprovider" }
$SourceDir = if ($Source) { $Source } elseif ($env:CODEX_MP_CODEX_SOURCE_DIR) { $env:CODEX_MP_CODEX_SOURCE_DIR } else { Join-Path $CacheRoot "codex-rs" }
$OutputDir = if ($Output) { $Output } elseif ($env:CODEX_MP_CODEX_OUTPUT_DIR) { $env:CODEX_MP_CODEX_OUTPUT_DIR } else { Join-Path $ProjectDir "dist\stock-codex" }
$CargoTargetDir = if ($TargetDir) { $TargetDir } elseif ($env:CODEX_MP_CODEX_TARGET_DIR) { $env:CODEX_MP_CODEX_TARGET_DIR } else { Join-Path $CacheRoot "target" }

function Invoke-Native([string]$File, [string[]]$Arguments) {
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$File failed with exit code $LASTEXITCODE"
    }
}

function Write-Utf8NoBom([string]$Path, [string]$Content) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($Path, $Content, $encoding)
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo is required"
}
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "git is required"
}
if (-not (Test-Path $SourceDir)) {
    if ($SkipFetch) {
        throw "--SkipFetch was requested but the Codex checkout is absent"
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $SourceDir) | Out-Null
    Invoke-Native git @("clone", "--filter=blob:none", "--no-checkout", $Repository, $SourceDir)
    Invoke-Native git @("-C", $SourceDir, "fetch", "--depth=1", "origin", $StockCodexCommit)
    Invoke-Native git @("-C", $SourceDir, "checkout", "--detach", $StockCodexCommit)
}

$TargetRoot = if (Test-Path (Join-Path $SourceDir "core\src\client.rs")) {
    (Resolve-Path $SourceDir).Path
} elseif (Test-Path (Join-Path $SourceDir "codex-rs\core\src\client.rs")) {
    (Resolve-Path (Join-Path $SourceDir "codex-rs")).Path
} else {
    throw "$SourceDir does not look like a Codex source checkout"
}

$GitRoot = (& git -C $TargetRoot rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "could not find the Codex git root"
}
$Head = (& git -C $GitRoot rev-parse HEAD).Trim()
if ($Head -ne $StockCodexCommit) {
    throw "expected stock Codex HEAD $StockCodexCommit, found $Head"
}

$ManifestPath = if (Test-Path (Join-Path $TargetRoot "Cargo.toml")) {
    Join-Path $TargetRoot "Cargo.toml"
} else {
    throw "Codex Cargo.toml was not found"
}

$PatchFile = Join-Path $ProjectDir ("patches\codex\{0}\0001-per-turn-local-router.patch" -f $StockCodexCommit)
if (-not (Test-Path $PatchFile)) {
    throw "no verified Codex patch is available for commit $StockCodexCommit ($PatchFile)"
}
$Prefix = (& git -C $TargetRoot rev-parse --show-prefix).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "could not determine the Codex checkout prefix"
}
$PatchApplyArgs = @()
if ($Prefix) {
    $PatchApplyArgs += "--directory=$($Prefix.TrimEnd('/'))"
}
$ReverseCheckArgs = @("-C", $GitRoot, "-c", "core.whitespace=error", "apply", "--reverse", "--check") + $PatchApplyArgs + @($PatchFile)
& git @ReverseCheckArgs *> $null
$ReverseCheckExit = $LASTEXITCODE
if ($ReverseCheckExit -ne 0) {
    $ForwardCheckArgs = @("-C", $GitRoot, "-c", "core.whitespace=error", "apply", "--check") + $PatchApplyArgs + @($PatchFile)
    Invoke-Native git $ForwardCheckArgs
    $ApplyArgs = @("-C", $GitRoot, "apply") + $PatchApplyArgs + @($PatchFile)
    Invoke-Native git $ApplyArgs
    Write-Host "applied per-turn local-router patch to $TargetRoot"
} else {
    Write-Host "patch already applied to $TargetRoot"
}

$PreviousTargetDir = $env:CARGO_TARGET_DIR
$env:CARGO_TARGET_DIR = $CargoTargetDir
try {
    if (-not $SkipBuild) {
        Invoke-Native cargo @("build", "--locked", "--release", "--manifest-path", $ManifestPath, "-p", "codex-cli", "--bin", "codex")
        Invoke-Native cargo @("build", "--locked", "--release", "--manifest-path", $ManifestPath, "-p", "codex-app-server", "--bin", "codex-app-server")
    }
} finally {
    if ($null -eq $PreviousTargetDir) {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    } else {
        $env:CARGO_TARGET_DIR = $PreviousTargetDir
    }
}

$CodexBinary = Join-Path $CargoTargetDir "release\codex.exe"
$AppServerBinary = Join-Path $CargoTargetDir "release\codex-app-server.exe"
if (-not (Test-Path $CodexBinary)) { throw "missing release binary $CodexBinary" }
if (-not (Test-Path $AppServerBinary)) { throw "missing release binary $AppServerBinary" }

$StagingDir = Join-Path $OutputDir ".staging.$PID"
New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
try {
    Copy-Item $CodexBinary (Join-Path $StagingDir "codex-mp-codex-bin.exe")
    Copy-Item $AppServerBinary (Join-Path $StagingDir "codex-mp-app-server-bin.exe")

    $ManagerLauncher = '@echo off
setlocal
set "SCRIPT_DIR=%~dp0"
if not defined CODEX_MP_MANAGER_BIN set "CODEX_MP_MANAGER_BIN=%SCRIPT_DIR%codex-mp.exe"
"%CODEX_MP_MANAGER_BIN%" launch --codex-binary "%SCRIPT_DIR%codex-mp-codex-bin.exe" -- %*
exit /b %ERRORLEVEL%
'
    $AppServerLauncher = $ManagerLauncher.Replace("codex-mp-codex-bin.exe", "codex-mp-app-server-bin.exe")
    Set-Content -Encoding ascii -Path (Join-Path $StagingDir "codex-mp-codex.cmd") -Value $ManagerLauncher
    Set-Content -Encoding ascii -Path (Join-Path $StagingDir "codex-mp-app-server.cmd") -Value $AppServerLauncher

    @{
        schema_version = 1
        upstream_repository = $Repository
        upstream_commit = $StockCodexCommit
        stock_runtime = $true
        omni_bridge_provider = "omnibridge"
        codex_binary = "codex-mp-codex-bin.exe"
        app_server_binary = "codex-mp-app-server-bin.exe"
        uses_existing_codex_home = $true
        official_codex_binary_untouched = $true
    } | ConvertTo-Json | ForEach-Object {
        Write-Utf8NoBom (Join-Path $StagingDir "codex-mp-build.json") $_
    }

    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
    Get-ChildItem -File $StagingDir | Copy-Item -Destination $OutputDir -Force
} finally {
    Remove-Item $StagingDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "stock Codex artifacts written to $OutputDir"
Write-Host "stock Codex commit: $StockCodexCommit"
