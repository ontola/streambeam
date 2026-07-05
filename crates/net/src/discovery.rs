//! mDNS discovery for StreamBeam hosts on the LAN.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use std::sync::Mutex as StdMutex;

use base64::Engine;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::NetError;

/// Service type from PROTOCOL.md / PLAN.md.
pub const SERVICE_TYPE: &str = "_streambeam._udp.local.";

/// TXT record key for the host Ed25519 public key (base64url, 32 bytes).
pub const TXT_PUBKEY: &str = "pk";

pub fn pubkey_txt_record(pubkey: [u8; 32]) -> std::collections::HashMap<String, String> {
    let mut props = std::collections::HashMap::new();
    props.insert(
        TXT_PUBKEY.into(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pubkey),
    );
    props
}

fn parse_pubkey(info: &ServiceInfo) -> Option<[u8; 32]> {
    let pk = info.get_property_val_str(TXT_PUBKEY)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(pk.as_bytes())
        .ok()?;
    bytes.try_into().ok()
}

fn instance_name_from_fullname(fullname: &str) -> String {
    fullname.split('.').next().unwrap_or(fullname).to_string()
}

/// A registerable mDNS advertisement whose visibility can be toggled at runtime.
///
/// Unlike [`advertise`], which registers immediately for the whole session, this
/// keeps the daemon alive and lets the host flip discoverability on and off (e.g.
/// from a tray checkbox) via [`set_visible`](Self::set_visible).
pub struct Advertiser {
    daemon: ServiceDaemon,
    info: ServiceInfo,
    fullname: String,
    registered: AtomicBool,
}

impl Advertiser {
    /// Build the advertisement (initially *not* registered — call `set_visible`).
    pub fn new(
        instance: &str,
        host_name: &str,
        port: u16,
        ips: &[IpAddr],
        host_pubkey: Option<[u8; 32]>,
    ) -> Result<Self, NetError> {
        if ips.is_empty() {
            return Err(NetError::Discovery("no IP addresses to advertise".into()));
        }
        let props = host_pubkey.map(pubkey_txt_record);
        let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            instance,
            &format!("{host_name}.local."),
            ips,
            port,
            props,
        )
        .map_err(|e| NetError::Discovery(e.to_string()))?;
        let fullname = info.get_fullname().to_string();
        Ok(Self {
            daemon,
            info,
            fullname,
            registered: AtomicBool::new(false),
        })
    }

    /// Register (advertise on the LAN) or unregister. Idempotent.
    pub fn set_visible(&self, visible: bool) -> Result<(), NetError> {
        let was = self.registered.load(Ordering::Relaxed);
        if visible && !was {
            self.daemon
                .register(self.info.clone())
                .map_err(|e| NetError::Discovery(e.to_string()))?;
            self.registered.store(true, Ordering::Relaxed);
        } else if !visible && was {
            self.daemon
                .unregister(&self.fullname)
                .map_err(|e| NetError::Discovery(e.to_string()))?;
            self.registered.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn is_visible(&self) -> bool {
        self.registered.load(Ordering::Relaxed)
    }
}

/// Register this host on the LAN. Returns the daemon; keep it alive for the session.
pub fn advertise(
    instance: &str,
    host_name: &str,
    port: u16,
    ips: &[IpAddr],
    host_pubkey: Option<[u8; 32]>,
) -> Result<ServiceDaemon, NetError> {
    if ips.is_empty() {
        return Err(NetError::Discovery("no IP addresses to advertise".into()));
    }

    let props = host_pubkey.map(pubkey_txt_record);
    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        instance,
        &format!("{host_name}.local."),
        ips,
        port,
        props,
    )
    .map_err(|e| NetError::Discovery(e.to_string()))?;

    daemon
        .register(info)
        .map_err(|e| NetError::Discovery(e.to_string()))?;
    Ok(daemon)
}

#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    /// Friendly instance name (same as host QR label).
    pub name: String,
    pub port: u16,
    pub addrs: Vec<IpAddr>,
    /// Host Ed25519 public key from mDNS TXT (required for LAN connect).
    pub host_pubkey: Option<[u8; 32]>,
}

/// Browse for StreamBeam hosts for up to `timeout`.
pub fn browse(timeout: Duration) -> Result<Vec<DiscoveredHost>, NetError> {
    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| NetError::Discovery(e.to_string()))?;

    let mut seen: HashMap<String, DiscoveredHost> = HashMap::new();
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let fullname = info.get_fullname().to_string();
                let name = instance_name_from_fullname(&fullname);
                let port = info.get_port();
                let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                addrs.sort_by_key(|ip| ip.is_loopback());
                let host_pubkey = parse_pubkey(&info);
                seen.insert(
                    fullname.clone(),
                    DiscoveredHost {
                        name,
                        port,
                        addrs,
                        host_pubkey,
                    },
                );
            }
            Ok(ServiceEvent::SearchStarted(_)) | Ok(ServiceEvent::ServiceFound(_, _)) => {}
            Ok(_) => {}
            Err(_) => break,
        }
    }

    Ok(seen.into_values().collect())
}

/// A long-lived mDNS browser that keeps one `ServiceDaemon` (and its warm,
/// continuously-updated resolver state) alive for as long as the caller
/// holds it, instead of [`browse`]'s cold start-resolve-discard cycle.
///
/// [`browse`] creates a brand new daemon on every call, which matters for a
/// UI that polls repeatedly (e.g. an app's "hosts on your LAN" screen on a
/// timer): each call re-probes the network from scratch within its own
/// short timeout, so a response that lands a beat late — mDNS responders
/// intentionally jitter replies — makes that poll (and every host in it)
/// look offline even though nothing is actually wrong. It also means the
/// daemon's background thread and multicast socket get torn down and
/// recreated every cycle for the app's whole lifetime, which is wasteful
/// and was observed to flood the log with query/response churn. `Browser`
/// instead resolves once and keeps a cache that only gets updated
/// (services found/removed), never rebuilt from nothing.
pub struct Browser {
    _daemon: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    cache: StdMutex<HashMap<String, DiscoveredHost>>,
}

impl Browser {
    pub fn new() -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Self {
            _daemon: daemon,
            receiver,
            cache: StdMutex::new(HashMap::new()),
        })
    }

    /// Drain any events that have arrived since the last call and return the
    /// current set of known hosts. Never blocks.
    pub fn snapshot(&self) -> Vec<DiscoveredHost> {
        let mut cache = self.cache.lock().unwrap();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let fullname = info.get_fullname().to_string();
                    let name = instance_name_from_fullname(&fullname);
                    let port = info.get_port();
                    let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                    addrs.sort_by_key(|ip| ip.is_loopback());
                    let host_pubkey = parse_pubkey(&info);
                    cache.insert(
                        fullname,
                        DiscoveredHost {
                            name,
                            port,
                            addrs,
                            host_pubkey,
                        },
                    );
                }
                ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    cache.remove(&fullname);
                }
                _ => {}
            }
        }
        cache.values().cloned().collect()
    }
}

/// Best-effort non-loopback IPv4 for LAN pairing payloads.
pub fn guess_lan_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() { None } else { Some(ip) }
}

/// IPs to publish via mDNS for a bound listen address.
pub fn advertise_ips(listen: IpAddr) -> Vec<IpAddr> {
    if let Ok(value) = std::env::var("STREAMBEAM_ADVERTISE_IPS") {
        let ips: Vec<IpAddr> = value
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !ips.is_empty() {
            return ips;
        }
    }

    if listen.is_unspecified() {
        guess_lan_ip()
            .into_iter()
            .chain([IpAddr::from([127, 0, 0, 1])])
            .collect()
    } else {
        vec![listen]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_listen_yields_loopback() {
        let ips = advertise_ips("127.0.0.1".parse().unwrap());
        assert!(ips.contains(&"127.0.0.1".parse().unwrap()));
    }
}
