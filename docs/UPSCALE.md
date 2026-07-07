# Stream upscaling

Client-side GPU reconstruction of the decoded stream, so a 1080p (or
phone-resolution) stream looks good on a 4K/Retina display without spending
host bitrate. Client-only: no protocol or host changes.

## Shipped: MetalFX spatial (macOS watch)

`MetalFXUpscaler` in `app/macos/Runner/StreamBeamVideo.swift` runs every
decoded frame through `MTLFXSpatialScaler` before it reaches the Flutter
texture:

```
VideoToolbox decode → CVPixelBuffer (BGRA, IOSurface, Metal-compatible)
  → CVMetalTextureCache wrap → MTLFXSpatialScaler → private MTLTexture
  → blit → IOSurface-backed CVPixelBuffer (pool) → FlutterTexture
```

- **Scale factor**: up to 2× per axis, capped at the largest attached
  display's pixel size; ≤1.05× effective scale skips upscaling entirely
  (nothing to gain).
- **Private + blit**: MetalFX wants a private-storage render target, and
  Flutter needs an IOSurface — hence the intermediate texture and blit
  (one full-frame copy, cheap on Apple silicon unified memory).
- **Threading**: runs on VideoToolbox's decode callback thread with
  `waitUntilCompleted` (sub-ms GPU work); the UI thread never blocks.
- **Requirements**: macOS 13+, `MTLFXSpatialScalerDescriptor.supportsDevice`
  (Apple silicon; some late Intel/AMD GPUs). Unsupported machines silently
  pass frames through, and the toggle hides.
- **UX**: on by default; toggle in the desktop player overlay
  (`setUpscale` on the `streambeam/video_desktop` channel, returns whether
  it's actually active).

## Not (yet) doing — and why

- **`MTLFXTemporalScaler` / `MTLFXTemporalDenoisedScaler`**: built for game
  engines — they require per-pixel **motion vectors**, a **depth** buffer, and
  sub-pixel-jittered input. A decoded video stream has none of these; feeding
  zero motion ghosts badly on movement. Plausible experiment: derive coarse
  motion from H.264/HEVC macroblock motion vectors (VideoToolbox doesn't
  expose them — would need a parallel bitstream parse) or optical flow via
  `VNGenerateOpticalFlowRequest` (adds ~ms and its own errors). Parked until
  spatial proves insufficient.
- **Core ML / ANE super-resolution**: an SR CNN (Real-ESRGAN-class, or a
  lighter mobile SR net) compiled to the Neural Engine could beat MetalFX on
  quality, esp. for compression-artifact removal. Costs: model licensing,
  per-resolution compilation, and real-time 1080p60 is borderline even on ANE
  (batch-1 latency must stay <8ms to not eat the frame budget). Right shape if
  attempted: an "AI enhance" quality tier behind the same
  `upscale(CVPixelBuffer) -> CVPixelBuffer?` interface, letting frames fall
  back to spatial when the model falls behind.
- **Windows/Android parity**: same insertion point exists per platform —
  D3D11/D3D12 (AMD FSR1/DirectSR) after the desktop decode path lands on
  Windows; Android has no MetalFX equivalent (libplacebo/Vulkan would be the
  route). Out of scope for now.
