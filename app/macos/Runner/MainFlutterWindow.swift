import Cocoa
import FlutterMacOS
import UserNotifications

@_silgen_name("streambeam_macos_capture_wanted")
private func rustCaptureWanted() -> UInt8
@_silgen_name("streambeam_macos_capture_requested_max_dimension")
private func rustCaptureRequestedMaxDimension() -> UInt32
@_silgen_name("streambeam_macos_capture_request_revision")
private func rustCaptureRequestRevision() -> UInt32

class MainFlutterWindow: NSWindow, UNUserNotificationCenterDelegate {
  private static let activeStreamNotificationId = "streambeam.active-viewers"
  // UserNotifications uses synchronous XPC internally for removal calls.
  // Never let a wedged notification daemon block Flutter's platform thread.
  private static let notificationQueue = DispatchQueue(
    label: "io.streambeam.notifications", qos: .utility)
  private var videoChannel: FlutterMethodChannel?
  private var video: StreamBeamVideo?
  private let audioOut = StreamBeamAudioOut()
  private var discoveryChannel: FlutterMethodChannel?
  private var deviceChannel: FlutterMethodChannel?
  private var screencastChannel: FlutterMethodChannel?
  private var screenCapture: AnyObject?
  private var captureDemandTimer: Timer?
  private var captureStarting = false
  private var activeCaptureRevision: UInt32?
  private var failedCaptureRevision: UInt32?
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
    setUpHdrProbe(flutterViewController)
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
    guard !names.isEmpty else {
      clearViewerNotification()
      return
    }

    Self.notificationQueue.async {
      let center = UNUserNotificationCenter.current()
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
  }

  private func clearViewerNotification() {
    Self.notificationQueue.async {
      let center = UNUserNotificationCenter.current()
      let ids = [Self.activeStreamNotificationId]
      center.removePendingNotificationRequests(withIdentifiers: ids)
      center.removeDeliveredNotifications(withIdentifiers: ids)
    }
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
      let revision = rustCaptureRequestRevision()
      guard !captureStarting else { return }
      guard screenCapture == nil || activeCaptureRevision != revision else { return }
      // A permission denial is not transient within one request. Retrying it
      // on every 200 ms poll makes macOS show the same Screen Recording alert
      // forever. A fresh user connection has a new revision and may try once.
      guard failedCaptureRevision != revision else { return }
      captureStarting = true
      let previous = screenCapture as? StreamBeamCapture
      screenCapture = nil
      activeCaptureRevision = nil
      let capture = StreamBeamCapture()
      Task { @MainActor [weak self] in
        guard let self else { return }
        await previous?.stop()
        do {
          try await capture.start(maxDimension: Int(rustCaptureRequestedMaxDimension()))
          if rustCaptureWanted() != 0 && rustCaptureRequestRevision() == revision {
            self.screenCapture = capture
            self.activeCaptureRevision = revision
            self.failedCaptureRevision = nil
            self.screencastChannel?.invokeMethod(
              "captureState", arguments: ["active": true])
          } else {
            await capture.stop()
          }
        } catch {
          NSLog("StreamBeam on-demand capture failed: \(error.localizedDescription)")
          self.failedCaptureRevision = revision
          self.screencastChannel?.invokeMethod(
            "captureState",
            arguments: ["active": false, "error": error.localizedDescription])
        }
        self.captureStarting = false
      }
    } else {
      guard let capture = screenCapture as? StreamBeamCapture, !captureStarting else { return }
      screenCapture = nil
      activeCaptureRevision = nil
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
      case "permissionStatus":
        result([
          "screenRecording": CGPreflightScreenCaptureAccess(),
          "accessibility": CGPreflightPostEventAccess(),
        ])
      case "openPermissionSettings":
        let args = call.arguments as? [String: Any]
        let permission = args?["permission"] as? String
        let pane = permission == "screenRecording"
          ? "Privacy_ScreenCapture" : "Privacy_Accessibility"
        if let url = URL(
          string: "x-apple.systempreferences:com.apple.preference.security?\(pane)")
        {
          NSWorkspace.shared.open(url)
          result(nil)
        } else {
          result(FlutterError(
            code: "settings", message: "Could not open Privacy & Security settings.",
            details: nil))
        }
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
            self.activeCaptureRevision = nil
            result(true)
          } catch {
            result(FlutterError(
              code: "capture", message: error.localizedDescription, details: nil))
          }
        }
      case "stopCapture":
        let capture = self.screenCapture as? StreamBeamCapture
        self.screenCapture = nil
        self.activeCaptureRevision = nil
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
  /// EDR probe (docs/HDR.md stage 4): a native platform view is registered so
  /// HDR can be tested end-to-end on screen before any encoder or protocol work
  /// depends on it being possible.
  private func setUpHdrProbe(_ controller: FlutterViewController) {
    let registrar = controller.registrar(forPlugin: "StreamBeamHdrProbe")
    registrar.register(
      HdrProbeViewFactory(), withId: "streambeam/hdr_probe")
    // The real HDR video surface (docs/HDR.md stage 4).
    registrar.register(
      StreamBeamHdrViewFactory(), withId: "streambeam/video_hdr")
    let channel = FlutterMethodChannel(
      name: "streambeam/hdr_probe",
      binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "report":
        guard let view = HdrProbeViewFactory.latest else {
          result(nil)
          return
        }
        var report = view.report
        // Read the granted headroom *now*, after the ramp has been on screen —
        // macOS only raises it once EDR content is actually being displayed, so
        // the value captured at view-mount time understates it.
        report["edrHeadroomLive"] = view.liveHeadroom()
        result(report)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

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
      case "canDisplayHDR":
        // Both halves of the HDR promise, answered honestly: hardware AV1
        // decode (10-bit PQ comes over AV1 here) and a display that can
        // actually show more than SDR white. Advertising HDR without the
        // second gets a PQ stream flattened by an SDR compositor — worse than
        // the tone-mapped SDR it replaced, which is why this is checked rather
        // than assumed.
        let headroom = NSScreen.main?.maximumPotentialExtendedDynamicRangeColorComponentValue ?? 1.0
        result(StreamBeamVideo.canDecodeAV1() && headroom > 1.0)
      case "configure":
        let config = (args?["codecConfig"] as? FlutterStandardTypedData)?.data ?? Data()
        let codec = (args?["codec"] as? String) ?? "h264"
        let width = Int32((args?["width"] as? Int) ?? 0)
        let height = Int32((args?["height"] as? Int) ?? 0)
        // Dart sends this; not reading it meant the decoder ran in SDR while
        // the UI had already switched to the EDR view, so frames were decoded
        // and then routed nowhere — a black picture from a one-line omission.
        let hdr10 = (args?["hdr10"] as? Bool) ?? false
        let v = StreamBeamVideo(registry: registrar.textures)
        if v.configure(
          codecConfig: config, codec: codec, width: width, height: height, hdr10: hdr10)
        {
          self.video?.dispose()
          self.video = v
          // Idempotent: a mid-session reconfigure keeps the engine running.
          self.audioOut.start()
          let dims = v.videoDimensions
          result([
            "textureId": Int(v.textureId),
            "width": Int(dims.width),
            "height": Int(dims.height),
          ])
        } else {
          // Report *why*, so the user sees something better than a black
          // rectangle and a generic failure.
          let reason = v.lastSetupFailure ?? "decoder setup failed"
          v.dispose()
          result(FlutterError(code: "configure", message: reason, details: nil))
        }
      case "frame":
        if let data = (args?["data"] as? FlutterStandardTypedData)?.data {
          self.video?.decode(frame: data)
        }
        result(nil)
      case "setUpscale":
        let enabled = (args?["enabled"] as? Bool) ?? true
        result(self.video?.setUpscaling(enabled) ?? false)
      case "hdrStatus":
        // Whether HDR frames are actually reaching the EDR layer, and what
        // headroom the display granted — the objective half of "is HDR
        // working", independent of anyone's eyes.
        let view = StreamBeamHdrViewFactory.current
        result([
          "present": view != nil,
          "frames": view?.framesPresented ?? 0,
          // The number that separates real HDR from a PQ stream being crushed
          // into SDR. Nothing upstream can observe it.
          "headroom": view?.liveHeadroom() ?? 1.0,
          "drawFailures": view?.drawFailures ?? 0,
          "lastDrawFailure": view?.lastDrawFailure ?? "",
          "setupFailure": view?.setupFailure ?? "",
        ])
      case "health":
        // Decode counters for the watchdog — nil when there's no decoder, which
        // is itself the answer.
        result(self.video?.health)
      case "dispose":
        self.video?.dispose()
        self.video = nil
        self.audioOut.stop()
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    videoChannel = channel
  }
}
