import Flutter
import UIKit

/// iOS is a foreground viewer. The transport/input protocol remains in Rust;
/// Bonjour, VideoToolbox textures and the audio output belong to the runner.
final class StreamBeamViewerPlugin: NSObject, FlutterPlugin {
  private let registrar: FlutterPluginRegistrar
  private let discovery = StreamBeamDiscovery()
  private let audio = StreamBeamAudioOut()
  private var video: StreamBeamVideo?

  init(registrar: FlutterPluginRegistrar) {
    self.registrar = registrar
    super.init()
    NotificationCenter.default.addObserver(
      self, selector: #selector(stopForBackground),
      name: UIApplication.didEnterBackgroundNotification, object: nil)
  }

  static func register(with registrar: FlutterPluginRegistrar) {
    let plugin = StreamBeamViewerPlugin(registrar: registrar)
    for name in ["streambeam/video_desktop", "streambeam/native_discovery", "streambeam/device"] {
      registrar.addMethodCallDelegate(
        plugin, channel: FlutterMethodChannel(name: name, binaryMessenger: registrar.messenger()))
    }
  }

  func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    let args = call.arguments as? [String: Any]
    switch call.method {
    case "getName": result(UIDevice.current.name)
    case "start": discovery.start(); result(nil)
    case "getHosts": result(discovery.currentHosts())
    case "stop": discovery.stop(); result(nil)
    // SDR H.264 is the validated iOS decoder path. Do not advertise AV1/HDR
    // until their output has been verified on supported physical hardware.
    case "canDecodeAV1", "canDisplayHDR", "setUpscale": result(false)
    case "configure":
      let codec = args?["codec"] as? String ?? "h264"
      guard codec == "h264", args?["hdr10"] as? Bool != true else {
        result(FlutterError(code: "codec", message: "iOS currently supports SDR H.264 streams.", details: nil))
        return
      }
      let config = (args?["codecConfig"] as? FlutterStandardTypedData)?.data ?? Data()
      let candidate = StreamBeamVideo(registry: registrar.textures())
      guard candidate.configure(
        codecConfig: config, codec: codec,
        width: Int32(args?["width"] as? Int ?? 0),
        height: Int32(args?["height"] as? Int ?? 0)) else {
        let reason = candidate.lastSetupFailure ?? "Video decoder setup failed."
        candidate.dispose()
        result(FlutterError(code: "configure", message: reason, details: nil))
        return
      }
      video?.dispose()
      video = candidate
      audio.start()
      UIApplication.shared.isIdleTimerDisabled = true
      let dimensions = candidate.videoDimensions
      result(["textureId": candidate.textureId, "width": dimensions.width, "height": dimensions.height])
    case "frame":
      if let data = (args?["data"] as? FlutterStandardTypedData)?.data { video?.decode(frame: data) }
      result(nil)
    case "health": result(video?.health)
    case "dispose":
      disposePlayback()
      result(nil)
    default: result(FlutterMethodNotImplemented)
    }
  }

  private func disposePlayback() {
    video?.dispose()
    video = nil
    audio.stop()
    UIApplication.shared.isIdleTimerDisabled = false
  }

  @objc private func stopForBackground() {
    disposePlayback()
    discovery.stop()
  }

  deinit { NotificationCenter.default.removeObserver(self) }
}
