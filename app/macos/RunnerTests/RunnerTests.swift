import AVFoundation
import Cocoa
import FlutterMacOS
import Metal
import XCTest

@testable import streambeam

/// Minimal stand-in for Flutter's texture registry so the decoder can be built
/// without an engine.
private final class StubTextureRegistry: NSObject, FlutterTextureRegistry {
  var frameAvailableCount = 0

  func register(_ texture: FlutterTexture) -> Int64 { 1 }
  func textureFrameAvailable(_ textureId: Int64) { frameAvailableCount += 1 }
  func unregisterTexture(_ textureId: Int64) {}
}

/// Does the platform decoder actually turn our bitstream into pixels?
///
/// This is the test the project spent most of its debugging time not having.
/// `crates/devtools/tests/e2e_stream.rs` covers pairing, FEC, loss sweeps and
/// hash verification — everything *up to* the decoder — and passed happily
/// through every black screen, because `devclient` counts and hashes bytes but
/// never decodes them. Two of the worst bugs lived in exactly that blind spot:
///
///   - `makeSession` lost its `session = sess` assignment, so every frame was
///     silently discarded (720 dropped / 720 calls) while the app reported
///     "Streaming…" over a black texture.
///   - `av1_access_unit_without_redundant_headers` (required by Android's
///     MediaCodec) makes VideoToolbox reject every frame with `-12909`
///     (`kVTVideoDecoderBadDataErr`). ffmpeg decodes that same stripped stream
///     fine, so no cross-platform check would have caught it — only the real
///     platform decoder does.
///
/// Both would have failed this test in milliseconds, with no host, no network
/// and nobody clicking.
///
/// Fixtures are one real 3840x2160 AV1 keyframe from a live NVENC host plus the
/// `av1C` record built from its sequence header, captured with
/// `devclient --av1 --dump-fixture <path>`. The `av1C` must be a real
/// ISO-BMFF `AV1CodecConfigurationRecord` (starts `0x81`), *not* the bare
/// sequence-header OBU the host puts on the wire — the clients convert it
/// (`app/rust/src/api/mod.rs`) and VideoToolbox rejects the unconverted form
/// with `-12910` (`kVTVideoDecoderUnsupportedDataFormatErr`).
class StreamBeamVideoDecodeTests: XCTestCase {
  private func fixture(_ name: String) throws -> Data {
    let dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    return try Data(contentsOf: dir.appendingPathComponent("Fixtures/\(name)"))
  }

  /// Decode one keyframe and wait for a pixel buffer, or fail. `upscaling`
  /// off isolates the decoder; on, it also exercises the MetalFX path that
  /// sits between the decoder and the Flutter texture.
  private func decodeFixture(
    upscaling: Bool, registry: StubTextureRegistry = StubTextureRegistry()
  ) throws -> (video: StreamBeamVideo, pixels: CVPixelBuffer) {
    let video = StreamBeamVideo(registry: registry)
    _ = video.setUpscaling(upscaling)
    XCTAssertTrue(
      video.configure(
        codecConfig: try fixture("av1_2160p.av1c"), codec: "av01", width: 3840, height: 2160),
      "decoder setup failed for a known-good av1C record: "
        + (video.lastSetupFailure ?? "no reason recorded"))

    video.decode(frame: try fixture("av1_2160p.keyframe"))

    // Decode is asynchronous, so poll rather than assert immediately.
    var buffer: CVPixelBuffer?
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline, buffer == nil {
      buffer = video.copyPixelBuffer()?.takeRetainedValue()
      if buffer == nil { usleep(20_000) }
    }
    let pixels = try XCTUnwrap(
      buffer,
      "decoder produced no frame — this is the black screen, caught before shipping")
    return (video, pixels)
  }

  /// Configure + decode one AV1 keyframe and require pixels out the other end,
  /// at the size the *bitstream* declares.
  func testDecodesAV1KeyframeToPixels() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine (needs Apple silicon M3+)")
    }
    let registry = StubTextureRegistry()
    let (video, pixels) = try decodeFixture(upscaling: false, registry: registry)
    defer { video.dispose() }

    XCTAssertEqual(CVPixelBufferGetWidth(pixels), 3840)
    XCTAssertEqual(CVPixelBufferGetHeight(pixels), 2160)
    // Reported back to Dart to drive the aspect ratio, so a wrong value here
    // is a stretched or letterboxed picture.
    XCTAssertEqual(video.videoDimensions.width, 3840)
    XCTAssertEqual(video.videoDimensions.height, 2160)
    XCTAssertGreaterThan(
      registry.frameAvailableCount, 0, "Flutter was never told a frame is ready")
  }

  /// With upscaling on, the MetalFX stage must still deliver a frame — it sits
  /// directly in the display path, so a failure there is also a black screen.
  /// Its output size depends on the attached display (this asserts only that it
  /// never *shrinks* the picture), so the decoder assertions live in the test
  /// above with upscaling off.
  func testUpscalePathStillDeliversAFrame() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let (video, pixels) = try decodeFixture(upscaling: true)
    defer { video.dispose() }

    XCTAssertGreaterThanOrEqual(CVPixelBufferGetWidth(pixels), 3840)
    XCTAssertGreaterThanOrEqual(CVPixelBufferGetHeight(pixels), 2160)
  }

  /// The runtime watchdog (`DecodeWatchdog` in Dart) decides whether to tell the
  /// user "no picture" purely from these counters, so they have to mean what
  /// they say — a wrong `health` either hides a black screen or invents one.
  func testHealthReportsASuccessfulDecode() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let (video, _) = try decodeFixture(upscaling: false)
    defer { video.dispose() }

    let health = video.health
    XCTAssertEqual(health["decodeCalls"] as? Int, 1)
    XCTAssertEqual(health["decodedFrames"] as? Int, 1)
    XCTAssertEqual(health["decodeFailures"] as? Int, 0)
    XCTAssertEqual(health["dropped"] as? Int, 0)
    XCTAssertEqual(health["lastSetupFailure"] as? String, "")
  }

  /// Frames the decoder rejects must show up as failures with a status code —
  /// the shape of the `-12909` bug, and what makes the watchdog's message
  /// actionable rather than just "no picture".
  func testHealthReportsRejectedFrames() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let video = StreamBeamVideo(registry: StubTextureRegistry())
    defer { video.dispose() }
    XCTAssertTrue(
      video.configure(
        codecConfig: try fixture("av1_2160p.av1c"), codec: "av01", width: 3840, height: 2160),
      video.lastSetupFailure ?? "decoder setup failed")

    // A truncated keyframe: structurally an AV1 temporal unit, but the frame
    // data runs out mid-stream.
    let truncated = try fixture("av1_2160p.keyframe").prefix(4096)
    video.decode(frame: Data(truncated))

    var failures = 0
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline, failures == 0 {
      failures = (video.health["decodeFailures"] as? Int) ?? 0
      if failures == 0 { usleep(20_000) }
    }
    XCTAssertGreaterThan(failures, 0, "a truncated frame was silently accepted")
    XCTAssertNotEqual(
      video.health["lastDecodeStatus"] as? Int, 0,
      "a rejection must carry its OSStatus — that code is the whole diagnosis")
    XCTAssertEqual(video.health["decodedFrames"] as? Int, 0)
  }

  /// HDR10 passthrough (docs/HDR.md stage 3): the decoder must produce 10-bit
  /// frames and keep the PQ/BT.2020 tagging.
  ///
  /// Fixture is a real 3840x2160 10-bit PQ keyframe from Mancave's NVENC,
  /// captured with `devclient --av1 --hdr`. Its `av1C` reports
  /// `high_bitdepth=1`, which only became true once the sequence-header parser
  /// replaced the hardcoded 8-bit constants — an 8-bit `av1C` in front of a
  /// 10-bit stream is exactly the kind of mislabelling that costs a day.
  func testDecodesHDR10ToTenBitPixels() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let registry = StubTextureRegistry()
    let video = StreamBeamVideo(registry: registry)
    defer { video.dispose() }

    XCTAssertTrue(
      video.configure(
        codecConfig: try fixture("av1_2160p_hdr10.av1c"), codec: "av01",
        width: 3840, height: 2160, hdr10: true),
      video.lastSetupFailure ?? "HDR10 decoder setup failed")
    video.decode(frame: try fixture("av1_2160p_hdr10.keyframe"))

    var buffer: CVPixelBuffer?
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline, buffer == nil {
      buffer = video.copyPixelBuffer()?.takeRetainedValue()
      if buffer == nil { usleep(20_000) }
    }
    let pixels = try XCTUnwrap(
      buffer, "HDR10 frame never decoded — \(video.lastSetupFailure ?? "no reason recorded")")

    XCTAssertEqual(CVPixelBufferGetWidth(pixels), 3840)
    XCTAssertEqual(CVPixelBufferGetHeight(pixels), 2160)
    // 'x420' — 10-bit biplanar 4:2:0. Getting 32BGRA here would mean
    // VideoToolbox quietly tone-mapped for us, losing the HDR the host went to
    // the trouble of sending.
    XCTAssertEqual(
      CVPixelBufferGetPixelFormatType(pixels),
      kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
      "expected 10-bit biplanar output, got \(CVPixelBufferGetPixelFormatType(pixels))")

    // The transfer function has to survive to the buffer, or the display path
    // treats absolute-luminance PQ code values as ordinary gamma.
    let transfer = CVBufferGetAttachment(
      pixels, kCVImageBufferTransferFunctionKey, nil)?.takeUnretainedValue()
    XCTAssertEqual(
      transfer as! CFString?, kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ,
      "decoded frame lost its PQ tag")
  }

  /// A frame arriving is not enough: it must not be a uniformly blank image.
  /// A decoder that emits empty buffers, or a colour-space/format mismatch that
  /// yields flat black, looks identical to success from a frame counter.
  func testDecodedFrameIsNotBlank() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }

    let (video, pixels) = try decodeFixture(upscaling: false)
    defer { video.dispose() }

    CVPixelBufferLockBaseAddress(pixels, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
    let base = try XCTUnwrap(CVPixelBufferGetBaseAddress(pixels))
    let bytesPerRow = CVPixelBufferGetBytesPerRow(pixels)
    let height = CVPixelBufferGetHeight(pixels)
    let bytes = base.assumingMemoryBound(to: UInt8.self)

    // Sample a grid rather than every pixel — enough to distinguish real
    // content from a flat fill without walking 8MB.
    var minValue = UInt8.max
    var maxValue = UInt8.min
    for row in stride(from: 0, to: height, by: 8) {
      for col in stride(from: 0, to: bytesPerRow, by: 64) {
        let value = bytes[row * bytesPerRow + col]
        minValue = min(minValue, value)
        maxValue = max(maxValue, value)
      }
    }

    XCTAssertGreaterThan(
      Int(maxValue) - Int(minValue), 8,
      "decoded frame is a flat fill (min \(minValue), max \(maxValue)) — a blank picture")
  }
}

/// The EDR display path (docs/HDR.md stage 4), checked without a screen.
///
/// Every other link in the HDR chain has an objective check: the host's output
/// is read back with `ffprobe`, the decoder by `testDecodesHDR10ToTenBitPixels`.
/// The step between them — turning the decoder's 10-bit YCbCr into the PQ code
/// values handed to the compositor — had none, because it only ever ran into a
/// `CAMetalLayer` drawable, so the sole way to judge it was to look at it. On a
/// feature whose entire failure mode is "the picture looks wrong", eyes are the
/// worst available instrument, and this project has already paid a day for
/// that. `encodeConversion(of:into:)` exists so the same shader can render into
/// an ordinary texture and have its numbers read.
final class StreamBeamHdrRenderTests: XCTestCase {
  private func fixture(_ name: String) throws -> Data {
    let dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    return try Data(contentsOf: dir.appendingPathComponent("Fixtures/\(name)"))
  }

  private func makeView() throws -> StreamBeamHdrView {
    let view = StreamBeamHdrView(frame: NSRect(x: 0, y: 0, width: 16, height: 16))
    if let failure = view.setupFailure {
      throw XCTSkip("HDR view unavailable on this machine: \(failure)")
    }
    return view
  }

  /// A uniform frame in the decoder's own output format ('x420'). Uniform on
  /// purpose: the shader samples with a linear filter, so a patterned frame
  /// would let neighbouring chroma bleed into whichever pixel the assertion
  /// reads. With one colour everywhere, every output pixel must equal the
  /// reference exactly, and interpolation cannot hide a mistake.
  private func makeTenBitFrame(y: UInt16, cb: UInt16, cr: UInt16, size: Int = 64) throws
    -> CVPixelBuffer
  {
    var buffer: CVPixelBuffer?
    let status = CVPixelBufferCreate(
      kCFAllocatorDefault, size, size, kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
      [kCVPixelBufferMetalCompatibilityKey: true] as CFDictionary, &buffer)
    let pixels = try XCTUnwrap(buffer, "CVPixelBufferCreate failed (\(status))")
    CVPixelBufferLockBaseAddress(pixels, [])
    defer { CVPixelBufferUnlockBaseAddress(pixels, []) }

    // 'x420' keeps each 10-bit sample in the *most* significant bits of a
    // 16-bit word. That is the whole reason reading the planes as `r16Unorm` /
    // `rg16Unorm` lands on code/1023 in the shader instead of code/65535 — a
    // 64x error, i.e. a black picture. `testDecodedFrameIsMSBAligned` holds
    // this assumption against a frame VideoToolbox actually produced.
    let lumaBase = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(pixels, 0))
    let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0)
    for row in 0..<size {
      let line = (lumaBase + row * lumaStride).assumingMemoryBound(to: UInt16.self)
      for col in 0..<size { line[col] = y << 6 }
    }
    let chromaBase = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(pixels, 1))
    let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 1)
    for row in 0..<(size / 2) {
      let line = (chromaBase + row * chromaStride).assumingMemoryBound(to: UInt16.self)
      for col in 0..<(size / 2) {
        line[col * 2] = cb << 6
        line[col * 2 + 1] = cr << 6
      }
    }
    return pixels
  }

  /// Run one frame through the real shader and read the RGB code values back.
  private func render(_ buffer: CVPixelBuffer, with view: StreamBeamHdrView, size: Int = 8) throws
    -> [(r: Double, g: Double, b: Double)]
  {
    let device = try XCTUnwrap(view.renderDevice)
    let desc = MTLTextureDescriptor.texture2DDescriptor(
      pixelFormat: .rgb10a2Unorm, width: size, height: size, mipmapped: false)
    desc.usage = [.renderTarget, .shaderRead]
    desc.storageMode = .shared
    let target = try XCTUnwrap(device.makeTexture(descriptor: desc))
    let cmd = try XCTUnwrap(
      view.encodeConversion(of: buffer, into: target),
      "the HDR view refused to encode a conversion — \(view.lastDrawFailure ?? "no reason recorded")")
    cmd.commit()
    cmd.waitUntilCompleted()

    var words = [UInt32](repeating: 0, count: size * size)
    words.withUnsafeMutableBytes { raw in
      target.getBytes(
        raw.baseAddress!, bytesPerRow: size * 4,
        from: MTLRegionMake2D(0, 0, size, size), mipmapLevel: 0)
    }
    // rgb10a2Unorm packs R in the low 10 bits, then G, then B, then 2 bits of
    // alpha.
    return words.map {
      (
        r: Double($0 & 0x3FF) / 1023.0,
        g: Double(($0 >> 10) & 0x3FF) / 1023.0,
        b: Double(($0 >> 20) & 0x3FF) / 1023.0
      )
    }
  }

  /// What the shader *should* produce, derived from the BT.2020 luma
  /// coefficients rather than copied from the shader's rounded constants — a
  /// reference that restates the code proves only that the code is consistent
  /// with itself.
  private func reference(y: UInt16, cb: UInt16, cr: UInt16) -> (r: Double, g: Double, b: Double) {
    let kr = 0.2627, kb = 0.0593
    let kg = 1 - kr - kb
    // Limited ("video") range: luma spans 64...940, so 876 code values map to
    // 0...1; chroma spans 64...960 centred on 512, so its 896 code values map
    // to -0.5...0.5 — the range the matrix below is written for. Halving that
    // denominator (448) desaturates by exactly 2x, which grey test patches
    // cannot catch: neutral chroma sits at the centre, where the scale factor
    // cancels. Only a coloured patch sees it.
    let yv = (Double(y) - 64) / 876
    let cbv = (Double(cb) - 512) / 896
    let crv = (Double(cr) - 512) / 896
    func clamp(_ v: Double) -> Double { min(max(v, 0), 1) }
    return (
      r: clamp(yv + 2 * (1 - kr) * crv),
      g: clamp(yv - (2 * (1 - kb) * kb / kg) * cbv - (2 * (1 - kr) * kr / kg) * crv),
      b: clamp(yv + 2 * (1 - kb) * cbv)
    )
  }

  /// The conversion itself: known 10-bit YCbCr in, known PQ code values out.
  ///
  /// This is what catches the failures that look identical from the outside —
  /// planes read at the wrong scale, limited range not expanded (grey blacks
  /// and clipped whites), the matrix transposed, or the output channels packed
  /// in the wrong order. All of them decode frames and present them happily.
  func testShaderConvertsKnownColours() throws {
    let view = try makeView()
    // Black, PQ peak white, mid grey, and one colour with real chroma in it —
    // chosen to stay inside [0,1] so a clamp can't mask a matrix error.
    let cases: [(name: String, y: UInt16, cb: UInt16, cr: UInt16)] = [
      ("video black", 64, 512, 512),
      ("PQ peak white", 940, 512, 512),
      ("mid grey", 502, 512, 512),
      ("warm mid-tone", 400, 450, 600),
    ]
    for c in cases {
      let pixels = try render(try makeTenBitFrame(y: c.y, cb: c.cb, cr: c.cr), with: view)
      let want = reference(y: c.y, cb: c.cb, cr: c.cr)
      // 10-bit output quantises at 1/1023, and reading MSB-aligned samples as
      // 16-bit unorm scales by 65535/65472 — a 0.1% stretch that the matrix
      // can amplify to ~0.3% on the blue axis. Anything past this is a bug,
      // not arithmetic.
      let tolerance = 0.006
      for (i, got) in pixels.enumerated() where i % 17 == 0 {
        XCTAssertEqual(
          got.r, want.r, accuracy: tolerance, "\(c.name): red is wrong (pixel \(i))")
        XCTAssertEqual(
          got.g, want.g, accuracy: tolerance, "\(c.name): green is wrong (pixel \(i))")
        XCTAssertEqual(
          got.b, want.b, accuracy: tolerance, "\(c.name): blue is wrong (pixel \(i))")
      }
    }
  }

  /// Limited range must actually be expanded. Called out separately because
  /// skipping it is the single most likely way to reproduce the washed-out
  /// picture HDR passthrough exists to fix: blacks lift off zero and whites
  /// stop short, which is precisely how the old SDR tone-map failed.
  func testLimitedRangeIsExpanded() throws {
    let view = try makeView()
    let black = try render(try makeTenBitFrame(y: 64, cb: 512, cr: 512), with: view)[0]
    let white = try render(try makeTenBitFrame(y: 940, cb: 512, cr: 512), with: view)[0]
    XCTAssertLessThan(
      black.r, 0.004, "video black (Y=64) must reach 0, not a lifted \(black.r) — washed out")
    XCTAssertGreaterThan(
      white.r, 0.996, "video white (Y=940) must reach 1, not a dull \(white.r)")
  }

  private func decodeHdrFixture() throws -> (video: StreamBeamVideo, pixels: CVPixelBuffer) {
    let video = StreamBeamVideo(registry: StubTextureRegistry())
    XCTAssertTrue(
      video.configure(
        codecConfig: try fixture("av1_2160p_hdr10.av1c"), codec: "av01",
        width: 3840, height: 2160, hdr10: true),
      video.lastSetupFailure ?? "HDR10 decoder setup failed")
    video.decode(frame: try fixture("av1_2160p_hdr10.keyframe"))
    var buffer: CVPixelBuffer?
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline, buffer == nil {
      buffer = video.copyPixelBuffer()?.takeRetainedValue()
      if buffer == nil { usleep(20_000) }
    }
    return (video, try XCTUnwrap(buffer, "HDR10 frame never decoded"))
  }

  /// Holds the shader's central assumption against reality: VideoToolbox really
  /// does hand back 10-bit samples left-aligned in 16-bit words. If it ever
  /// stopped, `makeTenBitFrame` would keep testing a fiction while the live
  /// picture went 64x too dark.
  func testDecodedFrameIsMSBAligned() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let (video, pixels) = try decodeHdrFixture()
    defer { video.dispose() }
    CVPixelBufferLockBaseAddress(pixels, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }

    let base = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(pixels, 0))
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0)
    let width = CVPixelBufferGetWidthOfPlane(pixels, 0)
    var lowBitsSet = 0
    var outOfRange = 0
    for row in Swift.stride(from: 0, to: CVPixelBufferGetHeightOfPlane(pixels, 0), by: 16) {
      let line = (base + row * stride).assumingMemoryBound(to: UInt16.self)
      for col in Swift.stride(from: 0, to: width, by: 16) {
        if line[col] & 0x3F != 0 { lowBitsSet += 1 }
        let code = line[col] >> 6
        if code < 64 || code > 940 { outOfRange += 1 }
      }
    }
    XCTAssertEqual(
      lowBitsSet, 0,
      "10-bit luma is no longer left-aligned in its 16-bit word — the shader's scaling is now wrong")
    XCTAssertEqual(
      outOfRange, 0,
      "luma escaped the 64...940 limited range, so this frame isn't video-range after all")
  }

  /// End to end for the client half: a real HDR10 keyframe from the host,
  /// decoded and pushed through the display shader, has to come out as a
  /// picture — not black, and not a flat fill.
  func testDecodedHDRFrameRendersAPicture() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }
    let view = try makeView()
    let (video, pixels) = try decodeHdrFixture()
    defer { video.dispose() }

    let rendered = try render(pixels, with: view, size: 64)
    let luma = rendered.map { 0.2627 * $0.r + 0.6780 * $0.g + 0.0593 * $0.b }
    let brightest = luma.max() ?? 0
    let darkest = luma.min() ?? 0
    XCTAssertGreaterThan(
      brightest, 0.05,
      "the whole frame rendered black — decoded pixels reached the shader and nothing came out")
    XCTAssertGreaterThan(
      brightest - darkest, 0.02,
      "the frame rendered as a flat fill (\(darkest)...\(brightest)) — a picture would vary")
    XCTAssertEqual(
      view.drawFailures, 0, "the view reported: \(view.lastDrawFailure ?? "")")
  }
}

class RunnerTests: XCTestCase {

  func testExample() {
    // If you add code to the Runner application, consider adding tests here.
    // See https://developer.apple.com/documentation/xctest for more information about using XCTest.
  }

}
