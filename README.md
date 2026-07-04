# StreamBeam

Low-latency game/desktop streaming (Apollo/Moonlight-class) with a radically simpler UX:
the host shows a QR code in a small native window, the phone scans it, and you're playing.
No web UI, no browser certificate warnings, no PIN dance.

- **Host:** Windows (WGC capture → NVENC/AMF/QSV hardware encode → QUIC)
- **Client:** Android (Flutter UI, Rust core, MediaCodec → Surface decode)
- **Protocol:** clean-slate over QUIC — unreliable datagrams for media with per-frame
  Reed-Solomon FEC, reliable streams for control/input, mutual TLS with pinned Ed25519 keys.
- **One QR does everything:** the host's QR is a link
  (`https://streambeam.eu/session/#sbm1:…`). Phone with the app → opens the stream
  directly (Android App Link). Phone without → download page, then connects. The
  pairing code rides in the URL *fragment*, so it never reaches the web server.

Docs: [PROTOCOL.md](docs/PROTOCOL.md) (wire format) · [PLAN.md](docs/PLAN.md) (original design & milestones) · [PROGRESS.md](docs/PROGRESS.md) (live status & decisions log) · [WAN.md](docs/WAN.md) (internet/NAT roadmap) · [SYMMETRY.md](docs/SYMMETRY.md) (symmetric apps / Flutter-everywhere direction) · [COMPETITION.md](docs/COMPETITION.md) (competitive landscape) · [AGENTS.md](AGENTS.md) (working conventions)

## Workspace

| Crate | Purpose |
|---|---|
| `crates/proto` | Pure wire types, media framing, FEC. No I/O; builds everywhere. |
| `crates/net` | QUIC endpoint, Ed25519 identity, pinned-key mTLS, pairing enrollment. |
| `crates/core` | Client session logic (reassembly, IDR policy, input coalescing). FRB-free. |
| `crates/host` | Windows host binary (WGC capture, encoder trait + mock, input injection, tray icon, terminal/GUI QR). LAN server works today with mock encoder. |
| `crates/devtools` | `fakehost`, `devclient`, `playclient`, `streamdemo`, `hostdisc` — the macOS dev loop; no Windows box needed. |
| `app/` | Flutter Android client (Rust core via flutter_rust_bridge). |
| `site/` | streambeam.eu — landing page, `session/` deep-link fallback, `assetlinks.json`. Static, no build step (see [site/README.md](site/README.md)). |

## Watch it stream (macOS)

Requires [ffmpeg](https://ffmpeg.org/) (`ffplay` on your PATH — `brew install ffmpeg`).

**One command** — starts fakehost, opens a video window, plays stereo audio:

```sh
cargo run -p streambeam-devtools --bin streamdemo
```

Close the ffplay window or Ctrl+C to stop.

### Windows / LAN host → Android phone

On the **host PC** (Windows, or macOS for dev):

```sh
cargo run -p streambeam-host --release
# or: ./scripts/run-host.sh
```

This listens on `0.0.0.0:47989`, prints a **QR code** and `sbm1:…` payload, and advertises via mDNS.

On **Android** (same Wi‑Fi):

1. `cd app && flutter run` (release build recommended on device)
2. **Watch** tab → **Scan QR** from the host terminal → **Play**

Allow the Windows firewall inbound rule for UDP/TCP port **47989** when prompted.

### Two machines (A → B on your LAN)

Terminal A (host):

```sh
STREAMBEAM_LISTEN=0.0.0.0:47989 cargo run -p streambeam-devtools --bin fakehost
```

Copy the `sbm1:…` line it prints.

Terminal B (client):

```sh
cargo run -p streambeam-devtools --bin playclient -- 'sbm1:…'
```

### Manual / headless

```sh
# Terminal 1
cargo run -p streambeam-devtools --bin fakehost

# Terminal 2 — stats + hash verify (no playback)
cargo run -p streambeam-devtools --bin devclient -- 'sbm1:…' --frames 60

# Terminal 2 — playback
cargo run -p streambeam-devtools --bin playclient -- 'sbm1:…'
```

### Android client (M3 scaffold)

See [app/README.md](app/README.md). Pairs with fakehost and reports stream stats from Rust.

```sh
cargo test   # proto + net + e2e pairing/stream smoke tests
```
