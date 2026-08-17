import AVFoundation
import Cocoa
import FlutterMacOS
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
/// and nobody clicking. Fixtures are a real `av1C` record plus one real keyframe
/// captured from a live 4K host with
/// `devclient --av1 --dump-fixture <path>`.
class StreamBeamVideoDecodeTests: XCTestCase {
  private func fixture(_ name: String) throws -> Data {
    let dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    return try Data(contentsOf: dir.appendingPathComponent("Fixtures/\(name)"))
  }

  /// Configure + decode one AV1 keyframe and require pixels out the other end.
  func testDecodesAV1KeyframeToPixels() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine (needs Apple silicon M3+)")
    }

    let av1c = try fixture("av1_1080p.av1c")
    let keyframe = try fixture("av1_1080p.keyframe")
    let registry = StubTextureRegistry()
    let video = StreamBeamVideo(registry: registry)
    defer { video.dispose() }

    XCTAssertTrue(
      video.configure(codecConfig: av1c, codec: "av01", width: 1920, height: 1080),
      "decoder setup failed for a known-good av1C record")

    video.decode(frame: keyframe)

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
    XCTAssertEqual(CVPixelBufferGetWidth(pixels), 1920)
    XCTAssertEqual(CVPixelBufferGetHeight(pixels), 1080)
    XCTAssertGreaterThan(
      registry.frameAvailableCount, 0, "Flutter was never told a frame is ready")
  }

  /// A frame arriving is not enough: it must not be a uniformly blank image.
  /// A decoder that emits empty buffers, or a colour-space/format mismatch that
  /// yields flat black, looks identical to success from a frame counter.
  func testDecodedFrameIsNotBlank() throws {
    guard StreamBeamVideo.canDecodeAV1() else {
      throw XCTSkip("no hardware AV1 decode on this machine")
    }

    let video = StreamBeamVideo(registry: StubTextureRegistry())
    defer { video.dispose() }
    XCTAssertTrue(
      video.configure(
        codecConfig: try fixture("av1_1080p.av1c"), codec: "av01", width: 1920, height: 1080))
    video.decode(frame: try fixture("av1_1080p.keyframe"))

    var buffer: CVPixelBuffer?
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline, buffer == nil {
      buffer = video.copyPixelBuffer()?.takeRetainedValue()
      if buffer == nil { usleep(20_000) }
    }
    let pixels = try XCTUnwrap(buffer, "decoder produced no frame")

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

class RunnerTests: XCTestCase {

  func testExample() {
    // If you add code to the Runner application, consider adding tests here.
    // See https://developer.apple.com/documentation/xctest for more information about using XCTest.
  }

}
