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
  `8fcca62302a7ab60e24f2f81e89e8ddfca3bfb5baaa0ab7e151d1b0ad1751e14`.
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

- Rust workspace: 119 tests passed, 1 ignored (the manual long soak).
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

## Android and controller follow-up

Both the Samsung Tab S9+ (SM-X810) and Xiaomi 15 (24129PN74G) were installed
through wireless ADB and paired normally with Mancave, preserving their app
identity. The temporary host used the existing service data directory; the
normal automatic service was restored afterward. Pairing output files were
removed rather than retained in diagnostics.

The tablet's original black picture was caused by stripping NVENC's essential
AV1 FRAME_HEADER OBU from its split FRAME_HEADER + TILE_GROUP output. The
client now keeps it, and configures MediaCodec with the documented av1C
record. The tablet displayed the actual 4K desktop at approximately 116–123
rendered fps. This confirms decode/display, not subjective audio quality.

First-contact cards now distinguish nearby from saved devices. An explicit
pairing rejection offers Scan QR / Paste pairing link recovery; an unrelated
network close no longer guesses that authentication failed. The host finishes
and waits for delivery of its rejection before dropping the connection.
A real-host rejection test and two dialog tests cover the failure path.
The service still has no approval/QR UI of its own; fully frictionless
service-mode onboarding remains unfinished.

Mancave controller support was installed using VIIPER 0.8.2 and the signed
usbip-win2 0.9.8.1 installer. Files live in
`C:\Users\joepm\StreamBeamController`; the current user's Startup shortcut
`StreamBeam Controller.lnk` starts VIIPER with both its control API (3242) and
USB transport (3241) bound to **127.0.0.1**. The installer returned success
but recorded that a restart is recommended; no restart was performed.
Windows successfully attached an Xbox 360 controller without restarting.

The phone's GameSir G8 is attached to the phone; the phone-to-PC connection
is wireless. Its side menu previously destroyed GamepadForwarder and unplugged
the Windows controller. The widget now stays mounted, sends neutral state
while input is paused, tracks button releases, and resumes without replugging.
A regression test checks that exact transition. Android's internal Xiaomi
uinput device is filtered out, and D-pad vertical direction is corrected.
The host also discards a cached VIIPER bus after the last device is removed,
so reconnecting does not loop on a deleted bus.

Android video now uses hybrid platform-view composition to retain the native
video consumer across orientation changes. Native logs confirmed a size
change without surface destruction during a portrait-to-landscape rotation.
Android loss recovery also runs outside the backlog branch and requests a
fresh keyframe if input arrives but the decoder stops producing pictures.
Release logging initializes at Info before FRB can install per-packet Trace.
The video sender refreshes its path packet-size limit per frame and recovers
from a mid-frame MTU reduction instead of silently stopping only video.

The user confirmed phone picture and input, then reported choppy/unreliable
playback. Logs exposed wire-frame discontinuities while FEC still reported
zero abandoned frames: entirely absent frames and out-of-order completions
were being fed into an AV1 reference chain. Android now restores frame order
with a bounded 30 ms wait (no added wait for in-order traffic), discards late
frames, and waits for a keyframe after a real gap. Three deterministic tests
cover reordering, whole-frame loss, and sequence rollover.

**Phone retest, 23:01–23:03:** wireless debugging reconnected and the final
ordering build was installed successfully on the Xiaomi. It displayed the
actual GameNight desktop at 3840×2160 / 120 target. The 72 one-second windows
after startup averaged 112.8 decoded fps (77.4–121.8), with no zero-output
window. There was one gap-driven recovery, rather than the previous repeating
decoder-stall recoveries. This is a short live sample, not a long-session or
input-to-photon latency measurement.

Opened the side menu and dismissed it by tapping the picture, then rotated
portrait → landscape → portrait. Video continued, native logs showed only
surface resizes (no surface destruction), and Windows XInput kept controller
slot 0 connected throughout the menu/rotation observation. Original Android
auto-rotation settings were restored afterward. The phone's media output is
unmuted and routed to its speaker; audibility and sync still require listening.

**Acceptance still open:** hands-on controller feel, physical button/stick
values in this final session, audio, and a sustained gaming run. The user
confirmed input on the preceding build, but also reported its choppiness;
that earlier observation must not be presented as acceptance of this build.
Rumble is not implemented. Current evidence:
`/tmp/streambeam-phone-ordered-live.log`,
`/tmp/streambeam-xinput-ordered.log`, and
`/tmp/phone-ordered-landscape.png`.

Latest validation: 119 Rust workspace tests passed, one manual soak ignored;
Flutter analysis clean; focused Flutter tests include menu/controller and
pairing recovery regressions. Temporary evidence includes
`/tmp/streambeam-phone-hybrid-live.log`, `/tmp/streambeam-xinput-final.log`,
`/tmp/streambeam-order-tests.log`, and `/tmp/streambeam-all-final-tests.log`.

### Frame-drop/input-lag follow-up (2026-10-10, 23:10–23:11)

The user's next report was many frame drops and slow input: the 4K/120 phone
session is not accepted as working smoothly. Host logs also show genuine
packet loss and occasional RTT spikes to 133 ms, beyond encode/decode cost.

New Android build defaults Game to up to 1920 longest-side / 60 fps. The host
preserves its capture aspect, yielding 1920×1080 on Mancave. Quality/Custom
still allow higher settings. Decoder output polls no longer sleep, and a
receive batch takes only already-queued packets instead of waiting another
2 ms for every future shard. The first-packet wait is bounded to 2 ms so
pending decoded frames can reach the surface even between network arrivals.

Built ARM64 release APK and installed on Tab S9+; analysis clean, 37 focused
Flutter tests passed. Visible GameNight picture confirmed. The 32 settled
one-second windows averaged 59.84 fps, minimum 53.9, maximum 60.9. There was
one gap-driven keyframe recovery and four skipped frames, no FEC abandonment.
Host encoder wait was about 3 ms/frame. Evidence:
`/tmp/streambeam-tablet-latency-live.log`,
`/tmp/streambeam-tablet-latency-frame.png`,
`/tmp/streambeam-latency-apk.log`, `/tmp/streambeam-latency-tests.log`.
This mostly static desktop test does not establish smooth gameplay or
controller input-to-photon latency. No physical controller test was performed
on the tablet. The stream had ended by 23:11:27.

Phone wireless debugging is unavailable again (last IP/port
192.168.0.167:43033 refused; no phone in ADB/mDNS). Asked user to re-enable it.
**Phone still has the preceding 4K/120 build; installing this newer APK and
retesting the attached controller remain pending.**

### Phone update installed (2026-10-10, 23:16)

Wireless ADB returned at `192.168.0.167:36959`. Installed the newer `9aceef5`
ARM64 release APK successfully (Android lastUpdateTime 23:16:03), launched it,
and opened saved Mancave. Screenshot confirms the GameNight picture at
1920×1080. The GameSir G8 is present as Android controller 1; Windows XInput
slot 0 attached at 23:16:25.301. The captured state remains neutral: physical
button/stick values have not yet been verified in this retest.

Startup paused and included a zero-output second plus keyframe requests.
Afterward, 68 one-second windows from 23:16:31 through 23:17:38 averaged
59.93 decoded fps (54.9–60.9), no FEC abandonment, one gap recovery and three
skipped frames. Host processing settled around 3 ms/frame. This is a short
mostly static desktop run; input-to-photon latency and smooth gameplay remain
open. Asked the user to play with the physical controller and open/dismiss
the side menu; no response yet at documentation time.

Evidence: `/tmp/streambeam-phone-1080-live.log`,
`/tmp/streambeam-phone-1080-frame.png`, `/tmp/streambeam-xinput-1080.log`,
`/tmp/streambeam-host-1080-sample.log`. The earlier wireless-debugging blocker
is resolved; this build is now installed on both phone and tablet.
