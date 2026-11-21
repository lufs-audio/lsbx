<#
.SYNOPSIS
  Read-only in-guest preflight for the Carnyx Windows audio canary.
.DESCRIPTION
  Fails if audio services are down or Windows has no active render endpoint.
  Run inside a disposable canary. This does not prove playback or loopback
  capture; verify those in the interactive session with the operator runbook.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'

foreach ($name in @('AudioEndpointBuilder', 'Audiosrv')) {
    $service = Get-Service -Name $name
    if ($service.Status -ne 'Running') {
        throw "Windows audio service '$name' is $($service.Status)"
    }
}

# MMDevice DEVICE_STATE_ACTIVE is 0x1. Check Render, not the Capture endpoints.
$render = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$active = @(Get-ChildItem -LiteralPath $render | Where-Object {
    $device = Get-ItemProperty -LiteralPath $_.PSPath -Name DeviceState
    ($device.DeviceState -band 1) -ne 0
})
if ($active.Count -eq 0) {
    throw 'Windows has no active render endpoint. Check the VM sound device and the guest driver.'
}
Write-Output "Active Windows render endpoints: $($active.Count)"
