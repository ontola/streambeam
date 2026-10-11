//! Windows host server embedded in the Flutter desktop app.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use streambeam_host::{HostControls, PairApproval, build_host_config, default_host_name, server};
use streambeam_proto::pair::PairInfo;

static SERVICE: OnceLock<Mutex<Option<HostService>>> = OnceLock::new();

fn service_slot() -> &'static Mutex<Option<HostService>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

struct HostService {
    controls: Arc<HostControls>,
    ui: Arc<Mutex<HostUiState>>,
    host_pubkey: [u8; 32],
    thread: JoinHandle<()>,
}

struct HostUiState {
    host_name: String,
    listen: SocketAddr,
    pairing_link: Option<String>,
    pairing_payload: Option<String>,
    fatal: Option<String>,
    next_approval_id: u64,
    pending_approval: Option<PendingApproval>,
}

struct PendingApproval {
    id: u64,
    client_name: String,
    client_addr: String,
    respond: tokio::sync::oneshot::Sender<bool>,
}

#[derive(Debug, Clone)]
pub struct HostClientInfo {
    pub name: String,
    pub addr: String,
    pub pubkey: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct HostPairRequest {
    pub id: u64,
    pub client_name: String,
    pub client_addr: String,
}

#[derive(Debug, Clone)]
pub struct HostSnapshot {
    pub running: bool,
    pub host_name: String,
    pub listen_addr: String,
    /// Reachable `ip:port` addresses, for showing the user where this device
    /// can be found — the same expansion a pairing payload gets.
    pub lan_addrs: Vec<String>,
    pub pairing_link: Option<String>,
    pub pairing_payload: Option<String>,
    pub error: Option<String>,
    pub discoverable: bool,
    pub accepts_connections: bool,
    pub clients: Vec<HostClientInfo>,
    pub viewers: Vec<HostClientInfo>,
    pub pending_approval: Option<HostPairRequest>,
}

static APPROVAL_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn start(
    host_name: Option<String>,
    listen: Option<String>,
    fps: u32,
    max_encode_dim: u32,
) -> Result<(), String> {
    let mut slot = service_slot().lock().map_err(|e| e.to_string())?;
    if slot.is_some() {
        // Idempotent: every caller means "make sure the host is up" (app
        // startup, resume, the Beam page's fallback button), and several of
        // them guard on a *cached* snapshot that can be stale — surfacing
        // "host is already running" as an error banner for what is actually
        // the desired state confused users (reported 2026-07-19). The
        // running instance keeps its original name/fps/dims; changing those
        // still requires stop_host() first.
        log::info!("start_host: already running — no-op");
        return Ok(());
    }

    let listen: SocketAddr = listen
        .as_deref()
        .unwrap_or("0.0.0.0:47989")
        .parse()
        .map_err(|e| format!("invalid listen address: {e}"))?;
    // Prefer an explicit name, then the device's own name (Android), then the
    // machine hostname (desktop) so peers see a recognizable device.
    let host_name = host_name
        .or_else(crate::store::device_name)
        .unwrap_or_else(default_host_name);

    let (payload_tx, payload_rx) = std::sync::mpsc::channel();
    let (approval_tx, approval_rx) = std::sync::mpsc::channel::<PairApproval>();
    let controls = Arc::new(HostControls::new());
    let ui = Arc::new(Mutex::new(HostUiState {
        host_name: host_name.clone(),
        listen,
        pairing_link: None,
        pairing_payload: None,
        fatal: None,
        next_approval_id: 0,
        pending_approval: None,
    }));

    let mut config = build_host_config(
        listen,
        host_name,
        fps,
        max_encode_dim,
        600,
        Some(payload_tx),
        Some(approval_tx),
        controls.clone(),
    );
    // `build_host_config`'s default data dir ($HOME/.streambeam) is read-only on
    // Android — store the host identity + paired list in the app-private dir the
    // client already uses (set from Flutter via `set_app_data_dir`).
    {
        let dir = crate::store::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        config.identity_path = dir.join("host_identity.pkcs8");
        config.paired_store = Some(dir.join("paired.json"));
    }
    // Resolve the identity before starting the server so the Watch UI can
    // reliably remove this exact host from its own mDNS results. Comparing
    // names or addresses would also hide a different machine with a collision.
    let host_pubkey = streambeam_net::Identity::load_or_generate(&config.identity_path)
        .map_err(|e| e.to_string())?
        .public_key();

    let ui_payload = ui.clone();
    let ui_fatal = ui.clone();
    let thread = std::thread::Builder::new()
        .name("streambeam-host".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    if let Ok(mut state) = ui_fatal.lock() {
                        state.fatal = Some(format!("host runtime error: {e}"));
                    }
                    return;
                }
            };
            if let Err(e) = rt.block_on(server::run(config)) {
                if let Ok(mut state) = ui_fatal.lock() {
                    state.fatal = Some(e.to_string());
                }
            }
        })
        .map_err(|e| format!("spawn host thread: {e}"))?;

    std::thread::Builder::new()
        .name("streambeam-host-ui".into())
        .spawn(move || {
            while let Ok(link) = payload_rx.recv() {
                let payload = PairInfo::decode_payload(&link)
                    .ok()
                    .and_then(|info| info.encode_payload().ok());
                if let Ok(mut state) = ui_payload.lock() {
                    state.pairing_link = Some(link);
                    state.pairing_payload = payload;
                }
            }
        })
        .map_err(|e| format!("spawn host ui thread: {e}"))?;

    let ui_approval = ui.clone();
    std::thread::Builder::new()
        .name("streambeam-host-approval".into())
        .spawn(move || {
            while let Ok(req) = approval_rx.recv() {
                let id = APPROVAL_SEQ.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut state) = ui_approval.lock() {
                    state.pending_approval = Some(PendingApproval {
                        id,
                        client_name: req.client_name,
                        client_addr: req.client_addr.to_string(),
                        respond: req.respond,
                    });
                }
            }
        })
        .map_err(|e| format!("spawn approval thread: {e}"))?;

    *slot = Some(HostService {
        controls,
        ui,
        host_pubkey,
        thread,
    });
    Ok(())
}

pub fn public_key() -> Vec<u8> {
    service_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|svc| svc.host_pubkey.to_vec()))
        .unwrap_or_default()
}

pub fn publish_clipboard(text: String) {
    if let Ok(slot) = service_slot().lock()
        && let Some(service) = slot.as_ref()
    {
        service.controls.publish_clipboard(text);
    }
}

pub fn take_remote_clipboard() -> Option<String> {
    service_slot().lock().ok().and_then(|slot| {
        slot.as_ref()
            .and_then(|service| service.controls.take_remote_clipboard())
    })
}

pub fn stop() {
    // Take the service out (and release the slot lock) before joining, so a
    // concurrent start()/snapshot() doesn't block on the lock while we wind down.
    let svc = match service_slot().lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => return,
    };
    if let Some(svc) = svc {
        // Signal the accept loop to stop, then wait for run() to return so the
        // UDP port is free — otherwise a Stop → Start leaves a stale server
        // holding :47989 and the restart can't bind or mint a fresh QR.
        svc.controls.request_shutdown();
        let _ = svc.thread.join();
    }
}

pub fn snapshot() -> HostSnapshot {
    let slot = service_slot().lock().ok();
    let Some(svc) = slot.as_ref().and_then(|s| s.as_ref()) else {
        return HostSnapshot {
            running: false,
            host_name: String::new(),
            listen_addr: String::new(),
            lan_addrs: Vec::new(),
            pairing_link: None,
            pairing_payload: None,
            error: None,
            discoverable: false,
            accepts_connections: false,
            clients: Vec::new(),
            viewers: Vec::new(),
            pending_approval: None,
        };
    };

    let ui = svc.ui.lock().ok();
    let ui = ui.as_ref().map(|g| &**g);
    let clients: Vec<HostClientInfo> = svc
        .controls
        .clients()
        .into_iter()
        .map(|c| HostClientInfo {
            name: c.name,
            addr: c.addr.to_string(),
            pubkey: c.pubkey.to_vec(),
        })
        .collect();
    let viewers: Vec<HostClientInfo> = svc
        .controls
        .viewers()
        .into_iter()
        .map(|c| HostClientInfo {
            name: c.name,
            addr: c.addr.to_string(),
            pubkey: c.pubkey.to_vec(),
        })
        .collect();

    HostSnapshot {
        running: true,
        host_name: ui.map(|u| u.host_name.clone()).unwrap_or_default(),
        listen_addr: ui.map(|u| u.listen.to_string()).unwrap_or_default(),
        lan_addrs: ui
            .map(|u| {
                streambeam_host::pairing_addrs(u.listen)
                    .into_iter()
                    .map(|a| a.to_string())
                    .collect()
            })
            .unwrap_or_default(),
        pairing_link: ui.and_then(|u| u.pairing_link.clone()),
        pairing_payload: ui.and_then(|u| u.pairing_payload.clone()),
        error: ui.and_then(|u| u.fatal.clone()),
        discoverable: svc.controls.is_discoverable(),
        accepts_connections: svc.controls.accepts_connections(),
        clients,
        viewers,
        pending_approval: ui
            .and_then(|u| u.pending_approval.as_ref())
            .filter(|p| !p.respond.is_closed())
            .map(|p| HostPairRequest {
                id: p.id,
                client_name: p.client_name.clone(),
                client_addr: p.client_addr.clone(),
            }),
    }
}

pub fn set_discoverable(on: bool) -> Result<(), String> {
    with_controls(|c| c.set_discoverable(on))
}

pub fn set_accepts_connections(on: bool) -> Result<(), String> {
    with_controls(|c| c.set_accepts_connections(on))
}

pub fn disconnect_client(addr: &str) -> Result<bool, String> {
    let addr: SocketAddr = addr.parse().map_err(|e| format!("invalid address: {e}"))?;
    with_controls(|c| c.disconnect(addr))
}

pub fn forget_client(addr: &str) -> Result<bool, String> {
    let addr: SocketAddr = addr.parse().map_err(|e| format!("invalid address: {e}"))?;
    with_controls(|c| c.forget(addr))
}

/// The pending pairing request, if any, as `(id, client_name)` — used by the
/// capture service to raise a system notification while the app is backgrounded.
pub fn pending_approval() -> Option<(u64, String)> {
    let slot = service_slot().lock().ok()?;
    let svc = slot.as_ref()?;
    let ui = svc.ui.lock().ok()?;
    ui.pending_approval
        .as_ref()
        .filter(|p| !p.respond.is_closed())
        .map(|p| (p.id, p.client_name.clone()))
}

pub fn respond_pairing(id: u64, allow: bool) -> Result<bool, String> {
    let slot = service_slot().lock().map_err(|e| e.to_string())?;
    let Some(svc) = slot.as_ref() else {
        return Ok(false);
    };
    let mut ui = svc.ui.lock().map_err(|e| e.to_string())?;
    let Some(pending) = ui.pending_approval.take() else {
        return Ok(false);
    };
    if pending.id != id {
        ui.pending_approval = Some(pending);
        return Ok(false);
    }
    let _ = pending.respond.send(allow);
    Ok(true)
}

fn with_controls<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&HostControls) -> T,
{
    let slot = service_slot().lock().map_err(|e| e.to_string())?;
    let Some(svc) = slot.as_ref() else {
        return Err("host is not running".into());
    };
    Ok(f(&svc.controls))
}
