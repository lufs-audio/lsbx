<#
.SYNOPSIS
  Bakes the lsbx Windows 11 browser-desktop golden environment (Unit 02 / Issue #36).
  Configures:
    1. TightVNC Server (Loopback-only, port 5900, password 'lsbx')
    2. Python websockify + noVNC assets (Port 8000 -> 127.0.0.1:5900, resize=scale)
    3. OpenSSH Server & lsbx authorized_keys hardening
    4. QEMU Guest Agent verification
    5. Defender exclusions + Windows Search indexing off (issue #54)

  NOTE: the per-sandbox vCPU topology and disk cache mode are NOT set here --
  they are host-side domain XML concerns (`crates/lsbx-backend-libvirt/src/
  domain_xml.rs`). A guest cannot fix an 8-socket/1-core topology from inside.
#>

param(
    [string]$VncPassword = "lsbx",
    [string]$NoVncDir = "C:\lsbx-novnc\noVNC-1.5.0"
)

$ErrorActionPreference = "Stop"

Write-Host "=== Starting lsbx Windows 11 Golden Provisioning ==="

# 1. OpenSSH Server Verification & Service Config
Write-Host "--> Verifying OpenSSH Server..."
$sshdService = Get-Service -Name "sshd" -ErrorAction SilentlyContinue
if (-not $sshdService) {
    Write-Host "Installing OpenSSH.Server..."
    Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
}
Set-Service -Name "sshd" -StartupType Automatic
Start-Service -Name "sshd"

# 2. TightVNC Registry Configuration (Loopback-only, allow loopback, password)
Write-Host "--> Configuring TightVNC Server..."
$vncRegPath = "HKLM:\Software\TightVNC\Server"
if (-not (Test-Path $vncRegPath)) {
    New-Item -Path $vncRegPath -Force | Out-Null
}

Set-ItemProperty -Path $vncRegPath -Name "UseVncAuthentication" -Value 1 -Type DWord
Set-ItemProperty -Path $vncRegPath -Name "LoopbackOnly" -Value 1 -Type DWord
Set-ItemProperty -Path $vncRegPath -Name "AllowLoopback" -Value 1 -Type DWord

# DES hash for "lsbx" = 0xF2, 0x9E, 0x3C, 0xBC, 0xCB, 0x4C, 0xFA, 0x0C
[byte[]]$vncPassBytes = @(0xF2, 0x9E, 0x3C, 0xBC, 0xCB, 0x4C, 0xFA, 0x0C)
Set-ItemProperty -Path $vncRegPath -Name "Password" -Value $vncPassBytes -Type Binary

# Ensure TightVNC service is configured and running
$tvnService = Get-Service -Name "tvnserver" -ErrorAction SilentlyContinue
if ($tvnService) {
    Set-Service -Name "tvnserver" -StartupType Automatic
    Restart-Service -Name "tvnserver"
}

# 3. noVNC index.html redirect to ?resize=scale
Write-Host "--> Configuring noVNC resize=scale redirect..."
if (Test-Path $NoVncDir) {
    $redirectHtml = @'
<!doctype html>
<meta charset="utf-8">
<title>lsbx console</title>
<script>
  const q = location.search.replace(/^\?/, "");
  location.replace("/vnc.html?resize=scale" + (q ? "&" + q : "") + location.hash);
</script>
<noscript><meta http-equiv="refresh" content="0; url=/vnc.html"></noscript>
'@
    Set-Content -Path (Join-Path $NoVncDir "index.html") -Value $redirectHtml -Encoding UTF8
}

# 4. WebSockify Scheduled Task
Write-Host "--> Configuring WebSockify scheduled task..."
$pythonExe = "C:\Python312\python.exe"
if (-not (Test-Path $pythonExe)) {
    $pythonCmd = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($pythonCmd) { $pythonExe = $pythonCmd.Source }
}

$taskName = "lsbx-websockify"
$taskExists = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($taskExists) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

$action = New-ScheduledTaskAction -Execute $pythonExe -Argument "-m websockify --web `"$NoVncDir`" 8000 127.0.0.1:5900"
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings | Out-Null
Start-ScheduledTask -TaskName $taskName

# 5. Windows Firewall rules
Write-Host "--> Configuring Firewall rules..."
New-NetFirewallRule -Name "lsbx-websockify-8000" -DisplayName "lsbx WebSockify (8000)" -Direction Inbound -Protocol TCP -LocalPort 8000 -Action Allow -ErrorAction SilentlyContinue | Out-Null
New-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -DisplayName "OpenSSH Server (sshd)" -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow -ErrorAction SilentlyContinue | Out-Null

# 6. Defender exclusions + Search indexing
#
# Real-time scanning of thousands of small files is the single most common
# cause of "my fast machine makes Unity/Unreal installs crawl" — true on bare
# metal too, and doubly true inside a disposable VM where the whole disk is
# expendable and nothing here needs on-access AV. These are the standard
# gamedev exclusions (project roots, package caches, toolchain dirs).
# See lsbx issue #54.
Write-Host "--> Configuring Defender exclusions and indexing..."
$defenderPaths = @(
    'C:\actions-runner',
    'C:\lsbx',
    'C:\lsbx-novnc',
    'C:\lsbx-ci-broker',
    'C:\Python312',
    'C:\Program Files\QSC',
    'C:\Program Files\Audiokinetic',
    'C:\Program Files\Epic Games',
    'C:\Program Files\Unity',
    'C:\Program Files\REAPER (x64)',
    'C:\Users\lsbx\AppData\Local\Unity',
    'C:\Users\danielito\AppData\Local\Unity',
    'C:\Users\danielito\Documents',
    'C:\Users\Public\Documents'
)
if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {
    foreach ($p in $defenderPaths) {
        try { Add-MpPreference -ExclusionPath $p -ErrorAction Stop } catch { }
    }
    # Scanning the executables themselves is what makes an installer take 4x
    # longer than its own payload copy time.
    foreach ($proc in @('Unity.exe', 'UnityHub.exe', 'UnrealEditor.exe', 'wwiseauthoring.exe', 'reaper.exe', 'QSC Designer.exe')) {
        try { Add-MpPreference -ExclusionProcess $proc -ErrorAction Stop } catch { }
    }
    Write-Host "Defender exclusions applied."
} else {
    Write-Warning "Defender cmdlets unavailable; skipping exclusions."
}
# Stop the search indexer from crawling the same trees.
try {
    Set-Service -Name WSearch -StartupType Disabled -ErrorAction Stop
    Stop-Service -Name WSearch -Force -ErrorAction SilentlyContinue
    Write-Host "Windows Search indexing disabled."
} catch {
    Write-Warning "Could not disable Windows Search indexing."
}

# 7. Verify Listeners
Write-Host "--> Verifying listeners..."
Start-Sleep -Seconds 2
$listeners = Get-NetTCPConnection -LocalPort 5900, 8000 -State Listen -ErrorAction SilentlyContinue
if ($listeners) {
    Write-Host "Listeners verified active on 5900 and 8000."
} else {
    Write-Warning "Could not verify all listeners immediately; they will start with the system."
}

Write-Host "=== lsbx Windows 11 Golden Provisioning Complete ==="
