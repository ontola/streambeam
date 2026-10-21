# Build an unsigned Store submission package. Microsoft signs certified MSIXs;
# this file is not a signed sideloading installer.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$IdentityName,
    [Parameter(Mandatory)][string]$Publisher,
    [Parameter(Mandatory)][string]$PublisherDisplayName,
    [Parameter(Mandatory)][string]$Version,
    [string]$Bundle = 'app/build/windows/x64/runner/Release',
    [string]$OutputDirectory = 'build/windows-store'
)
$ErrorActionPreference = 'Stop'
if ($IdentityName -notmatch '^[A-Za-z0-9.-]{3,50}$' -or $IdentityName -eq 'REQUIRED') { throw 'Exact Package/Identity/Name from Partner Center is required' }
if (!$Publisher.StartsWith('CN=') -or [string]::IsNullOrWhiteSpace($PublisherDisplayName)) { throw 'Exact Publisher and PublisherDisplayName from Partner Center are required' }
if ($Version -notmatch '^\d+\.\d+\.\d+\.0$') { throw 'Store version must be four numeric parts with the last part zero' }
$parts = $Version.Split('.') | ForEach-Object { [int]$_ }
if (($parts | Where-Object { $_ -gt 65535 }).Count -or $Version -eq '0.0.0.0') { throw 'Invalid Store version' }
foreach ($file in @('streambeam.exe', 'flutter_windows.dll', 'rust_lib_streambeam.dll', 'msvcp140.dll', 'vcruntime140.dll')) {
    if (!(Test-Path (Join-Path $Bundle $file))) { throw "Missing packaged runtime: $file; run package-windows.ps1 first" }
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$stage = Join-Path $OutputDirectory ('stage-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $stage | Out-Null
try {
    Copy-Item (Join-Path $Bundle '*') $stage -Recurse
    # The opt-in SYSTEM service needs a dedicated packaged-service design and
    # Store review. Omitting its executable hides the existing service toggle.
    foreach ($file in @('streambeam-svc.exe', 'streambeam-host.exe')) {
        Remove-Item (Join-Path $stage $file) -ErrorAction SilentlyContinue
    }
    Get-ChildItem $stage -Filter '*.pdb' -Recurse | Remove-Item
    Copy-Item 'packaging/windows/Assets' $stage -Recurse
    [xml]$manifest = Get-Content 'packaging/windows/AppxManifest.xml'
    $manifest.Package.Identity.Name = $IdentityName
    $manifest.Package.Identity.Publisher = $Publisher
    $manifest.Package.Identity.Version = $Version
    $manifest.Package.Properties.PublisherDisplayName = $PublisherDisplayName
    $manifest.Save((Join-Path (Resolve-Path $stage).Path 'AppxManifest.xml'))
    $sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
    $makeappx = Get-ChildItem "$sdk/*/x64/makeappx.exe" | Sort-Object FullName -Descending | Select-Object -First 1
    if (!$makeappx) { throw 'Windows SDK MakeAppx.exe is required' }
    $output = Join-Path (Resolve-Path $OutputDirectory).Path "StreamBeam-$Version-x64.msix"
    & $makeappx.FullName pack /d $stage /p $output /o
    if ($LASTEXITCODE -ne 0) { throw "MakeAppx failed: $LASTEXITCODE" }
    Write-Output "Created Store submission package: $output"
} finally {
    Remove-Item $stage -Recurse -Force
}
