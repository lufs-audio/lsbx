<#
.SYNOPSIS
  Install a pinned Sunshine build on a disposable Carnyx Windows clone only.
.DESCRIPTION
  Run as an administrator inside the clone on Carnyx's private libvirt bridge.
  Supply the upstream Sunshine-Windows-AMD64-installer.msi. The script accepts
  only the tested hash and a valid Authenticode signature. It does not download
  anything, configure pairing, expose the admin UI, or enable UPnP.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string]$Installer,
    [Parameter(Mandatory = $true)] [switch]$DisposableClone,
    [ValidateRange(1029, 65514)] [int]$AnchorPort = 47989,
    [switch]$ReconfigureExisting
)
$ErrorActionPreference = 'Stop'
if (-not $DisposableClone) { throw 'This script must only run in a disposable clone.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]::new($identity)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run as administrator in the disposable VM.'
}

$path = (Resolve-Path -LiteralPath $Installer).Path
# Upstream Sunshine v2026.914.233613 AMD64 MSI (GitHub release asset digest).
$sha256 = '1D7FED8BEECD5889DC7FF14CF9F42D6D38F37C3066C13C6C2A5F4E91847E0CCF'
if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $sha256 -or
    (Get-AuthenticodeSignature -LiteralPath $path).Status -ne 'Valid') {
    throw 'Sunshine package failed the pinned SHA256 or Authenticode verification.'
}
if ($path -match '["\r\n]') { throw 'Invalid installer path.' }
$service = Get-Service -Name SunshineService -ErrorAction SilentlyContinue
if ($service -and -not $ReconfigureExisting) {
    throw 'Sunshine is already installed. Start with a fresh disposable clone or explicitly reconfigure this canary.'
}
if (-not $service -and $ReconfigureExisting) {
    throw 'There is no Sunshine installation to reconfigure.'
}

# Carnyx's libvirt bridge is the only supported media source for this canary.
$privateHost = '192.168.122.1'
$tcpName = 'LSBX disposable Sunshine test TCP'
$udpName = 'LSBX disposable Sunshine test UDP'
try {
    if (-not $ReconfigureExisting) {
        $msi = Start-Process -FilePath msiexec.exe -ArgumentList "/i `"$path`" /qn /norestart" -Wait -PassThru
        if ($msi.ExitCode -ne 0) { throw "Sunshine MSI returned $($msi.ExitCode); use a fresh clone to retry." }
    }
    Stop-Service -Name SunshineService
    Get-NetFirewallRule -DisplayName $tcpName,$udpName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Get-NetFirewallRule | Where-Object {
        $_.DisplayName -match 'Sunshine' -and $_.DisplayName -notin @($tcpName, $udpName)
    } | Disable-NetFirewallRule

    if (Get-NetFirewallProfile | Where-Object { -not $_.Enabled }) {
        throw 'Windows Defender Firewall must be enabled on all profiles for this canary.'
    }
    $dir = Join-Path $env:ProgramFiles 'Sunshine\config'
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    @"
port = $AnchorPort
upnp = off
origin_web_ui_allowed = pc
sunshine_name = lsbx-carnyx-canary
encoder = software
capture = ddx
hevc_mode = 1
av1_mode = 1
stream_audio = yes
install_steam_audio_drivers = off
dd_configuration_option = disabled
max_bitrate = 20000
"@ | Set-Content -LiteralPath (Join-Path $dir 'sunshine.conf') -Encoding ascii

    $tcpPorts = @(-5, 0, 21) | ForEach-Object { $_ + $AnchorPort }
    $udpPorts = @(9, 10, 11, 13, 21) | ForEach-Object { $_ + $AnchorPort }
    New-NetFirewallRule -DisplayName $tcpName -Direction Inbound -Action Allow `
        -Protocol TCP -LocalPort $tcpPorts -RemoteAddress $privateHost | Out-Null
    New-NetFirewallRule -DisplayName $udpName -Direction Inbound -Action Allow `
        -Protocol UDP -LocalPort $udpPorts -RemoteAddress $privateHost | Out-Null
    Start-Service -Name SunshineService
    Get-Service -Name SunshineService | Select-Object Name, Status
} catch {
    Stop-Service -Name SunshineService -ErrorAction SilentlyContinue
    Get-NetFirewallRule -DisplayName $tcpName,$udpName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    throw
}
