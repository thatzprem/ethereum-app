# Live meter for the Dabao signer's touch pad (firmware 0.2.1+).
# Shows what the touch sensor reads, twice a second, so you can find the pad and see how strongly a touch registers.
# Usage: pwsh -File dabao-touch-meter.ps1 [-Seconds 60]      (Ctrl+C stops it early)
param([int]$Seconds = 60)

Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class DabaoHidMeter {
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
    public static FileStream Open(string path) {
        var h = CreateFile(path, 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        if (h.IsInvalid) throw new IOException("CreateFile failed, win32 error " + Marshal.GetLastWin32Error());
        return new FileStream(h, FileAccess.ReadWrite, 65, false);
    }
}
"@

$dev = Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'HID\VID_1D50&PID_6197*' } | Select-Object -First 1
if (-not $dev) { Write-Host 'Dabao HID device not found (is the board plugged in and in signer mode?)' -ForegroundColor Red; exit 1 }
$path = '\\?\' + ($dev.InstanceId -replace '\\', '#') + '#{4d1e55b2-f16f-11cf-88cb-001111000030}'
$fs = [DabaoHidMeter]::Open($path)

function Probe {
    # envelope: length 4, type 0x02, CBOR {1: 3} (probe-confirm)
    $rep = New-Object byte[] 65
    [byte[]](0, 0, 0, 4, 2, 0xA1, 1, 3) | ForEach-Object -Begin { $i = 1 } -Process { $rep[$i++] = $_ }
    $fs.Write($rep, 0, 65); $fs.Flush()
    $in = New-Object byte[] 65
    $task = $fs.ReadAsync($in, 0, 65)
    if (-not $task.Wait(5000)) { throw 'timeout waiting for the board' }
    $len = ($in[1] -shl 24) -bor ($in[2] -shl 16) -bor ($in[3] -shl 8) -bor $in[4]
    $body = $in[5..(4 + $len)]
    -join ($body | ForEach-Object { if ($_ -ge 32 -and $_ -lt 127) { [char]$_ } else { ' ' } })
}

Write-Host "Touch meter running for $Seconds s. Touch pads one at a time and watch the number." -ForegroundColor Yellow
Write-Host 'Untouched is the baseline; a real touch should make it jump well above that.'
$idle = $null
$end = (Get-Date).AddSeconds($Seconds)
try {
    while ((Get-Date) -lt $end) {
        $text = Probe
        if ($text -match '(\d+) (\d+)\s*$') {
            $lo = [int]$Matches[1]; $hi = [int]$Matches[2]
            if ($null -eq $idle) { $idle = $hi }
            $bar = '#' * [Math]::Min(60, [Math]::Max(0, [int](($hi - $idle) / 4)))
            $mark = if ($hi -gt $idle + [Math]::Max(5, [int]($idle / 4))) { '  <-- TOUCH' } else { '' }
            Write-Host ("{0,6} .. {1,6}  {2}{3}" -f $lo, $hi, $bar, $mark)
        } else {
            Write-Host "board said: $($text.Trim())" -ForegroundColor Red
            break
        }
    }
} finally { $fs.Close() }
