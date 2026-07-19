import FlutterMacOS
import Foundation

/// Native macOS mDNS host discovery. On macOS the OS's `mDNSResponder` owns the
/// mDNS port (5353), so the Rust `mdns-sd` library can't reliably receive — this
/// uses Bonjour (`NetServiceBrowser`) instead and hands the results to Flutter,
/// matching the `DiscoveredHost` shape (name, port, addrs, pubkey).
class StreamBeamDiscovery: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {
    private let browser = NetServiceBrowser()
    private var resolving: Set<NetService> = []
    private var hosts: [String: [String: Any]] = [:]
    private var started = false

    func start() {
        if started { return }
        started = true
        browser.delegate = self
        browser.searchForServices(ofType: "_streambeam._udp.", inDomain: "local.")
    }

    func stop() {
        browser.stop()
        started = false
        resolving.removeAll()
        hosts.removeAll()
    }

    func currentHosts() -> [[String: Any]] {
        Array(hosts.values)
    }

    // MARK: Browser

    func netServiceBrowser(
        _ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool
    ) {
        service.delegate = self
        resolving.insert(service)
        service.resolve(withTimeout: 5.0)
    }

    func netServiceBrowser(
        _ browser: NetServiceBrowser, didRemove service: NetService, moreComing: Bool
    ) {
        hosts.removeValue(forKey: service.name)
        resolving.remove(service)
    }

    // MARK: Resolve

    func netServiceDidResolveAddress(_ service: NetService) {
        var addrs: [String] = []
        for data in service.addresses ?? [] {
            data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
                guard let sa = raw.baseAddress?.assumingMemoryBound(to: sockaddr.self) else { return }
                var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                if getnameinfo(
                    sa, socklen_t(data.count), &host, socklen_t(host.count),
                    nil, 0, NI_NUMERICHOST) == 0
                {
                    let ip = String(cString: host)
                    if !ip.contains("%"), !ip.hasPrefix("fe80"), !addrs.contains(ip) {
                        addrs.append(ip)
                    }
                }
            }
        }

        var pubkey = Data()
        var platform = ""
        if let txt = service.txtRecordData() {
            let dict = NetService.dictionary(fromTXTRecord: txt)
            if let pkData = dict["pk"], let pkStr = String(data: pkData, encoding: .utf8),
                let decoded = Self.base64urlDecode(pkStr)
            {
                pubkey = decoded
            }
            if let osData = dict["os"], let osStr = String(data: osData, encoding: .utf8) {
                platform = osStr
            }
        }

        hosts[service.name] = [
            "name": service.name,
            "port": service.port,
            "addrs": addrs,
            "pubkey": FlutterStandardTypedData(bytes: pubkey),
            "platform": platform,
        ]
        resolving.remove(service)
    }

    func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        resolving.remove(sender)
    }

    private static func base64urlDecode(_ s: String) -> Data? {
        var b = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b.count % 4 != 0 { b += "=" }
        return Data(base64Encoded: b)
    }
}
