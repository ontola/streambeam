//! Windows host server embedded in the Flutter desktop app.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use streambeam_host::{
    build_host_config, default_host_name, server, HostControls, PairApproval,
};
use streambeam_proto::pair::PairInfo;

static SERVICE: OnceLock<Mutex<Option<HostService>>> = OnceLock::new();

fn service_slot() -> &'static Mutex<Option<HostService>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

struct HostService {
    controls: Arc<HostControls>,
    ui: Arc<Mutex<HostUiState>>,
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
    pub pairing_link: Option<String>,
    pub pairing_payload: Option<String>,
    pub error: Option<String>,
    pub discoverable: bool,
    pub accepts_connections: bool,
    pub clients: Vec<HostClientInfo>,
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
        return Err("host is already running".into());
    }

    let listen: SocketAddr = listen
        .as_deref()
        .unwrap_or("0.0.0.0:47989")
        .parse()
        .map_err(|e| format!("invalid listen address: {e}"))?;
    let host_name = host_name.unwrap_or_else(default_host_name);

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

    let config = build_host_config(
        listen,
        host_name,
        fps,
        max_encode_dim,
        600,
        Some(payload_tx),
        Some(approval_tx),
        controls.clone(),
    );

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
        thread,
    });
    Ok(())
}

pub fn stop() {
    if let Ok(mut slot) = service_slot().lock() {
        // Server thread runs until process exit; dropping the handle clears UI state.
        slot.take();
    }
}

pub fn snapshot() -> HostSnapshot {
    let slot = service_slot().lock().ok();
    let Some(svc) = slot.as_ref().and_then(|s| s.as_ref()) else {
        return HostSnapshot {
            running: false,
            host_name: String::new(),
            listen_addr: String::new(),
            pairing_link: None,
            pairing_payload: None,
            error: None,
            discoverable: false,
            accepts_connections: false,
            clients: Vec::new(),
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

    HostSnapshot {
        running: true,
        host_name: ui.map(|u| u.host_name.clone()).unwrap_or_default(),
        listen_addr: ui
            .map(|u| u.listen.to_string())
            .unwrap_or_default(),
        pairing_link: ui.and_then(|u| u.pairing_link.clone()),
        pairing_payload: ui.and_then(|u| u.pairing_payload.clone()),
        error: ui.and_then(|u| u.fatal.clone()),
        discoverable: svc.controls.is_discoverable(),
        accepts_connections: svc.controls.accepts_connections(),
        clients,
        pending_approval: ui.and_then(|u| u.pending_approval.as_ref()).map(|p| HostPairRequest {
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
