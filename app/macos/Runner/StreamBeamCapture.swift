import CoreMedia
import CoreGraphics
import CoreVideo
import Foundation
import ScreenCaptureKit
import VideoToolbox

@_silgen_name("streambeam_macos_capture_begin")
private func rustCaptureBegin(
  _ width: UInt32, _ height: UInt32, _ displayID: UInt32,
  _ originX: Double, _ originY: Double, _ displayWidth: Double, _ displayHeight: Double)
@_silgen_name("streambeam_macos_capture_requested_display")
private func rustCaptureRequestedDisplay() -> UInt32
@_silgen_name("streambeam_macos_capture_set_displays")
private func rustCaptureSetDisplays(_ bytes: UnsafePointer<UInt8>, _ length: Int)
@_silgen_name("streambeam_macos_capture_end")
private func rustCaptureEnd()
@_silgen_name("streambeam_macos_capture_submit")
private func rustCaptureSubmit(
  _ bytes: UnsafePointer<UInt8>, _ length: Int, _ keyframe: UInt8, _ isConfig: UInt8)
@_silgen_name("streambeam_macos_capture_take_idr_request")
private func rustCaptureTakeIDRRequest() -> UInt8
@_silgen_name("streambeam_macos_capture_submit_audio")
private func rustCaptureSubmitAudio(_ samples: UnsafePointer<Int16>, _ count: Int)

/// ScreenCaptureKit → VideoToolbox H.264 encoder for the macOS Beam role.
/// Encoded access units and captured system-audio PCM cross directly into
/// Rust; no media bytes touch Dart.
@available(macOS 13.0, *)
final class StreamBeamCapture: NSObject, SCStreamDelegate, SCStreamOutput {
  enum CaptureError: LocalizedError {
    case noDisplay
    case encoder(OSStatus)

    var errorDescription: String? {
      switch self {
      case .noDisplay:
        return "No display is available to capture."
      case .encoder(let status):
        return "VideoToolbox encoder setup failed (OSStatus \(status))."
      }
    }
  }

  private let captureQueue = DispatchQueue(label: "io.streambeam.capture", qos: .userInteractive)
  private let audioQueue = DispatchQueue(label: "io.streambeam.audio", qos: .userInteractive)
  private var warnedAudioFormat = false
  private var stream: SCStream?
  private var encoder: VTCompressionSession?
  private var codecConfig = Data()
  private var stopping = false
  private var width: Int32 = 0
  private var height: Int32 = 0
  // Input is sent at up to 125 Hz, but this device-tested capture cadence is
  // kept at 30 until the Qualcomm decoder failure seen with macOS 60 fps is
  // understood (it accepted the stream, rendered a few frames, then aborted).
  private let fps: Int32 = 30

  func start(maxDimension: Int = 1920) async throws {
    guard stream == nil else { return }
    stopping = false

    let content = try await SCShareableContent.excludingDesktopWindows(
      false, onScreenWindowsOnly: true)
    let primaryID = CGMainDisplayID()
    let catalog: [[String: Any]] = content.displays.enumerated().map { index, item in
      [
        "id": item.displayID,
        "name": item.displayID == primaryID ? "Main display" : "Display \(index + 1)",
        "width": min(item.width, Int(UInt16.max)),
        "height": min(item.height, Int(UInt16.max)),
        "primary": item.displayID == primaryID,
      ]
    }
    if let data = try? JSONSerialization.data(withJSONObject: catalog) {
      data.withUnsafeBytes { raw in
        if let base = raw.baseAddress?.assumingMemoryBound(to: UInt8.self) {
          rustCaptureSetDisplays(base, data.count)
        }
      }
    }
    let requestedID = rustCaptureRequestedDisplay()
    guard let display = content.displays.first(where: { $0.displayID == requestedID })
      ?? content.displays.first(where: { $0.displayID == primaryID })
      ?? content.displays.first else { throw CaptureError.noDisplay }

    // SCDisplay.width/height are logical *points* — on Retina that's roughly
    // half the real resolution, and silently capped every stream at ~1500px
    // regardless of what the client requested (found 2026-07-19 via host.log:
    // every encode was 1496x966). SCStreamConfiguration wants pixels, so use
    // the display mode's true pixel dimensions.
    let displayMode = CGDisplayCopyDisplayMode(display.displayID)
    let pixelWidth = displayMode?.pixelWidth ?? display.width
    let pixelHeight = displayMode?.pixelHeight ?? display.height
    let scale = min(1.0, Double(maxDimension) / Double(max(pixelWidth, pixelHeight)))
    width = Int32(max(2, Int(Double(pixelWidth) * scale) & ~1))
    height = Int32(max(2, Int(Double(pixelHeight) * scale) & ~1))

    let filter = SCContentFilter(
      display: display, excludingApplications: [], exceptingWindows: [])
    let configuration = SCStreamConfiguration()
    configuration.width = Int(width)
    configuration.height = Int(height)
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: fps)
    configuration.queueDepth = 3
    configuration.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    configuration.showsCursor = true
    // System audio rides the same SCStream. Own-process audio is excluded so a
    // self-viewing session doesn't feed back its own playback.
    configuration.capturesAudio = true
    configuration.sampleRate = 48_000
    configuration.channelCount = 2
    configuration.excludesCurrentProcessAudio = true

    try createEncoder()
    let bounds = CGDisplayBounds(display.displayID)
    rustCaptureBegin(
      UInt32(width), UInt32(height), display.displayID,
      bounds.origin.x, bounds.origin.y, bounds.width, bounds.height)

    let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
    do {
      try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: captureQueue)
      try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: audioQueue)
      self.stream = stream
      try await stream.startCapture()
    } catch {
      self.stream = nil
      destroyEncoder()
      rustCaptureEnd()
      throw error
    }
  }

  func stop() async {
    guard !stopping else { return }
    stopping = true
    if let stream {
      try? await stream.stopCapture()
    }
    stream = nil
    destroyEncoder()
    codecConfig.removeAll(keepingCapacity: false)
    rustCaptureEnd()
    stopping = false
  }

  private func createEncoder() throws {
    var session: VTCompressionSession?
    let status = VTCompressionSessionCreate(
      allocator: kCFAllocatorDefault,
      width: width,
      height: height,
      codecType: kCMVideoCodecType_H264,
      encoderSpecification: [
        kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as String: true
      ] as CFDictionary,
      imageBufferAttributes: nil,
      compressedDataAllocator: nil,
      outputCallback: { refcon, _, status, _, sampleBuffer in
        guard status == noErr, let refcon, let sampleBuffer else { return }
        let capture = Unmanaged<StreamBeamCapture>.fromOpaque(refcon).takeUnretainedValue()
        capture.handleEncoded(sampleBuffer)
      },
      refcon: Unmanaged.passUnretained(self).toOpaque(),
      compressionSessionOut: &session)
    guard status == noErr, let session else { throw CaptureError.encoder(status) }

    VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
    VTSessionSetProperty(
      session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
    VTSessionSetProperty(
      session, key: kVTCompressionPropertyKey_ProfileLevel,
      value: kVTProfileLevel_H264_Main_AutoLevel)
    VTSessionSetProperty(
      session, key: kVTCompressionPropertyKey_ExpectedFrameRate,
      value: NSNumber(value: fps))
    VTSessionSetProperty(
      session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval,
      value: NSNumber(value: fps * 30))
    let bitrate = min(60_000_000, max(2_000_000, Int(width) * Int(height) * Int(fps) / 15))
    VTSessionSetProperty(
      session, key: kVTCompressionPropertyKey_AverageBitRate,
      value: NSNumber(value: bitrate))
    let prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
    guard prepareStatus == noErr else {
      VTCompressionSessionInvalidate(session)
      throw CaptureError.encoder(prepareStatus)
    }
    encoder = session
  }

  private func destroyEncoder() {
    if let encoder {
      VTCompressionSessionCompleteFrames(encoder, untilPresentationTimeStamp: .invalid)
      VTCompressionSessionInvalidate(encoder)
    }
    encoder = nil
  }

  func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
              of outputType: SCStreamOutputType) {
    if outputType == .audio {
      if sampleBuffer.isValid { handleAudio(sampleBuffer) }
      return
    }
    guard outputType == .screen, sampleBuffer.isValid,
          let imageBuffer = sampleBuffer.imageBuffer, let encoder else { return }

    var frameProperties: CFDictionary?
    if rustCaptureTakeIDRRequest() != 0 {
      frameProperties = [kVTEncodeFrameOptionKey_ForceKeyFrame as String: true] as CFDictionary
    }
    let timestamp = sampleBuffer.presentationTimeStamp
    let duration = sampleBuffer.duration.isValid
      ? sampleBuffer.duration : CMTime(value: 1, timescale: fps)
    let status = VTCompressionSessionEncodeFrame(
      encoder, imageBuffer: imageBuffer, presentationTimeStamp: timestamp,
      duration: duration, frameProperties: frameProperties,
      sourceFrameRefcon: nil, infoFlagsOut: nil)
    if status != noErr {
      NSLog("StreamBeam: VideoToolbox encode failed: %d", status)
    }
  }

  func stream(_ stream: SCStream, didStopWithError error: Error) {
    NSLog("StreamBeam: screen capture stopped: %@", error.localizedDescription)
  }

  /// Converts a captured system-audio buffer to interleaved 48 kHz stereo i16
  /// and hands it to Rust, which Opus-encodes it for the audio channel.
  private func handleAudio(_ sampleBuffer: CMSampleBuffer) {
    guard let format = sampleBuffer.formatDescription,
          let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format)?.pointee,
          asbd.mFormatID == kAudioFormatLinearPCM,
          asbd.mSampleRate == 48_000
    else {
      warnAudioFormatOnce("unexpected stream format")
      return
    }
    let channels = Int(asbd.mChannelsPerFrame)
    let isFloat = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
    let frames = CMSampleBufferGetNumSamples(sampleBuffer)
    guard frames > 0, channels >= 1 else { return }

    let bufferList = AudioBufferList.allocate(maximumBuffers: max(2, channels))
    defer { free(bufferList.unsafeMutablePointer) }
    var blockBuffer: CMBlockBuffer?
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
      sampleBuffer,
      bufferListSizeNeededOut: nil,
      bufferListOut: bufferList.unsafeMutablePointer,
      bufferListSize: AudioBufferList.sizeInBytes(maximumBuffers: max(2, channels)),
      blockBufferAllocator: nil,
      blockBufferMemoryAllocator: nil,
      flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
      blockBufferOut: &blockBuffer)
    guard status == noErr else { return }

    var interleaved = [Int16](repeating: 0, count: frames * 2)
    if isFloat {
      let nonInterleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
      if nonInterleaved {
        // One buffer per channel; mono duplicates into both output channels.
        guard let left = bufferList[0].mData?.assumingMemoryBound(to: Float32.self) else { return }
        let right = bufferList.count > 1
          ? bufferList[1].mData?.assumingMemoryBound(to: Float32.self) : nil
        for frame in 0..<frames {
          interleaved[frame * 2] = pcm16(left[frame])
          interleaved[frame * 2 + 1] = pcm16((right ?? left)[frame])
        }
      } else {
        guard let data = bufferList[0].mData?.assumingMemoryBound(to: Float32.self) else { return }
        for frame in 0..<frames {
          let left = data[frame * channels]
          let right = channels > 1 ? data[frame * channels + 1] : left
          interleaved[frame * 2] = pcm16(left)
          interleaved[frame * 2 + 1] = pcm16(right)
        }
      }
    } else if asbd.mBitsPerChannel == 16,
              asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0 {
      guard let data = bufferList[0].mData?.assumingMemoryBound(to: Int16.self) else { return }
      for frame in 0..<frames {
        let left = data[frame * channels]
        let right = channels > 1 ? data[frame * channels + 1] : left
        interleaved[frame * 2] = left
        interleaved[frame * 2 + 1] = right
      }
    } else {
      warnAudioFormatOnce("unsupported PCM layout")
      return
    }

    interleaved.withUnsafeBufferPointer { buffer in
      guard let base = buffer.baseAddress else { return }
      rustCaptureSubmitAudio(base, buffer.count)
    }
  }

  private func warnAudioFormatOnce(_ reason: String) {
    if !warnedAudioFormat {
      warnedAudioFormat = true
      NSLog("StreamBeam: audio capture disabled — %@", reason)
    }
  }

  private func pcm16(_ sample: Float32) -> Int16 {
    Int16(max(-1.0, min(1.0, sample)) * Float32(Int16.max))
  }

  private func handleEncoded(_ sampleBuffer: CMSampleBuffer) {
    let attachments = CMSampleBufferGetSampleAttachmentsArray(
      sampleBuffer, createIfNecessary: false) as? [[CFString: Any]]
    let keyframe = !(attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false)

    if keyframe, let format = sampleBuffer.formatDescription,
       let config = h264CodecConfig(format) {
      codecConfig = config
      submit(config, keyframe: true, isConfig: true)
    }
    guard var accessUnit = annexBData(sampleBuffer) else { return }
    // Repeat SPS/PPS before every IDR so a decoder can recover without waiting
    // for a control-stream reconfiguration.
    if keyframe, !codecConfig.isEmpty {
      accessUnit.insert(contentsOf: codecConfig, at: 0)
    }
    submit(accessUnit, keyframe: keyframe, isConfig: false)
  }

  private func submit(_ data: Data, keyframe: Bool, isConfig: Bool) {
    data.withUnsafeBytes { raw in
      guard let bytes = raw.bindMemory(to: UInt8.self).baseAddress else { return }
      rustCaptureSubmit(bytes, data.count, keyframe ? 1 : 0, isConfig ? 1 : 0)
    }
  }

  private func h264CodecConfig(_ format: CMFormatDescription) -> Data? {
    var output = Data()
    var nalLength: Int32 = 0
    for index in 0..<2 {
      var pointer: UnsafePointer<UInt8>?
      var size = 0
      var count = 0
      let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
        parameterSetSizeOut: &size, parameterSetCountOut: &count,
        nalUnitHeaderLengthOut: &nalLength)
      guard status == noErr, let pointer else { return nil }
      output.append(contentsOf: [0, 0, 0, 1])
      output.append(pointer, count: size)
    }
    return output
  }

  private func annexBData(_ sampleBuffer: CMSampleBuffer) -> Data? {
    guard let block = sampleBuffer.dataBuffer else { return nil }
    let length = CMBlockBufferGetDataLength(block)
    var avcc = Data(count: length)
    let status = avcc.withUnsafeMutableBytes { raw in
      CMBlockBufferCopyDataBytes(
        block, atOffset: 0, dataLength: length, destination: raw.baseAddress!)
    }
    guard status == kCMBlockBufferNoErr else { return nil }

    var output = Data(capacity: length)
    var offset = 0
    while offset + 4 <= avcc.count {
      let nalLength = avcc[offset..<(offset + 4)].reduce(0) {
        ($0 << 8) | Int($1)
      }
      offset += 4
      guard nalLength > 0, offset + nalLength <= avcc.count else { return nil }
      output.append(contentsOf: [0, 0, 0, 1])
      output.append(avcc[offset..<(offset + nalLength)])
      offset += nalLength
    }
    return offset == avcc.count ? output : nil
  }
}
