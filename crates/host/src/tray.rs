use std::collections::VecDeque;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;

use qrcode::{Color, QrCode};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    SetBkMode, DT_CENTER, DT_TOP, HBRUSH, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, LoadIconW,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, ShowWindow,
    TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, IDC_ARROW,
    IDI_APPLICATION, IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MF_SEPARATOR, MF_STRING, MSG, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_LBUTTONUP,
    WM_PAINT, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::server::PairApproval;
use crate::{build_config, server, Opt};

const CLASS_NAME: &str = "StreamBeamHostWindow";
const TRAY_ID: u32 = 1;
const WM_TRAY: u32 = WM_USER + 1;
/// Posted to the UI thread when a device is waiting for pairing approval.
const WM_PAIR_REQUEST: u32 = WM_USER + 2;
const ID_COPY: usize = 1001;
const ID_SHOW: usize = 1002;
const ID_QUIT: usize = 1003;
/// `CF_UNICODETEXT` clipboard format (avoids pulling the `Win32_System_Ole` feature).
const CF_UNICODETEXT: u32 = 13;

static APP: OnceLock<Arc<AppState>> = OnceLock::new();

struct AppState {
    hwnd: AtomicIsize,
    payload: Mutex<Option<String>>,
    /// Devices waiting for the user to allow/deny, drained on the UI thread.
    approvals: Mutex<VecDeque<PairApproval>>,
}

impl AppState {
    fn hwnd(&self) -> HWND {
        self.hwnd.load(Ordering::SeqCst) as HWND
    }
}

pub fn run(opt: Opt) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (payload_tx, payload_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel::<PairApproval>();
    let state = Arc::new(AppState {
        hwnd: AtomicIsize::new(0),
        payload: Mutex::new(None),
        approvals: Mutex::new(VecDeque::new()),
    });
    let _ = APP.set(state.clone());

    let config = build_config(opt, Some(payload_tx), Some(approval_tx));
    thread::spawn(move || {
        eprintln!(
            "StreamBeam host {} (mock encoder — real capture in M2)",
            env!("CARGO_PKG_VERSION")
        );
        let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("host runtime error: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(server::run(config)) {
            eprintln!("host error: {e}");
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
            let button_class = wide("BUTTON");
            let label = wide("Copy code");
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                button_class.as_ptr(),
                label.as_ptr(),
                WS_CHILD | WS_VISIBLE,
                210,
                570,
                130,
                36,
                hwnd,
                ID_COPY as HMENU,
                GetModuleHandleW(null()),
                null_mut(),
            );
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
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_CLOSE => {
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_DESTROY => {
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

    let payload = current_payload();
    if let Some(payload) = payload {
        draw_qr(hdc, &client, &payload);
        draw_text(hdc, 24, 500, client.right - 24, 550, "Scan with StreamBeam Android");
    } else {
        draw_text(hdc, 24, 230, client.right - 24, 280, "Starting StreamBeam host...");
    }

    EndPaint(hwnd, &ps);
}

unsafe fn draw_qr(hdc: windows_sys::Win32::Graphics::Gdi::HDC, client: &RECT, payload: &str) {
    let Ok(code) = QrCode::new(payload.as_bytes()) else {
        draw_text(hdc, 24, 230, client.right - 24, 280, "Could not render QR code");
        return;
    };

    let modules = code.width() as i32;
    let quiet = 4;
    let total = modules + quiet * 2;
    let max_size = (client.right - client.left - 72).min(440).max(160);
    let cell = (max_size / total).max(1);
    let size = total * cell;
    let left = (client.right - client.left - size) / 2;
    let top = 36;
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
