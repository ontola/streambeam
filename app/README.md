# StreamBeam Android client (M3 scaffold)

Flutter UI + Rust core via [flutter_rust_bridge](https://codelabs.developers.google.com/codelabs/flutter-app-with-ffi).

## Dev loop (phone + fakehost on Mac)

1. Start fakehost on your Mac (`STREAMBEAM_LISTEN=0.0.0.0:47989 cargo run -p streambeam-devtools --bin fakehost`).
2. Copy the `sbm1:…` line (or print as QR later).
3. Build/run this app on a device:

```sh
cd app
flutter pub get
flutter_rust_bridge_codegen generate   # after changing rust/src/api.rs
flutter run
```

Tap **Receive 30 frames** to pair over QUIC and print stream stats from Rust (no MediaCodec yet).

## Rust API

See `rust/src/api.rs` — `connect_and_receive()` uses the same protocol stack as `playclient`.

## Next (M3)

- SurfaceView → JNI Surface → `AMediaCodec` decode (NALs stay in Rust)
- Wire stats overlay + low-latency codec keys
