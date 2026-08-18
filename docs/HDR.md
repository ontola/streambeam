# HDR passthrough (plan)

Status (2026-08-17): **stages 1–3 done and verified; stage 4 outstanding**, so
nothing reaches a user's screen yet. Written after establishing that
tone-mapping HDR to SDR cannot look right, no matter whose tone-map does it.

| Stage | State |
|---|---|
| 4. EDR display *(prototyped first, as the gating risk)* | **EDR proven reachable** — a native `CAMetalLayer` platform view got 16× headroom granted alongside Flutter's SDR UI. Routing *decoded video* onto that layer is the remaining work. |
| 1. Host encodes HDR10 | **done** — verified by parsing the sequence header off the wire: `bit_depth=10, primaries=9, transfer=16, matrix=9` at 4K |
| 2. Negotiation | **done** — `VideoCaps::hdr`, proto v5; host sends passthrough only when the client allows it, the desktop is HDR, and the codec is AV1 |
| 3. macOS 10-bit decode | **done** — VideoToolbox outputs `x420` with the PQ tag intact, tested against a real 4K 10-bit keyframe |
| Remaining | plumb the bitstream-derived HDR flag through FRB to `configure(hdr10:)`, and render decoded frames on the EDR layer instead of a Flutter `Texture` |

## Why

An HDR desktop streams washed out: blacks lifted, saturated colours pulled
toward grey. Measured off two screenshots of the same UI, converted to linear
light: background **3.5× brighter**, saturated red **3.3× darker** — contrast
crushed from both ends.

The important result is a negative one. We A/B'd our in-shader tone-map against
Windows' own (`STREAMBEAM_HDR_CAPTURE=dwm`, two hosts, same content, one on
`WGC … (FP16 scRGB)` and one on `WGC … (BGRA8)`) and they were **visually
identical**. So this is not a bug in `scale.rs`'s `ps_tonemap`; it is what
HDR→SDR conversion does when the target has nowhere to put the highlights.
Improving the curve (BT.2390 roll-off, saturation compensation) would recover
some punch, but it can't reproduce an HDR image on an SDR pipeline.

The pipeline is 8-bit end to end, and every stage has to change:

| Stage | Today | Needed |
|---|---|---|
| Capture | FP16 scRGB (linear, P709) — already HDR | unchanged |
| Tone-map | `ps_tonemap` → SDR sRGB | bypassed when passthrough is on |
| Encoder input | `NVENC_BUFFER_FORMAT_ARGB` (8-bit) | `ABGR10` (R10G10B10A2) |
| Encoder | 8-bit H.264 / AV1 | HEVC Main10 or AV1 10-bit |
| Signalling | none | BT.2020 primaries, PQ transfer, BT.2020-NC matrix |
| Wire | no HDR concept | negotiated capability + colour info |
| Decode (macOS) | `kCVPixelFormatType_32BGRA` | 10-bit biplanar (`x420`) |
| Display | Flutter `Texture` | EDR-capable layer — **the risky part** |

## Design

Capture already hands us exactly what's needed: FP16 scRGB, linear, 1.0 = 80
nits. Passthrough replaces the tone-map with a colour-space conversion:

1. linear scRGB (P709 primaries) → linear BT.2020 via a 3×3 matrix
2. scale by `80/10000` so 1.0 = 10000 nits (PQ's reference range)
3. PQ (ST.2084) encode
4. write `R10G10B10A2_UNORM`, hand to NVENC as `ABGR10`

NVENC does RGB→YCbCr itself, so no YUV shader is needed. It must be told the
colour description or the client cannot know what it received: primaries 9
(BT.2020), transfer 16 (PQ), matrix 9 (BT.2020-NC). Mastering-display and
MaxCLL metadata are optional and can come later.

## Staging

Each stage is independently verifiable — a whole-feature big-bang here would be
undebuggable, and this project has paid for that lesson.

**1. Host encodes HDR10.** Shader variant + `ABGR10` + 10-bit codec config.
Verify with `devclient --dump` and `ffprobe`: expect `pix_fmt=yuv420p10le`,
`color_primaries=bt2020`, `color_transfer=smpte2084`, `color_space=bt2020nc`.
No client changes; the existing 8-bit client will show garbage, which is
expected and is why stage 2 gates it.

**2. Negotiation.** Client advertises HDR decode+display capability in
`VideoCaps`; host only switches when the client asked *and* the desktop is
actually HDR. Everyone else keeps today's tone-mapped SDR path, which must
remain the default and the fallback. Wire changes land with PROTOCOL.md in the
same commit.

**3. macOS decode.** Ask VideoToolbox for a 10-bit biplanar output format and
carry the colour attachments through. Testable in isolation via
`RunnerTests` with a captured 10-bit fixture, the same way the AV1 decode test
works today.

**4. macOS display.** The genuinely uncertain stage: Flutter's `Texture` path
composites through the engine, which is SDR. Reaching EDR likely means a native
platform view owning a `CAMetalLayer` with
`wantsExtendedDynamicRangeContent = true` and its own EDR colour space, drawn
outside Flutter's compositor. Prototype this *before* committing to the rest —
if HDR can't reach the screen, stages 1–3 buy nothing.

Built (`StreamBeamHdrView`) and now verified by numbers rather than by eye:
`encodeConversion(of:into:)` renders the same shader into an ordinary texture,
so `StreamBeamHdrRenderTests` can read the PQ code values back. What that
proves — the conversion, the limited-range expansion, the plane scaling, that
a real decoded keyframe comes out as a picture — and what it can't: whether
the *display* grants EDR headroom, which is a property of the screen and the
power mode, not of this code. For that, the sidebar reports live headroom
during a stream (see "Verifying it works" below).

## Risks

- **Flutter + EDR is unproven here.** Stage 4 may force a platform view, which
  changes how the player screen is composed (input overlays, the sidebar, the
  upscaler all currently sit over a `Texture`).
- **MetalFX upscaling** runs on the decoded buffer and is wired for BGRA; a
  10-bit path needs it re-checked or disabled for HDR.
- **Bitrate.** 10-bit PQ at 4K120 needs more headroom than today's 60 Mbps
  target for the same perceived quality; the adaptive controller assumes 8-bit
  characteristics.
- **Codec support.** AV1 10-bit needs Ada+ on the host and M3+ on the client;
  HEVC Main10 is the wider-compatibility choice but the host's HEVC path is
  unexplored. Falling back to tone-mapped SDR must stay automatic.
- ~~`av1c_from_sequence_header` pins 8-bit~~ — **fixed**: `parse_sequence_header`
  reads bit depth, subsampling and the colour description, and `av1C` uses them.
  It refuses (falls back to the old constants) when `timing_info_present`, which
  needs `uvlc` and decoder-model state to skip exactly.
- **MetalFX upscaling is disabled for HDR** rather than adapted: its textures
  are `bgra8Unorm`. A 10-bit path needs its own pipeline.
- **A mid-session SDR↔HDR desktop flip** currently keeps the negotiated
  passthrough pipeline: the shader falls back to a plain blit, so SDR content
  lands in a PQ-signalled stream. Rebuilding the pipeline on that transition is
  the real fix.

## Verifying it works

Three separate questions, and conflating them is what made HDR hard to judge:

1. **Does the host send HDR10?** `devclient --av1 --hdr --dump` then `ffprobe`:
   expect `pix_fmt=yuv420p10le`, `color_transfer=smpte2084`,
   `color_primaries=bt2020`. Requires the Windows desktop to be in HDR mode —
   the host refuses to switch otherwise, by design.
2. **Does the client decode and convert it correctly?** `xcodebuild test
   -only-testing:RunnerTests` — `StreamBeamVideoDecodeTests` covers the decode,
   `StreamBeamHdrRenderTests` the shader. No host, no network, no eyes.
3. **Does the screen actually show it?** This is the only part that needs a
   live stream, because it depends on the display. Launch with
   `flutter run -d macos --dart-define=STREAMBEAM_HDR=true` (passthrough is
   opt-in), connect, and open the sidebar: the HDR row reports
   frames drawn and the headroom granted. Above ~1.5x means real EDR is in
   play. **1.0 means none**, and the usual cause is Low Power Mode, which
   disables EDR on Apple displays — measured on the same MacBook Pro the same
   day: 3.68x on mains, 1.0 in Low Power Mode. A game is not required and is
   the worst thing to test with; the Windows desktop in HDR mode showing any
   HDR content is enough.
