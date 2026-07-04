# Wake, sleep, and hibernate from the phone

Waking a sleeping host and putting it back to sleep, driven from the Flutter
Android client. Two independent features that get talked about as one:

| Direction | Mechanism | Difficulty |
|---|---|---|
| Phone → wake a sleeping/off host | Wake-on-LAN magic packet | Host-OS/BIOS dependent, best-effort |
| Phone → send a running host to sleep/hibernate | Control-channel RPC over the existing session | Easy, fully in our control |

See also: [PROTOCOL.md](PROTOCOL.md) (wire format) · [SYMMETRY.md](SYMMETRY.md)
(Flutter-everywhere direction — this is where the tray lives going forward) ·
[PROGRESS.md](PROGRESS.md) (decisions log)

---

## Goal

"My PC is asleep, I tap the app, it wakes up and I'm streaming in a few
seconds" is a real UX differentiator (Moonlight/Sunshine users script this
today with third-party WOL apps). We want it built in, but WOL has genuine
platform sharp edges — this doc is about not overpromising what we can't
guarantee, and keeping the always-easy half (sleep-on-command) from getting
tangled up with the sometimes-hard half (wake-on-command).

---

## Part A — sleep/hibernate on command (do this first)

The host is awake and connected; the phone asks it to suspend. No networking
problem at all — it's a new control message on the existing mTLS QUIC stream.

### Proto change

Add to `crates/proto/src/control.rs`:

```rust
pub enum ClientMsg {
    // ...existing...
    RequestPower { action: PowerAction },
}

pub enum PowerAction { Sleep, Hibernate }

pub enum HostMsg {
    // ...existing...
    PowerAck, // sent best-effort before the box actually goes down
}
```

Update `docs/PROTOCOL.md` in the same commit (repo rule).

### Host side

- Windows: `SetSuspendState(hibernate, force, disable_wake_events)` via
  `windows-sys::Win32::System::Power`. Requires `SeShutdownPrivilege` —
  acquire it once at host startup (`AdjustTokenPrivileges`), same pattern as
  any Win32 privilege-gated call.
- Gate behind the same `Hello`-verified-session authorization everything else
  uses — never a message an unauthenticated peer can trigger.
- Send `PowerAck` before calling `SetSuspendState`; the QUIC connection dies
  the instant the box suspends, so don't wait for a client ack.

### App side

A "Sleep" / "Hibernate" button in the session UI (or paired-device list),
sends `RequestPower`, shows a toast, drops the connection gracefully.

No open questions here — ship it.

---

## Part B — waking a sleeping/off host (the hard half)

### How magic packets work (recap)

A magic packet is 6× `0xFF` followed by the target MAC repeated 16×, sent as
a UDP datagram (conventionally to port 9, broadcast address). The *NIC*, not
the OS, recognizes it while in a low-power state and asserts a wake signal to
the chipset — this is why it works even when Windows itself isn't running.

### Windows-side requirements — where this gets complicated

For WOL to work at all, **all** of these have to be true simultaneously, and
we don't control most of them:

1. **NIC hardware supports it** and **BIOS/UEFI** has "Wake on LAN" /
   "Power On by PCI-E" enabled. We cannot touch BIOS settings from software —
   this is a one-time manual step we can only document, not automate.
2. **Windows device power settings**: Device Manager → NIC → Power Management
   → "Allow this device to wake the computer", and Advanced → "Wake on Magic
   Packet" = Enabled. Automatable via `powercfg /deviceenablewake "<NIC>"`
   (needs admin) — the host installer/tray *can* do this for the user instead
   of sending them into Device Manager.
3. **Fast Startup** (`HiberBoot`, on by default on most consumer Windows
   installs) turns "Shut Down" into a hybrid hibernate. Many NICs are powered
   further down in that state than in real sleep (S3), so WOL from a full
   shutdown is unreliable even with (1) and (2) correct. Practical
   consequence: **promise wake-from-sleep, not wake-from-shutdown**, unless
   the user has also disabled Fast Startup (`powercfg /h off` disables
   hibernation entirely and Fast Startup with it — a bigger ask, surface it
   as an optional toggle, not a requirement).
4. **Modern Standby (S0ix)** vs legacy **S3 sleep**: most laptops and many
   recent desktops (OEM default since ~Win10 2018+) use Modern Standby, where
   the NIC's behavior under sleep is vendor/driver-dependent and less
   consistently WOL-friendly than classic S3. `powercfg /a` reports which
   sleep states the box actually supports — worth having the host tray run
   this once and surface the result rather than silently failing later.

None of this is exotic — it's the exact matrix every WOL-based product
(including Moonlight/Sunshine community guides) runs into. The honest framing
for users: **"Wake on LAN: supported / not detected / needs setup"**, checked
and reported by the host app, not a blanket promise.

### What the host app can automate for the user

At host startup (and from a "Fix Wake-on-LAN" button), run and report:

- `powercfg /a` — confirm S3 or Modern Standby is available.
- `powercfg /deviceenablewake "<active NIC>"` — enable wake for the NIC
  (needs to already support it per (1)/(2); this only flips the Windows-side
  switch, not BIOS).
- Read back `powercfg /devicequery wake_armed` to confirm it stuck.

This needs the host process (or an elevated helper it launches) to run with
admin rights at least once — same trust boundary as installing the VIIPER
driver already required for gamepad injection (`docs/PROGRESS.md` decisions
log), so it's not a new category of ask for this user.

### Where the MAC address comes from

The client needs the host's MAC + a broadcast address to send the magic
packet — and needs it *before* the host is reachable to ask for it, so it
must be cached from a prior successful pairing, not fetched live.

Extend `SavedHost` (`crates/core/src/saved_host.rs`) with:

```rust
pub struct SavedHost {
    pub host_name: String,
    pub host_pubkey: [u8; 32],
    pub addrs: Vec<SocketAddr>,
    pub mac: Option<[u8; 6]>,           // NEW
    pub wol_broadcast: Option<SocketAddr>, // NEW — subnet broadcast, e.g. 192.168.1.255:9
}
```

The host doesn't know its own outbound MAC is meaningful to send until pairing
succeeds, so report it once enrollment completes rather than putting it in
the `PairInfo` QR payload (keep the QR minimal/pairing-only, per existing
invariant). Simplest hook: add `mac: [u8; 6]` to `HostMsg::PairAck`. Client
persists it into `SavedHost` alongside the existing `addrs`.

Getting the MAC on the Windows host: `GetAdaptersAddresses` (already the
standard way; no new dependency needed beyond `windows-sys`, which the host
already links) filtered to the adapter matching the interface the QUIC socket
is bound on.

### App side — sending the packet

No library needed — it's ~25 lines: build the 102-byte payload, open a UDP
socket with `SO_BROADCAST`, send to `wol_broadcast` (fall back to
`255.255.255.255:9` if we didn't capture a subnet broadcast). Put this in
`app/rust/src/api/` (new `wol.rs`) rather than Dart, so Android and a future
"beam-Mac-from-Mac" client share one implementation — consistent with the
"one Rust core" direction in SYMMETRY.md.

Flow when the user taps a sleeping/unreachable saved host:

1. mDNS lookup fails / connect times out.
2. Show "Host looks asleep — Wake it?" using the cached `SavedHost`.
3. Send magic packet(s) — send 2-3 with short delays, it's UDP and cheap.
4. Poll (mDNS re-browse or direct connect retry) for ~15-20s with backoff.
5. Timeout → "Didn't wake up. Check Wake-on-LAN is enabled on the PC" with a
   link/explainer, not a dead end.

### Cross-subnet / WAN wake

Out of scope here, same reasoning as [WAN.md](WAN.md): broadcast doesn't
route across a typical home router. Waking a host from outside the LAN needs
either port-forwarded UDP 9 (fragile, same objection as WAN port forwarding)
or a small always-on relay/helper on the LAN — revisit only if/when the iroh
WAN work lands, don't build a bespoke solution for this alone.

---

## Where the UI lives

Per [SYMMETRY.md](SYMMETRY.md), the legacy Win32 tray (`crates/host/src/tray.rs`)
is being superseded by the Flutter host UI (`app/`, `tray_manager` +
`window_manager`). The pairing-approval popup this task originally asked
about ("someone tries to link, I approve it") **already exists** in the
legacy tray (`WM_PAIR_REQUEST` → Yes/No dialog in `tray.rs`) — new work
(WOL status/fix button, sleep/hibernate controls) should land in the Flutter
host UI, not be added to the Win32 tray we're retiring. If the Flutter host
UI isn't yet at parity for the pairing-approval flow, porting that dialog is
a prerequisite here, not new scope invented by this doc.

---

## Phasing

| When | Work |
|---|---|
| Now / independent of milestones | Part A (sleep/hibernate RPC) — small, no risk, ship any time |
| With M7 (pairing/discovery polish) | `SavedHost` MAC field, `PairAck` MAC, magic-packet sender in `app/rust` |
| With Flutter host UI work (SYMMETRY.md) | WOL capability check + "Fix Wake-on-LAN" flow in host tray/settings |
| Post-v1 / with WAN (iroh) | Cross-subnet wake, if ever |

---

## Open questions

1. Do we gate the admin-rights `powercfg` calls behind an explicit user
   button ("Enable Wake-on-LAN"), or attempt it silently once at first host
   run? Leaning explicit — the existing VIIPER install already asks for
   elevation once; stacking a second silent elevation request is worse UX
   than one clear opt-in per feature.
2. `PairAck` MAC exposure: any concern with putting a MAC address on the
   wire? It's inside the already-authenticated post-`Hello` channel, same
   trust level as everything else in `PairAck` — no new invariant broken, but
   worth a line in PROTOCOL.md's security notes when this lands.
3. Multi-NIC hosts (Wi-Fi + Ethernet): which adapter's MAC do we report if
   both are up? Prefer the one the QUIC listener is actually bound to;
   fall back to "first adapter with a wake-capable flag" if ambiguous.

---

## Summary

- **Sleep/hibernate-on-command** is easy and safe — a new authenticated
  control message + `SetSuspendState`. Build it now.
- **Wake-on-LAN** is genuinely good UX but is bounded by BIOS + Windows power
  settings + Fast Startup/Modern Standby quirks we can't fully control. Ship
  it as a checked, reported capability ("Wake on LAN: enabled / not
  detected"), not a silent guarantee — same honesty principle as the WAN
  relay-latency caveat in [WAN.md](WAN.md).
- Needs a small proto addition (`PairAck.mac`) and a `SavedHost` extension so
  the client has something to wake *before* it can reach the host.
- New UI (WOL status, fix button, sleep/hibernate controls) belongs in the
  Flutter host UI per the Flutter-everywhere direction, not the legacy Win32
  tray being phased out.
