@echo off
REM Launch the StreamBeam host (tray + QR) from Windows against the WSL repo.
REM pushd maps the \\wsl.localhost UNC path to a temporary drive letter so
REM cargo has a real working directory.
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
