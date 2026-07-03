@echo off
REM Build the StreamBeam host from the WSL repo, then launch it as a tray app.

REM Build artifacts (incl. the C/cmake opus build) go to a native NTFS folder,
REM NOT the \\wsl.localhost share -- UNC build dirs break cmake/MSVC and are slow.
set "CARGO_TARGET_DIR=C:\Users\joepm\streambeam-target"
set "HOST_EXE=%CARGO_TARGET_DIR%\release\streambeam-host.exe"

REM A still-running previous instance locks the .exe, so the linker can't
REM overwrite it ("Access is denied"). Close it first; ignore the error if it
REM wasn't running.
taskkill /IM streambeam-host.exe /F >nul 2>&1

REM pushd maps the \\wsl.localhost UNC path to a temporary drive letter so cargo
REM has a real working directory to read the source from.
pushd "\\wsl.localhost\Ubuntu\home\joep\dev\streambeam"
if errorlevel 1 (
    echo Could not reach the WSL repo path. Is WSL running? Is the distro named "Ubuntu"?
    pause
    exit /b 1
)

echo Building StreamBeam host (the first build is slow)...
cargo build --release -p streambeam-host
set "BUILD_ERR=%errorlevel%"
popd

if not "%BUILD_ERR%"=="0" (
    echo.
    echo Build failed.
    pause
    exit /b 1
)

REM Launch detached: the release build is a GUI app (no console). This window closes.
start "" "%HOST_EXE%"
