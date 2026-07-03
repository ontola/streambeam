# StreamBeam

Low-latency game/desktop streaming (Apollo/Moonlight-class) with a radically simpler UX:
the host shows a QR code in a small native window, the phone scans it, and you're playing.
No web UI, no browser certificate warnings, no PIN dance.

- **Host:** Windows (WGC capture → NVENC/AMF/QSV hardware encode → QUIC)
- **Client:** Android (Flutter UI, Rust core, MediaCodec → Surface decode)
- **Protocol:** clean-slate over QUIC — unreliable datagrams for media with per-frame
  Reed-Solomon FEC, reliable streams for control/input, mutual TLS with pinned Ed25519 keys.

Docs: [PROTOCOL.md](docs/PROTOCOL.md) (wire format) · [PLAN.md](docs/PLAN.md) (full design & milestones) · [PROGRESS.md](docs/PROGRESS.md) (live status & decisions log)

## Workspace

| Crate | Purpose |
|---|---|
| `crates/proto` | Pure wire types, media framing, FEC. No I/O; builds everywhere. |
| `crates/net` | QUIC endpoint, Ed25519 identity, pinned-key mTLS, pairing enrollment. |
| `crates/core` | Client session logic (reassembly, IDR policy, input coalescing). FRB-free. |
| `crates/host` | Windows host binary (capture, encode, inject, egui panel). |
| `crates/devtools` | `fakehost` + `devclient` — the macOS dev loop; no Windows box needed. |
| `app/` | Flutter Android client (added in M3). |

## Dev quickstart (macOS-friendly)

```sh
cargo test                          # proto + net unit/integration tests
cargo run -p streambeam-devtools --bin fakehost      # prints a pairing payload
cargo run -p streambeam-devtools --bin devclient -- <sbm1:payload>
```
