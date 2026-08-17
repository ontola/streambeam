# Debugging the watch path (a.k.a. "why is the screen black")

The single most expensive bug class in this project has been **"data arrives, no
picture"**. It has cost more debugging time than everything else combined, across
many sessions, and it kept recurring because *the client had no observable
surface*: the tests stopped above the decoder, the Rust logs went nowhere, the
Swift decoder swallowed every error, and reproducing anything needed a human to
click a button.

This document is the playbook that came out of finally fixing that (2026-08-17).
Read it before debugging a picture problem — most of the dead ends below have
already been walked.

## The one-line diagnosis flow

A black picture is always one of four things, and the stats line tells you which
in two seconds:

```
desktop stream: 1733 complete / 0 abandoned / 7817 shards, fed 75 skipped 1658, awaiting_keyframe=true
```

Since 2026-08-17 the client also **tells you itself**: `DecodeWatchdog`
(`app/lib/decode_watchdog.dart`) polls the platform decoder's counters and, if
frames are being handed to it and nothing comes back for 3s, replaces the black
rectangle with the failing stage — "the decoder rejected all 400 frames (status
-12909)". It only fires when frames actually reached the decoder, so a stalled
host or dead network can't trigger it. If you are debugging a black screen and
the UI says nothing, that itself narrows things down: either the watchdog is
unwired on this platform (see open items) or frames never arrived at all.

| Symptom in that line | Meaning |
|---|---|
| `complete` flat, `shards` climbing | reassembly never finishes a frame — FEC/loss or a bucket being dropped mid-fill (`reassemble.rs`) |
| `abandoned` climbing | frames are being given up on — real loss, or the staleness rule is too aggressive |
| `skipped` >> `fed` | the frame gate is discarding received frames (`frame_gate.rs`) — policy bug, not a network one |
| `fed` climbing but no picture | frames reach the platform decoder and die there — see the `StreamBeam:` NSLog lines |

Then the decoder's own lines:

```
StreamBeam: decoder ready — 3840x2160 av01 (hw av1=true) [obj … tex …]
StreamBeam: decode() first call — 366636 bytes, av1=yes
StreamBeam: first frame decoded — 3840x2160          ← healthy
StreamBeam: decode failed (status -12909), …         ← VideoToolbox rejected the bitstream
StreamBeam: frame dropped before decode (…), …       ← never reached VideoToolbox
```

**Never add a silent `guard … else { return }` to this path.** Both of the worst
bugs of 2026-08-17 were invisible purely because of that pattern.

## Where the logs actually go

- **Rust client logs**: nothing installs a `log` sink on desktop, so `log::info!`
  from `app/rust` is **invisible**. The stream stats use `eprintln!` deliberately.
  Launching the app bundle directly (`open …/streambeam.app`) discards stderr —
  run the binary directly with a redirect, or use `flutter run` (which captures
  it).
- **Swift logs**: `NSLog`. These appear in `flutter run` output and in a stderr
  redirect. Note `NSLog` in Swift can't take raw pointers — build a `String` and
  pass it via `"%@"` or you get "Variadic function is unavailable".
- **Host logs**: `~/.streambeam/host.log` (macOS), `%APPDATA%\streambeam\host.log`
  (Windows). The host is usually the *innocent* party — check it first to rule it
  out, it reports `sent frame …` with fps, loss and bitrate.

## Driving the app without a human

`app/test_driver/app.dart` is a dev-only entrypoint that enables the Flutter
Driver extension. This is what turns "ask the user to click and report back" into
a scripted loop.

```bash
cd app
# stdin must stay open or `flutter run` exits on EOF; there is no `setsid` on macOS
nohup sh -c 'tail -f /dev/null | flutter run -d macos --debug \
    -t test_driver/app.dart --print-dtd' > /tmp/sb-run.log 2>&1 &
grep -i "Dart Tooling Daemon" /tmp/sb-run.log      # → ws://127.0.0.1:PORT/KEY=
```

Then, via the Dart MCP server: `dtd connect <uri>`, `widget_inspector
get_widget_tree` to see real widget text, and `flutter_driver_command` to tap.

Gotchas, all learned the hard way:

- **`set_frame_sync` → `false` first.** A live video texture never settles, so
  every driver command otherwise times out.
- **Tap by a unique string.** `ByText "Mancave-dev"` matched two widgets (saved +
  discovered host); the address (`192.168.0.89:47990`) is unique.
- **`screenshot` does not capture platform textures.** The video area comes back
  black even when decoding is fine, so it is useless as a "is there a picture"
  oracle. Use the decoder logs, or `screencapture` of the real window.
- Multiple app instances fight over the host's single shared encoder (constant
  rebuilds, sessions superseded). Kill strays before concluding anything.

## Capturing decoder fixtures

`devclient` can produce the exact pair a platform-decoder test needs:

```bash
export STREAMBEAM_CLIENT_DATA="$HOME/.streambeam"
cargo run -p streambeam-devtools --bin devclient -- "sbm1:ignored" --reconnect \
  --av1 --width 3840 --height 2160 --fps 60 --frames 3 \
  --dump-fixture app/macos/RunnerTests/Fixtures/av1_2160p.av1c
# writes the av1C record + <path>.keyframe
```

`--width/--height/--fps` exist because a real host sizes its encoder from client
caps — it is the only way to ask for 4K from the CLI. `--dump` writes the raw
bitstream for `ffprobe`/`ffmpeg` replay. Note the host may keep an
already-built encoder at its existing size, so **check what you actually got**
(`ffprobe`, or the printed byte counts) instead of trusting the flags.

`--dump-fixture` writes **what the platform decoder is handed**, not the wire
bytes: for AV1 it converts the host's sequence header into an `av1C` record, the
same conversion `app/rust/src/api/mod.rs` does. It originally dumped the wire
config, which cost a debugging round — VideoToolbox rejects a bare sequence
header with `-12910`, so the test failed on the fixture, not the product. A
fixture that isn't byte-identical to what production passes down tests nothing.

**`ffmpeg` is not a substitute for the platform decoder.** It happily decoded the
exact AV1 stream that VideoToolbox rejected with `-12909`. Cross-platform replay
proves the *host* is sane; only the platform decoder proves the *client* is.

## Test layers, and what each one can catch

| Layer | Catches | Blind to |
|---|---|---|
| `crates/core` unit tests (`reassemble.rs`, `frame_gate.rs`) | reassembly and frame-gate policy bugs — historically the majority | anything platform-specific |
| `crates/devtools/tests/e2e_stream.rs` | pairing, FEC, loss sweeps, hash verify | **the decoder entirely** — `devclient` never decodes, so it passes through every black screen |
| `app/macos/RunnerTests` | the real VideoToolbox contract: bitstream shaping, session setup | Windows/Android decoders |
| Real device | everything else | nothing — but it needs the tooling above to be diagnosable |

### Running the macOS decoder tests

Neither `cargo test` nor `flutter test` runs these — they need `xcodebuild`, and
the scheme only exists in the **workspace** (`-project` shows just "Flutter
Assemble"):

```bash
cd app/macos
xcodebuild test -workspace Runner.xcworkspace -scheme Runner \
  -destination 'platform=macOS' -only-testing:RunnerTests \
  2>&1 | grep -vE "ld: warning|was built for newer" > /tmp/sb-test.log
grep -E "Test case '|\*\* TEST" /tmp/sb-test.log
```

The `grep -v` matters: ~4,000 lines of `rust_lib_streambeam` deployment-target
linker warnings bury the result. **Assertion messages don't appear in that
output at all** — pull them from the xcresult bundle whose path the log prints:

```bash
xcrun xcresulttool get test-results tests --path <…>.xcresult --compact
```

`NSLog` from inside the XCTest host doesn't reach it either, which is why
`configure()` records `lastSetupFailure` (with the `OSStatus`) instead of only
logging — that's what turned an unattributable false return into
`VTDecompressionSessionCreate status -12910`.

Two live traps when running the suite:

- **Rebuild the host binary first.** `cargo test -p streambeam-devtools` spawns a
  *prebuilt* `streambeam-host`; a stale one produces confusing failures that look
  like regressions. `cargo build -p streambeam-host` first.
- `STREAMBEAM_MOCK=1` makes the headless host use the mock encoder — required on
  macOS, where the real capture layer only exists inside the app bundle.

## Open items (2026-08-17)

- No ffmpeg-level decode assertion in `e2e_stream` yet (would catch undecodable
  bitstreams and literal black output, but *not* platform-decoder rejection).
- `app/test/widget_test.dart` ("app loads") **fails on main**, unrelated to any
  of this: it never calls `RustLib.init()`, so `appCapabilities` throws. Fix it
  or delete it — a permanently red test trains people to ignore the suite.
- The watchdog is macOS-only so far: Windows and Android have no `health`
  method, so `probeDecodeHealth` is null there and the check is silently off.
  It fails safe, but those platforms keep the old blind spot.
- Running `xcodebuild test` **launches the full app UI** (the test host is the
  Runner app), which can crash on Flutter's raster thread in Skia
  (`GrMakeCachedBitmapProxyView`, an SVG/bitmap draw) after the tests pass. The
  crash report says `streambeam`, so it looks alarming and isn't — check the pid
  against the test-run pid before chasing it.
- No **runtime watchdog**. The highest-value remaining item: if bytes are
  arriving and zero frames have decoded for ~3s, say so in the UI and the log.
  Most of these bugs only ever appeared against real hardware, where the user's
  only signal was a black rectangle.
- `MAX_INFLIGHT_FRAMES` eviction in `reassemble.rs` can still evict an
  actively-filling bucket — same blind spot as the staleness rule that caused the
  4K black screen, not yet implicated but the same shape.
- The host emits duplicate frames at the full negotiated rate (120/s) when the
  desktop is static, which floods the client and is upstream of the frame-gate
  pressure. Worth questioning.

## Windows host (Mancave) specifics

See the PROGRESS decisions log (2026-08-17) for detail. In short: `ssh
mancave.local` lands in **WSL2**, not Win32 (`appendWindowsPath=false`, so use
absolute paths like `/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe`);
Windows-native cargo lives under `C:\Users\joepm\.cargo` (Windows user is
`joepm`); build with `CARGO_TARGET_DIR=C:\Users\joepm\streambeam-target` and a
UNC `--manifest-path`. Three things cannot be done over SSH without elevation at
the machine: unlocking the session (`tscon`), killing the SYSTEM-spawned host,
and adding the firewall rule. And **Desktop Duplication cannot capture a locked
session** — a locked host degrades to ~0.5fps of stale frames, which reads as a
black screen; the host log says so explicitly.
