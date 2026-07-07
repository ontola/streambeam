#include "flutter_window.h"

#include <flutter/plugin_registrar.h>
#include <flutter/standard_method_codec.h>

#include <optional>
#include <vector>

#include "flutter/generated_plugin_registrant.h"

namespace {

const std::vector<uint8_t>* GetBytesArg(const flutter::EncodableMap* args,
                                        const char* key) {
  auto it = args->find(flutter::EncodableValue(key));
  if (it == args->end()) return nullptr;
  return std::get_if<std::vector<uint8_t>>(&it->second);
}

const bool* GetBoolArg(const flutter::EncodableMap* args, const char* key) {
  auto it = args->find(flutter::EncodableValue(key));
  if (it == args->end()) return nullptr;
  return std::get_if<bool>(&it->second);
}

}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  SetUpVideoChannel();
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  video_.reset();
  video_channel_.reset();
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

// Registers the `streambeam/video_desktop` MethodChannel the Dart side
// (app/lib/main.dart's `_desktopVideoChannel`) already talks to on macOS —
// same four methods (configure/frame/setUpscale/dispose), same argument
// shapes, so no Dart changes are needed for Windows support.
void FlutterWindow::SetUpVideoChannel() {
  FlutterDesktopPluginRegistrarRef registrar_ref =
      flutter_controller_->engine()->GetRegistrarForPlugin("StreamBeamVideo");
  auto* registrar =
      flutter::PluginRegistrarManager::GetInstance()
          ->GetRegistrar<flutter::PluginRegistrar>(registrar_ref);

  video_channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      registrar->messenger(), "streambeam/video_desktop",
      &flutter::StandardMethodCodec::GetInstance());

  video_channel_->SetMethodCallHandler(
      [this, registrar](const flutter::MethodCall<flutter::EncodableValue>& call,
                        std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> result) {
        const auto* args = std::get_if<flutter::EncodableMap>(call.arguments());

        if (call.method_name() == "configure") {
          const std::vector<uint8_t>* config =
              args ? GetBytesArg(args, "codecConfig") : nullptr;
          if (!config) {
            result->Error("bad_args", "codecConfig missing");
            return;
          }
          auto candidate =
              std::make_unique<StreamBeamVideo>(registrar->texture_registrar());
          if (!candidate->Configure(*config)) {
            result->Error("configure_failed", "Media Foundation H.264 decoder init failed");
            return;
          }
          video_ = std::move(candidate);
          result->Success(flutter::EncodableValue(video_->texture_id()));
        } else if (call.method_name() == "frame") {
          const std::vector<uint8_t>* data = args ? GetBytesArg(args, "data") : nullptr;
          if (video_ && data) {
            video_->Decode(*data);
          }
          result->Success();
        } else if (call.method_name() == "setUpscale") {
          const bool* enabled = args ? GetBoolArg(args, "enabled") : nullptr;
          bool active = video_ && enabled && video_->SetUpscaling(*enabled);
          result->Success(flutter::EncodableValue(active));
        } else if (call.method_name() == "dispose") {
          video_.reset();
          result->Success();
        } else {
          result->NotImplemented();
        }
      });
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
