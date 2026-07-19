# Streaming the Windows lock / login / UAC screen (the "secure desktop")

Goal (user, 2026-07-19): *"I wake my PC from my Mac, it's at the login screen,
and I want to type my password there — like TeamViewer."* Also covers the
idle-lock case we kept hitting all session: the machine locks itself while
unattended and the stream goes blind.

This is the single biggest architectural item in the host. It is **not** a
patch on the current host — it needs a privileged Windows **service**. This
doc scopes it honestly before any code lands (repo rule: plan in-repo first).

## Why the current host can't do it

Today the host is the Flutter desktop app (`app/`), running as **you**, in
your **interactive session on the default desktop** (`Winsta0\Default`).
Windows deliberately walls off three things from any such process:

- The **lock screen, login screen (`Winlogon` desktop), and UAC prompts
  (`Winlogon\Secure Desktop`)** live on *different desktops* in the same
  session. A process on `Default` cannot read pixels from or inject input
  into them. This is a security boundary, not a bug — it's what stops
  ordinary apps from scraping your password field.
- DXGI Desktop Duplication returns `ACCESS_LOST` the moment the session
  locks (what we've been seeing all day), because *our desktop* is no longer
  the input desktop.
- `SendInput` / our injection goes to whatever desktop *our thread* is
  attached to — never the secure one.

TeamViewer, AnyDesk, Sunshine, and Chrome Remote Desktop all solve this the
same way: a **service running as `LOCAL SYSTEM`** that can enumerate and
*switch onto* whichever desktop is currently the input desktop
(`OpenInputDesktop` → `SetThreadDesktop`), including `Winlogon`.

## Target architecture

```
┌─ streambeam-svc  (Windows service, LOCAL SYSTEM, auto-start) ─────────┐
│   • owns the QUIC listener, pairing store, host identity              │
│   • every capture/encode/inject tick: OpenInputDesktop() and          │
│     SetThreadDesktop() onto the current input desktop, so it follows   │
│     Default → Winlogon(lock/login) → Secure(UAC) automatically         │
│   • runs in Session 1 (the console session), NOT Session 0 — a         │
│     SYSTEM service can be told to launch/attach into the active        │
│     console session; Session 0 is headless and cannot see the console  │
│     desktop at all (the classic "Interactive Services" dead end)       │
└───────────────────────────────────────────────────────────────────────┘
        ▲ named pipe / localhost control
┌─ StreamBeam.exe  (the Flutter app, unchanged, runs as the user) ──────┐
│   • pairing UI, "This device", viewer list, settings                  │
│   • talks to the service for status; installs/starts it (elevated)    │
└───────────────────────────────────────────────────────────────────────┘
```

Key point: the **service is the host**; the app becomes a control panel for
it. That's a real refactor — `crate::host::start` currently spins the
listener inside the app process.

### Capturing the secure desktop specifically

- DXGI Desktop Duplication **can** capture `Winlogon`/lock/UAC **only** from a
  process that (a) runs as SYSTEM and (b) has done `SetThreadDesktop` onto
  that desktop before creating the D3D device + duplication. Our capture
  thread already isolates all its COM on one thread (`capture.rs`), which is
  the right shape — it needs a per-reacquire "am I still on the input
  desktop? if not, re-attach and rebuild the duplication" step. This folds
  naturally into the existing `ACCESS_LOST` recovery path (that error is
  *exactly* the "desktop switched under me" signal).
- Some GPUs/drivers refuse Desktop Duplication on the secure desktop and need
  the WinRT `Windows.Graphics.Capture` path instead; Sunshine keeps both.
  Treat DXGI as primary, WGC as the documented fallback (mirrors how
  `scale.rs`/`nvenc.rs` already keep a fallback per stage).

### Injecting the password

Once the capture thread is on the `Winlogon` desktop, the **input thread must
be on the same desktop** (`SetThreadDesktop` there too) for `SendInput` to
reach the password box. Our injection already lives behind a trait
(`input_inject.rs`); it gains a "follow the input desktop" wrapper. No new
wire protocol — the client already sends VK key events (`send_key`), which is
all the login box needs.

## Phasing (each independently shippable)

| Phase | Work | Verifies |
|---|---|---|
| **0 — interim, no service** | Offer, as an explicit opt-in setting, disabling *require-sign-in-on-wake* + enabling auto-logon so a *wake* lands on the unlocked desktop. Doesn't stream the login screen, but closes the wake→use loop today. Pure `powercfg`/registry, reversible from the same toggle. | The everyday "wake and use it" case |
| **1 — service scaffolding** | `streambeam-svc` crate: install/uninstall/start/stop (SCM), runs as SYSTEM, launches into the active console session, hosts the existing listener. App detects + installs it (one elevation prompt, same trust ask as the VIIPER gamepad driver). No secure-desktop capture yet — just "host survives logout / runs without the app open." | Host is up before login, app is just a panel |
| **2 — follow the input desktop** | Capture + inject threads `OpenInputDesktop`/`SetThreadDesktop` each tick; fold into `ACCESS_LOST` recovery. This is the actual "see and type into the lock/login/UAC screen." | The TeamViewer behavior the user asked for |
| **3 — polish** | WGC capture fallback for GPUs that refuse DXGI on secure desktop; "Fix Wake-on-LAN" and service-health UI; UAC-elevation visibility indicator for the viewer. | Robustness across machines |

## Risks / honesty

- **Installs a SYSTEM service** — persistent, privileged, survives reboot.
  Biggest trust ask in the product; must be an explicit, clearly-labelled
  opt-in with a matching uninstall, not silent.
- **Can't be verified without real reboots and lock/unlock cycles** on the
  physical machine — no macOS/WSL type-check substitute for phases 2–3.
- **Security surface**: a SYSTEM process that can be driven to type into the
  login box is exactly as sensitive as it sounds. Every existing invariant
  (pairing-gated, nothing acted on before `Hello` verified, constant-time
  proof compare) must hold *before* any secure-desktop input is honored — and
  arguably secure-desktop input should require an already-paired client, no
  first-time pairing from the lock screen.
- **Anti-cheat / EDR**: some environments flag SYSTEM services that switch
  desktops. Fine for a personal machine; note it before any wider use.

## Non-goals here

- Cross-WAN wake/unlock (see WAN.md — same routing caveats).
- Running the *Flutter app itself* as a service (it stays the user-session
  control panel; only the headless host logic moves into the service).

## Testing phases 1+2 (do this AT the machine, elevated)

Phases 1 and 2 are built (`streambeam-svc`, `desktop_win.rs`). Verifying them
needs an Administrator prompt and reboots/lock cycles — never install this
remotely over the only control path.

Two gotchas to know first:
- **Port collision**: the service's worker binds `47989`, same as the in-app
  host. Quit the StreamBeam app (or stop its host) before starting the
  service, or one will fail to bind.
- **Separate identity (phase-1.5 gap)**: the SYSTEM worker reads
  `%APPDATA%` as `C:\Windows\System32\config\systemprofile\...`, a *different*
  host identity + pairing store than the logged-in app. So the first
  service-hosted session needs a fresh pair from the client. Sharing the
  logged-in user's data dir (so pairing carries over) is the next task.

```powershell
# From an elevated PowerShell, in the build output dir with both exes:
#   streambeam-svc.exe, streambeam-host.exe
.\streambeam-svc.exe install          # registers the SYSTEM auto-start service
.\streambeam-svc.exe start            # launches a worker in the console session
# Watch C:\ProgramData\streambeam\svc.log for "worker launched in session N".
# Then from the client: pair fresh, start a stream — expect normal desktop.
# Lock the PC (Win+L): capture should keep going (svc.log shows re-follow /
# the worker re-attaching), and you should see + be able to type into the
# login box from the client.
.\streambeam-svc.exe stop
.\streambeam-svc.exe uninstall        # full removal
```

If capture goes black specifically on the secure desktop (some GPUs/drivers
refuse DXGI Duplication there), that's the known phase-3 item: add the WinRT
`Windows.Graphics.Capture` fallback.

## Relationship to other docs

- [WOL.md](WOL.md) — waking the box; this doc is what makes a woken box
  *usable* when it comes up locked. They ship together as the "remote power"
  story.
- [SYMMETRY.md](SYMMETRY.md) — the legacy Win32 tray is being retired; the
  service is headless, its UI is the Flutter app, consistent with that.
