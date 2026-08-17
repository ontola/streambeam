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
  private(set) var framesPresented = 0
  private(set) var setupFailure: String?

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
    // 10-bit unorm carries PQ code values exactly; a float layer would work too
    // but costs bandwidth for precision PQ doesn't need.
    metalLayer.pixelFormat = .rgb10a2Unorm
    // BT.2100 PQ needs macOS 11; the project still targets 10.15. Hardware
    // AV1 decode (M3+) implies a far newer OS anyway, so this branch is
    // unreachable in practice — but failing loudly beats a silently wrong
    // colour space, which would look like every other washed-out bug here.
    if #available(macOS 11.0, *) {
      metalLayer.colorspace = CGColorSpace(name: CGColorSpace.itur_2100_PQ)
    } else {
      setupFailure = "HDR display needs macOS 11 or newer"
      return
    }
    metalLayer.wantsExtendedDynamicRangeContent = true
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
        return float4(saturate(float3(r, g, b)), 1.0);
      }
      """
    do {
      let library = try device.makeLibrary(source: source, options: nil)
      let desc = MTLRenderPipelineDescriptor()
      desc.vertexFunction = library.makeFunction(name: "v_main")
      desc.fragmentFunction = library.makeFunction(name: "f_main")
      desc.colorAttachments[0].pixelFormat = .rgb10a2Unorm
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
    lock.unlock()
    DispatchQueue.main.async { [weak self] in self?.draw() }
  }

  override func layout() {
    super.layout()
    let scale = window?.backingScaleFactor ?? 2
    metalLayer.contentsScale = scale
    metalLayer.drawableSize = CGSize(
      width: bounds.width * scale, height: bounds.height * scale)
    draw()
  }

  private func draw() {
    lock.lock()
    let buffer = pending
    lock.unlock()
    // Say why nothing is drawn rather than returning silently: "frames arrive
    // and the screen is black" with no explanation is the single most expensive
    // failure shape in this project.
    guard let buffer else { return }
    guard let pipeline, let queue, let textureCache else {
      noteDrawFailure("view not initialised: \(setupFailure ?? "unknown")")
      return
    }
    guard metalLayer.drawableSize.width > 0, metalLayer.drawableSize.height > 0 else {
      noteDrawFailure("layer has no drawable size yet")
      return
    }
    guard let drawable = metalLayer.nextDrawable() else {
      noteDrawFailure("nextDrawable returned nil")
      return
    }

    let width = CVPixelBufferGetWidth(buffer)
    let height = CVPixelBufferGetHeight(buffer)
    guard
      let luma = makeTexture(buffer, cache: textureCache, plane: 0, format: .r16Unorm,
        width: width, height: height),
      let chroma = makeTexture(buffer, cache: textureCache, plane: 1, format: .rg16Unorm,
        width: width / 2, height: height / 2)
    else { return }

    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = drawable.texture
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
    guard let cmd = queue.makeCommandBuffer(),
      let encoder = cmd.makeRenderCommandEncoder(descriptor: pass)
    else { return }
    encoder.setRenderPipelineState(pipeline)
    encoder.setFragmentTexture(CVMetalTextureGetTexture(luma), index: 0)
    encoder.setFragmentTexture(CVMetalTextureGetTexture(chroma), index: 1)
    encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
    encoder.endEncoding()
    cmd.present(drawable)
    cmd.commit()
    // The CVMetalTexture wrappers must outlive the GPU work.
    withExtendedLifetime((luma, chroma)) {}
    framesPresented += 1
    if framesPresented == 1 {
      NSLog(
        "%@",
        "StreamBeam: first HDR frame presented — \(width)x\(height), "
          + "EDR headroom \(window?.screen?.maximumExtendedDynamicRangeColorComponentValue ?? 1)")
    }
  }

  private var drawFailures = 0

  private func noteDrawFailure(_ reason: String) {
    drawFailures += 1
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
