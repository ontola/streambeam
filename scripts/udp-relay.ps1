param(
    [string]$ListenIp = "0.0.0.0",
    [int]$ListenPort = 47989,
    [string]$TargetIp = "172.25.64.180",
    [int]$TargetPort = 47989
)

$ErrorActionPreference = "Stop"

$listenEndpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Parse($ListenIp), $ListenPort)
$targetEndpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Parse($TargetIp), $TargetPort)
$anyEndpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)

$public = [System.Net.Sockets.UdpClient]::new($listenEndpoint)
$wsl = [System.Net.Sockets.UdpClient]::new(0)
$wsl.Connect($targetEndpoint)

$clientEndpoint = $null
Write-Host "UDP relay listening on ${ListenIp}:${ListenPort}, forwarding to ${TargetIp}:${TargetPort}"

try {
    while ($true) {
        if ($public.Available -gt 0) {
            $remote = $anyEndpoint
            $bytes = $public.Receive([ref]$remote)
            $clientEndpoint = $remote
            [void]$wsl.Send($bytes, $bytes.Length)
        }

        if ($wsl.Available -gt 0) {
            $remote = $anyEndpoint
            $bytes = $wsl.Receive([ref]$remote)
            if ($null -ne $clientEndpoint) {
                [void]$public.Send($bytes, $bytes.Length, $clientEndpoint)
            }
        }

        Start-Sleep -Milliseconds 1
    }
} finally {
    $public.Close()
    $wsl.Close()
}
