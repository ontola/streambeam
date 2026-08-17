# HDR passthrough (plan)

Status: **planned, not implemented**. Written 2026-08-17 after establishing that
tone-mapping HDR to SDR cannot look right, no matter whose tone-map does it.

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
- **`av1c_from_sequence_header` pins 8-bit** (`high_bitdepth=0`) in
  `crates/core/src/obu.rs` — it must parse the real value instead.
