# Experimental browser viewer

The browser path is a **development preview with optional sound and desktop
control**, not a replacement for native apps. It needs a separately launched `webbridge` process. Native hosts do
not yet offer a browser-sharing button or generate browser invitations. Saved
browser devices, mobile-browser acceptance, and WAN routing are not implemented. Do not advertise the website as a working browser viewer
until the assets are deployed and the host integration is complete.

The media path provides real H.264 and optional Opus playback: native host →
pinned mutual-TLS QUIC client → WebTransport gateway → WebCodecs decoders →
browser canvas and Web Audio. It does not
capture the browser's own screen or play a pre-recorded video URL. The fixture
smoke test sends the repository's H.264 fixture through the same real network path.

## Why a gateway

Browsers cannot connect to StreamBeam's raw QUIC ALPN, present its Ed25519 client
identity, or obtain its native pairing TLS exporter. WebTransport is HTTP/3 and
needs its own endpoint and application authentication. Its certificate-hash
option authenticates the server, not the client. This experiment preserves the
native protocol and authorization gate instead of weakening it to fit browsers.

WebCodecs can decode the native Annex-B H.264 stream without transcoding. Opus
uses the native session’s negotiated sample rate, channels and
frame duration; this preview supports 48 kHz mono/stereo playback. The
current slice uses a reliable WebTransport stream for framed access units; it
is not yet the datagram/FEC design required for competitive game-stream latency.

Primary references:

- [WebTransport security and certificate hashes](https://www.w3.org/TR/webtransport/)
- [WebCodecs H.264 access-unit requirements](https://www.w3.org/TR/webcodecs-avc-codec-registration/)
- [WebCodecs video decoding](https://developer.chrome.com/docs/web-platform/best-practices/webcodecs)
- [Raw Opus codec registration](https://www.w3.org/TR/webcodecs-opus-codec-registration/)
- [wtransport short-lived self-signed identities](https://docs.rs/wtransport/0.7.2/wtransport/tls/struct.Identity.html)

## Run locally

Requires Rust from `rust-toolchain.toml`, Node 22+, and a current Chrome or Edge
with WebTransport certificate hashes and H.264 WebCodecs support. Browsers with
missing APIs show an explanation. This is not a claim of Safari/iOS support.

```sh
cargo build -p streambeam-devtools --features web --bin webbridge --bin fakehost --locked
python3 -m http.server 8876 --bind 127.0.0.1 --directory site
```

In another terminal, create private fixture pairing input, then a one-time browser
invitation (keep the fixture process running):

```sh
umask 077
STREAMBEAM_NO_MDNS=1 cargo run -p streambeam-devtools --bin fakehost > /tmp/streambeam-pair
```

```sh
cargo run -p streambeam-devtools --features web --bin webbridge -- \
  --pair-file /tmp/streambeam-pair \
  --invite-file /tmp/streambeam-browser-invitation
```

Open the URL from the private invitation file in the browser and click Connect.
Sound is off by default; click Enable sound to start playback. Mute and hiding
the tab clear queued sound. Physical audibility and A/V synchronization still
need real-device acceptance; the automated test proves decoded PCM and scheduled
browser playback, not what reaches the speakers.

Control is off by default. Enable “Allow remote control for this session” to use
the shared desktop; leaving the canvas, hiding the tab, or pressing Escape revokes
control and releases held keys/buttons. Keyboard mapping currently assumes the
standard physical Windows VK layout; IME composition, clipboard, gamepads and
Android multi-touch are not provided. Stop closes the native stream and consumes the invitation. Start a new bridge
with a fresh host pairing token to reconnect. Remove the private pairing file
when finished. The invitation file must not already exist; it is deleted on normal
exit, timeout, or Ctrl+C. After a forced kill, delete the stale file yourself.
Never paste invitations into logs, issues, analytics or chat transcripts.

To use a real native host, put its current pairing payload into the private
pairing file. The host still authenticates the bridge and requires its existing
interactive approval when applicable. Real desktop browser streaming must be
verified separately from the fixture; a fixture pass does not prove it.

The gateway defaults to `127.0.0.1:4433`. A non-loopback listener requires an
explicit local IP and an HTTPS `--viewer-url`, with the site assets deployed at
that trusted origin. This is an opt-in development deployment, not an automatic
network service. UDP reachability and browser local-network permission can still
block it. Do not publish the invitation or expose the gateway to the internet.

## Security boundary and browser wire format

- The gateway is trusted with native screen contents and acts as a native client.
  It is not an end-to-end opaque relay between the browser and the original host.
- The native pairing payload stays in a local file. The bridge does not contact
  the native host until it has authenticated the browser. Native `Hello` and
  pinned host certificate verification are unchanged.
- A fresh, in-memory ECDSA TLS identity uses WebTransport's ≤14-day certificate
  validity. The invitation pins the certificate's SHA-256 fingerprint.
- The invitation is `viewer-url#v=1&endpoint=...&cert=...&token=...`. The random
  32-byte capability lives only in the fragment and first encrypted stream.
  It is never sent in an HTTP path/query/header. The viewer immediately removes
  the fragment from browser history and uses no storage or analytics.
- Origin and `/view` path are checked before accepting the WebTransport session.
  Accepting TLS/HTTP transport does not authorize media. The first client-created
  bidirectional stream must send the exact 32-byte capability within five seconds;
  constant-time comparison and a five-minute expiry gate all native connection
  attempts and application responses. Invalid attempts do not consume the invite.
- Only one authenticated browser is accepted per process. The gateway does not
  spawn a task for each unauthenticated peer. One bounded input reader task is
  created after authentication and aborted at session teardown. QUIC ingress is
  limited to four
  pending handshakes, 128 KiB total initial data, and 256 KiB per connection.
  After successful authentication,
  the native connection has a 60-second setup timeout. Video packets are capped
  at 4 MiB, writes at two seconds, and decode backlog at eight queued frames. At that cap the browser pauses
  reads for at most two seconds, preserving reference frames through short CPU
  stalls; a persistently slow decoder still ends the session.
- Server messages use a big-endian u32 length. The first is UTF-8 JSON with
  `version:2`, H.264 `codec`, `width`, `height`, `fps`, desktop `control` support and
  negotiated `audio {sample_rate, channels, frame_ms}`. Following messages contain
  one media flag byte, a big-endian u64 timestamp in microseconds, and payload.
  Flags 0/1 are delta/keyframe Annex-B H.264; flag 2 is raw Opus. Keyframes include
  SPS/PPS only from the native CodecConfig, even when that config also includes
  an IDR picture. Prepending that picture would put two access units in one
  WebCodecs chunk, which Linux Chrome rejects. Native FEC completion order is restored and
  native packet loss requests IDR recovery before dependent frames resume.
- After the initial capability, the client side of the same bidirectional stream
  carries u16 big-endian-length JSON input commands (maximum 512 bytes, queue 32,
  250 commands/second). The restricted tagged commands are `control {enabled}`,
  `move {x,y}`, `button {button,down}`, `key {vk,down}`, and `scroll {dx,dy}`.
  Unknown fields/commands, out-of-range positions/VKs and more than 16 held keys
  are rejected. No arbitrary protocol, clipboard, power, or secure-attention
  message can be tunneled through this surface. Input is ignored until explicit
  session consent and remains disabled for Android/touch hosts.
- Revoking consent releases all tracked held keys/buttons. Teardown sends releases
  and finishes the native input stream with a bounded acknowledgement wait. The
  native host also now guards held input with a per-authenticated-connection
  generation: the newer connection's first input releases old holds atomically;
  old input/cleanup cannot release keys belonging to the new owner. Stream EOF,
  task cancellation and connection loss all release owned keys/buttons.
- Audio is decoded only after a user click. Decoder backlog is capped at 12
  packets; excess Opus packets are dropped so a brief browser stall can recover; scheduled playback is capped at 200 ms and 24 sources. Muting, hiding
  the tab or teardown stops every scheduled source. Duplicate/old native Opus
  datagrams are discarded; full packet-loss concealment and synchronized A/V
  presentation remain follow-up work.
- Closing/stopping the viewer releases the native session. A slow decoder
  terminates instead of dropping reference frames and displaying corrupted video.

## Verification

```sh
cargo test -p streambeam-devtools --features web --bin webbridge --locked
cargo clippy --no-deps -p streambeam-devtools --features web --bin webbridge --locked -- -D warnings
npm ci --prefix site/view --ignore-scripts
npm test --prefix site/view
```

`smoke.mjs` launches the real fixture and bridge, rejects a wrong capability, a wrong origin and a wrong certificate pin,
then requires at least 30 decoded 640×360 frames with non-uniform image pixels
and 30 more after a forced 750 ms browser scheduling stall.
It verifies that the invitation fragment is cleared, no native input arrives before
consent, consented browser key/button events reach the actual native fixture,
focus loss releases held input, and closing the actual tab releases held keys,
closes the bridge and removes the private invitation. The audio test also verifies no decoding before a click, nonzero decoded PCM,
a bounded source count, recovery after a 250 ms main-thread stall, and an empty
queue after Mute. Chrome runs headless, so
this is not an audibility assertion. The fixture’s opt-in private JSONL input
recorder never injects events into the OS. Credentials stay in temporary private files
and are not printed. Screenshot output defaults to Cargo's target directory
under `web-smoke/`; `STREAMBEAM_WEB_ARTIFACTS` overrides it. The dedicated Browser
preview workflow runs this same path in Chrome and retains the screenshot. On
failure it retains a sanitized status/capability/process-exit report and page
screenshot, never raw logs, browser traces, URLs or private invitations.

Production follow-up: integrate invitation and lifecycle with native hosts,
replace the prototype bearer capability with reviewed persistent browser pairing,
add A/V synchronization, keyboard-layout/IME support and visibility recovery, evaluate unreliable media delivery,
exercise LAN browser permissions and target browsers, then ship the viewer URL.

## Native host integration path

The next useful product step is a “Share in browser” action in the desktop app,
with a QR/link, expiry and Stop button owned by the existing host UI. Extract the
bridge from the devtool into an optional library task and make the app own its
lifetime. Keep the same invitation/origin/TLS restrictions; do not silently open
a listener at application startup. The owner explicitly choosing browser sharing
should be the action that creates an invitation.

The current prototype pairs an ephemeral native bridge identity on every launch.
That is suitable for an isolated fixture but would accumulate paired records on
real hosts. Production integration needs a reviewed local bridge identity or a
host-owned media adapter, with revocation and display selection tied to the
browser session. Avoid exposing the native pairing payload to JavaScript or
reusing the native pairing token as the browser bearer capability. Browser LAN
permission prompts, the deployed HTTPS viewer, codec support and actual desktop
permission/approval behavior need their own acceptance test before calling this
install-free sharing.
