[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$TaskName = "CodexMultiProviderRouter"

$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($task) {
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "removed scheduled task $TaskName"
} else {
    Write-Host "scheduled task $TaskName was not installed"
}
