import FlutterMacOS
import Foundation
import VideoToolbox

/// Desktop (macOS) watch: hardware-decodes the H.264 stream from Rust with
/// VideoToolbox and exposes the frames as a Flutter texture. Rust
/// (`play_stream_desktop`) reassembles the media plane and hands us Annex-B
/// access units over a MethodChannel; we decode → CVPixelBuffer → texture.
class StreamBeamVideo: NSObject, FlutterTexture {
    private let registry: FlutterTextureRegistry
    private(set) var textureId: Int64 = -1

    private var formatDesc: CMVideoFormatDescription?
    private var session: VTDecompressionSession?
    private var latest: CVPixelBuffer?
    private let lock = NSLock()

    init(registry: FlutterTextureRegistry) {
        self.registry = registry
        super.init()
        textureId = registry.register(self)
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
        lock.lock()
        latest = imageBuffer
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
