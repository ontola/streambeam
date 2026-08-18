//! Host power control + Wake-on-LAN facts (docs/WOL.md).
//!
//! - [`suspend`] services `ClientMsg::RequestPower` — sleep/hibernate on
//!   command from an authenticated viewer.
//! - [`wake_info`] finds the MAC + IPv4 subnet broadcast of the adapter the
//!   host actually serves on, for `HostMsg::WakeInfo` — the client caches it
//!   (`SavedHost`) and sends magic packets at it later, while this box is
//!   asleep and can't be asked.

use std::net::{IpAddr, SocketAddr};

use streambeam_proto::control::PowerAction;

/// Suspend the machine. Returns an error string on platforms/paths where it
/// can't (macOS host support is future work; the message goes back in a log,
/// the client already got its best-effort `PowerAck`).
pub fn suspend(action: PowerAction) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::{HANDLE, LUID};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Power::SetSuspendState;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows::core::w;

        // `SetSuspendState` needs SE_SHUTDOWN_NAME enabled on the process
        // token; harmless to (re-)enable per call — this is a rare RPC.
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| format!("OpenProcessToken: {e}"))?;
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, w!("SeShutdownPrivilege"), &mut luid)
            .map_err(|e| format!("LookupPrivilegeValue: {e}"))?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
            .map_err(|e| format!("AdjustTokenPrivileges: {e}"))?;

        let hibernate = matches!(action, PowerAction::Hibernate);
        // force=false (legacy, ignored since Vista), wake events stay enabled
        // — Wake-on-LAN is the whole point.
        if !SetSuspendState(hibernate, false, false) {
            return Err("SetSuspendState refused".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("suspend-on-command is only implemented on the Windows host".into())
    }
}

/// MAC + IPv4 subnet broadcast (port 9) of the adapter carrying `serve_ips`
/// (the addresses the host advertises for pairing) — `None` when it can't be
/// determined; the client then simply gets no `WakeInfo`.
pub fn wake_info(serve_ips: &[IpAddr]) -> Option<([u8; 6], Option<SocketAddr>)> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersInfo, IP_ADAPTER_INFO};

        // Two calls: sizing, then fill. The list is a linked list of
        // fixed-size nodes; over-allocate by node count.
        let mut len = 0u32;
        let _ = GetAdaptersInfo(None, &mut len);
        if len == 0 {
            return None;
        }
        let count = (len as usize).div_ceil(std::mem::size_of::<IP_ADAPTER_INFO>());
        let mut buf: Vec<IP_ADAPTER_INFO> = vec![std::mem::zeroed(); count.max(1)];
        if GetAdaptersInfo(Some(buf.as_mut_ptr()), &mut len) != 0 {
            return None;
        }

        let parse_ip = |bytes: &[i8]| -> Option<std::net::Ipv4Addr> {
            let s: String = bytes
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8 as char)
                .collect();
            s.parse().ok()
        };

        let mut node: *const IP_ADAPTER_INFO = buf.as_ptr();
        while !node.is_null() {
            let info = &*node;
            let mut ip_list: *const windows::Win32::NetworkManagement::IpHelper::IP_ADDR_STRING =
                &info.IpAddressList;
            while !ip_list.is_null() {
                let entry = &*ip_list;
                if let (Some(ip), Some(mask)) = (
                    parse_ip(&entry.IpAddress.String),
                    parse_ip(&entry.IpMask.String),
                ) {
                    if !ip.is_unspecified() && serve_ips.contains(&IpAddr::V4(ip)) {
                        let mac_len = info.AddressLength as usize;
                        if mac_len == 6 {
                            let mut mac = [0u8; 6];
                            mac.copy_from_slice(&info.Address[..6]);
                            let bcast = std::net::Ipv4Addr::from(u32::from(ip) | !u32::from(mask));
                            return Some((mac, Some(SocketAddr::from((bcast, 9)))));
                        }
                    }
                }
                ip_list = entry.Next;
            }
            node = info.Next;
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = serve_ips;
        None
    }
}
