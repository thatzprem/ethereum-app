# Asks the Dabao signer to sign the message "Hello" and shows the answer.
# With the touch-to-confirm firmware (0.2.0+), touch pad PB1 within 15 seconds to approve.
# Usage: pwsh -File dabao-sign-hello.ps1
$send = Join-Path $PSScriptRoot 'dabao-hid-send.ps1'
$rid = -join ((1..16) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })
# ERC-4527 eth-sign-request: request id, message "Hello", type 3 (EIP-191), path m/44'/60'/0'/0/0, origin
$hex = "01a501d82550$rid" + '024548656c6c6f030305' + 'd90130a1018a182cf5183cf500f500f400f4' + '076a6574685f636c69656e74'

Write-Host ''
Write-Host '>>> Request sent. TOUCH PAD PB1 NOW (you have 15 seconds) <<<' -ForegroundColor Yellow
Write-Host ''
$out = & pwsh -NoProfile -File $send -Hex $hex -TimeoutMs 25000 2>&1
$type = ($out | Where-Object { $_ -match '^REPLY_TYPE ' }) -replace '^REPLY_TYPE ', ''
$reply = ($out | Where-Object { $_ -match '^REPLY ' }) -replace '^REPLY ', ''
$ms = ($out | Where-Object { $_ -match '^ELAPSED_MS ' }) -replace '^ELAPSED_MS ', ''
if (-not $reply) { $out; exit 1 }

if ($type -eq '1') {
    Write-Host "APPROVED and signed after $ms ms" -ForegroundColor Green
    Write-Host ('Signature: 0x' + $reply.Substring($reply.Length - 130))
    Write-Host 'Message:   Hello'
} else {
    $bytes = for ($i = 0; $i -lt $reply.Length; $i += 2) { [Convert]::ToByte($reply.Substring($i, 2), 16) }
    $text = -join ($bytes | ForEach-Object { if ($_ -ge 32 -and $_ -lt 127) { [char]$_ } else { '.' } })
    Write-Host "NOT signed after $ms ms" -ForegroundColor Red
    Write-Host ('Board said: ' + ($text -replace '^[^a-zA-Z]*x?.?', ''))
}
