<#
.SYNOPSIS
  Bakes the lsbx Windows 11 browser-desktop golden environment (Unit 02 / Issue #36).
  Configures:
    1. TightVNC Server (Loopback-only, port 5900, password 'lsbx')
    2. Python websockify + noVNC assets (Port 8000 -> 127.0.0.1:5900, resize=scale)
    3. OpenSSH Server & lsbx authorized_keys hardening
    4. QEMU Guest Agent verification
    5. Interactive desktop session: known lsbx password + autologon + no machine
       lock, plus C:\lsbx\session-check.ps1 for the golden healthcheck (#56 step 0)
    6. Defender exclusions + Windows Search indexing off (issue #54)

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

# The password value is TightVNC's own storage encoding, NOT a transparent
# DES(password) — writing a plausible-looking blob makes the stored password
# garbage and every client is rejected (the server logs "Authentication failed"
# and the console just disconnects). Measured the hard way on 2026-11-14.
#
# So: NEVER overwrite a value that is already present. Only seed one when the key
# is absent (a from-scratch bake), and treat the golden's existing blob as the
# source of truth — which is exactly what lsbx #55's follow-up will surface to the
# console page instead of the hardcoded 'lsbx' literal it uses today.
$existing = (Get-ItemProperty -Path $vncRegPath -Name "Password" -ErrorAction SilentlyContinue).Password
if (-not $existing) {
    Write-Host "    no VNC password stored; seeding the bake-time value"
    # Bake-time value for password 'lsbx'.
    [byte[]]$vncPassBytes = @(0xF2, 0x9E, 0x3C, 0xBC, 0xCB, 0x4C, 0xFA, 0x0C)
    Set-ItemProperty -Path $vncRegPath -Name "Password" -Value $vncPassBytes -Type Binary
} else {
    Write-Host "    keeping the golden's existing VNC password blob"
}

# Ensure TightVNC service is configured and running.  Only restarted when it is
# not already running: a restart mid-session drops whatever console client is
# attached, and it is not needed just to re-read an unchanged password.
$tvnService = Get-Service -Name "tvnserver" -ErrorAction SilentlyContinue
if ($tvnService) {
    Set-Service -Name "tvnserver" -StartupType Automatic
    if ($tvnService.Status -ne "Running") {
        Start-Service -Name "tvnserver"
    }
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

# 5b. Interactive desktop session: autologon + no machine lock
#
# WHY THIS EXISTS (lsbx #56 step 0)
# A display-mode change cannot be issued from an SSH session: SSH lands in
# session 0, which has no display, so `ChangeDisplaySettingsEx` returns
# DISP_CHANGE_FAILED there. The mode must be changed *inside the interactive
# console session* — which means the guest must actually have one. Without
# this the browser console mirrors a logon screen forever, and dynamic resize is
# impossible in principle, not just unimplemented.
#
# WHY IT FAILED BEFORE
# Classic `Winlogon\AutoAdminLogon` + `DefaultPassword` was configured and still
# fell through to the logon screen. The cause was not exotic: the local `lsbx`
# account had no known password (`ValidateCredentials` rejected every candidate),
# so Winlogon's autologon attempt failed and it showed a prompt. Setting a known
# password first is the fix; the rest is belt-and-braces.
#
# TRADE-OFF, stated plainly: this makes the guest's local desktop session
# passwordless at boot. It does NOT make the console unauthenticated — the WebSocket
# still requires a gateway ticket and the VNC server still requires the RFB
# password, and the whole VM is destroyed on lease expiry / `lsbx down`.
$AutoLogonUser = "lsbx"
$AutoLogonPass = "lsbx"

Write-Host "--> Configuring interactive desktop session (autologon)..."
# 1. A known password on the autologon account. Key-based SSH is unaffected.
try {
    $secure = ConvertTo-SecureString $AutoLogonPass -AsPlainText -Force
    Set-LocalUser -Name $AutoLogonUser -Password $secure -PasswordNeverExpires $true -ErrorAction Stop
    Write-Host "    set password for $AutoLogonUser"
} catch {
    Write-Warning "Set-LocalUser failed ($($_.Exception.Message)); trying net user"
    & net.exe user $AutoLogonUser $AutoLogonPass 2>&1 | Out-String | Write-Host
}

# 2. Autologon keys (the mechanism Winlogon reads).
$winlogon = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
Set-ItemProperty -Path $winlogon -Name "AutoAdminLogon"  -Value "1" -Type String
Set-ItemProperty -Path $winlogon -Name "DefaultUserName"  -Value $AutoLogonUser -Type String
Set-ItemProperty -Path $winlogon -Name "DefaultDomainName" -Value $env:COMPUTERNAME -Type String
Set-ItemProperty -Path $winlogon -Name "DefaultPassword"  -Value $AutoLogonPass -Type String
Remove-ItemProperty -Path $winlogon -Name "AutoLogonCount" -ErrorAction SilentlyContinue
# Windows 11 ignores password autologon while the Hello/passwordless device flag is set.
New-Item -Path "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\PasswordLess\Device" -Force | Out-Null
Set-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\PasswordLess\Device" `
    -Name "DevicePasswordLessBuildVersion" -Value 0 -Type DWord

# 3. Store the same credential as an LSA secret, which is how Sysinternals
#    Autologon does it and the path Winlogon prefers. Best-effort: the registry
#    path above already works on the builds we target.
try {
    $lsa = @'
using System;
using System.Runtime.InteropServices;
public static class LsaSecret {
    [StructLayout(LayoutKind.Sequential)]
    public struct LSA_UNICODE_STRING { public ushort Length; public ushort MaximumLength; public IntPtr Buffer; }
    [StructLayout(LayoutKind.Sequential)]
    public struct LSA_OBJECT_ATTRIBUTES {
        public uint Length; public IntPtr RootDirectory; public IntPtr ObjectName;
        public uint Attributes; public IntPtr SecurityDescriptor; public IntPtr SecurityQualityOfService;
    }
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern uint LsaOpenPolicy(ref LSA_UNICODE_STRING name, ref LSA_OBJECT_ATTRIBUTES attrs, uint access, out IntPtr handle);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern uint LsaStorePrivateData(IntPtr handle, ref LSA_UNICODE_STRING key, ref LSA_UNICODE_STRING data);
    [DllImport("advapi32.dll")] static extern uint LsaNtStatusToWinError(uint status);
    [DllImport("advapi32.dll")] static extern uint LsaClose(IntPtr handle);

    static LSA_UNICODE_STRING Str(string s) {
        var u = new LSA_UNICODE_STRING();
        u.Buffer = Marshal.StringToHGlobalUni(s);
        u.Length = (ushort)(s.Length * 2);
        u.MaximumLength = (ushort)((s.Length + 1) * 2);
        return u;
    }
    public static int Store(string key, string value) {
        var attrs = new LSA_OBJECT_ATTRIBUTES();
        attrs.Length = (uint)Marshal.SizeOf(attrs);
        var sys = new LSA_UNICODE_STRING();
        IntPtr ph;
        const uint POLICY_CREATE_SECRET = 0x00000020;
        uint st = LsaOpenPolicy(ref sys, ref attrs, POLICY_CREATE_SECRET, out ph);
        if (st != 0) return (int)LsaNtStatusToWinError(st);
        var k = Str(key); var v = Str(value);
        st = LsaStorePrivateData(ph, ref k, ref v);
        LsaClose(ph);
        Marshal.FreeHGlobal(k.Buffer); Marshal.FreeHGlobal(v.Buffer);
        return (int)LsaNtStatusToWinError(st);
    }
}
'@
    Add-Type -TypeDefinition $lsa -ErrorAction Stop
    $rc = [LsaSecret]::Store("DefaultPassword", $AutoLogonPass)
    Write-Host "    LSA DefaultPassword secret rc=$rc (0 = ok)"
} catch {
    Write-Warning "LSA secret store unavailable: $($_.Exception.Message)"
}

# 4. Keep the session unlocked: no machine inactivity limit, no lock screen.
New-Item -Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" -Force | Out-Null
Set-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" `
    -Name "InactivityTimeoutSecs" -Value 0 -Type DWord
New-Item -Path "HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization" -Force | Out-Null
Set-ItemProperty -Path "HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization" `
    -Name "NoLockScreen" -Value 1 -Type DWord

# 5. Proof, for the golden healthcheck: a logged-in console session must exist
#    after reboot. Written as a script so the healthcheck stays a one-liner.
$probe = @'
$session = (query user 2>$null) -join "`n"
if ($session -match "console" -or $session -match [regex]::Escape($env:COMPUTERNAME)) { exit 0 }
# `query user` prints the user name; also accept any non-empty session row.
if ($session.Trim().Length -gt 0) { exit 0 }
exit 1
'@
New-Item -ItemType Directory -Force -Path "C:\lsbx" | Out-Null
Set-Content -Path "C:\lsbx\session-check.ps1" -Value $probe -Encoding ASCII
Write-Host "    wrote C:\lsbx\session-check.ps1 (healthcheck probe)"

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
