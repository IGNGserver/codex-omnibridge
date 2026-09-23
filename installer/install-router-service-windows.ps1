[CmdletBinding()]
param(
    [string]$InstallDir = "$(Join-Path $env:LOCALAPPDATA 'CodexMultiProvider\bin')",
    [string]$CliBinary = "",
    [switch]$EnableService
)

$ErrorActionPreference = "Stop"

$CliDestination = if ($CliBinary) {
    $CliBinary
} else {
    Join-Path $InstallDir "codex-mp.exe"
}

if (-not (Test-Path -LiteralPath $CliDestination -PathType Leaf)) {
    throw "codex-mp.exe was not found: $CliDestination"
}

$ConfigDir = Join-Path $env:APPDATA "dev.codex-multiprovider\Codex MultiProvider"
$RegistryPath = if ($env:CODEX_MP_REGISTRY) {
    $env:CODEX_MP_REGISTRY
} else {
    Join-Path $ConfigDir "providers.json"
}
$EndpointFile = if ($env:CODEX_MP_ROUTER_ENDPOINT_FILE) {
    $env:CODEX_MP_ROUTER_ENDPOINT_FILE
} else {
    Join-Path $ConfigDir "router-endpoint.json"
}

$ParentDir = Split-Path -Parent $RegistryPath
if (-not (Test-Path -LiteralPath $ParentDir)) {
    New-Item -ItemType Directory -Force -Path $ParentDir | Out-Null
}

$TaskName = "CodexMultiProviderRouter"

# Remove existing task if already present
$existingTask = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existingTask) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
}

$action = New-ScheduledTaskAction `
    -Execute $CliDestination `
    -Argument "--registry `"$RegistryPath`" manager --endpoint-file `"$EndpointFile`""

$trigger = New-ScheduledTaskTrigger -AtLogOn

# The manager owns the Router child and keeps recovering it after a local
# crash. If the manager itself is killed by OOM or an update, use the largest
# Task Scheduler retry count instead of a small finite budget that can leave
# the service permanently stopped.
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -RestartCount ([uint32]::MaxValue) `
    -RestartInterval (New-TimeSpan -Seconds 2) `
    -MultipleInstances IgnoreNew

Register-ScheduledTask `
    -TaskName $TaskName `
    -Description "Codex MultiProvider OmniBridge router background task" `
    -Action $action `
    -Trigger $trigger `
    -Settings $settings `
    -Force | Out-Null

if ($EnableService.IsPresent -or $env:CODEX_MP_ENABLE_SERVICE -eq "1") {
    Start-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
}

Write-Host "registered scheduled task $TaskName"
Write-Host "router manager task configured to start at logon; endpoint file at $EndpointFile"
