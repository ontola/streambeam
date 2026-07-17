# Symmetric apps & the Flutter-everywhere direction

Direction agreed 2026-07-04. The symmetric app shape is now implemented on
Windows, Android, and macOS; platform-specific follow-ups remain.
This supersedes the "host = egui panel, client = Flutter" split in
[PLAN.md](PLAN.md) as the *target* shape — PLAN.md stays as the historical
record, [PROGRESS.md](PROGRESS.md) tracks what has actually landed.

## The idea

One StreamBeam app per platform, built from one Flutter UI and one Rust core.
Every build can **watch**; builds on capable platforms can also **beam**
(host). The product story becomes "beam any screen to any screen" — PC→phone,
Mac→PC, Mac→Mac — with the same one-QR pairing everywhere.

**Symmetric in code, not per platform.** The protocol already has no notion of
a "host app" vs a "client app" — roles are just who listens and who dials, so
symmetry is nearly free at the wire level. It is *not* free at the OS level:

| Platform | Watch | Beam (host) |
|---|---|---|
| Windows | ✅ | ✅ WGC + NVENC/AMF/QSV (v1 path, unchanged) |
| macOS | ✅ | ✅ ScreenCaptureKit + VideoToolbox |
| Android | ✅ | ✅ MediaProjection + MediaCodec (no input inject) |
| iOS | ✅ later | ❌ effectively impossible (no background capture/inject) |
| Linux | ✅ later | ⚠️ later (PipeWire portal + VAAPI) |

The UI shows the "Beam my screen" section only where the platform can host.
Don't promise a symmetry matrix; promise one app.

## Why Flutter for the host GUI too

- **Validated by RustDesk** — the same architecture (Flutter UI on every
  platform, Rust core via flutter_rust_bridge, desktop builds both host and
  connect) ships in production. This is not a research risk.
- One codebase for pairing/QR/settings/paired-device list instead of egui +
  Flutter duplicates that must stay in sync.
- The "small native GUI" feel survives: Flutter desktop cold-starts fast, and
  `tray_manager` / `window_manager` cover the tray icon + tiny-window UX.
- Cost: the egui host GUI and the current `crates/host` binary structure get
  replaced by a Flutter shell embedding the same Rust logic. Pivoting early is
  cheap; pivoting after M8 polish is not.

## Target architecture

```
crates/proto      unchanged — pure wire types
crates/net        unchanged — QUIC, identity, pairing
crates/core       client engine (reassembly, IDR, input coalescing) — FRB-free
crates/hostcore   NEW: host engine — capture/encode/inject/audio behind traits,
                  per-cfg(target_os) impls, FRB-free (mirrors crates/core)
crates/host       shrinks to a thin headless/CLI binary over hostcore
                  (scripts, Windows box, CI — keeps the sbm1: stdout contract)
crates/devtools   unchanged — fakehost stays the macOS dev loop
app/              the ONE Flutter app; app/rust FRB API grows host-role
                  bindings (start/stop hosting, pairing QR, session list)
```

Migration is mostly *moving* code: `crates/host`'s capture/encode/inject
modules relocate into `hostcore` behind the existing traits. The `VideoEncoder`
trait and mock impls already make this workspace-checkable on macOS.

### macOS host specifics

- **Capture + system audio:** ScreenCaptureKit (macOS 13+; SCK does audio
  loopback natively — no BlackHole-style virtual device).
- **Encode:** VideoToolbox. `hwcodec` already wraps it (RustDesk ships it on
  Mac) — same no-declared-license caveat as on Windows; it stays behind
  `VideoEncoder` so it can be swapped (see AGENTS.md quirks).
- **Input injection:** `CGEvent` mouse/keyboard is implemented (absolute touch,
  relative trackpad, tap/drag, pixel scroll, and VK→macOS keycode translation).
  Gamepad has no ViGEm equivalent; virtual-controller support is out of scope
  initially.
- **Permissions UX:** first hosting run triggers TCC prompts (Screen Recording
  + Accessibility). Needs a guided onboarding screen — the prompts don't
  re-appear, but a denied prompt silently yields black frames / dead input, so
  the app must detect and explain the state.

## Risks

1. **Desktop client rendering** — the one genuinely new technical risk. On
   Android video bypasses Dart (MediaCodec→Surface). A desktop *client* needs
   decode → GPU texture → Flutter (`irondash_texture` / Texture API). RustDesk
   proves viability, but it may cost ~a frame vs a raw native window — measure
   before committing the latency budget (same discipline as M3/M4).
2. **Concurrent work collision** — this restructuring touches `crates/host`
   while milestone work (M2/M6) is active there. Coordinate; don't restructure
   and feature-build in the same files at the same time.
3. **hwcodec license** — unchanged, but now applies on two platforms.

## Sequencing

1. **First finish the M2 Windows WGC→NVENC spike** — it's the top project risk
   and this decision doesn't affect it (the spike lands behind `VideoEncoder`
   either way).
2. Extract `crates/hostcore` from `crates/host` (pure refactor, no behavior
   change; CLI binary keeps working).
3. ✅ Grow the FRB API + Flutter host UI ("Beam my screen" → QR screen).
4. 🔨 macOS host: SCK capture + VideoToolbox encoder and CGEvent mouse/keyboard
   input landed 2026-07-17; system audio, display selection, dynamic encode
   settings, gamepad, and fuller TCC onboarding remain.
5. ✅ Desktop client watch on macOS and Windows.
