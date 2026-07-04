@echo off
REM Build and launch the StreamBeam Flutter desktop app -- this IS the host UI
REM now (Beam tab: QR/pairing, connected devices, tray icon that shows/hides
REM the window). The old standalone streambeam-host.exe + Win32 tray are gone;
REM don't confuse this with `cargo build -p streambeam-host`, which only
REM builds the headless CLI binary devtools/CI use, with no UI at all.

REM Flutter/cargokit can't build from the \\wsl.localhost UNC share: cmd.exe
REM refuses a UNC path as its starting directory, and cargokit's deep build
REM tree (build\windows\x64\...\cargokit_build\...) blows past MAX_PATH unless
REM the project root itself is short. So we mirror the repo to a short native
REM path first and build from there.
set "MIRROR=C:\dev\sb"
set "APP_EXE=%MIRROR%\app\build\windows\x64\runner\Release\streambeam.exe"
set "FLUTTER=C:\src\flutter\bin\flutter.bat"

REM A previous instance keeps its exe file locked (and its own host server
REM bound to the port), so close it first; ignore the error if it wasn't running.
taskkill /IM streambeam.exe /F >nul 2>&1
taskkill /IM streambeam-host.exe /F >nul 2>&1

echo Syncing repo to %MIRROR% ...
REM /XD matches bare directory names at any depth (a path like
REM "android\.gradle" resolves relative to cwd, not the source root, and
REM silently fails to match -- bare names avoid that footgun). /XF skips
REM CLAUDE.md: it's a symlink to AGENTS.md, and robocopy can't follow
REM symlinks over the \\wsl.localhost 9P share ("cannot find the path
REM specified", retried until it aborts the whole sync).
robocopy "\\wsl.localhost\Ubuntu\home\joep\dev\streambeam" "%MIRROR%" /MIR ^
  /XD .dart_tool build .git target ephemeral .gradle ^
  /XF CLAUDE.md ^
  /R:2 /W:1 /MT:8 /NFL /NDL /NJH
if %errorlevel% GEQ 8 (
    echo.
    echo Repo sync failed. Is WSL running? Is the distro named "Ubuntu"?
    pause
    exit /b 1
)

echo Building StreamBeam (the first build is slow)...
pushd "%MIRROR%\app"
call "%FLUTTER%" build windows
set "BUILD_ERR=%errorlevel%"
popd

if not "%BUILD_ERR%"=="0" (
    echo.
    echo Build failed.
    pause
    exit /b 1
)

REM Launch detached: the release build has no console. Look for the
REM StreamBeam window and/or tray icon; check host.log
REM (%%APPDATA%%\streambeam\host.log) if neither appears.
start "" "%APP_EXE%"
