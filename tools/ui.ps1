<#
Minimal screenshot / click helper for driving the Atom Fishing 2 client window.

The game is a normal Win32 window, so we can capture it and inject clicks with
user32 instead of needing a full desktop-automation stack.

    powershell -File tools\ui.ps1 -Action info
    powershell -File tools\ui.ps1 -Action shot  -Out shot.png
    powershell -File tools\ui.ps1 -Action click -X 640 -Y 400

X and Y are relative to the window's CLIENT area (top-left = 0,0), which is
what you measure off the screenshot.

Keep this file ASCII: Windows PowerShell 5.1 reads it as ANSI, so anything
fancier (em dashes and the like) turns into a parser error.
#>
param(
    [Parameter(Mandatory = $true)][ValidateSet('info', 'shot', 'click', 'focus')][string]$Action,
    [int]$X = -1,
    [int]$Y = -1,
    [string]$Out = 'shot.png',
    [string]$ProcessName = 'AFCGame'
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public struct RECT { public int Left, Top, Right, Bottom; }
public struct POINT { public int X, Y; }

[StructLayout(LayoutKind.Sequential)]
public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }

[StructLayout(LayoutKind.Sequential)]
public struct INPUT { public uint type; public MOUSEINPUT mi; }

public static class Win32Ui
{
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] private static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

    private const uint LEFTDOWN = 0x0002;
    private const uint LEFTUP   = 0x0004;
    private const uint KEYUP    = 0x0002;
    private const byte VK_MENU  = 0x12;   // Alt

    [DllImport("user32.dll", SetLastError=true)] private static extern uint SendInput(uint n, INPUT[] p, int cb);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int i);

    private const uint MOUSE_MOVE = 0x0001;
    private const uint MOUSE_LDOWN = 0x0002;
    private const uint MOUSE_LUP = 0x0004;
    private const uint MOUSE_ABSOLUTE = 0x8000;

    private static INPUT MouseInput(uint flags, int x, int y)
    {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        INPUT i = new INPUT();
        i.type = 0;                                   // INPUT_MOUSE
        i.mi.dx = (int)((x * 65535.0) / (w - 1));     // absolute coords are 0..65535
        i.mi.dy = (int)((y * 65535.0) / (h - 1));
        i.mi.dwFlags = flags | MOUSE_ABSOLUTE;
        return i;
    }

    // SendInput with absolute coordinates, as a real mouse driver produces.
    // mouse_event clicks worked only some of the time; the cause is unknown.
    public static uint MoveTo(int x, int y)
    {
        return SendInput(1, new INPUT[] { MouseInput(MOUSE_MOVE, x, y) }, Marshal.SizeOf(typeof(INPUT)));
    }

    public static uint ClickAt(int x, int y)
    {
        INPUT[] seq = new INPUT[] {
            MouseInput(MOUSE_MOVE | MOUSE_LDOWN, x, y),
            MouseInput(MOUSE_MOVE | MOUSE_LUP, x, y)
        };
        return SendInput((uint)seq.Length, seq, Marshal.SizeOf(typeof(INPUT)));
    }

    // Windows refuses SetForegroundWindow from a process that does not own the
    // foreground. Tapping Alt makes this process the most recent input source,
    // which lifts that lock for the next call.
    public static void NudgeForegroundLock()
    {
        keybd_event(VK_MENU, 0, 0, UIntPtr.Zero);
        keybd_event(VK_MENU, 0, KEYUP, UIntPtr.Zero);
    }
}
'@

function Get-GameWindow {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
         Where-Object { $_.MainWindowHandle -ne 0 } |
         Select-Object -First 1
    if (-not $p) { throw "No '$ProcessName' process with a window found." }
    return $p
}

function Get-ClientOriginAndSize($hWnd) {
    $client = New-Object RECT
    [void][Win32Ui]::GetClientRect($hWnd, [ref]$client)
    $origin = New-Object POINT
    [void][Win32Ui]::ClientToScreen($hWnd, [ref]$origin)
    return @{
        X      = $origin.X
        Y      = $origin.Y
        Width  = $client.Right - $client.Left
        Height = $client.Bottom - $client.Top
    }
}

function Restore-GameWindow($proc) {
    # A full-screen Unity window minimises itself when it loses focus, which
    # parks it at -32000,-32000 with a 0x0 client area. Restoring it from a
    # background process takes a few tries.
    #
    # We also insist the window is actually in the FOREGROUND: capturing is done
    # by screen coordinates, so if something else is on top we would silently
    # screenshot (or click into) the wrong application.
    $hWnd = $proc.MainWindowHandle
    $shell = New-Object -ComObject WScript.Shell
    for ($i = 0; $i -lt 12; $i++) {
        $area = Get-ClientOriginAndSize $hWnd
        $restored = ($area.Width -gt 0 -and $area.X -gt -10000)
        $front = ([Win32Ui]::GetForegroundWindow() -eq $hWnd)
        if ($restored -and $front) { return $area }

        [Win32Ui]::NudgeForegroundLock()
        [void][Win32Ui]::ShowWindow($hWnd, 9)      # SW_RESTORE
        [void][Win32Ui]::SetForegroundWindow($hWnd)
        try { [void]$shell.AppActivate($proc.Id) } catch { }
        Start-Sleep -Milliseconds 500
    }
    return Get-ClientOriginAndSize $hWnd
}

function Assert-Foreground($proc) {
    if ([Win32Ui]::GetForegroundWindow() -ne $proc.MainWindowHandle) {
        throw ("'$($proc.ProcessName)' is not the foreground window; refusing to capture or click the wrong app. " +
               "Bring the game to the front yourself, or run -Action focus if stealing focus is acceptable right now.")
    }
}

$proc = Get-GameWindow
$hWnd = $proc.MainWindowHandle

# Only actions that genuinely need input focus may take it. 'shot' must never
# yank the window to the front: that interrupts whoever is using the machine.
if ($Action -in @('click', 'focus')) {
    $area = Restore-GameWindow $proc
} else {
    $area = Get-ClientOriginAndSize $hWnd
}

switch ($Action) {
    'info' {
        "process  : $($proc.ProcessName) PID $($proc.Id)"
        "title    : $($proc.MainWindowTitle)"
        "minimised: $([Win32Ui]::IsIconic($hWnd))"
        "client   : origin $($area.X),$($area.Y)  size $($area.Width) x $($area.Height)"
    }

    'focus' {
        "focused $($proc.ProcessName): origin $($area.X),$($area.Y)  size $($area.Width) x $($area.Height)"
    }

    'shot' {
        if ($area.Width -le 0 -or $area.Height -le 0) { throw "Window has no client area (still minimised?)." }
        Assert-Foreground $proc
        $bmp = New-Object System.Drawing.Bitmap $area.Width, $area.Height
        $gfx = [System.Drawing.Graphics]::FromImage($bmp)
        $gfx.CopyFromScreen($area.X, $area.Y, 0, 0, $bmp.Size)
        $gfx.Dispose()
        $full = [System.IO.Path]::GetFullPath($Out)
        $bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        "saved $full  size $($area.Width) x $($area.Height)"
    }

    'click' {
        if ($X -lt 0 -or $Y -lt 0) { throw "Pass -X and -Y (client-area coordinates)." }
        if ($area.Width -le 0 -or $area.X -lt -10000) { throw "Window still minimised at $($area.X),$($area.Y); not clicking." }
        Assert-Foreground $proc
        $sx = $area.X + $X
        $sy = $area.Y + $Y
        # Always re-assert focus and let the game run a few frames first. Being
        # the foreground window by the API is not the same as Unity having
        # processed focus: without this settle the click is silently dropped.
        [void][Win32Ui]::ShowWindow($hWnd, 9)
        [void][Win32Ui]::SetForegroundWindow($hWnd)
        try { [void](New-Object -ComObject WScript.Shell).AppActivate($proc.Id) } catch { }
        Start-Sleep -Milliseconds 900
        # Unity updates its hovered UI element from mouse MOVEMENT, so park the
        # cursor nearby, let a few frames run, then slide onto the target before
        # pressing. Clicking straight after a warp is swallowed.
        [void][Win32Ui]::MoveTo($sx - 60, $sy - 40)
        Start-Sleep -Milliseconds 300
        [void][Win32Ui]::MoveTo($sx, $sy)
        Start-Sleep -Milliseconds 500
        $sent = [Win32Ui]::ClickAt($sx, $sy)
        Start-Sleep -Milliseconds 300
        "clicked client ($X,$Y) -> screen ($sx,$sy), $sent input events accepted"
    }
}
