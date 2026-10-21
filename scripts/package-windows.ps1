# Preserve the complete Flutter bundle: the executable alone is not runnable.
[CmdletBinding()]
param(
    [string]$Bundle = 'app/build/windows/x64/runner/Release',
    [string]$OutputDirectory = 'build/desktop'
)
$ErrorActionPreference = 'Stop'
$bundlePath = (Resolve-Path $Bundle).Path
foreach ($name in @('streambeam.exe', 'flutter_windows.dll', 'rust_lib_streambeam.dll', 'data/flutter_assets')) {
    if (!(Test-Path (Join-Path $bundlePath $name))) { throw "Missing Windows bundle component: $name" }
}
foreach ($name in @('streambeam-host.exe', 'streambeam-svc.exe')) {
    $source = Join-Path 'target/release' $name
    if (!(Test-Path $source)) { throw "Missing secure-desktop helper: $source" }
    Copy-Item $source $bundlePath -Force
}
# Flutter's Windows build does not bundle the Visual C++ runtime. Include the
# redistributable DLLs beside our executables so the ZIP works on a clean PC.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) { throw 'Visual Studio C++ toolchain was not found' }
$runtime = Get-ChildItem (Join-Path $vs 'VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT') -Directory | Sort-Object FullName -Descending | Select-Object -First 1
if (!$runtime) { throw 'Visual C++ redistributable runtime was not found' }
Copy-Item (Join-Path $runtime.FullName '*.dll') $bundlePath -Force
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
Compress-Archive -Path (Join-Path $bundlePath '*') -DestinationPath (Join-Path $OutputDirectory 'streambeam-windows-x64.zip') -Force
