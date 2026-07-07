import AppKit
import FlutterMacOS
import Foundation
import Metal
import MetalFX
import VideoToolbox

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

    // MARK: FlutterTexture

    func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
        lock.lock()
        defer { lock.unlock() }
        guard let pb = latest else { return nil }
        return Unmanaged.passRetained(pb)
    }

    // MARK: Decode setup

    /// Configure from the Annex-B codec config (SPS + PPS). Returns success.
    func configure(codecConfig: Data) -> Bool {
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
        formatDesc = fd

        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var callback = VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: { refCon, _, status, _, imageBuffer, _, _ in
                guard status == noErr, let imageBuffer = imageBuffer, let refCon = refCon else { return }
                let me = Unmanaged<StreamBeamVideo>.fromOpaque(refCon).takeUnretainedValue()
                me.onDecoded(imageBuffer)
            },
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())

        var sess: VTDecompressionSession?
        let s = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: fd,
            decoderSpecification: nil,
            imageBufferAttributes: attrs as CFDictionary,
            outputCallback: &callback,
            decompressionSessionOut: &sess)
        guard s == noErr, let sess = sess else { return false }
        session = sess
        return true
    }

    /// Decode one Annex-B access unit.
    func decode(frame: Data) {
        guard let session = session, let fd = formatDesc else { return }
        guard let avcc = Self.annexBToAVCC(frame) else { return }

        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: avcc.count,
            flags: 0,
            blockBufferOut: &blockBuffer)
        guard status == kCMBlockBufferNoErr, let blockBuffer = blockBuffer else { return }

        status = avcc.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(
                with: raw.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: avcc.count)
        }
        guard status == kCMBlockBufferNoErr else { return }

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = avcc.count
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
        guard status == noErr, let sampleBuffer = sampleBuffer else { return }

        VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [._EnableAsynchronousDecompression],
            frameRefcon: nil,
            infoFlagsOut: nil)
    }

    private func onDecoded(_ imageBuffer: CVImageBuffer) {
        // Runs on VideoToolbox's decode callback thread, so the (short) GPU
        // wait inside the upscaler never blocks the UI or decode submission.
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
        latest = frame
        lock.unlock()
        registry.textureFrameAvailable(textureId)
    }

    func dispose() {
        if let session = session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        formatDesc = nil
        lock.lock()
        latest = nil
        lock.unlock()
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
