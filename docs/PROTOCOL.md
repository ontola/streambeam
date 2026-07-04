# StreamBeam protocol v1

Everything runs over a single QUIC connection (client-initiated), mutual TLS with
pinned Ed25519 keys. `PROTO_VERSION = 1`.

## Identity & transport security

- Both sides hold a long-term Ed25519 identity key, wrapped in a self-signed X.509
  cert (rcgen) presented in the TLS handshake.
- The **client** verifies the host cert's SPKI public key equals the pinned key
  (from the QR on first contact, from `hosts.json` thereafter). No CAs, no hostnames.
- The **host** requires a client cert and accepts any well-formed one at the TLS
  layer; authorization happens at the first control message (`Hello`): the client's
  key must be in the paired store, or the message must carry a valid pairing proof.
  Nothing is sent to a peer before `Hello` is verified.

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
    version: u8,            // 1
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
a frame ≥ 2 ahead completes or after 50 ms, then sends `RequestIdr` (host rate-limits
IDR to 1 / 250 ms). Audio: one 10 ms Opus packet per datagram, K=1 R=0 (Opus in-band
FEC + PLC covers loss).

## Control messages (S0)

```rust
ClientMsg { Hello { proto_version, client_name, pair_proof: Option<[u8;32]> },
            SessionRequest { video: VideoCaps, audio: AudioCaps, bitrate_kbps },
            RequestIdr, SetBitrate { kbps }, Ping { t_us }, Bye }
HostMsg   { PairAck, PairReject { reason }, SessionAccept { session_id, codec, width,
            height, fps, audio: AudioParams }, CodecConfig { data },
            Rumble { pad, low, high }, HostStats { capture_us, encode_us, send_queue },
            Pong { t_us }, Bye }
```

## Input events (S1)

```rust
InputEvent { MouseMove { dx, dy },                       // coalesced ≤ 250 Hz
             MouseAbs { x, y, ref_w, ref_h },            // touch; host scales
             MouseButton { button, down }, Scroll { dx, dy },
             Key { vk, down },                           // Windows VK codes on the wire
             GamepadState { pad, buttons, lt, rt, lx, ly, rx, ry },  // full state, idempotent
             GamepadPlug { pad, connected } }
```

## Discovery

mDNS `_streambeam._udp.local`, TXT: `v=1`, `name=...`, `fp=<b64 host pubkey>`, `port=...`.
QR is only needed for first pairing; paired hosts reconnect via mDNS + pinned key.
