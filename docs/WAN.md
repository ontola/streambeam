# WAN connectivity & iroh

How StreamBeam reaches a host over the internet, what v1 does today, and why
[iroh](https://docs.iroh.computer/) is the leading candidate for a future WAN layer.

See also: [PLAN.md](PLAN.md) (milestones) · [PROTOCOL.md](PROTOCOL.md) (pairing & wire format)

---

## Goal

Pairing UX is solved on LAN: scan QR, pinned Ed25519 key, mutual TLS, play.

**WAN** means the phone is on cellular or someone else's Wi‑Fi while the host sits
at home behind a router — often with **CG-NAT**, no public IPv4, and no appetite for
port forwarding or a VPN install. The product still needs to feel like "scan / tap
reconnect and play," not "configure your router."

Performance target over WAN remains **Moonlight-class** when the network allows it.
Reachability is a separate problem from encode/decode latency; this doc is about the
former.

---

## v1 scope: LAN first

Early milestones (M0–M4) prove transport, media, pairing, and glass-to-glass latency
on a **local network**:

| Mechanism | Role |
|---|---|
| QR `PairInfo.addrs` | Client dials known `SocketAddr`(s) from the payload |
| mDNS `_streambeam._udp.local` | Reconnect without re-scanning when on the same LAN |
| `quinn` + pinned mTLS | Single QUIC connection; datagrams for media, streams for control |
| Paired store | After first pairing, client connects by pinned key + stored host record |

This is enough for home Wi‑Fi and dev (`fakehost` on localhost). It deliberately
does **not** require NAT traversal, signaling servers, or relays.

---

## The WAN problem

Most home hosts are not directly dialable from the public internet:

```
 Phone (LTE)                    Home router (NAT)              Host PC
     │                                │                          │
     │  "connect to 203.0.113.5:47984"                           │
     │ ───────────────────────────────► ✗ no route / CG-NAT       │
```

Common workarounds in other stacks:

- **Port forwarding** — fragile; users hate it; fails on CG-NAT.
- **VPN overlay** (Tailscale, WireGuard) — works well for Moonlight power users;
  extra install and account; not "scan QR, done."
- **Cloud signaling + hole punch + relay** (Parsec, WebRTC, iroh) — account or
  pubkey-based dial; P2P when possible; encrypted relay as fallback.

StreamBeam's QR already carries **`host_pubkey`** and a one-time **`token`**. Over
LAN, addrs are enough. Over WAN, something must turn `host_pubkey` into a live QUIC
path without the user touching network settings.

---

## Requirements for a StreamBeam WAN layer

1. **Dial by host identity** — align with Ed25519 pubkey in `PairInfo`, not a
   fragile IP:port the user must discover.
2. **QUIC with datagrams** — same shape as today's protocol (see [PROTOCOL.md](PROTOCOL.md));
   no second tunnel (no TUN/VPN inside the app).
3. **Hole punching first** — direct UDP/QUIC when NAT allows; lowest latency.
4. **Relay fallback** — encrypted relay when punch fails; accept higher latency
   for the ~10% of paths that need it.
5. **Contained in `crates/net`** — `proto`, `core`, host, and client session logic
   stay unchanged; only endpoint establishment differs LAN vs WAN.
6. **Optional self-hosted relays** — for production, control relay placement and
   latency in regions we care about.

---

## iroh (candidate)

[iroh](https://github.com/n0-computer/iroh) is a Rust peer-to-peer library built on
QUIC. You connect to a **`EndpointId`** (derived from a public key) instead of an IP.
It handles NAT discovery, simultaneous hole punching, and fallback to encrypted
relays — the same broad pattern as Parsec's Signal API + STUN + P2P.

### How it connects (simplified)

```
  Client                              Relay (signaling)                    Host
    │                                       │                                │
    │  register / discover public endpoints │  register                      │
    │ ─────────────────────────────────────►│◄───────────────────────────────│
    │  exchange dial info                   │                                │
    │ ◄────────────────────────────────────►│                                │
    │                                                                         
    │  simultaneous UDP hole punch ─────────────────────────────────────────►│
    │  direct QUIC (preferred) ◄────────────────────────────────────────────►│
    │                                                                         
    │  if punch fails: relayed QUIC (encrypted, higher latency)              │
```

n0 reports hole punching succeeds in roughly **9 out of 10** real-world networks;
most bytes then flow **direct** between peers.

### Why it fits StreamBeam

| StreamBeam today | iroh |
|---|---|
| Ed25519 `host_pubkey` in QR | Dial by public key / `EndpointId` |
| `quinn` QUIC + datagrams | QUIC (via iroh's stack) + datagram API |
| Pinned mTLS in `crates/net` | Can layer mutual auth on top of established QUIC |
| "Future relay story" in PLAN | Built-in relay ecosystem + self-host option |
| Rust workspace | Rust-native, same ecosystem as the rest of the project |

Real-time video over iroh is production-tested (e.g. large-scale MoQ deployments;
[n0's iroh-live](https://github.com/n0-computer/iroh-live) for AV pipelines). We
would **not** adopt iroh-live or MoQ wholesale — StreamBeam keeps its own FEC,
framing, and session messages in `crates/proto`.

### Latency expectations

| Path | Typical extra latency | Game streaming |
|---|---|---|
| LAN direct (`quinn` to QR addrs) | Baseline | ✅ Target for M4 |
| WAN direct (hole punch) | ~3–10 ms reported overhead vs bare path | ✅ Moonlight-class band |
| WAN relay | Depends on relay RTT; often ~60–100 ms+ RTT | ⚠️ Casual play; not competitive |

Relay latency is **path-dependent**, not a fixed iroh tax. Geo-placed relays matter;
public default relays are fine for development, not necessarily for production SLA.

**Important:** relay performance is sensitive to server tuning (e.g. TCP_NODELAY on
relay control paths). Treat relay fallback as a mode we measure and monitor, not
"always good enough."

### What iroh does *not* replace

- Pairing proof (`token`, `pair_proof`, pinned keys) — still StreamBeam.
- Media framing, FEC, IDR policy, input coalescing — still `proto` / `core`.
- Capture, encode, decode — still host / Android.
- QR UX — still the host egui panel + phone scanner.

iroh is **reachability**, not the streaming protocol.

---

## Proposed architecture

Keep a thin transport boundary in `crates/net`:

```
┌─────────────────────────────────────────────────────────┐
│  host / app / core / proto   (unchanged)                │
└───────────────────────────┬─────────────────────────────┘
                            │
                    crates/net
                    ├── Identity (Ed25519, rcgen, paired store)
                    ├── Pairing enrollment (Hello / PairAck)
                    └── Transport trait
                          ├── LanTransport   — quinn to PairInfo.addrs (v1)
                          └── WanTransport   — iroh dial by host_pubkey (future)
```

After `Transport::connect` returns a QUIC connection, the rest of the stack is
identical. Reconnect on LAN can keep using mDNS; reconnect over WAN uses stored
pubkey + iroh (no need to embed routable IPs in QR for return visits).

### QR over WAN

First-time pairing away from home is the hard case (phone must reach host once to
exchange keys). Options to evaluate when implementing WAN:

- Host online with iroh endpoint active; QR still shown for **token + pubkey**;
  addrs in QR become optional hints only.
- Short-lived pairing assist via relay (host and phone both outbound-only).
- User completes first pair on LAN, then "works everywhere" — acceptable v1.1
  compromise but weak for true cold-start remote setup.

---

## Phasing

| Milestone | WAN work |
|---|---|
| M0–M4 | None required. LAN / localhost; validate latency and protocol. |
| M7 (pairing polish) | Reconnect paths; optional iroh spike behind `Transport` trait. |
| Post‑v1 | WAN as product feature: iroh integration, relay deployment, WAN QA matrix. |

We should **not** block LAN end-to-end on iroh. We **should** avoid painting
`crates/net` into a corner where only literal `SocketAddr` dial is possible.

---

## Alternatives considered

| Approach | Pros | Cons |
|---|---|---|
| **Status quo + port forward docs** | Simple | Poor UX; fails on CG-NAT |
| **Require Tailscale / VPN** | Proven for Moonlight community | Extra install; not StreamBeam UX |
| **Custom STUN + TURN + signaling** | Full control | Large effort; operate TURN at scale |
| **Parsec-style proprietary relay** | Polished WAN UX | Closed ecosystem; not self-host friendly |
| **iroh** | Rust, QUIC, dial-by-key, punch + relay | External dependency; relay ops for production |

---

## Open questions

1. **First pair off-LAN** — can QR + iroh outbound-only on both sides suffice without
   ever exposing a host IP in the payload?
2. **Identity mapping** — map StreamBeam Ed25519 keys to iroh `EndpointId` 1:1 or
   derive one from the other?
3. **mTLS story** — keep rcgen pinned certs on the iroh QUIC connection, or rely on
   iroh's transport crypto plus StreamBeam `Hello` authorization only?
4. **Relay operator** — public n0 relays for beta; self-hosted or managed for release?
5. **Android** — confirm iroh endpoint + datagram performance on target phones early in
   the WAN spike.

---

## Summary

- **v1** targets LAN: QR addrs, mDNS, direct `quinn`. No WAN dependency.
- **WAN** needs dial-by-pubkey and NAT traversal without a VPN.
- **iroh** is a strong fit for that layer: hole punching for low latency, relays for
  reachability, Rust/QUIC alignment with the existing design.
- **Performance** over WAN is still dominated by encode/decode and network RTT;
  iroh's job is to make the path exist. Relay fallback is the slow path — plan for it,
  measure it, and place relays deliberately if WAN is a ship criterion.
