import Cocoa
import CoreVideo
import FlutterMacOS
import Metal
import QuartzCore

/// Displays decoded HDR10 frames on an EDR-capable layer.
///
/// Flutter's `Texture` path composites through the engine, which is SDR: a
/// 10-bit PQ frame handed to it gets flattened, which would undo the entire
/// passthrough pipeline at the last step. So HDR frames bypass Flutter's
/// compositor and land on a native `CAMetalLayer` instead — the arrangement the
/// EDR probe (`HdrProbeView`) validated before any of the encoder work was
/// built, when it was granted 16× headroom alongside Flutter's own UI.
///
/// The layer is configured for **ITU-R BT.2100 PQ** rather than extended-linear,
/// which keeps this cheap and lossless: the decoder already hands us PQ-encoded
/// BT.2020 samples, so the shader only has to do YCbCr → RGB and hand the code
/// values straight to the compositor. Decoding PQ to linear, converting
/// primaries and re-encoding would be three chances to get the transfer
/// function wrong for no gain — and getting it wrong looks exactly like the
/// washed-out picture this feature exists to remove.
class StreamBeamHdrView: NSView {
  private let device: MTLDevice?
  private let queue: MTLCommandQueue?
  private var pipeline: MTLRenderPipelineState?
  private var textureCache: CVMetalTextureCache?
  private let metalLayer = CAMetalLayer()
  private let lock = NSLock()
  private var pending: CVPixelBuffer?
  /// Rendering runs off the main thread. `nextDrawable()` blocks while the
  /// drawable pool is exhausted — at 120fps on the main queue that is a frozen
  /// UI, which is exactly what happened the first time this shipped. Metal
  /// supports drawing a CAMetalLayer from a background thread; only layout
  /// stays on main.
  private let renderQueue = DispatchQueue(
    label: "io.streambeam.hdr-render", qos: .userInteractive)
  /// One draw in flight at a time. Without this, a 120fps decoder queues draw
  /// blocks faster than the GPU retires them and the backlog grows forever;
  /// the newest frame is the only one worth showing anyway.
  private var drawScheduled = false
  /// Set from `layout()` on the main thread, read by the render thread.
  private var targetSize = CGSize.zero
  private(set) var framesPresented = 0
  private(set) var setupFailure: String?
  /// The GPU this view renders on, so an offscreen check can allocate its
  /// target on the same device — a texture from another one would crash on
  /// use rather than fail a comparison.
  var renderDevice: MTLDevice? { device }

  override init(frame frameRect: NSRect) {
    device = MTLCreateSystemDefaultDevice()
    queue = device?.makeCommandQueue()
    super.init(frame: frameRect)
    wantsLayer = true
    setUp()
  }

  required init?(coder: NSCoder) { nil }

  private func setUp() {
    guard let device else {
      setupFailure = "no Metal device"
      return
    }
    metalLayer.device = device
    // Half-float, because what we hand the compositor is no longer PQ code
    // values but *linear light relative to SDR white* — 1.0 is diffuse white
    // and highlights go above it, so the range needed is open-ended.
    metalLayer.pixelFormat = .rgba16Float
    // Extended-linear BT.2020 rather than PQ.
    //
    // Handing the compositor absolute PQ meant the picture's appearance was
    // whatever macOS decided to map 10000 nits onto — against headroom that
    // *moves*: measured live on 2026-08-18, this display ramped 1.42 -> 4.00
    // over frames 22-42, inside 1.2 seconds. The image visibly shifted colour
    // a moment after it appeared, with nothing upstream changing, and the
    // steady state looked washed out because nothing anchored it to SDR white.
    //
    // In extended-linear, 1.0 *is* SDR white by definition. The base image
    // therefore sits still no matter what headroom the display grants, and
    // headroom only buys highlights above 1.0 — which is what headroom is
    // supposed to be for. Keeping BT.2020 primaries also means the shader
    // doesn't have to convert them; the compositor does that, correctly, for
    // whatever panel this is.
    if #available(macOS 10.14.3, *) {
      metalLayer.colorspace = CGColorSpace(name: CGColorSpace.extendedLinearITUR_2020)
    } else {
      setupFailure = "HDR display needs macOS 10.14.3 or newer"
      return
    }
    metalLayer.wantsExtendedDynamicRangeContent = true
    // Deliberately no `edrMetadata`: it describes how to interpret a PQ or HLG
    // signal, and this layer no longer carries one. Setting it here (tried
    // first, 2026-08-18) treats the symptom — it tells macOS what to assume
    // while still leaving the mapping to macOS.
    metalLayer.framebufferOnly = true
    metalLayer.isOpaque = true
    layer = metalLayer

    var cache: CVMetalTextureCache?
    guard
      CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache) == kCVReturnSuccess,
      let cache
    else {
      setupFailure = "CVMetalTextureCacheCreate failed"
      return
    }
    textureCache = cache

    // BT.2020 non-constant-luminance, limited ("video") range — matching what
    // the host signalled and what the decoder tagged the buffers with. The
    // offsets undo the limited-range scaling (16/255 luma floor, 128/255 chroma
    // centre, expressed for 10-bit as 64/1023 and 512/1023).
    let source = """
      #include <metal_stdlib>
      using namespace metal;
      // ST.2084 constants, from the standard.
      constant float PQ_M1 = 0.1593017578125;   // 2610 / 16384
      constant float PQ_M2 = 78.84375;          // 2523 / 4096 * 128
      constant float PQ_C1 = 0.8359375;         // 3424 / 4096
      constant float PQ_C2 = 18.8515625;        // 2413 / 4096 * 32
      constant float PQ_C3 = 18.6875;           // 2392 / 4096 * 32
      // The luminance that means "white" here — i.e. what maps to 1.0, the
      // display's own SDR white.
      //
      // 100 nits, the reference-display value sRGB and BT.1886 are defined
      // against, and what macOS EDR treats 1.0 as. *Not* BT.2408's 203-nit
      // graphics white, which was tried first (2026-08-18) and rendered
      // everything half as bright as it should be: measured live, HITMAN's
      // menu peaked at 113 nits, which 203 turns into 0.55 — a white logo
      // arriving as mid-grey, which is exactly what was reported.
      //
      // 113 nits for a game's white is not an error either: HDR titles render
      // UI at their own paper-white, near 100 nits, rather than at the
      // desktop's SDR white level (228 here). So this constant has to be the
      // *display's* white, not the host's.
      constant float REFERENCE_WHITE_NITS = 100.0;
      struct VsOut { float4 pos [[position]]; float2 uv; };
      vertex VsOut v_main(uint id [[vertex_id]]) {
        float2 p = float2((id << 1) & 2, id & 2);
        VsOut o;
        o.pos = float4(p * float2(2.0, -2.0) + float2(-1.0, 1.0), 0, 1);
        o.uv = p;
        return o;
      }
      fragment float4 f_main(VsOut i [[stage_in]],
                             texture2d<float> luma [[texture(0)]],
                             texture2d<float> chroma [[texture(1)]]) {
        constexpr sampler s(filter::linear, address::clamp_to_edge);
        float y = luma.sample(s, i.uv).r;
        float2 cbcr = chroma.sample(s, i.uv).rg;
        // Limited range -> full range.
        y = (y - 64.0 / 1023.0) * (1023.0 / 876.0);
        float cb = (cbcr.x - 512.0 / 1023.0) * (1023.0 / 896.0);
        float cr = (cbcr.y - 512.0 / 1023.0) * (1023.0 / 896.0);
        // BT.2020 NCL YCbCr -> R'G'B' (still PQ-encoded).
        float r = y + 1.4746 * cr;
        float g = y - 0.16455 * cb - 0.57135 * cr;
        float b = y + 1.8814 * cb;
        float3 pq = saturate(float3(r, g, b));
        // PQ (ST.2084) EOTF: code value -> absolute luminance in nits.
        float3 e = pow(pq, 1.0 / PQ_M2);
        float3 num = max(e - PQ_C1, 0.0);
        float3 den = PQ_C2 - PQ_C3 * e;
        float3 nits = 10000.0 * pow(num / den, 1.0 / PQ_M1);
        // Anchor to SDR white: 1.0 out == diffuse white on this display, so
        // the base image cannot drift as the panel's headroom ramps. Anything
        // brighter than reference white lands above 1.0 and uses the headroom
        // that actually exists, which the compositor rolls off for us.
        return float4(nits / REFERENCE_WHITE_NITS, 1.0);
      }
      """
    do {
      let library = try device.makeLibrary(source: source, options: nil)
      let desc = MTLRenderPipelineDescriptor()
      desc.vertexFunction = library.makeFunction(name: "v_main")
      desc.fragmentFunction = library.makeFunction(name: "f_main")
      desc.colorAttachments[0].pixelFormat = .rgba16Float
      pipeline = try device.makeRenderPipelineState(descriptor: desc)
    } catch {
      setupFailure = "pipeline: \(error)"
      NSLog("%@", "StreamBeam HDR view: \(error)")
    }
  }

  /// Hand over the newest decoded frame. Called from VideoToolbox's decode
  /// thread, so it only stores the buffer and asks AppKit to draw — newest
  /// frame wins, matching the texture path's behaviour under load.
  func present(_ buffer: CVPixelBuffer) {
    lock.lock()
    pending = buffer
    let alreadyScheduled = drawScheduled
    drawScheduled = true
    lock.unlock()
    guard !alreadyScheduled else { return }
    renderQueue.async { [weak self] in self?.draw() }
  }

  override func layout() {
    super.layout()
    let scale = window?.backingScaleFactor ?? 2
    let size = CGSize(width: bounds.width * scale, height: bounds.height * scale)
    metalLayer.contentsScale = scale
    metalLayer.drawableSize = size
    lock.lock()
    targetSize = size
    let alreadyScheduled = drawScheduled
    drawScheduled = true
    lock.unlock()
    guard !alreadyScheduled else { return }
    renderQueue.async { [weak self] in self?.draw() }
  }

  private func draw() {
    lock.lock()
    let buffer = pending
    let size = targetSize
    drawScheduled = false
    lock.unlock()
    // Say why nothing is drawn rather than returning silently: "frames arrive
    // and the screen is black" with no explanation is the single most expensive
    // failure shape in this project.
    guard let buffer else { return }
    // Checked before asking for a drawable: a view that failed to set up has
    // no working layer either, and "nextDrawable returned nil" would bury the
    // real reason under a symptom.
    if let setupFailure {
      noteDrawFailure("view not initialised: \(setupFailure)")
      return
    }
    guard size.width > 0, size.height > 0 else {
      noteDrawFailure("layer has no drawable size yet")
      return
    }
    guard let drawable = metalLayer.nextDrawable() else {
      noteDrawFailure("nextDrawable returned nil")
      return
    }
    guard let cmd = encodeConversion(of: buffer, into: drawable.texture) else { return }
    cmd.present(drawable)
    cmd.commit()
    framesPresented += 1
    framesSinceMeasure += 1
    if framesSinceMeasure >= 60 || framesPresented == 1 {
      framesSinceMeasure = 0
      measureContent(buffer)
    }
    // macOS grants EDR headroom only once EDR content is actually on screen,
    // so it ramps *after* the first frames — and because PQ is absolute, the
    // same code values map to a different luminance before and after. That is
    // a picture visibly changing colour a moment after it appears, with
    // nothing upstream changing at all. Log the ramp so it can be correlated
    // with what the eye sees instead of guessed at.
    let headroomNow = liveHeadroom()
    if abs(headroomNow - lastLoggedHeadroom) > 0.05 {
      lastLoggedHeadroom = headroomNow
      NSLog(
        "%@",
        "StreamBeam: EDR headroom now \(headroomNow) (frame \(framesPresented))")
    }
    if framesPresented == 1 {
      let width = CVPixelBufferGetWidth(buffer)
      let height = CVPixelBufferGetHeight(buffer)
      let headroom = liveHeadroom()
      NSLog(
        "%@",
        "StreamBeam: first HDR frame presented — \(width)x\(height), "
          + "EDR headroom \(headroom)")
      // Headroom of 1.0 means the display is granting no extra range, so PQ —
      // which encodes absolute luminance up to 10000 nits — gets squashed
      // toward black. The picture looks broken rather than merely dim, and
      // nothing upstream is wrong, so say so plainly. Low Power Mode disabling
      // EDR is the usual cause (measured 2026-08-18: 3.68 on mains, 1.0 in
      // Low Power Mode on the same display).
      if headroom <= 1.0 {
        NSLog(
          "%@",
          "StreamBeam: display granted no EDR headroom — HDR content will look "
            + "very dark. Low Power Mode disables EDR on Apple displays; turn it "
            + "off, or stream SDR instead.")
      }
    }
  }

  /// Encode the YCbCr → R'G'B' conversion for one frame into `target`, handing
  /// back the command buffer so the caller decides what happens to it: present
  /// it to a drawable on screen, or wait on it and read the pixels back.
  ///
  /// Split out of `draw()` so the conversion can be checked offscreen. While it
  /// only ever rendered into a `CAMetalLayer` drawable no test could reach the
  /// shader at all — and a wrong matrix, mis-scaled planes or the wrong
  /// limited-range offsets each look like exactly the washed-out picture this
  /// feature exists to remove.
  func encodeConversion(of buffer: CVPixelBuffer, into target: MTLTexture) -> MTLCommandBuffer? {
    guard let pipeline, let queue, let textureCache else {
      noteDrawFailure("view not initialised: \(setupFailure ?? "unknown")")
      return nil
    }
    let width = CVPixelBufferGetWidth(buffer)
    let height = CVPixelBufferGetHeight(buffer)
    guard
      let luma = makeTexture(buffer, cache: textureCache, plane: 0, format: .r16Unorm,
        width: width, height: height),
      let chroma = makeTexture(buffer, cache: textureCache, plane: 1, format: .rg16Unorm,
        width: width / 2, height: height / 2)
    else {
      noteDrawFailure("could not wrap the frame's planes as Metal textures")
      return nil
    }

    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = target
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
    guard let cmd = queue.makeCommandBuffer(),
      let encoder = cmd.makeRenderCommandEncoder(descriptor: pass)
    else {
      noteDrawFailure("could not encode a render pass")
      return nil
    }
    encoder.setRenderPipelineState(pipeline)
    encoder.setFragmentTexture(CVMetalTextureGetTexture(luma), index: 0)
    encoder.setFragmentTexture(CVMetalTextureGetTexture(chroma), index: 1)
    encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
    encoder.endEncoding()
    // The CVMetalTexture wrappers must outlive the *GPU* work, not just the
    // encoding of it — holding them to the end of this function would let them
    // go while the frame is still in flight.
    cmd.addCompletedHandler { _ in withExtendedLifetime((luma, chroma)) {} }
    return cmd
  }

  /// EDR headroom the display is granting right now, as a multiple of SDR
  /// white. 1.0 means none: PQ encodes absolute luminance, so the picture goes
  /// dark and nothing upstream is at fault. macOS only raises this once EDR
  /// content is actually on screen, so it reads low until frames flow.
  func liveHeadroom() -> Double {
    Double(
      window?.screen?.maximumExtendedDynamicRangeColorComponentValue
        ?? NSScreen.main?.maximumExtendedDynamicRangeColorComponentValue ?? 1.0)
  }

  /// Last headroom value reported, so only real changes are logged.
  private var lastLoggedHeadroom: Double = 0

  /// What the incoming picture is actually *made of*, in absolute nits.
  ///
  /// The single number this feature kept lacking. "Washed out" and "too dim"
  /// are both consistent with a correct pipeline carrying content at the wrong
  /// luminance, and nothing on either side reported it — so three separate
  /// theories about the display got tested before anyone checked the signal.
  /// Windows composites its desktop at the SDR white level it reports (228
  /// nits on the host measured here), so that is what a white window should
  /// read as.
  private(set) var contentMedianNits: Double = 0
  private(set) var contentPeakNits: Double = 0
  private var framesSinceMeasure = 0

  /// Sample the decoded luma plane and convert PQ code values to nits.
  /// Strided hard and run rarely — this is a diagnostic, not a per-frame cost.
  private func measureContent(_ buffer: CVPixelBuffer) {
    guard CVPixelBufferGetPixelFormatType(buffer)
      == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
    else { return }
    CVPixelBufferLockBaseAddress(buffer, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0) else { return }
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
    let width = CVPixelBufferGetWidthOfPlane(buffer, 0)
    let height = CVPixelBufferGetHeightOfPlane(buffer, 0)

    var codes: [Double] = []
    codes.reserveCapacity(4096)
    for row in Swift.stride(from: 0, to: height, by: 16) {
      let line = (base + row * stride).assumingMemoryBound(to: UInt16.self)
      for col in Swift.stride(from: 0, to: width, by: 16) {
        // 10 bits in the MSBs, limited range 64...940.
        let code = Double(line[col] >> 6)
        codes.append(min(max((code - 64) / 876, 0), 1))
      }
    }
    guard !codes.isEmpty else { return }
    codes.sort()
    contentMedianNits = pqToNits(codes[codes.count / 2])
    contentPeakNits = pqToNits(codes[min(codes.count - 1, Int(Double(codes.count) * 0.99))])
  }

  /// ST.2084 EOTF, matching the shader's.
  private func pqToNits(_ code: Double) -> Double {
    let m1 = 2610.0 / 16384, m2 = 2523.0 / 4096 * 128
    let c1 = 3424.0 / 4096, c2 = 2413.0 / 4096 * 32, c3 = 2392.0 / 4096 * 32
    let e = pow(max(code, 0), 1 / m2)
    return 10000 * pow(max(e - c1, 0) / (c2 - c3 * e), 1 / m1)
  }

  private(set) var drawFailures = 0
  private(set) var lastDrawFailure: String?

  private func noteDrawFailure(_ reason: String) {
    drawFailures += 1
    lastDrawFailure = reason
    if drawFailures <= 3 || drawFailures % 240 == 0 {
      NSLog("%@", "StreamBeam HDR view: not drawing — \(reason) (#\(drawFailures))")
    }
  }

  private func makeTexture(
    _ buffer: CVPixelBuffer, cache: CVMetalTextureCache, plane: Int,
    format: MTLPixelFormat, width: Int, height: Int
  ) -> CVMetalTexture? {
    var texture: CVMetalTexture?
    let status = CVMetalTextureCacheCreateTextureFromImage(
      kCFAllocatorDefault, cache, buffer, nil, format, width, height, plane, &texture)
    return status == kCVReturnSuccess ? texture : nil
  }
}

/// Flutter platform-view plumbing. The decoder finds the live view through
/// `current`, the same pattern the EDR probe uses — a platform view is created
/// by Flutter, not by us, so the two have to meet somewhere.
class StreamBeamHdrViewFactory: NSObject, FlutterPlatformViewFactory {
  static private(set) var current: StreamBeamHdrView?

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let view = StreamBeamHdrView(frame: .zero)
    StreamBeamHdrViewFactory.current = view
    NSLog("%@", "StreamBeam: HDR view created — \(view.setupFailure ?? "ok")")
    return view
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}
