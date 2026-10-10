# Mac and Mancave setup — 2026-10-10

## Installed setup

- Mac: optimized Apple-silicon app at `/Users/joep/Applications/StreamBeam.app`.
  Open it and select **MANCAVE**; the existing pairing is preserved.
- Mancave: Windows `StreamBeamHost` service, automatic startup, serving
  `192.168.0.85:47989`. Its worker is
  `C:\dev\sb\app\build\windows\x64\runner\Release\streambeam-host.exe`.
- Windows source snapshot for this deployment:
  `/home/joep/dev/streambeam-setup-20261010` (WSL). The original dirty checkout
  at `/home/joep/dev/streambeam` was not overwritten.
- The optimized worker's SHA-256 is
  `8efa9fb915ee0867123f855d98ab4d4408b634d1ee92857b2f6ac34541cb6d5b`.
  Verified against the running service's worker path after staging.

## Changes and observed behavior

Desktop Duplication now polls immediately once a previous texture exists.
The server already schedules each frame, so another full-frame capture wait
was adding delay before encoding. Startup keeps its bounded initial wait.

Before/after 30-second real-host receive tests, AV1 at 2560×1440 / 120 fps:

| Run | Complete frames | Average receive rate | Abandoned frames |
|---|---:|---:|---:|
| Previous installation | 2,678 | 89.3 fps | 0 |
| Updated installation | 3,595 | 119.8 fps | 0 |

The Mac then displayed the real animated GameNight desktop at 3840×2160.
The app reported 118 fps against a 120 fps target. A later 30-window host
sample averaged 119.9 fps, 4.6 ms capture/encoder wait and 7.0 ms network RTT
(4–18 ms range), with no lost transport packets in that sample. Client logs
showed no abandoned frames or decode errors during those samples. A subsequent
three-minute 4K run exceeded 20,000 complete frames with 11 abandoned frames
from one loss burst; playback recovered without a decoder error or disconnect.

These are stream/decoder-feed rates, **not** a measurement of unique game
frames, display refresh, or total input-to-photon latency. Capture repeats the
last frame when the desktop has not changed; the tested scene updated at
roughly 60 fps.

Changing stream quality previously returned to the device list and logged an
unhandled `AnyhowException(closed)` while cancelling the old Rust stream.
Cancellation errors are now caught so native decoder disposal and reconnect
still run. Verified live by switching Game → Quality (4K30) → Game (4K120),
with a decoded first frame after each switch.

Also verified through the actual Mac stream:

- Mouse click opens Windows Start; typed text appears in Windows Search.
- Explicit game mouse capture activates, and Cmd+Esc releases it.
- Sound is enabled; perceived audio quality and audiovisual sync require a
  listening check and are not established by the video tests.

## Rebuild and update

Use `./scripts/build-macos-local.sh` for an optimized build for the local Mac.
Set `FLUTTER_BIN` if Flutter is not in PATH and there is no generated project
configuration. This script builds only the local CPU architecture and sets
the build's deployment target to macOS 12: Xcode 27 rejected the old Pods
10.15 target, and its `lipo -verify_arch arm64 x86_64` failed despite both
architectures being present. The script was exercised successfully.

After quitting the installed app:

```sh
ditto app/build/macos/Build/Products/Release/streambeam.app \
  "$HOME/Applications/StreamBeam.app"
```

On Mancave, use Windows-native Cargo from WSL as documented in
[DEBUGGING_CLIENT.md](DEBUGGING_CLIENT.md), with
`CARGO_TARGET_DIR=C:\Users\joepm\streambeam-target`. Copy a completed worker
to `streambeam-host.upload.exe` beside the installed worker, then rename it
to `streambeam-host.new.exe`. The existing supervisor rotates it in and
retains `.old.exe` for rollback. Do not overwrite a running executable or
replace the original dirty WSL checkout.

## Validation and evidence

- Rust workspace: 114 tests passed, 1 ignored (the manual long soak).
- Native Windows optimized host build passed; capture change exercised on
  Mancave's RTX 5070 Ti, not just checked on macOS.
- Flutter analysis passed; 33 focused tests passed (stream modes, decoder
  watchdog, HDR status and keyboard mapping).
- Both Cargo workspaces passed formatting checks after normalizing existing
  generated import order. The unrelated `app loads` widget smoke test was
  not included; see the known limitation in DEBUGGING_CLIENT.md.
- Local logs for this run: `/tmp/streambeam-client-fixed.log`,
  `/tmp/streambeam-baseline-1440p120.log`,
  `/tmp/streambeam-optimized-1440p120.log`,
  `/tmp/streambeam-setup-20261010/host-final.log`, and
  `/tmp/streambeam-local-build-script.log`. These are temporary evidence,
  not persistent application configuration.

Long-session gaming, controller/rumble, lock-screen handover and end-to-end
audio latency were not covered by these checks.
