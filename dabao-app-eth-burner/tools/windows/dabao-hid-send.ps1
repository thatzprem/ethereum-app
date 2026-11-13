# Sends one raw request envelope to the Dabao Ethereum signer over USB HID and prints the reply as hex.
# Usage: pwsh -File dabao-hid-send.ps1 -Hex <type byte + CBOR message, as hex>
# Examples: -Hex 02a10102 (get-version), -Hex 02a10103 (probe-confirm), -Hex 02a10104 (reboot)
# Uses only Windows' built-in HID support; nothing to install.
param([Parameter(Mandatory = $true)][string]$Hex, [int]$TimeoutMs = 20000)

Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class DabaoHid {
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
if (-not $dev) { Write-Output "ERROR Dabao HID device not found (is the board plugged in and in signer mode?)"; exit 1 }
$path = '\\?\' + ($dev.InstanceId -replace '\\', '#') + '#{4d1e55b2-f16f-11cf-88cb-001111000030}'

$Hex = $Hex -replace '^0x', ''
$body = [byte[]]::new($Hex.Length / 2)
for ($i = 0; $i -lt $body.Length; $i++) { $body[$i] = [Convert]::ToByte($Hex.Substring($i * 2, 2), 16) }

# Envelope: 4-byte big-endian length, then type byte + message; cut into 64-byte reports (report id 0).
$len = $body.Length
$hdr = [byte[]]@((($len -shr 24) -band 255), (($len -shr 16) -band 255), (($len -shr 8) -band 255), ($len -band 255))
$frame = [byte[]]($hdr + $body)

$fs = [DabaoHid]::Open($path)
try {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    for ($off = 0; $off -lt $frame.Length; $off += 64) {
        $rep = New-Object byte[] 65
        [Array]::Copy($frame, $off, $rep, 1, [Math]::Min(64, $frame.Length - $off))
        $fs.Write($rep, 0, 65); $fs.Flush()
    }
    $buf = New-Object System.Collections.Generic.List[byte]
    $need = -1
    while ($need -lt 0 -or $buf.Count -lt 4 + $need) {
        $rep = New-Object byte[] 65
        $task = $fs.ReadAsync($rep, 0, 65)
        if (-not $task.Wait($TimeoutMs)) { Write-Output "ERROR timeout waiting for the board to reply"; exit 2 }
        $buf.AddRange([byte[]]$rep[1..64])
        if ($need -lt 0 -and $buf.Count -ge 4) { $need = ($buf[0] -shl 24) -bor ($buf[1] -shl 16) -bor ($buf[2] -shl 8) -bor $buf[3] }
    }
    $reply = [byte[]]$buf.GetRange(4, $need).ToArray()
    Write-Output "ELAPSED_MS $($sw.ElapsedMilliseconds)"
    Write-Output "REPLY_TYPE $($reply[0])"
    Write-Output ("REPLY " + (($reply | ForEach-Object { $_.ToString('x2') }) -join ''))
} finally { $fs.Close() }
