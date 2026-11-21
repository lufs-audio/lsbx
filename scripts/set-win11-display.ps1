<#
.SYNOPSIS
  Applies one approved display preset to the active Windows console session.

.DESCRIPTION
  Windows OpenSSH commands run in session 0, where display APIs cannot control
  the interactive desktop. From session 0, this helper registers a short-lived
  interactive scheduled task for the already logged-in user and relaunches
  itself on that user's desktop. The in-session copy applies the selected QXL
  mode with ChangeDisplaySettingsExW.

  The command surface is deliberately narrow: callers select a fixed preset;
  there is no width/height pair and no arbitrary command or executable input.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File C:\lsbx\set-win11-display.ps1 -Preset 1920x1080
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet(
        '1024x768',
        '1280x800',
        '1440x900',
        '1600x900',
        '1920x1080',
        '2560x1440',
        '2560x1600',
        '3840x2160'
    )]
    [string]$Preset
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$parts = $Preset -split 'x', 2
$requestedWidth = [int]$parts[0]
$requestedHeight = [int]$parts[1]

function Stop-LsbxDisplay {
    param(
        [Parameter(Mandatory = $true)][string]$Message,
        [Parameter(Mandatory = $true)][int]$Code
    )
    [Console]::Error.WriteLine("set-win11-display: $Message")
    exit $Code
}

if (-not ('LsbxDisplay.Native' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace LsbxDisplay
{
    public static class Native
    {
        private const int ENUM_CURRENT_SETTINGS = -1;
        private const int DM_BITSPERPEL = 0x00040000;
        private const int DM_PELSWIDTH = 0x00080000;
        private const int DM_PELSHEIGHT = 0x00100000;
        private const int DM_DISPLAYFREQUENCY = 0x00400000;
        private const int DISP_CHANGE_SUCCESSFUL = 0;
        private const int CDS_TEST = 0x00000002;

        [StructLayout(LayoutKind.Sequential)]
        private struct POINTL
        {
            public int x;
            public int y;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct DEVMODE
        {
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
            public string dmDeviceName;
            public short dmSpecVersion;
            public short dmDriverVersion;
            public short dmSize;
            public short dmDriverExtra;
            public int dmFields;
            public POINTL dmPosition;
            public int dmDisplayOrientation;
            public int dmDisplayFixedOutput;
            public short dmColor;
            public short dmDuplex;
            public short dmYResolution;
            public short dmTTOption;
            public short dmCollate;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
            public string dmFormName;
            public short dmLogPixels;
            public int dmBitsPerPel;
            public int dmPelsWidth;
            public int dmPelsHeight;
            public int dmDisplayFlags;
            public int dmDisplayFrequency;
            public int dmICMMethod;
            public int dmICMIntent;
            public int dmMediaType;
            public int dmDitherType;
            public int dmReserved1;
            public int dmReserved2;
            public int dmPanningWidth;
            public int dmPanningHeight;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern bool EnumDisplaySettingsW(string deviceName, int modeNum, ref DEVMODE devMode);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int ChangeDisplaySettingsExW(string deviceName, ref DEVMODE devMode, IntPtr hwnd, int flags, IntPtr lParam);

        [DllImport("kernel32.dll")]
        public static extern uint WTSGetActiveConsoleSessionId();

        private static DEVMODE NewDevMode()
        {
            DEVMODE mode = new DEVMODE();
            mode.dmDeviceName = new string('\0', 32);
            mode.dmFormName = new string('\0', 32);
            mode.dmSize = (short)Marshal.SizeOf(typeof(DEVMODE));
            return mode;
        }

        public static bool GetCurrentMode(out int width, out int height, out string error)
        {
            DEVMODE mode = NewDevMode();
            if (!EnumDisplaySettingsW(null, ENUM_CURRENT_SETTINGS, ref mode))
            {
                width = 0;
                height = 0;
                error = "EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS) failed";
                return false;
            }
            width = mode.dmPelsWidth;
            height = mode.dmPelsHeight;
            error = null;
            return true;
        }

        public static bool SetMode(int width, int height, out string error)
        {
            DEVMODE current = NewDevMode();
            if (!EnumDisplaySettingsW(null, ENUM_CURRENT_SETTINGS, ref current))
            {
                error = "EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS) failed";
                return false;
            }

            DEVMODE selected = NewDevMode();
            bool found = false;
            for (int index = 0; ; index++)
            {
                DEVMODE candidate = NewDevMode();
                if (!EnumDisplaySettingsW(null, index, ref candidate))
                    break;
                if (candidate.dmPelsWidth == width && candidate.dmPelsHeight == height)
                {
                    if (!found || candidate.dmBitsPerPel == current.dmBitsPerPel)
                    {
                        selected = candidate;
                        found = true;
                    }
                    if (candidate.dmBitsPerPel == current.dmBitsPerPel &&
                        candidate.dmDisplayFrequency == current.dmDisplayFrequency)
                        break;
                }
            }

            if (!found)
            {
                error = String.Format("QXL does not advertise preset {0}x{1}", width, height);
                return false;
            }

            selected.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
            if (selected.dmBitsPerPel > 0)
                selected.dmFields |= DM_BITSPERPEL;
            if (selected.dmDisplayFrequency > 0)
                selected.dmFields |= DM_DISPLAYFREQUENCY;

            int test = ChangeDisplaySettingsExW(null, ref selected, IntPtr.Zero, CDS_TEST, IntPtr.Zero);
            if (test != DISP_CHANGE_SUCCESSFUL)
            {
                error = String.Format("ChangeDisplaySettingsExW test rejected {0}x{1}: {2}", width, height, DescribeDisplayChange(test));
                return false;
            }

            int apply = ChangeDisplaySettingsExW(null, ref selected, IntPtr.Zero, 0, IntPtr.Zero);
            if (apply != DISP_CHANGE_SUCCESSFUL)
            {
                error = String.Format("ChangeDisplaySettingsExW failed for {0}x{1}: {2}", width, height, DescribeDisplayChange(apply));
                return false;
            }

            error = null;
            return true;
        }

        private static string DescribeDisplayChange(int result)
        {
            switch (result)
            {
                case 1: return "DISP_CHANGE_RESTART";
                case -1: return "DISP_CHANGE_FAILED";
                case -2: return "DISP_CHANGE_BADMODE";
                case -3: return "DISP_CHANGE_NOTUPDATED";
                case -4: return "DISP_CHANGE_BADFLAGS";
                case -5: return "DISP_CHANGE_BADPARAM";
                case -6: return "DISP_CHANGE_BADDUALVIEW";
                default: return result.ToString();
            }
        }
    }
}
'@
}

$currentSession = [uint32]([System.Diagnostics.Process]::GetCurrentProcess().SessionId)
$consoleSession = [LsbxDisplay.Native]::WTSGetActiveConsoleSessionId()
if ($consoleSession -eq [uint32]::MaxValue) {
    Stop-LsbxDisplay -Message 'Windows reports no active console session.' -Code 20
}

if ($currentSession -ne $consoleSession) {
    $explorer = Get-Process explorer -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -eq $consoleSession } |
        Select-Object -First 1
    if (-not $explorer) {
        Stop-LsbxDisplay -Message "No Explorer process is running in active console session $consoleSession." -Code 21
    }

    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $explorerProcess = Get-CimInstance Win32_Process -Filter "ProcessId = $($explorer.Id)"
    $explorerOwner = Invoke-CimMethod -InputObject $explorerProcess -MethodName GetOwner
    $consoleIdentity = "$($explorerOwner.Domain)\$($explorerOwner.User)"
    if ($identity -ine $consoleIdentity) {
        Stop-LsbxDisplay -Message "SSH identity $identity is not the interactive console identity $consoleIdentity." -Code 22
    }

    $taskName = 'LsbxDisplayPreset-' + [guid]::NewGuid().ToString('N')
    $powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $quotedScript = '"' + $PSCommandPath.Replace('"', '""') + '"'
    $arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $quotedScript -Preset $Preset"

    try {
        $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments
        $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Highest
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName

        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        do {
            Start-Sleep -Milliseconds 200
            $task = Get-ScheduledTask -TaskName $taskName
        } while ($task.State -eq 'Running' -and [DateTime]::UtcNow -lt $deadline)

        if ($task.State -eq 'Running') {
            Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
            Stop-LsbxDisplay -Message 'Interactive display helper timed out after 30 seconds.' -Code 124
        }

        $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName
        if ($taskInfo.LastTaskResult -ne 0) {
            Stop-LsbxDisplay -Message "Interactive display helper exited $($taskInfo.LastTaskResult)." -Code 23
        }
    } finally {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $video = Get-CimInstance Win32_VideoController |
            Where-Object { $_.CurrentHorizontalResolution -and $_.CurrentVerticalResolution } |
            Select-Object -First 1
        if ($video -and
            $video.CurrentHorizontalResolution -eq $requestedWidth -and
            $video.CurrentVerticalResolution -eq $requestedHeight) {
            Write-Output "Applied preset $Preset in console session $consoleSession; current mode is $($video.CurrentHorizontalResolution)x$($video.CurrentVerticalResolution)."
            exit 0
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)

    Stop-LsbxDisplay -Message "Interactive helper exited successfully, but Win32_VideoController did not report $Preset within 5 seconds." -Code 24
}

$applyError = $null
if (-not [LsbxDisplay.Native]::SetMode($requestedWidth, $requestedHeight, [ref]$applyError)) {
    Stop-LsbxDisplay -Message $applyError -Code 25
}

Start-Sleep -Milliseconds 250
$currentWidth = 0
$currentHeight = 0
$queryError = $null
if (-not [LsbxDisplay.Native]::GetCurrentMode([ref]$currentWidth, [ref]$currentHeight, [ref]$queryError)) {
    Stop-LsbxDisplay -Message $queryError -Code 26
}
if ($currentWidth -ne $requestedWidth -or $currentHeight -ne $requestedHeight) {
    Stop-LsbxDisplay -Message "Display API returned success, but the console mode is ${currentWidth}x${currentHeight}, not $Preset." -Code 27
}

exit 0
