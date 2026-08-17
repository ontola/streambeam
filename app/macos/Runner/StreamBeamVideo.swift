import AVFoundation
import AppKit
import FlutterMacOS
import Foundation
import Metal
import MetalFX
import VideoToolbox

@_silgen_name("streambeam_macos_audio_fill")
private func rustAudioFill(_ out: UnsafeMutablePointer<Float32>, _ count: Int)

/// Watch-role audio output. An `AVAudioSourceNode` pulls interleaved 48 kHz
/// stereo f32 straight from Rust's decoded-Opus ring (the mirror image of the
/// capture-side FFI — no audio bytes touch Dart). Rust hands back silence when
/// no watch session is active, so the engine simply runs between `configure`
/// and `dispose` of the video channel.
final class StreamBeamAudioOut {
  private let engine = AVAudioEngine()
  private var source: AVAudioSourceNode?
  private var running = false

  func start() {
    guard !running else { return }
    guard
      let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 2, interleaved: true),
      // Engine connections must use a deinterleaved ("standard") format —
      // connecting with the interleaved render format raises an NSException
      // inside AVAudioEngine. The engine converts between the two.
      let connectFormat = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)
    else { return }
    let node = AVAudioSourceNode(format: format) { _, _, frameCount, audioBufferList -> OSStatus in
      let buffers = UnsafeMutableAudioBufferListPointer(audioBufferList)
      guard let buffer = buffers.first, let data = buffer.mData else { return noErr }
      rustAudioFill(data.assumingMemoryBound(to: Float32.self), Int(frameCount) * 2)
      return noErr
    }
    engine.attach(node)
    engine.connect(node, to: engine.mainMixerNode, format: connectFormat)
    do {
      try engine.start()
      source = node
      running = true
    } catch {
      NSLog("StreamBeam: audio output engine failed to start: %@", error.localizedDescription)
      engine.detach(node)
    }
  }

  func stop() {
    guard running else { return }
    engine.stop()
    if let source { engine.detach(source) }
    source = nil
    running = false
  }
}

/// Desktop (macOS) watch: hardware-decodes the H.264 stream from Rust with
/// VideoToolbox and exposes the frames as a Flutter texture. Rust
/// (`play_stream_desktop`) reassembles the media plane and hands us Annex-B
/// access units over a MethodChannel; we decode → CVPixelBuffer →
/// (optional MetalFX upscale) → texture.
class StreamBeamVideo: NSObject, FlutterTexture {
    private let registry: FlutterTextureRegistry
    private(set) var textureId: Int64 = -1

    private var formatDesc: CMVideoFormatDescription?
    private var session: VTDecompressionSession?
    private var latest: CVPixelBuffer?
    private var disposed = false
    private let lock = NSLock()

    /// MetalFXUpscaler when supported (macOS 13+, Metal GPU); typed `AnyObject`
    /// so the stored property doesn't need an availability annotation.
    private var upscalerStorage: AnyObject?
    /// User preference — actual upscaling also requires `upscalerStorage`.
    private var wantUpscale = true

    init(registry: FlutterTextureRegistry) {
        self.registry = registry
        super.init()
        textureId = registry.register(self)
        if #available(macOS 13.0, *) {
            upscalerStorage = MetalFXUpscaler()
        }
    }

    /// Enable/disable MetalFX upscaling. Returns whether upscaling is actually
    /// active (i.e. enabled *and* supported on this machine).
    func setUpscaling(_ enabled: Bool) -> Bool {
        lock.lock()
        wantUpscale = enabled
        lock.unlock()
        return enabled && upscalerStorage != nil
    }

    /// Actual coded dimensions from the format description (SPS / av1C
    /// derived) — reported back to Dart so the aspect ratio tracks in-band
    /// reconfigurations (device rotation) without wire-level geometry.
    var videoDimensions: (width: Int32, height: Int32) {
        guard let fd = formatDesc else { return (0, 0) }
        let dims = CMVideoFormatDescriptionGetDimensions(fd)
        return (dims.width, dims.height)
    }

    // MARK: FlutterTexture

    func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
        lock.lock()
        defer { lock.unlock() }
        guard let pb = latest else { return nil }
        return Unmanaged.passRetained(pb)
    }

    // MARK: Decode setup

    /// `'av01'` — spelled as a literal so the 10.15 deployment target doesn't
    /// trip over the newer `kCMVideoCodecType_AV1` constant.
    private static let av1CodecType: CMVideoCodecType = 0x6176_3031
    /// True when the negotiated stream is AV1 (frames are raw OBU temporal
    /// units, not Annex-B).
    private var isAV1 = false

    /// Hardware AV1 decode exists on Apple silicon M3+ — the capability the
    /// client advertises to the host during session negotiation.
    static func canDecodeAV1() -> Bool {
        VTIsHardwareDecodeSupported(av1CodecType)
    }

    /// Configure the decoder. `codec` is the negotiated wire id: `"h264"`
    /// (codecConfig = Annex-B SPS/PPS) or `"av01"` (codecConfig = `av1C`
    /// record built by the Rust side). Returns success.
    func configure(codecConfig: Data, codec: String, width: Int32, height: Int32) -> Bool {
        if codec == "av01" {
            isAV1 = true
            return configureAV1(av1c: codecConfig, width: width, height: height)
        }
        isAV1 = false
        return configureH264(codecConfig: codecConfig)
    }

    private func configureAV1(av1c: Data, width: Int32, height: Int32) -> Bool {
        guard width > 0, height > 0 else { return false }
        let atoms: [CFString: Any] = ["av1C" as CFString: av1c as CFData]
        let extensions: [CFString: Any] = [
            kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms:
                atoms as CFDictionary
            // Deliberately NOT forcing FullRangeVideo here (unlike H.264
            // below): NVENC's AV1 output is limited-range YUV and *says so*
            // in-band — ffprobe on a live host bitstream dump (2026-07-19)
            // reports `color_range=tv` from the sequence header, and since
            // `repeatSeqHdr` every keyframe re-states it. A full-range
            // container tag fighting a limited-range bitstream is at best a
            // constant contrast error and at worst a per-keyframe contrast
            // flip, depending on which one the decoder honors when.
        ]
        var fd: CMVideoFormatDescription?
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: Self.av1CodecType,
            width: width,
            height: height,
            extensions: extensions as CFDictionary,
            formatDescriptionOut: &fd)
        guard status == noErr, let fd = fd else { return false }
        formatDesc = fd
        return makeSession(formatDescription: fd)
    }

    /// Configure from the Annex-B codec config (SPS + PPS). Returns success.
    private func configureH264(codecConfig: Data) -> Bool {
        let nals = Self.annexBNALs(codecConfig)
        var sps: [UInt8]?
        var pps: [UInt8]?
        for nal in nals {
            guard let first = nal.first else { continue }
            switch first & 0x1f {
            case 7: sps = nal
            case 8: pps = nal
            default: break
            }
        }
        guard let sps = sps, let pps = pps else { return false }

        var fd: CMVideoFormatDescription?
        let status = sps.withUnsafeBufferPointer { spsBuf in
            pps.withUnsafeBufferPointer { ppsBuf -> OSStatus in
                let ptrs: [UnsafePointer<UInt8>] = [spsBuf.baseAddress!, ppsBuf.baseAddress!]
                let sizes: [Int] = [sps.count, pps.count]
                return ptrs.withUnsafeBufferPointer { p in
                    sizes.withUnsafeBufferPointer { s in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: p.baseAddress!,
                            parameterSetSizes: s.baseAddress!,
                            nalUnitHeaderLength: 4,
                            formatDescriptionOut: &fd)
                    }
                }
            }
        }
        guard status == noErr, let fd = fd else { return false }

        // Desktop-capture content is genuinely full-range RGB (true black is
        // 0, not 16) — but NVENC's H.264 output doesn't signal VUI
        // color-range info (that lives inside the codec-specific config
        // struct the host deliberately treats as opaque, see
        // crates/host/src/nvenc.rs), so VideoToolbox's default assumption
        // for untagged content (limited/16-235) stretches the full-range
        // signal outward — crushing near-black values to true black and
        // clipping near-white ones, most visible on a full keyframe repaint
        // (e.g. a UI hover highlight). Force full range by overriding the
        // format description's extensions, preserving everything else
        // (including the SPS/PPS-derived parameter-set atom the decoder
        // needs) via a dictionary copy rather than building one from
        // scratch. Falls back to the untagged `fd` if the override fails —
        // decodes the same either way, just without the color-range fix.
        var extensions =
            (CMFormatDescriptionGetExtensions(fd) as? [CFString: Any]) ?? [:]
        extensions[kCMFormatDescriptionExtension_FullRangeVideo] = true
        let dims = CMVideoFormatDescriptionGetDimensions(fd)
        var fullRangeFd: CMVideoFormatDescription?
        let extStatus = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCMVideoCodecType_H264,
            width: dims.width,
            height: dims.height,
            extensions: extensions as CFDictionary,
            formatDescriptionOut: &fullRangeFd)
        let effectiveFd = (extStatus == noErr ? fullRangeFd : nil) ?? fd
        formatDesc = effectiveFd
        return makeSession(formatDescription: effectiveFd)
    }

    private func makeSession(formatDescription: CMVideoFormatDescription) -> Bool {
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var callback = VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: { refCon, _, status, _, imageBuffer, _, _ in
                guard let refCon = refCon else { return }
                let me = Unmanaged<StreamBeamVideo>.fromOpaque(refCon).takeUnretainedValue()
                guard status == noErr, let imageBuffer = imageBuffer else {
                    // Silently dropping these is why a decoder that fails on
                    // every frame looks identical to one that works: the app
                    // reports "Streaming…" over a black texture forever.
                    me.noteDecodeFailure(status)
                    return
                }
                me.onDecoded(imageBuffer)
            },
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())

        var sess: VTDecompressionSession?
        let s = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: formatDescription,
            decoderSpecification: nil,
            imageBufferAttributes: attrs as CFDictionary,
            outputCallback: &callback,
            decompressionSessionOut: &sess)
        guard s == noErr, let sess = sess else {
            let dims = CMVideoFormatDescriptionGetDimensions(formatDescription)
            NSLog(
                "StreamBeam: VTDecompressionSessionCreate failed (status %d) for %dx%d %@",
                s, dims.width, dims.height, isAV1 ? "av01" : "h264")
            return false
        }
        let dims = CMVideoFormatDescriptionGetDimensions(formatDescription)
        let objId = UInt(bitPattern: ObjectIdentifier(self).hashValue)
        NSLog(
            "%@",
            "StreamBeam: decoder ready — \(dims.width)x\(dims.height) "
                + "\(isAV1 ? "av01" : "h264") (hw av1=\(Self.canDecodeAV1())) "
                + "[obj \(objId) tex \(textureId)]")
        session = sess
        decodeFailures = 0
        decodedFrames = 0
        decodeCalls = 0
        dropped = 0
        return true
    }

    /// Decode-error bookkeeping — logged sparsely (first few, then every
    /// 120th) so a persistent failure is obvious without flooding the log.
    private var decodeFailures = 0
    private var decodedFrames = 0
    private var decodeCalls = 0
    private var dropped = 0

    /// A frame that never reached VideoToolbox at all.
    fileprivate func noteDropped(_ reason: String) {
        dropped += 1
        if dropped <= 3 || dropped % 240 == 0 {
            let objId = UInt(bitPattern: ObjectIdentifier(self).hashValue)
            NSLog(
                "%@",
                "StreamBeam: frame dropped before decode (\(reason)), "
                    + "\(dropped) dropped / \(decodeCalls) calls "
                    + "[obj \(objId) tex \(textureId) disposed=\(disposed)]")
        }
    }

    fileprivate func noteDecodeFailure(_ status: OSStatus) {
        decodeFailures += 1
        if decodeFailures <= 3 || decodeFailures % 120 == 0 {
            NSLog(
                "StreamBeam: decode failed (status %d), %d failures / %d decoded",
                status, decodeFailures, decodedFrames)
        }
    }

    /// Decode one access unit — an Annex-B AU for H.264, a raw OBU temporal
    /// unit for AV1 (which needs no repackaging).
    func decode(frame: Data) {
        // Every early return below used to be silent, so "the decoder is never
        // called" and "the decoder rejects every frame" looked identical from
        // outside — see `noteDecodeFailure`.
        decodeCalls += 1
        if decodeCalls == 1 {
            NSLog("StreamBeam: decode() first call — %d bytes, av1=%@", frame.count, isAV1 ? "yes" : "no")
        }
        guard let session = session, let fd = formatDesc else {
            noteDropped("no session/formatDesc")
            return
        }
        guard let sample = isAV1 ? frame : Self.annexBToAVCC(frame) else {
            noteDropped("annexBToAVCC returned nil")
            return
        }

        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: sample.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: sample.count,
            flags: 0,
            blockBufferOut: &blockBuffer)
        guard status == kCMBlockBufferNoErr, let blockBuffer = blockBuffer else {
            noteDropped("CMBlockBufferCreateWithMemoryBlock status \(status)")
            return
        }

        status = sample.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(
                with: raw.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: sample.count)
        }
        guard status == kCMBlockBufferNoErr else {
            noteDropped("CMBlockBufferReplaceDataBytes status \(status)")
            return
        }

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = sample.count
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: fd,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer)
        guard status == noErr, let sampleBuffer = sampleBuffer else {
            noteDropped("CMSampleBufferCreateReady status \(status)")
            return
        }

        let decodeStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [._EnableAsynchronousDecompression],
            frameRefcon: nil,
            infoFlagsOut: nil)
        if decodeStatus != noErr {
            // Submission itself failed (bad parameter, session gone) — distinct
            // from the async per-frame failures the output callback reports.
            noteDecodeFailure(decodeStatus)
        }
    }

    private func onDecoded(_ imageBuffer: CVImageBuffer) {
        // Runs on VideoToolbox's decode callback thread, so the (short) GPU
        // wait inside the upscaler never blocks the UI or decode submission.
        decodedFrames += 1
        if decodedFrames == 1 {
            NSLog(
                "StreamBeam: first frame decoded — %dx%d",
                CVPixelBufferGetWidth(imageBuffer), CVPixelBufferGetHeight(imageBuffer))
        }
        var frame = imageBuffer
        lock.lock()
        let upscale = wantUpscale
        lock.unlock()
        if upscale, #available(macOS 13.0, *),
            let up = upscalerStorage as? MetalFXUpscaler,
            let scaled = up.upscale(imageBuffer)
        {
            frame = scaled
        }
        lock.lock()
        // A decode callback can land after dispose() (async decompression);
        // marking an unregistered texture trips kInternalInconsistency.
        guard !disposed else {
            lock.unlock()
            return
        }
        latest = frame
        lock.unlock()
        registry.textureFrameAvailable(textureId)
    }

    func dispose() {
        NSLog("%@", "StreamBeam: dispose() [obj \(UInt(bitPattern: ObjectIdentifier(self).hashValue)) tex \(textureId)]")
        lock.lock()
        disposed = true
        latest = nil
        lock.unlock()
        if let session = session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        formatDesc = nil
        registry.unregisterTexture(textureId)
    }

    // MARK: Annex-B helpers

    /// Split an Annex-B buffer into NAL units (payload without start codes).
    private static func annexBNALs(_ data: Data) -> [[UInt8]] {
        let bytes = [UInt8](data)
        var nals: [[UInt8]] = []
        var i = 0
        let n = bytes.count
        var start = -1
        func startCodeLen(_ at: Int) -> Int {
            if at + 3 < n, bytes[at] == 0, bytes[at + 1] == 0, bytes[at + 2] == 0, bytes[at + 3] == 1 { return 4 }
            if at + 2 < n, bytes[at] == 0, bytes[at + 1] == 0, bytes[at + 2] == 1 { return 3 }
            return 0
        }
        while i < n {
            let sc = startCodeLen(i)
            if sc > 0 {
                if start >= 0 { nals.append(Array(bytes[start..<i])) }
                i += sc
                start = i
            } else {
                i += 1
            }
        }
        if start >= 0, start < n { nals.append(Array(bytes[start..<n])) }
        return nals
    }

    /// Convert an Annex-B access unit to AVCC (4-byte big-endian length prefixes).
    private static func annexBToAVCC(_ data: Data) -> Data? {
        let nals = annexBNALs(data)
        if nals.isEmpty { return nil }
        var out = Data()
        for nal in nals {
            var len = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
            out.append(contentsOf: nal)
        }
        return out
    }
}

/// GPU upscaling of decoded frames with MetalFX's spatial scaler
/// (`MTLFXSpatialScaler`): each decoded CVPixelBuffer is reconstructed to up to
/// 2× per axis (capped at the largest attached display) before it reaches the
/// Flutter texture. Spatial-only for now — the temporal scaler and Core ML/ANE
/// models need inputs a video stream doesn't have (see docs/UPSCALE.md).
@available(macOS 13.0, *)
final class MetalFXUpscaler {
    /// MetalFX spatial quality drops off and GPU cost climbs past 2× per axis.
    private static let maxScale = 2.0

    private let device: MTLDevice
    private let queue: MTLCommandQueue
    private let textureCache: CVMetalTextureCache
    /// Largest attached display in pixels, captured at init (main thread) —
    /// NSScreen isn't safe from the decode thread where rebuilds happen.
    private let maxOutputW: Double
    private let maxOutputH: Double

    private var scaler: MTLFXSpatialScaler?
    /// MetalFX requires a private-storage render target; we blit from here
    /// into the IOSurface-backed buffer Flutter samples.
    private var scalerOutput: MTLTexture?
    private var pool: CVPixelBufferPool?
    private var inputW = 0
    private var inputH = 0

    init?() {
        guard let device = MTLCreateSystemDefaultDevice(),
            MTLFXSpatialScalerDescriptor.supportsDevice(device),
            let queue = device.makeCommandQueue()
        else { return nil }
        var cache: CVMetalTextureCache?
        guard
            CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache)
                == kCVReturnSuccess, let cache
        else { return nil }
        self.device = device
        self.queue = queue
        self.textureCache = cache
        var w = 3840.0
        var h = 2160.0
        if !NSScreen.screens.isEmpty {
            w = 0
            h = 0
            for screen in NSScreen.screens {
                w = max(w, screen.frame.width * screen.backingScaleFactor)
                h = max(h, screen.frame.height * screen.backingScaleFactor)
            }
        }
        maxOutputW = w
        maxOutputH = h
    }

    /// Upscale one decoded frame. Returns nil when there's nothing to gain
    /// (stream already at/above display size) or on any failure — the caller
    /// then shows the original buffer.
    func upscale(_ input: CVPixelBuffer) -> CVPixelBuffer? {
        let w = CVPixelBufferGetWidth(input)
        let h = CVPixelBufferGetHeight(input)
        if w != inputW || h != inputH { rebuild(inW: w, inH: h) }
        guard let scaler, let scalerOutput, let pool else { return nil }

        guard let inputTex = wrap(input, width: w, height: h) else { return nil }
        var outBuffer: CVPixelBuffer?
        guard
            CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &outBuffer)
                == kCVReturnSuccess,
            let outBuffer,
            let outTex = wrap(outBuffer, width: scalerOutput.width, height: scalerOutput.height),
            let cb = queue.makeCommandBuffer()
        else { return nil }

        scaler.colorTexture = inputTex.texture
        scaler.outputTexture = scalerOutput
        scaler.encode(commandBuffer: cb)
        guard let blit = cb.makeBlitCommandEncoder() else { return nil }
        blit.copy(from: scalerOutput, to: outTex.texture)
        blit.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
        // The CVMetalTexture wrappers must outlive the GPU work.
        withExtendedLifetime((inputTex, outTex)) {}
        return cb.error == nil ? outBuffer : nil
    }

    /// Keeps the CVMetalTexture alive alongside the MTLTexture it backs.
    private struct Wrapped {
        let cv: CVMetalTexture
        let texture: MTLTexture
    }

    private func wrap(_ buffer: CVPixelBuffer, width: Int, height: Int) -> Wrapped? {
        var cv: CVMetalTexture?
        guard
            CVMetalTextureCacheCreateTextureFromImage(
                kCFAllocatorDefault, textureCache, buffer, nil, .bgra8Unorm,
                width, height, 0, &cv) == kCVReturnSuccess,
            let cv, let texture = CVMetalTextureGetTexture(cv)
        else { return nil }
        return Wrapped(cv: cv, texture: texture)
    }

    private func rebuild(inW: Int, inH: Int) {
        inputW = inW
        inputH = inH
        scaler = nil
        scalerOutput = nil
        pool = nil
        guard inW > 0, inH > 0 else { return }

        let scale = min(Self.maxScale, maxOutputW / Double(inW), maxOutputH / Double(inH))
        // Below ~1.05× the reconstruction can't add detail worth the GPU time.
        guard scale > 1.05 else { return }
        let outW = Int(Double(inW) * scale) & ~1
        let outH = Int(Double(inH) * scale) & ~1

        let desc = MTLFXSpatialScalerDescriptor()
        desc.inputWidth = inW
        desc.inputHeight = inH
        desc.outputWidth = outW
        desc.outputHeight = outH
        desc.colorTextureFormat = .bgra8Unorm
        desc.outputTextureFormat = .bgra8Unorm
        desc.colorProcessingMode = .perceptual
        guard let scaler = desc.makeSpatialScaler(device: device) else { return }

        let texDesc = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: .bgra8Unorm, width: outW, height: outH, mipmapped: false)
        texDesc.usage = [.renderTarget, .shaderRead]
        texDesc.storageMode = .private
        guard let output = device.makeTexture(descriptor: texDesc) else { return }

        let bufAttrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey: outW,
            kCVPixelBufferHeightKey: outH,
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var pool: CVPixelBufferPool?
        guard
            CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, bufAttrs as CFDictionary, &pool)
                == kCVReturnSuccess
        else { return }
        self.scaler = scaler
        self.scalerOutput = output
        self.pool = pool
    }
}
