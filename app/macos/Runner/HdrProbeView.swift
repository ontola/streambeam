import Cocoa
import FlutterMacOS
import Metal
import QuartzCore

/// Can HDR actually reach the screen through this app?
///
/// This is the gating unknown for HDR passthrough (see `docs/HDR.md`): capture
/// and encode can be made 10-bit PQ, but if the picture can only be composited
/// as SDR then none of that reaches the user. Flutter draws through its own
/// engine-owned layer, which is SDR, so the plan assumes a native platform view
/// owning an EDR-capable `CAMetalLayer` — this proves or disproves that
/// assumption before any of the encoder or protocol work is built on it.
///
/// It renders a horizontal ramp from 0 to `peak` in EDR units, where 1.0 is SDR
/// white, with a hard marker line at exactly 1.0. If EDR works, everything right
/// of the marker is visibly brighter than the brightest white the rest of the UI
/// can produce. If it doesn't, the ramp simply clips to white at the marker and
/// the right half is a flat block.
///
/// Deliberately synthetic: no decoder, no network, no host. A negative result
/// here is worth knowing before touching NVENC.
class HdrProbeView: NSView {
  /// Peak in EDR units. 8 is beyond most displays' sustained headroom, which is
  /// the point — the ramp should visibly keep climbing past 1.0 and then stop
  /// wherever this display actually runs out.
  private let peak: Float = 8.0

  private let device: MTLDevice?
  private let queue: MTLCommandQueue?
  private var pipeline: MTLRenderPipelineState?
  private let metalLayer = CAMetalLayer()

  /// What the probe found, reported back to Dart so the answer is recorded
  /// rather than left to eyeballing a screenshot.
  private(set) var report: [String: Any] = [:]

  override init(frame frameRect: NSRect) {
    device = MTLCreateSystemDefaultDevice()
    queue = device?.makeCommandQueue()
    super.init(frame: frameRect)
    wantsLayer = true
    setUpMetal()
  }

  required init?(coder: NSCoder) { nil }

  private func setUpMetal() {
    guard let device else {
      report = ["ok": false, "reason": "no Metal device"]
      return
    }
    metalLayer.device = device
    // Half-float is the point: an 8-bit layer has nowhere to put values above
    // 1.0, so EDR needs a float (or 10-bit) surface.
    metalLayer.pixelFormat = .rgba16Float
    metalLayer.wantsExtendedDynamicRangeContent = true
    // Extended-range linear Display P3: values above 1.0 mean "brighter than
    // SDR white" rather than clipping.
    metalLayer.colorspace = CGColorSpace(name: CGColorSpace.extendedLinearDisplayP3)
    metalLayer.framebufferOnly = true
    layer = metalLayer

    let source = """
      #include <metal_stdlib>
      using namespace metal;
      struct VsOut { float4 pos [[position]]; float2 uv; };
      vertex VsOut v_main(uint id [[vertex_id]]) {
        float2 p = float2((id << 1) & 2, id & 2);
        VsOut o;
        o.pos = float4(p * 2.0 - 1.0, 0, 1);
        o.uv = p;
        return o;
      }
      fragment float4 f_main(VsOut i [[stage_in]], constant float &peak [[buffer(0)]]) {
        float v = i.uv.x * peak;
        // Marker at SDR white: a dark notch, so the eye has a reference for
        // where 1.0 is without needing a colour picker.
        if (abs(i.uv.x - 1.0 / peak) < 0.002) { return float4(0, 0, 0, 1); }
        return float4(v, v, v, 1);
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
      report = ["ok": false, "reason": "pipeline: \(error)"]
      NSLog("%@", "StreamBeam HDR probe: \(error)")
    }
  }

  override func viewDidMoveToWindow() {
    super.viewDidMoveToWindow()
    guard let screen = window?.screen else { return }
    report = [
      "ok": pipeline != nil,
      "peakRequested": peak,
      // The headroom the display is *currently* granting. macOS ramps this up
      // once EDR content is on screen, so reading it after a draw is the real
      // answer; a value > 1.0 means EDR is genuinely live.
      "edrHeadroom": screen.maximumExtendedDynamicRangeColorComponentValue,
      "edrPotential": screen.maximumPotentialExtendedDynamicRangeColorComponentValue,
      "screen": screen.localizedName,
      "wantsEDR": metalLayer.wantsExtendedDynamicRangeContent,
    ]
    NSLog("%@", "StreamBeam HDR probe: \(report)")
    draw()
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
    guard let pipeline, let queue, let drawable = metalLayer.nextDrawable() else { return }
    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = drawable.texture
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
    guard let buffer = queue.makeCommandBuffer(),
      let encoder = buffer.makeRenderCommandEncoder(descriptor: pass)
    else { return }
    var peakValue = peak
    encoder.setRenderPipelineState(pipeline)
    encoder.setFragmentBytes(&peakValue, length: MemoryLayout<Float>.size, index: 0)
    encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
    encoder.endEncoding()
    buffer.present(drawable)
    buffer.commit()
  }

  /// Re-read the display's granted headroom after content has been on screen —
  /// this is the number that actually answers "did EDR engage".
  func liveHeadroom() -> Double {
    Double(window?.screen?.maximumExtendedDynamicRangeColorComponentValue ?? 1.0)
  }
}

/// Flutter platform-view plumbing for [HdrProbeView].
class HdrProbeViewFactory: NSObject, FlutterPlatformViewFactory {
  static var latest: HdrProbeView?

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let view = HdrProbeView(frame: .zero)
    HdrProbeViewFactory.latest = view
    return view
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}
