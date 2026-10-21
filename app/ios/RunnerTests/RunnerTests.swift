import CoreVideo
import Flutter
import XCTest

@testable import Runner

private final class TextureRegistry: NSObject, FlutterTextureRegistry {
  var texture: FlutterTexture?
  var notifications = 0
  var unregisters = 0
  func register(_ texture: FlutterTexture) -> Int64 { self.texture = texture; return 7 }
  func unregisterTexture(_ textureId: Int64) { texture = nil; unregisters += 1 }
  func textureFrameAvailable(_ textureId: Int64) { notifications += 1 }
}

/// Exercises the actual iOS VideoToolbox decoder, not just the Rust packet
/// counters. These fixtures are the SPS/PPS and first IDR from devtools' test
/// clip; a successful assertion requires decoded, non-uniform BGRA pixels.
final class RunnerTests: XCTestCase {
  private func fixture(_ name: String) throws -> Data {
    let url = try XCTUnwrap(Bundle(for: Self.self).url(
      forResource: name, withExtension: "h264", subdirectory: "Fixtures"))
    return try Data(contentsOf: url)
  }

  func testH264ProducesTexturePixelsAndStopsCleanly() throws {
    let registry = TextureRegistry()
    let decoder = StreamBeamVideo(registry: registry)
    XCTAssertEqual(decoder.textureId, 7)
    XCTAssertTrue(decoder.configure(
      codecConfig: try fixture("config"), codec: "h264", width: 640, height: 360),
      decoder.lastSetupFailure ?? "configuration failed")
    decoder.decode(frame: try fixture("keyframe"))
    let deadline = Date().addingTimeInterval(5)
    while registry.notifications == 0 && Date() < deadline {
      RunLoop.current.run(until: Date().addingTimeInterval(0.01))
    }
    let pixels = try XCTUnwrap(decoder.copyPixelBuffer()?.takeRetainedValue(),
      "iOS decoder received a valid frame but produced no visible texture")
    XCTAssertEqual(CVPixelBufferGetWidth(pixels), 640)
    XCTAssertEqual(CVPixelBufferGetHeight(pixels), 360)
    XCTAssertEqual(CVPixelBufferGetPixelFormatType(pixels), kCVPixelFormatType_32BGRA)
    CVPixelBufferLockBaseAddress(pixels, .readOnly)
    let bytes = try XCTUnwrap(CVPixelBufferGetBaseAddress(pixels))
      .assumingMemoryBound(to: UInt8.self)
    let rowBytes = CVPixelBufferGetBytesPerRow(pixels)
    var colors = Set<UInt32>()
    for y in stride(from: 0, to: 360, by: 20) {
      for x in stride(from: 0, to: 640, by: 20) {
        let offset = y * rowBytes + x * 4
        let color = UInt32(bytes[offset]) | UInt32(bytes[offset + 1]) << 8 | UInt32(bytes[offset + 2]) << 16
        colors.insert(color)
      }
    }
    CVPixelBufferUnlockBaseAddress(pixels, .readOnly)
    XCTAssertGreaterThan(colors.count, 8, "Decoded output is blank/uniform")
    XCTAssertGreaterThan(registry.notifications, 0)
    XCTAssertEqual(decoder.health["decodeFailures"] as? Int, 0)
    decoder.dispose()
    XCTAssertNil(decoder.copyPixelBuffer())
    XCTAssertNil(registry.texture)
    XCTAssertEqual(registry.unregisters, 1)
  }

  func testAudioOutputStartsAndStops() {
    let output = StreamBeamAudioOut()
    output.start()
    XCTAssertTrue(output.isRunning, "AVAudioEngine failed to attach/start the Rust PCM source")
    output.start() // Reconfiguration must not attach a second source.
    XCTAssertTrue(output.isRunning)
    output.stop()
    XCTAssertFalse(output.isRunning)
  }

  func testMalformedConfigurationGivesAnActionableError() {
    let decoder = StreamBeamVideo(registry: TextureRegistry())
    XCTAssertFalse(decoder.configure(codecConfig: Data([0, 1]), codec: "h264", width: 640, height: 360))
    XCTAssertTrue(decoder.lastSetupFailure?.contains("SPS/PPS") ?? false)
    decoder.dispose()
  }
}
