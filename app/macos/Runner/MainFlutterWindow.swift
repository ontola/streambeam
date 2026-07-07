import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  private var videoChannel: FlutterMethodChannel?
  private var video: StreamBeamVideo?
  private var discoveryChannel: FlutterMethodChannel?
  private let discovery = StreamBeamDiscovery()

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    setUpVideoChannel(flutterViewController)
    setUpDiscoveryChannel(flutterViewController)

    super.awakeFromNib()
  }

  /// Native Bonjour host discovery (the Rust mdns-sd library can't reliably
  /// receive on macOS — mDNSResponder owns port 5353).
  private func setUpDiscoveryChannel(_ controller: FlutterViewController) {
    let registrar = controller.registrar(forPlugin: "StreamBeamDiscovery")
    let channel = FlutterMethodChannel(
      name: "streambeam/native_discovery",
      binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { return }
      switch call.method {
      case "start":
        self.discovery.start()
        result(nil)
      case "getHosts":
        result(self.discovery.currentHosts())
      case "stop":
        self.discovery.stop()
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    discoveryChannel = channel
  }

  /// Desktop watch: Rust hands us Annex-B frames over this channel; we decode
  /// with VideoToolbox into a Flutter texture (see StreamBeamVideo).
  private func setUpVideoChannel(_ controller: FlutterViewController) {
    let registrar = controller.registrar(forPlugin: "StreamBeamVideo")
    let channel = FlutterMethodChannel(
      name: "streambeam/video_desktop",
      binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { return }
      let args = call.arguments as? [String: Any]
      switch call.method {
      case "configure":
        let config = (args?["codecConfig"] as? FlutterStandardTypedData)?.data ?? Data()
        let v = StreamBeamVideo(registry: registrar.textures)
        if v.configure(codecConfig: config) {
          self.video?.dispose()
          self.video = v
          result(Int(v.textureId))
        } else {
          v.dispose()
          result(FlutterError(code: "configure", message: "decoder setup failed", details: nil))
        }
      case "frame":
        if let data = (args?["data"] as? FlutterStandardTypedData)?.data {
          self.video?.decode(frame: data)
        }
        result(nil)
      case "setUpscale":
        let enabled = (args?["enabled"] as? Bool) ?? true
        result(self.video?.setUpscaling(enabled) ?? false)
      case "dispose":
        self.video?.dispose()
        self.video = nil
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    videoChannel = channel
  }
}
