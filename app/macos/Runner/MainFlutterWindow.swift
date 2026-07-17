import Cocoa
import FlutterMacOS
import UserNotifications

@_silgen_name("streambeam_macos_capture_wanted")
private func rustCaptureWanted() -> UInt8

class MainFlutterWindow: NSWindow, UNUserNotificationCenterDelegate {
  private static let activeStreamNotificationId = "streambeam.active-viewers"
  private var videoChannel: FlutterMethodChannel?
  private var video: StreamBeamVideo?
  private var discoveryChannel: FlutterMethodChannel?
  private var deviceChannel: FlutterMethodChannel?
  private var screencastChannel: FlutterMethodChannel?
  private var screenCapture: AnyObject?
  private var captureDemandTimer: Timer?
  private var captureStarting = false
  private var captureAttemptedForDemand = false
  private let discovery = StreamBeamDiscovery()

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    UNUserNotificationCenter.current().delegate = self
    setUpVideoChannel(flutterViewController)
    setUpDiscoveryChannel(flutterViewController)
    setUpDeviceChannel(flutterViewController)
    setUpScreencastChannel(flutterViewController)
    startCaptureDemandPolling()

    super.awakeFromNib()
  }

  deinit {
    captureDemandTimer?.invalidate()
    clearViewerNotification()
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    if #available(macOS 11.0, *) {
      completionHandler([.banner, .sound])
    } else {
      completionHandler([.alert, .sound])
    }
  }

  private func updateViewerNotification(names: [String]) {
    let center = UNUserNotificationCenter.current()
    guard !names.isEmpty else {
      clearViewerNotification()
      return
    }

    center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
      guard granted else { return }
      let content = UNMutableNotificationContent()
      if names.count == 1 {
        content.title = "\(names[0]) is watching your screen"
      } else {
        content.title = "\(names.count) devices are watching your screen"
      }
      content.body = "A StreamBeam session is open. Open StreamBeam to stop it or remove access."
      content.sound = .default
      let request = UNNotificationRequest(
        identifier: Self.activeStreamNotificationId,
        content: content,
        trigger: nil)
      center.add(request)
    }
  }

  private func clearViewerNotification() {
    let center = UNUserNotificationCenter.current()
    let ids = [Self.activeStreamNotificationId]
    center.removePendingNotificationRequests(withIdentifiers: ids)
    center.removeDeliveredNotifications(withIdentifiers: ids)
  }

  /// The Rust host is always available, but ScreenCaptureKit only runs while
  /// an authorized client owns a stream. Polling keeps all Apple API work on
  /// the main thread and avoids routing encoded frames or lifecycle commands
  /// through Dart.
  private func startCaptureDemandPolling() {
    captureDemandTimer?.invalidate()
    captureDemandTimer = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) {
      [weak self] _ in
      self?.syncCaptureDemand()
    }
  }

  private func syncCaptureDemand() {
    guard #available(macOS 13.0, *) else { return }
    let wanted = rustCaptureWanted() != 0
    if wanted {
      guard screenCapture == nil, !captureStarting, !captureAttemptedForDemand else { return }
      captureAttemptedForDemand = true
      captureStarting = true
      let capture = StreamBeamCapture()
      Task { @MainActor [weak self] in
        guard let self else { return }
        do {
          try await capture.start(maxDimension: 1920)
          if rustCaptureWanted() != 0 {
            self.screenCapture = capture
            self.screencastChannel?.invokeMethod(
              "captureState", arguments: ["active": true])
          } else {
            await capture.stop()
          }
        } catch {
          NSLog("StreamBeam on-demand capture failed: \(error.localizedDescription)")
          self.screencastChannel?.invokeMethod(
            "captureState",
            arguments: ["active": false, "error": error.localizedDescription])
        }
        self.captureStarting = false
      }
    } else {
      captureAttemptedForDemand = false
      guard let capture = screenCapture as? StreamBeamCapture, !captureStarting else { return }
      screenCapture = nil
      captureStarting = true
      Task { @MainActor [weak self] in
        await capture.stop()
        self?.captureStarting = false
        self?.screencastChannel?.invokeMethod(
          "captureState", arguments: ["active": false])
      }
    }
  }

  /// macOS Beam: request Screen Recording permission, capture with
  /// ScreenCaptureKit and encode with VideoToolbox. Encoded access units go
  /// directly from Swift into Rust (see StreamBeamCapture), never via Dart.
  private func setUpScreencastChannel(_ controller: FlutterViewController) {
    let registrar = controller.registrar(forPlugin: "StreamBeamScreencast")
    let channel = FlutterMethodChannel(
      name: "streambeam/screencast", binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { return }
      guard #available(macOS 13.0, *) else {
        result(FlutterError(
          code: "unsupported", message: "Beaming requires macOS 13 or newer.", details: nil))
        return
      }
      switch call.method {
      case "requestInputAccess":
        if CGPreflightPostEventAccess() {
          result(true)
        } else {
          result(CGRequestPostEventAccess())
        }
      case "requestCapture":
        let capture = (self.screenCapture as? StreamBeamCapture) ?? StreamBeamCapture()
        Task { @MainActor in
          do {
            try await capture.start(maxDimension: 1920)
            self.screenCapture = capture
            result(true)
          } catch {
            result(FlutterError(
              code: "capture", message: error.localizedDescription, details: nil))
          }
        }
      case "stopCapture":
        let capture = self.screenCapture as? StreamBeamCapture
        self.screenCapture = nil
        Task { @MainActor in
          await capture?.stop()
          result(nil)
        }
      case "setActiveViewers":
        let args = call.arguments as? [String: Any]
        let names = args?["names"] as? [String] ?? []
        self.updateViewerNotification(names: names)
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    screencastChannel = channel
  }

  /// Device metadata — same channel contract as Android's MainActivity.
  /// `getName` returns the user-facing computer name ("Joep's MacBook Pro"),
  /// which peers see instead of a generic "StreamBeam".
  private func setUpDeviceChannel(_ controller: FlutterViewController) {
    let registrar = controller.registrar(forPlugin: "StreamBeamDevice")
    let channel = FlutterMethodChannel(
      name: "streambeam/device",
      binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "getName":
        result(Host.current().localizedName ?? ProcessInfo.processInfo.hostName)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    deviceChannel = channel
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
      case "canDecodeAV1":
        result(StreamBeamVideo.canDecodeAV1())
      case "configure":
        let config = (args?["codecConfig"] as? FlutterStandardTypedData)?.data ?? Data()
        let codec = (args?["codec"] as? String) ?? "h264"
        let width = Int32((args?["width"] as? Int) ?? 0)
        let height = Int32((args?["height"] as? Int) ?? 0)
        let v = StreamBeamVideo(registry: registrar.textures)
        if v.configure(codecConfig: config, codec: codec, width: width, height: height) {
          self.video?.dispose()
          self.video = v
          let dims = v.videoDimensions
          result([
            "textureId": Int(v.textureId),
            "width": Int(dims.width),
            "height": Int(dims.height),
          ])
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
