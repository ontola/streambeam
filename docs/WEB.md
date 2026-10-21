# Experimental browser viewer

The browser path is a **video-only development preview**, not a replacement for
native apps. It needs a separately launched `webbridge` process. Native hosts do
not yet offer a browser-sharing button or generate browser invitations. Audio,
remote input, saved browser devices, mobile-browser acceptance, and WAN routing
are not implemented. Do not advertise the website as a working browser viewer
until the assets are deployed and the host integration is complete.

The first slice is real H.264 playback: native host → pinned mutual-TLS QUIC
client → WebTransport gateway → WebCodecs decoder → browser canvas. It does not
capture the browser's own screen or play a pre-recorded video URL. The fixture
smoke test sends the repository's H.264 fixture through the same real network path.

## Why a gateway

Browsers cannot connect to StreamBeam's raw QUIC ALPN, present its Ed25519 client
identity, or obtain its native pairing TLS exporter. WebTransport is HTTP/3 and
needs its own endpoint and application authentication. Its certificate-hash
option authenticates the server, not the client. This experiment preserves the
native protocol and authorization gate instead of weakening it to fit browsers.

WebCodecs can decode the native Annex-B H.264 stream without transcoding. The
current slice uses a reliable WebTransport stream for framed access units; it
is not yet the datagram/FEC design required for competitive game-stream latency.

Primary references:

- [WebTransport security and certificate hashes](https://www.w3.org/TR/webtransport/)
- [WebCodecs video decoding](https://developer.chrome.com/docs/web-platform/best-practices/webcodecs)
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
Stop closes the native stream and consumes the invitation. Start a new bridge
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
- Only one authenticated browser is accepted per process. No parallel per-peer
  tasks or unbounded media queues are created. QUIC ingress is limited to four
  pending handshakes, 128 KiB total initial data, and 256 KiB per connection.
  After successful authentication,
  the native connection has a 60-second setup timeout. Video packets are capped
  at 4 MiB, writes at two seconds, and decode backlog at eight queued frames.
- Server messages use a big-endian u32 length. The first is UTF-8 JSON with
  `version:1`, H.264 `codec`, `width`, `height`, `fps`. Following messages contain
  one keyframe flag byte, a big-endian u64 timestamp in microseconds, and Annex-B
  H.264. Keyframes include SPS/PPS. Native FEC completion order is restored and
  native packet loss requests IDR recovery before dependent frames resume.
- There is no browser input channel in this slice. Closing/stopping the viewer
  releases the native session. A slow decoder terminates instead of dropping
  reference frames and displaying corrupted video.

## Verification

```sh
cargo test -p streambeam-devtools --features web --bin webbridge --locked
cargo clippy --no-deps -p streambeam-devtools --features web --bin webbridge --locked -- -D warnings
npm ci --prefix site/view --ignore-scripts
npm test --prefix site/view
```

`smoke.mjs` launches the real fixture and bridge, rejects a wrong capability, a wrong origin and a wrong certificate pin,
then requires at least 30 decoded 640×360 frames with non-uniform image pixels.
It verifies that the invitation fragment is cleared and Stop closes the bridge
and removes the private invitation. Credentials stay in temporary private files
and are not printed. Screenshot output defaults to Cargo's target directory
under `web-smoke/`; `STREAMBEAM_WEB_ARTIFACTS` overrides it. The dedicated Browser
preview workflow runs this same path in Chrome and retains the screenshot.

Production follow-up: integrate invitation and lifecycle with native hosts,
replace the prototype bearer capability with reviewed persistent browser pairing,
add audio/input and visibility recovery, evaluate unreliable media delivery,
exercise LAN browser permissions and target browsers, then ship the viewer URL.
