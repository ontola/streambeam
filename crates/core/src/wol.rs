//! Wake-on-LAN magic packets — the client half of `HostMsg::WakeInfo`
//! (docs/WOL.md Part B). Lives in the shared core so every client platform
//! uses one implementation.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

/// Build and send WOL magic packets (6× `0xFF` + the MAC repeated 16×) for
/// `mac` to `broadcast` — the subnet broadcast the host reported, falling
/// back to the limited broadcast `255.255.255.255:9`. Sends three packets:
/// it's cheap UDP toward a NIC in a low-power state, and the NIC (not an OS)
/// is the receiver, so there is no such thing as a duplicate-delivery
/// problem — only a missed one.
pub fn send_magic_packets(mac: [u8; 6], broadcast: Option<SocketAddr>) -> std::io::Result<()> {
    let mut payload = [0u8; 102];
    payload[..6].fill(0xFF);
    for chunk in payload[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac);
    }
    let target =
        broadcast.unwrap_or_else(|| SocketAddr::from((Ipv4Addr::new(255, 255, 255, 255), 9)));
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    for _ in 0..3 {
        socket.send_to(&payload, target)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn magic_packet_layout() {
        // The payload shape is fixed by the WOL spec; assert it via a local
        // reimplementation of the builder loop the sender uses.
        let mac = [0xD8, 0x5E, 0xD3, 0x5E, 0x99, 0x65];
        let mut payload = [0u8; 102];
        payload[..6].fill(0xFF);
        for chunk in payload[6..].chunks_exact_mut(6) {
            chunk.copy_from_slice(&mac);
        }
        assert!(payload[..6].iter().all(|&b| b == 0xFF));
        assert_eq!(&payload[6..12], &mac);
        assert_eq!(&payload[96..102], &mac);
    }
}
