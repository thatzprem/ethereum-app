# Flashes firmware onto the Dabao and starts it.
# Usage: pwsh -File dabao-flash.ps1 -Dir <folder with loader.uf2, xous.uf2, apps.uf2> [-AppsOnly]
#
# If the board is running signer firmware 0.3.0 or later, it is asked to restart into its bootloader
# by itself. Otherwise put it in the bootloader first: hold PROG, tap RESET, release PROG.
param([string]$Dir = '.', [switch]$AppsOnly)

$src = (Resolve-Path $Dir -ErrorAction Stop).Path
$send = Join-Path $PSScriptRoot 'dabao-hid-send.ps1'
$files = if ($AppsOnly) { @('apps.uf2') } else { @('loader.uf2', 'xous.uf2', 'apps.uf2') }
foreach ($f in $files) { if (-not (Test-Path (Join-Path $src $f))) { Write-Host "Missing $f in $src" -ForegroundColor Red; exit 1 } }

function Get-BaoDrive { Get-Volume | Where-Object { $_.FileSystemLabel -eq 'BAOCHIP' } | Select-Object -First 1 }
function Test-Signer { [bool](Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'HID\VID_1D50&PID_6197*' }) }

if (-not (Get-BaoDrive)) {
    if (Test-Signer) {
        Write-Host 'Asking the signer to restart into the bootloader...'
        & pwsh -NoProfile -File $send -Hex 02a10104 -TimeoutMs 5000 | Out-Null   # CBOR {1: 4} = reboot
    }
    $deadline = (Get-Date).AddSeconds(15)
    while (-not (Get-BaoDrive) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
}
$vol = Get-BaoDrive
if (-not $vol) { Write-Host 'No BAOCHIP drive. Hold PROG, tap RESET, release PROG, then run this again.' -ForegroundColor Red; exit 1 }
$drive = "$($vol.DriveLetter):\"

foreach ($f in $files) {
    Copy-Item (Join-Path $src $f) $drive -Force -ErrorAction Stop
    Write-Host "copied $f"
    Start-Sleep -Milliseconds 1500
}

# Tell the bootloader to start the program (same as pressing PROG).
$port = [System.IO.Ports.SerialPort]::GetPortNames() | Select-Object -Last 1
if ($port -and (Get-BaoDrive)) {
    $p = New-Object System.IO.Ports.SerialPort $port, 115200, 'None', 8, 'One'
    $p.DtrEnable = $true; $p.RtsEnable = $true; $p.WriteTimeout = 2000
    try { $p.Open(); Start-Sleep -Milliseconds 400; $p.Write("boot`r"); Start-Sleep -Milliseconds 1000 } catch { } finally { try { if ($p.IsOpen) { $p.Close() } } catch { } }
}

$deadline = (Get-Date).AddSeconds(25)
while (-not (Test-Signer) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
if (-not (Test-Signer)) { Write-Host 'The signer did not come up. Press PROG once to start it.' -ForegroundColor Red; exit 1 }
Start-Sleep -Seconds 2
$out = & pwsh -NoProfile -File $send -Hex 02a10102 -TimeoutMs 5000   # get-version
$reply = ($out | Where-Object { $_ -match '^REPLY ' }) -replace '^REPLY ', ''
if ($reply) {
    $bytes = for ($i = 0; $i -lt $reply.Length; $i += 2) { [Convert]::ToByte($reply.Substring($i, 2), 16) }
    $ver = (-join ($bytes | ForEach-Object { if ($_ -ge 32 -and $_ -lt 127) { [char]$_ } else { ' ' } })) -replace '^.*?(\d+\.\d+\.\d+).*$', '$1'
    Write-Host "Signer is running, version $ver" -ForegroundColor Green
} else { $out }
