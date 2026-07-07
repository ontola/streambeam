#ifndef RUNNER_STREAMBEAM_VIDEO_H_
#define RUNNER_STREAMBEAM_VIDEO_H_

#include <flutter/texture_registrar.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wrl/client.h>

#include <cstdint>
#include <memory>
#include <mutex>
#include <vector>

// Windows watch: decodes the H.264 stream from Rust with the built-in Media
// Foundation H.264 decoder MFT and exposes frames as a Flutter pixel-buffer
// texture. Mirrors app/macos/Runner/StreamBeamVideo.swift's contract
// (configure/decode/setUpscaling/dispose over the same `codecConfig` +
// Annex-B access-unit shapes) but has no MetalFX-equivalent upscaler yet —
// `SetUpscaling` always reports disabled (see docs/PLATFORMS.md).
class StreamBeamVideo {
 public:
  explicit StreamBeamVideo(flutter::TextureRegistrar* registrar);
  ~StreamBeamVideo();

  int64_t texture_id() const { return texture_id_; }

  // Configure the decoder from the Annex-B codec config (SPS + PPS, with
  // start codes) sent once up front by Rust. Returns success.
  bool Configure(const std::vector<uint8_t>& codec_config);

  // Decode one Annex-B access unit and, on success, mark the texture ready.
  void Decode(const std::vector<uint8_t>& frame);

  // No GPU upscaler on Windows yet — always returns false (not active).
  bool SetUpscaling(bool enabled);

  void Dispose();

 private:
  const FlutterDesktopPixelBuffer* CopyBuffer(size_t width, size_t height);
  bool NegotiateOutputType();
  void DrainOutput();
  void ConvertNv12ToRgba(const uint8_t* nv12, uint32_t width, uint32_t height,
                         uint32_t y_stride);

  flutter::TextureRegistrar* registrar_;
  std::unique_ptr<flutter::TextureVariant> texture_;
  int64_t texture_id_ = -1;

  Microsoft::WRL::ComPtr<IMFTransform> decoder_;
  uint32_t width_ = 0;
  uint32_t height_ = 0;
  int64_t sample_index_ = 0;

  std::mutex lock_;
  // Decoded, converted RGBA pixels ready for Flutter — `pixel_buffer_` is
  // the stable copy handed out via CopyBuffer (must stay valid until the
  // next callback invocation, per the PixelBufferTexture contract), refreshed
  // from `latest_rgba_` under `lock_` each time the engine asks for a frame.
  std::vector<uint8_t> latest_rgba_;
  std::vector<uint8_t> pixel_buffer_;
  uint32_t latest_width_ = 0;
  uint32_t latest_height_ = 0;
  FlutterDesktopPixelBuffer desc_{};
};

#endif  // RUNNER_STREAMBEAM_VIDEO_H_
