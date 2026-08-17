# StreamBeam protocol v3

Everything runs over a single QUIC connection (client-initiated), mutual TLS with
pinned Ed25519 keys. `PROTO_VERSION = 5`.

## Identity & transport security

- Both sides hold a long-term Ed25519 identity key, wrapped in a self-signed X.509
  cert (rcgen) presented in the TLS handshake.
- The **client** verifies the host cert's SPKI public key equals the pinned key
  (from the QR on first contact, from `hosts.json` thereafter). No CAs, no hostnames.
- The **host** requires a client cert and accepts any well-formed one at the TLS
  layer; authorization happens at the first control message (`Hello`): the client's
  key must already be in the paired store, or the client must be freshly enrolled.
  Enrollment always requires an explicit approval step — either a valid pairing
  proof presented alongside *interactive* host-side approval, or (headless hosts
  with no approval UI) a valid pairing proof alone. A `Hello` with no proof is
  never auto-enrolled: on a headless host it is rejected outright, and even on a
  host with an approval UI it still requires a human to accept the prompt before
  the client is committed to the paired store. Nothing is sent to a peer before
  `Hello` is verified.

## Pairing

Payload: `sbm1:` + base64url(postcard(PairInfo))

QR content is the **URL form**: `https://streambeam.eu/session/#sbm1:…` — one QR
both opens the app directly (Android App Link, verified via
`/.well-known/assetlinks.json`) and routes phones without the app to a
download/fallback page. The payload rides in the URL *fragment* so it is never
sent to the streambeam.eu server (no token in access logs). Decoders accept both
forms (`PairInfo::decode_payload` splits on the last `#`; base64url contains no
`#`). A `streambeam://pair#sbm1:…` custom-scheme fallback exists for sideloaded
builds where the https link isn't verified.

```rust
PairInfo {
    version: u8,            // 3
    host_name: String,
    host_pubkey: [u8; 32],  // Ed25519
    addrs: Vec<SocketAddr>,
    token: [u8; 16],        // one-time, valid only while the QR is displayed
    expires_unix: u64,
}
```

Enrollment (first message on control stream S0):

```
pair_proof = HMAC-SHA256(key = token,
                         msg = client_pubkey || tls_exporter("streambeam-pair", 32))
```

The TLS keying-material exporter binds the proof to this exact connection
(no replay, no MITM). Host verifies, burns the token, persists the client key,
replies `PairAck`.

## Streams

- **S0** — client-opened bidi: control (`ClientMsg` / `HostMsg`), postcard frames
  with `u16` length prefix. First client message must be `Hello`.
- **S1** — client-opened uni: `InputEvent` frames only (same framing).
- **Datagrams** — media (video ch 0, audio ch 1), unreliable.

## Media datagram: 18-byte little-endian header + shard payload

| off | field | notes |
|---|---|---|
| 0 | `u8 channel` | 0 = video, 1 = audio |
| 1 | `u8 flags` | bit0 keyframe, bit1 parity shard |
| 2 | `u16 frame_id` | wrapping, per channel |
| 4 | `u16 shard_index` | 0..k+r |
| 6 | `u16 k` | data shard count |
| 8 | `u16 r` | parity shard count |
| 10 | `u32 frame_len` | true frame byte length (strip FEC padding) |
| 14 | `u32 capture_ts_us` | host clock µs, wrapping |

Per video frame: payload split into K shards of `min(max_datagram_size − 18, 1200)`
bytes (shard length rounded to even for the RS codec), R = ceil(K × fec_ratio)
Reed-Solomon parity shards. Client recovers from any K of K+R; abandons a frame when
a frame ≥ 4 ahead completes (headroom over reordering/jitter) or after 50 ms.

Recovery is **Reference-Frame-Invalidation (RFI)**, the same surgical approach
Moonlight/Sunshine use: instead of always forcing a full IDR, the client reports
exactly which video `frame_id`s it lost so the host's encoder can skip them as
references for the next frame — a cheap P-frame instead of an expensive keyframe.
`FrameReassembler` tracks the highest completed `frame_id` as its last known-good
reference; when it abandons frame(s), it decides between two reports (see
`streambeam_core::reassemble::LossReport`):

- **`InvalidateRefFrames { first_frame, last_frame }`** — there's a known-good
  reference within `REF_WINDOW` (32) frame_ids of the loss: the encoder almost
  certainly still holds it, so ask the host to skip `[first_frame, last_frame]`
  as references instead of forcing a keyframe.
- **`RequestIdr`** (full refresh) — no known-good reference exists yet (stream
  start), or the loss run is wider than `REF_WINDOW`: the encoder's reference
  window has almost certainly moved past it, so a surgical fix can't work and a
  full keyframe is requested instead.

The host rate-limits `RequestIdr` to 1 / 250 ms and `InvalidateRefFrames` to
1 / 60 ms (cheaper to service, but still capped against a runaway client).
Real hardware-encoder invalidation (NVENC `NvEncInvalidateRefFrames`, AMF/QSV
equivalents) is M2 scope — until then every encoder falls back to
`force_idr()` behind the `VideoEncoder::invalidate_ref_frames` trait method,
so this control loop is exercised end-to-end today, just not "surgically" yet.
Audio: one 10 ms Opus packet per datagram, K=1 R=0 (Opus in-band FEC + PLC
covers loss).

## Control messages (S0)

```rust
ClientMsg { Hello { proto_version, client_name, pair_proof: Option<[u8;32]> },
            SessionRequest { video: VideoCaps { ..., display_id: Option<u32>,
                                               hdr: bool },              // v5
                             audio: AudioCaps, bitrate_kbps },
            RequestIdr, InvalidateRefFrames { first_frame: u16, last_frame: u16 },
            SetBitrate { kbps }, Ping { t_us }, Bye,
            RequestPower { action: PowerAction::{Sleep|Hibernate} } }   // v4
HostMsg   { PairAck, PairReject { reason },
            DisplayList { displays: Vec<DisplayInfo>, selected_id },
            SessionAccept { session_id, codec, width,
            height, fps, audio: AudioParams, input_mode: HostInputMode },
            CodecConfig { data },
            Rumble { pad, low, high }, HostStats { capture_us, encode_us, send_queue },
            ClipboardText { text }, Pong { t_us }, Bye,
            PowerAck,                                                    // v4
            WakeInfo { mac: [u8;6], broadcast: Option<SocketAddr> } }    // v4
```

`DisplayInfo` contains the host-platform display id, user-facing name, native
width/height and `primary` flag. A client uses `display_id = None` for the
primary display. To change displays it reconnects with the chosen id; this
keeps capture geometry, encoder configuration and absolute-input mapping on
one session boundary.

v4 (2026-07-19) adds host power control and Wake-on-LAN facts. `RequestPower`
is only honored on an authenticated session; the host replies `PowerAck`
best-effort and then suspends (the connection drops immediately after —
clients must not wait for more). `WakeInfo` is sent once after
`SessionAccept` so the client can cache the host's MAC + subnet broadcast in
`SavedHost` and send WOL magic packets later, while the host is unreachable.
Security note: the MAC travels only inside the authenticated post-`Hello`
channel — same trust level as the rest of `HostMsg` — and never in the
pairing QR payload.

v5 (2026-08-17) adds `VideoCaps::hdr`: the client can decode **and display**
HDR10 (10-bit PQ, BT.2020 primaries, BT.2020-NCL matrix). Both halves are
required — decoding 10-bit into an SDR compositor produces a worse picture than
the tone-mapped SDR it replaces, so a client that can only do one must send
`false`. The flag is permission, not instruction: the host sends passthrough
only when the client allows it *and* the desktop is genuinely HDR *and* the
codec is AV1; otherwise it tone-maps to SDR as before. When passthrough is
active the colour description travels in the AV1 sequence header (primaries 9,
transfer 16, matrix 9) rather than out of band, so a decoder that ignores this
flag still sees what it was given. See docs/HDR.md.

Framing is `postcard`, which is not self-describing, so a new field is a
breaking change: `Hello` carries `proto_version` and the host rejects anything
that isn't exactly `PROTO_VERSION`. A v4 client meeting a v5 host gets a clean
`PairReject`-style refusal rather than a misparsed struct.

## Input events (S1)

```rust
InputEvent { MouseMove { dx, dy },                       // coalesced ≤ 250 Hz
             MouseAbs { x, y, ref_w, ref_h },            // touch/mouse position; host scales
             TouchFrame { contacts, ref_w, ref_h },      // full active touch set; up to 10 contacts
             MouseButton { button, down },               // 0 left · 1 right · 2 middle · 3 back · 4 forward
             Scroll { dx, dy },                          // wheel units: ±120 = one notch; fractions allowed (trackpads)
             ClipboardText { text },                     // UTF-8 text, reliable, max 60 KB
             Key { vk, down },                           // Windows VK codes on the wire
             GamepadState { pad, buttons, lt, rt, lx, ly, rx, ry },  // full state, idempotent
             GamepadPlug { pad, connected } }
```

`HostInputMode` is `Mouse` for desktop hosts and `Touch` for Android hosts.
Touch-capable viewers send a full `TouchFrame` on every contact transition or
move when the host advertises `Touch`; because each frame is self-contained, a
coalesced move cannot leave a contact stuck. Each contact has a stable `u8` id,
`u16 x/y` in the advertised reference space, and `u16 pressure`. A mouse or
trackpad remains mouse input even for a touch host.
Android maps primary-button press/drag/release to one touch contact when its
unprivileged AccessibilityService cannot inject a native mouse cursor.

Text clipboard sync is active only for the authenticated device currently in
a stream session. Client→host updates use S1; host→client updates use S0. Both
sides de-duplicate the last applied value to prevent echo loops. Images, files,
and rich clipboard formats are not part of v3.

## Discovery

mDNS `_streambeam._udp.local`; the instance name is the host's friendly name and
the SRV record carries the port. TXT keys (see `crates/net/src/discovery.rs`):

- `pk=<base64url host Ed25519 pubkey>` — required for tap-to-connect. (Earlier
  drafts of this doc said `fp`; the implementation has always sent `pk`.)
- `os=<macos|windows|android|linux|ios>` — host platform
  (`std::env::consts::OS`), used for client UI icons and tailored connect
  instructions. Absent on old hosts; clients must tolerate that.

QR is only needed for first pairing; paired hosts reconnect via mDNS + pinned key.
