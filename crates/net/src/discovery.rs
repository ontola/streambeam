//! mDNS discovery for StreamBeam hosts on the LAN.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::NetError;

/// Service type from PROTOCOL.md / PLAN.md.
pub const SERVICE_TYPE: &str = "_streambeam._udp.local.";

/// Register this host on the LAN. Returns the daemon; keep it alive for the session.
pub fn advertise(
    instance: &str,
    host_name: &str,
    port: u16,
    ips: &[IpAddr],
) -> Result<ServiceDaemon, NetError> {
    if ips.is_empty() {
        return Err(NetError::Discovery(
            "no IP addresses to advertise".into(),
        ));
    }

    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        instance,
        &format!("{host_name}.local."),
        ips,
        port,
        None::<std::collections::HashMap<String, String>>,
    )
    .map_err(|e| NetError::Discovery(e.to_string()))?;

    daemon
        .register(info)
        .map_err(|e| NetError::Discovery(e.to_string()))?;
    Ok(daemon)
}

#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    pub instance: String,
    pub host: String,
    pub port: u16,
    pub addrs: Vec<IpAddr>,
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
                let instance = info.get_fullname().to_string();
                let host = info.get_hostname().to_string();
                let port = info.get_port();
                let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                seen.insert(
                    instance.clone(),
                    DiscoveredHost {
                        instance,
                        host,
                        port,
                        addrs,
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

/// Best-effort non-loopback IPv4 for LAN pairing payloads.
pub fn guess_lan_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() {
        None
    } else {
        Some(ip)
    }
}

/// IPs to publish via mDNS for a bound listen address.
pub fn advertise_ips(listen: IpAddr) -> Vec<IpAddr> {
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
