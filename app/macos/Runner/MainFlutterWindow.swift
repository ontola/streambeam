import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  private var videoChannel: FlutterMethodChannel?
  private var video: StreamBeamVideo?

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    setUpVideoChannel(flutterViewController)

    super.awakeFromNib()
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
