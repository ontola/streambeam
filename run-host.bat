@echo off
REM Launch the StreamBeam host (tray + QR) from Windows against the WSL repo.

REM Build artifacts (incl. the C/cmake opus build) go to a native NTFS folder,
REM NOT the \\wsl.localhost share -- UNC build dirs break cmake/MSVC and are slow.
set "CARGO_TARGET_DIR=C:\Users\joepm\streambeam-target"

REM pushd maps the \\wsl.localhost UNC path to a temporary drive letter so
REM cargo has a real working directory to read the source from.
pushd "\\wsl.localhost\Ubuntu\home\joep\dev\streambeam"
if errorlevel 1 (
    echo Could not reach the WSL repo path. Is WSL running? Is the distro named "Ubuntu"?
    pause
    exit /b 1
)
cargo run --release -p streambeam-host
popd
echo.
echo StreamBeam host exited.
pause
