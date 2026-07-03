# Working on StreamBeam

Conventions for agents (and humans) working in this repo. Multiple agent
sessions often work here **concurrently** — read the git rules below before
committing.

## Orientation

Apollo/Moonlight-class game/desktop streaming with a one-QR pairing UX.
Read these before changing anything substantial:

- [docs/PLAN.md](docs/PLAN.md) — original design + milestone plan (M0–M8). Historical; don't rewrite it, PROGRESS supersedes it.
- [docs/PROGRESS.md](docs/PROGRESS.md) — **live** milestone status + decisions log. Keep it truthful.
- [docs/PROTOCOL.md](docs/PROTOCOL.md) — wire format. Must stay in lockstep with `crates/proto`.
- [docs/WAN.md](docs/WAN.md) — internet/NAT roadmap (iroh candidate). Not v1 scope.
- [site/README.md](site/README.md) — streambeam.eu deploy + Android App Links checklist.

## Documentation discipline

Plans, decisions and progress live **in the repo**, never only in an agent's
session memory. Concretely:

- Finished something on a milestone checklist → tick it in PROGRESS.md.
- Made a non-obvious choice → one dated line in the PROGRESS.md decisions log
  (what + why, absolute date).
- Changed wire types in `crates/proto` → update PROTOCOL.md in the same commit.

## Build & verify

- `cargo test` — everything except `crates/host`'s Windows paths runs on macOS.
- `cargo check -p streambeam-host` type-checks on macOS too: Windows-only deps
  are `[target.'cfg(windows)'.dependencies]` and capture/encode/inject sit
  behind traits with mock impls. Real host runtime work needs the Windows box.
- Flutter app: `cd app && flutter analyze --no-pub`. After changing
  `app/rust/src/api/`, regenerate bindings with `flutter_rust_bridge_codegen generate`.
- Visual smoke test (needs ffplay): `cargo run -p streambeam-devtools --bin streamdemo`
  or `./scripts/visual-test.sh`.
- Dev loop env vars: `STREAMBEAM_LISTEN=0.0.0.0:47989` (bind), `STREAMBEAM_NO_MDNS=1`.

## Git rules (concurrent agents!)

- Commit small and often; another session may be mid-flight in the same files.
- **Stage selectively** (`git add <file>`, or `git apply --cached` with
  filtered hunks when a file mixes your work with someone else's WIP). Never
  `git add -A` blindly, never commit work you don't recognize, never revert
  uncommitted changes that aren't yours.
- Build artifacts are gitignored (`/target`, `app/rust/target/`) — keep it that way.
- Milestone completion gets a tag (`m0`, `m1`, …).

## Invariants (don't break these silently)

- **Pairing token stays out of server logs**: QR/link form is
  `https://streambeam.eu/session/#sbm1:…` — payload in the URL *fragment* only.
  `PairInfo::decode_payload` accepts both raw and URL forms.
- **Nothing is sent to a peer before its `Hello` is verified** (host authorizes
  at the app layer, not in the TLS verifier — see PROGRESS decisions log).
- Proof comparisons are constant-time (`subtle`).
- `crates/proto` stays pure (no I/O) and `crates/core` stays FRB-free —
  `devtools` links them on macOS.
- stdout of `fakehost`/`streambeam-host` is the raw `sbm1:` payload (scripts
  pipe it); human-facing extras go to stderr.
- App signing keys ↔ `site/.well-known/assetlinks.json` must match or App Links
  silently stop verifying (release-key entry is still a placeholder).

## Known deliberate quirks

- `hwcodec` (planned M2 encoder) has **no declared license** — it stays behind
  the `VideoEncoder` trait so it can be swapped; check before public distribution.
- `docs/PLAN.md` is the approved original plan, kept as-is for context even
  where reality has since diverged (PROGRESS.md wins).
