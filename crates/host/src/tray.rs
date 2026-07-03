// The whole module is Win32 FFI glue; each `unsafe fn` body is deliberately
// one big unsafe region rather than nesting `unsafe {}` around every call.
#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::VecDeque;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;

use qrcode::{Color, QrCode};
use windows_sys::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, SetBkMode, DT_CENTER, DT_TOP, HBRUSH, HDC, PAINTSTRUCT, TRANSPARENT,
    WHITE_BRUSH,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, KillTimer, LoadIconW, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer,
    ShowWindow, TrackPopupMenu, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, IDC_ARROW, IDI_APPLICATION, IDYES, MB_ICONERROR,
    MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MF_SEPARATOR, MF_STRING, MSG,
    SW_HIDE, SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_LBUTTONUP, WM_PAINT,
    WM_RBUTTONUP, WM_TIMER, WM_USER, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::server::{HostControls, PairApproval};
use crate::{build_config, server, Opt};

const CLASS_NAME: &str = "StreamBeamHostWindow";
const TRAY_ID: u32 = 1;
const WM_TRAY: u32 = WM_USER + 1;
/// Posted to the UI thread when a device is waiting for pairing approval.
const WM_PAIR_REQUEST: u32 = WM_USER + 2;
/// Posted when the server thread dies (e.g. port in use) — show it and quit.
const WM_SERVER_ERROR: u32 = WM_USER + 3;
const ID_COPY: usize = 1001;
const ID_SHOW: usize = 1002;
const ID_QUIT: usize = 1003;
const ID_DISCOVER: usize = 1004;
const ID_ACCEPT: usize = 1005;
/// Timer id used to refresh the connected-clients list.
const REFRESH_TIMER: usize = 1;
/// `CF_UNICODETEXT` clipboard format (avoids pulling the `Win32_System_Ole` feature).
const CF_UNICODETEXT: u32 = 13;
/// `BST_CHECKED` (from `Win32_UI_Controls`, inlined to skip that feature).
const BST_CHECKED: isize = 1;

static APP: OnceLock<Arc<AppState>> = OnceLock::new();

struct AppState {
    hwnd: AtomicIsize,
    payload: Mutex<Option<String>>,
    /// Devices waiting for the user to allow/deny, drained on the UI thread.
    approvals: Mutex<VecDeque<PairApproval>>,
    /// A fatal server-thread error to surface on the UI thread before quitting.
    fatal: Mutex<Option<String>>,
    /// Live toggles + connected-client list, shared with the server thread.
    controls: Arc<HostControls>,
}

impl AppState {
    fn hwnd(&self) -> HWND {
        self.hwnd.load(Ordering::SeqCst) as HWND
    }
}

pub fn run(opt: Opt) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Refuse to start a second copy — otherwise the new process fights the old
    // one for the UDP port and dies with "address in use".
    unsafe {
        if already_running() {
            MessageBoxW(
                null_mut(),
                wide(
                    "StreamBeam is already running.\n\n\
                     Open it from the system tray (click the ^ arrow near the clock).",
                )
                .as_ptr(),
                wide("StreamBeam").as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            );
            return Ok(());
        }
    }

    let (payload_tx, payload_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel::<PairApproval>();
    let controls = Arc::new(HostControls::new());
    let state = Arc::new(AppState {
        hwnd: AtomicIsize::new(0),
        payload: Mutex::new(None),
        approvals: Mutex::new(VecDeque::new()),
        fatal: Mutex::new(None),
        controls: controls.clone(),
    });
    let _ = APP.set(state.clone());

    let config = build_config(opt, Some(payload_tx), Some(approval_tx), controls);
    let server_state = state.clone();
    thread::spawn(move || {
        eprintln!(
            "StreamBeam host {} (mock encoder — real capture in M2)",
            env!("CARGO_PKG_VERSION")
        );
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("host runtime error: {e}"))
            .and_then(|rt| rt.block_on(server::run(config)).map_err(|e| e.to_string()));
        if let Err(e) = result {
            eprintln!("host error: {e}");
            report_fatal(&server_state, e);
        }
    });

    let ui_state = state.clone();
    thread::spawn(move || {
        while let Ok(payload) = payload_rx.recv() {
            *ui_state.payload.lock().unwrap() = Some(payload);
            let hwnd = ui_state.hwnd();
            if !hwnd.is_null() {
                unsafe {
                    InvalidateRect(hwnd, null(), 1);
                }
            }
        }
    });

    // Approval requests arrive on the server's threads; queue them and poke the
    // UI thread, which owns the modal prompt.
    let approval_state = state.clone();
    thread::spawn(move || {
        while let Ok(req) = approval_rx.recv() {
            approval_state.approvals.lock().unwrap().push_back(req);
            let hwnd = approval_state.hwnd();
            if !hwnd.is_null() {
                unsafe {
                    PostMessageW(hwnd, WM_PAIR_REQUEST, 0, 0);
                }
            }
        }
    });

    unsafe { run_message_loop(state) }
}

unsafe fn run_message_loop(
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let instance = GetModuleHandleW(null());
    let class_name = wide(CLASS_NAME);
    let cursor = LoadIconW(null_mut(), IDC_ARROW);
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: class_name.as_ptr(),
        ..std::mem::zeroed()
    };
    if RegisterClassW(&wc) == 0 {
        return Err("failed to register StreamBeam window class".into());
    }

    let title = wide("StreamBeam Host");
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        560,
        660,
        null_mut(),
        null_mut(),
        instance,
        null_mut(),
    );
    if hwnd.is_null() {
        return Err("failed to create StreamBeam window".into());
    }
    state.hwnd.store(hwnd as isize, Ordering::SeqCst);
    add_tray_icon(hwnd);
    ShowWindow(hwnd, SW_HIDE);
    // A device may have connected before the window existed — drain the queue.
    PostMessageW(hwnd, WM_PAIR_REQUEST, 0, 0);
    // The server may have already failed to bind before the window existed.
    PostMessageW(hwnd, WM_SERVER_ERROR, 0, 0);

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    remove_tray_icon(hwnd);
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cb_discover = create_checkbox(
                hwnd,
                ID_DISCOVER,
                "Discoverable on the local network",
                24,
                336,
            );
            let cb_accept = create_checkbox(hwnd, ID_ACCEPT, "Accept new connections", 24, 366);
            if let Some(controls) = app_controls() {
                SendMessageW(cb_discover, BM_SETCHECK, controls.is_discoverable() as usize, 0);
                SendMessageW(cb_accept, BM_SETCHECK, controls.accepts_connections() as usize, 0);
            }

            let button_class = wide("BUTTON");
            let label = wide("Copy code");
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                button_class.as_ptr(),
                label.as_ptr(),
                WS_CHILD | WS_VISIBLE,
                210,
                596,
                130,
                34,
                hwnd,
                ID_COPY as HMENU,
                GetModuleHandleW(null()),
                null_mut(),
            );

            // Refresh the connected-clients list once a second.
            SetTimer(hwnd, REFRESH_TIMER, 1000, None);
            0
        }
        WM_COMMAND => match (wparam & 0xffff) as usize {
            ID_COPY => {
                if let Some(payload) = current_payload() {
                    let _ = copy_to_clipboard(hwnd, &payload);
                }
                0
            }
            ID_SHOW => {
                show_window(hwnd);
                0
            }
            ID_QUIT => {
                DestroyWindow(hwnd);
                0
            }
            ID_DISCOVER => {
                let on = SendMessageW(lparam as HWND, BM_GETCHECK, 0, 0) == BST_CHECKED;
                if let Some(controls) = app_controls() {
                    controls.set_discoverable(on);
                }
                0
            }
            ID_ACCEPT => {
                let on = SendMessageW(lparam as HWND, BM_GETCHECK, 0, 0) == BST_CHECKED;
                if let Some(controls) = app_controls() {
                    controls.set_accepts_connections(on);
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        },
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => show_window(hwnd),
                WM_RBUTTONUP => show_tray_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_PAIR_REQUEST => {
            process_approvals(hwnd);
            0
        }
        WM_SERVER_ERROR => {
            if let Some(message) = APP.get().and_then(|s| s.fatal.lock().ok()?.take()) {
                let body = format!("StreamBeam host could not start:\n\n{message}");
                MessageBoxW(
                    hwnd,
                    wide(&body).as_ptr(),
                    wide("StreamBeam — error").as_ptr(),
                    MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
                );
                DestroyWindow(hwnd);
            }
            0
        }
        WM_PAINT => {
            paint(hwnd);
            0
        }
        // Keep child controls (checkbox labels) on the same white background.
        WM_CTLCOLORBTN | WM_CTLCOLORSTATIC => {
            SetBkMode(wparam as HDC, TRANSPARENT as i32);
            GetStockObject(WHITE_BRUSH) as LRESULT
        }
        WM_TIMER => {
            if wparam == REFRESH_TIMER {
                // Repaint only the client-list panel to avoid flickering the QR.
                let panel = RECT {
                    left: 0,
                    top: 400,
                    right: 560,
                    bottom: 660,
                };
                InvalidateRect(hwnd, &panel, 1);
            }
            0
        }
        WM_CLOSE => {
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, REFRESH_TIMER);
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_window(hwnd: HWND) {
    ShowWindow(hwnd, SW_SHOW);
    ShowWindow(hwnd, SW_RESTORE);
    SetForegroundWindow(hwnd);
    InvalidateRect(hwnd, null(), 1);
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    AppendMenuW(menu, MF_STRING, ID_SHOW, wide("Show pairing QR").as_ptr());
    AppendMenuW(menu, MF_STRING, ID_COPY, wide("Copy pairing code").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit StreamBeam").as_ptr());

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // Foreground the owner first, or the menu won't dismiss on an outside click.
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    DestroyMenu(menu);
}

/// Drain queued pairing requests, prompting the user modally for each.
unsafe fn process_approvals(hwnd: HWND) {
    while let Some(req) = APP.get().and_then(|s| s.approvals.lock().ok()?.pop_front()) {
        let body = format!(
            "\"{}\" ({}) wants to link to this PC.\n\nAllow it to connect and receive your screen?",
            req.client_name, req.client_addr
        );
        let approved = MessageBoxW(
            hwnd,
            wide(&body).as_ptr(),
            wide("StreamBeam — pairing request").as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
        ) == IDYES;
        let _ = req.respond.send(approved);
    }
}

unsafe fn add_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
    write_wide_fixed(&mut nid.szTip, "StreamBeam Host");
    Shell_NotifyIconW(NIM_ADD, &mut nid);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    Shell_NotifyIconW(NIM_DELETE, &mut nid);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);

    let white = brush(rgb(255, 255, 255));
    FillRect(hdc, &client, white);
    DeleteObject(white);

    if let Some(payload) = current_payload() {
        draw_qr(hdc, &client, &payload);
        draw_text(
            hdc,
            24,
            300,
            client.right - 24,
            326,
            "Scan with the StreamBeam Android app",
        );
    } else {
        draw_text(hdc, 24, 150, client.right - 24, 190, "Starting StreamBeam host…");
    }

    draw_clients(hdc, &client);

    EndPaint(hwnd, &ps);
}

/// Draw the "Connected clients" heading and the current list below the toggles.
unsafe fn draw_clients(hdc: HDC, client: &RECT) {
    draw_text_left(hdc, 24, 408, client.right - 24, 430, "Connected clients");
    let clients = app_controls().map(|c| c.clients()).unwrap_or_default();
    if clients.is_empty() {
        draw_text_left(hdc, 32, 434, client.right - 24, 458, "— none connected —");
        return;
    }
    let mut y = 434;
    for entry in clients.iter().take(7) {
        let line = format!("•  {}   {}", entry.name, entry.addr.ip());
        draw_text_left(hdc, 32, y, client.right - 24, y + 22, &line);
        y += 22;
    }
}

unsafe fn draw_qr(hdc: windows_sys::Win32::Graphics::Gdi::HDC, client: &RECT, payload: &str) {
    let Ok(code) = QrCode::new(payload.as_bytes()) else {
        draw_text(hdc, 24, 230, client.right - 24, 280, "Could not render QR code");
        return;
    };

    let modules = code.width() as i32;
    let quiet = 4;
    let total = modules + quiet * 2;
    let max_size = (client.right - client.left - 160).min(280).max(140);
    let cell = (max_size / total).max(1);
    let size = total * cell;
    let left = (client.right - client.left - size) / 2;
    let top = 16;
    let black = brush(rgb(0, 0, 0));

    for y in 0..modules {
        for x in 0..modules {
            if code[(x as usize, y as usize)] == Color::Dark {
                let rect = RECT {
                    left: left + (x + quiet) * cell,
                    top: top + (y + quiet) * cell,
                    right: left + (x + quiet + 1) * cell,
                    bottom: top + (y + quiet + 1) * cell,
                };
                FillRect(hdc, &rect, black);
            }
        }
    }

    DeleteObject(black);
}

unsafe fn draw_text(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    text: &str,
) {
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    let text = wide(text);
    DrawTextW(hdc, text.as_ptr(), -1, &mut rect, DT_CENTER | DT_TOP);
}

unsafe fn draw_text_left(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32, text: &str) {
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    let text = wide(text);
    // DT_LEFT is the default (0), so DT_TOP alone left-aligns.
    DrawTextW(hdc, text.as_ptr(), -1, &mut rect, DT_TOP);
}

/// Create a themed auto-checkbox child control and return its handle.
unsafe fn create_checkbox(parent: HWND, id: usize, label: &str, x: i32, y: i32) -> HWND {
    let class = wide("BUTTON");
    let text = wide(label);
    CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class.as_ptr(),
        text.as_ptr(),
        WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32,
        x,
        y,
        360,
        24,
        parent,
        id as HMENU,
        GetModuleHandleW(null()),
        null_mut(),
    )
}

unsafe fn copy_to_clipboard(hwnd: HWND, value: &str) -> Result<(), String> {
    if OpenClipboard(hwnd) == 0 {
        return Err("OpenClipboard failed".into());
    }
    EmptyClipboard();

    let mut text = wide(value);
    let bytes = text.len() * std::mem::size_of::<u16>();
    let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if handle.is_null() {
        CloseClipboard();
        return Err("GlobalAlloc failed".into());
    }
    let ptr = GlobalLock(handle) as *mut u16;
    if ptr.is_null() {
        CloseClipboard();
        return Err("GlobalLock failed".into());
    }
    std::ptr::copy_nonoverlapping(text.as_mut_ptr(), ptr, text.len());
    GlobalUnlock(handle);
    SetClipboardData(CF_UNICODETEXT, handle);
    CloseClipboard();
    Ok(())
}

/// Record a fatal server error and wake the UI thread to show it and quit.
fn report_fatal(state: &AppState, message: String) {
    *state.fatal.lock().unwrap() = Some(message);
    let hwnd = state.hwnd();
    if !hwnd.is_null() {
        unsafe {
            PostMessageW(hwnd, WM_SERVER_ERROR, 0, 0);
        }
    }
}

/// Create a named mutex; if it already existed, another instance owns it.
///
/// The handle is intentionally leaked — the OS holds the named mutex until this
/// process exits, which is exactly the single-instance lifetime we want.
unsafe fn already_running() -> bool {
    let name = wide("StreamBeamHostSingletonMutex");
    let _ = CreateMutexW(null(), 1, name.as_ptr());
    GetLastError() == ERROR_ALREADY_EXISTS
}

fn app_controls() -> Option<Arc<HostControls>> {
    APP.get().map(|s| s.controls.clone())
}

fn current_payload() -> Option<String> {
    APP.get()
        .and_then(|state| state.payload.lock().ok()?.clone())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn write_wide_fixed(target: &mut [u16], value: &str) {
    let encoded: Vec<u16> = value.encode_utf16().collect();
    let len = encoded.len().min(target.len().saturating_sub(1));
    target[..len].copy_from_slice(&encoded[..len]);
    target[len] = 0;
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

unsafe fn brush(color: COLORREF) -> HBRUSH {
    CreateSolidBrush(color)
}
