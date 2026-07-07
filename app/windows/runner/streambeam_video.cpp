#include "streambeam_video.h"

#include <mfapi.h>
#include <mferror.h>
#include <mfobjects.h>

#include <algorithm>
#include <cstdlib>
#include <mutex>

using Microsoft::WRL::ComPtr;

namespace {

// The built-in Media Foundation H.264 decoder MFT (software; works without a
// DXVA/hardware path, matching the "correctness first" approach the software
// H.264 *encoder* fallback already uses in crates/host/src/capture.rs).
// {62CE7E72-4C71-4d20-B15D-452831A87D9D}, confirmed against
// https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder
const GUID kClsidH264Decoder = {
    0x62ce7e72,
    0x4c71,
    0x4d20,
    {0xb1, 0x5d, 0x45, 0x28, 0x31, 0xa8, 0x7d, 0x9d}};

void EnsureMfStarted() {
  static std::once_flag once;
  std::call_once(once, [] { MFStartup(MF_VERSION, MFSTARTUP_FULL); });
}

}  // namespace

StreamBeamVideo::StreamBeamVideo(flutter::TextureRegistrar* registrar)
    : registrar_(registrar) {
  texture_ = std::make_unique<flutter::TextureVariant>(
      flutter::PixelBufferTexture(
          [this](size_t width, size_t height) {
            return CopyBuffer(width, height);
          }));
  texture_id_ = registrar_->RegisterTexture(texture_.get());
}

StreamBeamVideo::~StreamBeamVideo() { Dispose(); }

bool StreamBeamVideo::SetUpscaling(bool /*enabled*/) {
  // No GPU upscaler on Windows yet — always inactive.
  return false;
}

bool StreamBeamVideo::Configure(const std::vector<uint8_t>& codec_config) {
  EnsureMfStarted();

  ComPtr<IMFTransform> decoder;
  HRESULT hr = CoCreateInstance(kClsidH264Decoder, nullptr, CLSCTX_INPROC_SERVER,
                                IID_PPV_ARGS(&decoder));
  if (FAILED(hr)) return false;

  ComPtr<IMFMediaType> in_type;
  hr = MFCreateMediaType(&in_type);
  if (FAILED(hr)) return false;
  in_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  in_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
  in_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
  // Concatenated SPS+PPS NAL units in Annex-B form (with start codes) — same
  // shape Rust already hands us as `codecConfig`, and what the decoder needs
  // to know the picture size before the first sample arrives.
  if (!codec_config.empty()) {
    in_type->SetBlob(MF_MT_MPEG_SEQUENCE_HEADER, codec_config.data(),
                     static_cast<UINT32>(codec_config.size()));
  }
  hr = decoder->SetInputType(0, in_type.Get(), 0);
  if (FAILED(hr)) return false;

  decoder_ = decoder;
  if (!NegotiateOutputType()) {
    decoder_.Reset();
    return false;
  }

  decoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  decoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
  sample_index_ = 0;
  return true;
}

bool StreamBeamVideo::NegotiateOutputType() {
  // NV12 is the decoder's native output — always supported, unlike RGB32
  // (which is only sometimes offered and whose padding byte isn't a real
  // alpha channel). We convert to RGBA ourselves in ConvertNv12ToRgba.
  for (DWORD i = 0;; i++) {
    ComPtr<IMFMediaType> out_type;
    HRESULT hr = decoder_->GetOutputAvailableType(0, i, &out_type);
    if (hr == MF_E_NO_MORE_TYPES || FAILED(hr)) break;
    GUID subtype{};
    if (SUCCEEDED(out_type->GetGUID(MF_MT_SUBTYPE, &subtype)) &&
        subtype == MFVideoFormat_NV12) {
      if (SUCCEEDED(decoder_->SetOutputType(0, out_type.Get(), 0))) {
        UINT32 w = 0, h = 0;
        if (SUCCEEDED(MFGetAttributeSize(out_type.Get(), MF_MT_FRAME_SIZE, &w, &h))) {
          width_ = w;
          height_ = h;
        }
        return width_ > 0 && height_ > 0;
      }
    }
  }
  return false;
}

void StreamBeamVideo::Decode(const std::vector<uint8_t>& frame) {
  if (!decoder_ || frame.empty()) return;

  ComPtr<IMFSample> sample;
  if (FAILED(MFCreateSample(&sample))) return;
  ComPtr<IMFMediaBuffer> buffer;
  if (FAILED(MFCreateMemoryBuffer(static_cast<DWORD>(frame.size()), &buffer))) return;

  BYTE* ptr = nullptr;
  if (FAILED(buffer->Lock(&ptr, nullptr, nullptr))) return;
  std::copy(frame.begin(), frame.end(), ptr);
  buffer->Unlock();
  buffer->SetCurrentLength(static_cast<DWORD>(frame.size()));
  sample->AddBuffer(buffer.Get());
  // Frame rate isn't known here (Windows watch has no fps caps handshake
  // yet); a placeholder 60fps tick is fine — the decoder only uses this for
  // its own internal pacing heuristics, not for what we display.
  sample->SetSampleTime(sample_index_ * (10'000'000LL / 60));
  sample->SetSampleDuration(10'000'000LL / 60);
  sample_index_++;

  HRESULT hr = decoder_->ProcessInput(0, sample.Get(), 0);
  if (FAILED(hr) && hr != MF_E_NOTACCEPTING) return;

  DrainOutput();
}

void StreamBeamVideo::DrainOutput() {
  for (;;) {
    // Re-fetched every iteration: a mid-stream MF_E_TRANSFORM_STREAM_CHANGE
    // (renegotiated below) can change the required buffer size.
    MFT_OUTPUT_STREAM_INFO info{};
    if (FAILED(decoder_->GetOutputStreamInfo(0, &info))) return;

    ComPtr<IMFSample> out_sample;
    if (FAILED(MFCreateSample(&out_sample))) return;
    ComPtr<IMFMediaBuffer> out_buffer;
    if (FAILED(MFCreateMemoryBuffer(std::max<DWORD>(info.cbSize, 1), &out_buffer))) return;
    out_sample->AddBuffer(out_buffer.Get());

    MFT_OUTPUT_DATA_BUFFER data{};
    data.dwStreamID = 0;
    data.pSample = out_sample.Get();
    DWORD status = 0;
    HRESULT hr = decoder_->ProcessOutput(0, 1, &data, &status);
    if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return;
    if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
      NegotiateOutputType();
      continue;
    }
    if (FAILED(hr)) return;

    ComPtr<IMFMediaBuffer> contiguous;
    if (FAILED(out_sample->ConvertToContiguousBuffer(&contiguous))) continue;

    // MF decoder output buffers are frequently row-padded/aligned wider than
    // the frame (hardware alignment) — assuming stride == width silently
    // misaligns every row after the first as soon as the two diverge,
    // producing a garbled/shifting-color picture that gets worse with
    // motion. IMF2DBuffer::Lock2D reports the *real* stride; only fall back
    // to the flat Lock()+width_ assumption if the buffer doesn't support it.
    ComPtr<IMF2DBuffer> buffer2d;
    if (SUCCEEDED(contiguous.As(&buffer2d))) {
      BYTE* scanline0 = nullptr;
      LONG pitch = 0;
      if (SUCCEEDED(buffer2d->Lock2D(&scanline0, &pitch)) && pitch != 0) {
        // A negative pitch means the buffer is bottom-up; not something this
        // decoder's NV12 output produces in practice, so just take the
        // magnitude rather than implementing a row-flip for a case that
        // shouldn't occur.
        ConvertNv12ToRgba(scanline0, width_, height_, static_cast<uint32_t>(std::abs(pitch)));
        buffer2d->Unlock2D();
      }
    } else {
      BYTE* ptr = nullptr;
      DWORD len = 0;
      if (SUCCEEDED(contiguous->Lock(&ptr, nullptr, &len))) {
        ConvertNv12ToRgba(ptr, width_, height_, width_);
        contiguous->Unlock();
      }
    }
  }
}

void StreamBeamVideo::ConvertNv12ToRgba(const uint8_t* nv12, uint32_t width,
                                        uint32_t height, uint32_t y_stride) {
  if (width == 0 || height == 0) return;
  const uint8_t* y_plane = nv12;
  const uint8_t* uv_plane = nv12 + static_cast<size_t>(y_stride) * height;

  // Flutter's PixelBufferTexture (FlutterDesktopPixelBuffer) has no format
  // field at all — unlike the GPU-surface texture variant, it's hardcoded to
  // RGBA8888, not BGRA. Despite the function/variable names inherited from
  // the initial (wrong) assumption, the byte order written below is R,G,B,A.
  std::vector<uint8_t> rgba(static_cast<size_t>(width) * height * 4);
  for (uint32_t row = 0; row < height; row++) {
    const uint8_t* y_row = y_plane + static_cast<size_t>(row) * y_stride;
    const uint8_t* uv_row = uv_plane + static_cast<size_t>(row / 2) * y_stride;
    uint8_t* out_row = rgba.data() + static_cast<size_t>(row) * width * 4;
    for (uint32_t col = 0; col < width; col++) {
      int y = y_row[col];
      int u = uv_row[(col / 2) * 2] - 128;
      int v = uv_row[(col / 2) * 2 + 1] - 128;

      // Full-range BT.601 (Y already 0-255, not the 16-235 "video range"
      // formula) — matches the full-range fix applied to the Android/macOS
      // decoders this session: NVENC doesn't signal VUI color range, and
      // desktop capture is genuinely full-range RGB, so treating Y/U/V as
      // full range is what avoids the near-black crush ("flashing") bug.
      int r = y + ((91881 * v) >> 16);
      int g = y - ((22554 * u + 46802 * v) >> 16);
      int b = y + ((116130 * u) >> 16);

      uint8_t* px = out_row + col * 4;
      px[0] = static_cast<uint8_t>(std::clamp(r, 0, 255));
      px[1] = static_cast<uint8_t>(std::clamp(g, 0, 255));
      px[2] = static_cast<uint8_t>(std::clamp(b, 0, 255));
      px[3] = 0xFF;
    }
  }

  std::lock_guard<std::mutex> guard(lock_);
  latest_rgba_ = std::move(rgba);
  latest_width_ = width;
  latest_height_ = height;
  registrar_->MarkTextureFrameAvailable(texture_id_);
}

const FlutterDesktopPixelBuffer* StreamBeamVideo::CopyBuffer(size_t /*width*/,
                                                              size_t /*height*/) {
  std::lock_guard<std::mutex> guard(lock_);
  if (latest_rgba_.empty()) return nullptr;
  // `desc_`/`pixel_buffer_` are the stable copies the engine reads from
  // after we return — the PixelBufferTexture contract requires the pointer
  // to stay valid until the next callback, so we can't hand out
  // `latest_rgba_` (which the decode thread — really, the same UI thread
  // here, but the contract doesn't assume that) may overwrite next frame.
  pixel_buffer_ = latest_rgba_;
  desc_ = FlutterDesktopPixelBuffer{};
  desc_.buffer = pixel_buffer_.data();
  desc_.width = latest_width_;
  desc_.height = latest_height_;
  return &desc_;
}

void StreamBeamVideo::Dispose() {
  if (decoder_) {
    decoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
    decoder_.Reset();
  }
  {
    std::lock_guard<std::mutex> guard(lock_);
    latest_rgba_.clear();
    pixel_buffer_.clear();
  }
  if (texture_id_ >= 0) {
    registrar_->UnregisterTexture(texture_id_, nullptr);
    texture_id_ = -1;
  }
}
