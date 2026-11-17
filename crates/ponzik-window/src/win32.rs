//! Fenêtre Win32 : FFI directe vers user32 / gdi32 / dwmapi, sans crate `windows`.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::Duration;

use crate::{Cursor, Event, Key, Modifiers, MouseButton, NativeHandle, PluginWindowOptions, WindowError};

type HANDLE = *mut c_void;
type HWND = HANDLE;
type HDC = HANDLE;
type HINSTANCE = HANDLE;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WNDPROC = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: u32,
    style: u32,
    lpfnWndProc: WNDPROC,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HINSTANCE,
    hIcon: HANDLE,
    hCursor: HANDLE,
    hbrBackground: HANDLE,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wParam: WPARAM,
    lParam: LPARAM,
    time: u32,
    pt: POINT,
}

#[repr(C)]
#[derive(Default)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MINMAXINFO {
    ptReserved: POINT,
    ptMaxSize: POINT,
    ptMaxPosition: POINT,
    ptMinTrackSize: POINT,
    ptMaxTrackSize: POINT,
}

#[repr(C)]
struct TRACKMOUSEEVENT {
    cbSize: u32,
    dwFlags: u32,
    hwndTrack: HWND,
    dwHoverTime: u32,
}

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(class: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HANDLE,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const MSG) -> i32;
    fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    fn MsgWaitForMultipleObjects(count: u32, handles: *const HANDLE, wait_all: i32, ms: u32, mask: u32) -> u32;
    fn LoadCursorW(instance: HINSTANCE, name: *const u16) -> HANDLE;
    fn SetCursor(cursor: HANDLE) -> HANDLE;
    fn AdjustWindowRectExForDpi(rect: *mut RECT, style: u32, menu: i32, ex_style: u32, dpi: u32) -> i32;
    fn SetProcessDpiAwarenessContext(context: isize) -> i32;
    fn GetDpiForWindow(hwnd: HWND) -> u32;
    fn SetWindowPos(hwnd: HWND, after: HWND, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn ValidateRect(hwnd: HWND, rect: *const RECT) -> i32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> i32;
    fn ScreenToClient(hwnd: HWND, pt: *mut POINT) -> i32;
    fn TrackMouseEvent(tme: *mut TRACKMOUSEEVENT) -> i32;
    fn GetKeyState(vk: i32) -> i16;
    fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn GetWindowLongPtrW(hwnd: HWND, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: HWND, index: i32, value: isize) -> isize;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(hwnd: HWND, accept: i32);
    fn DragQueryFileW(drop: HANDLE, index: u32, file: *mut u16, len: u32) -> u32;
    fn DragQueryPoint(drop: HANDLE, pt: *mut POINT) -> i32;
    fn DragFinish(drop: HANDLE);
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn StretchDIBits(
        hdc: HDC,
        xd: i32,
        yd: i32,
        wd: i32,
        hd: i32,
        xs: i32,
        ys: i32,
        ws: i32,
        hs: i32,
        bits: *const c_void,
        info: *const BITMAPINFO,
        usage: u32,
        rop: u32,
    ) -> i32;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: HWND, attr: u32, value: *const c_void, size: u32) -> i32;
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;
const CS_HREDRAW: u32 = 0x2;
const CS_VREDRAW: u32 = 0x1;
const IDC_ARROW: usize = 32512;
const IDC_IBEAM: usize = 32513;
const IDC_CROSS: usize = 32515;
const IDC_SIZENWSE: usize = 32642;
const IDC_SIZEWE: usize = 32644;
const IDC_SIZENS: usize = 32645;
const IDC_SIZEALL: usize = 32646;
const IDC_NO: usize = 32648;
const IDC_HAND: usize = 32649;
const HTCLIENT: isize = 1;
const PM_REMOVE: u32 = 1;
const QS_ALLINPUT: u32 = 0x04FF;
const SRCCOPY: u32 = 0x00CC_0020;
const TME_LEAVE: u32 = 0x2;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 : Windows ne met plus la fenêtre
/// à l'échelle en bitmap, on reçoit les vrais pixels et WM_DPICHANGED.
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;
const BASE_DPI: u32 = 96;
const SWP_NOMOVE: u32 = 0x2;
const SWP_NOZORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
const SW_SHOW: i32 = 5;

const WM_SIZE: u32 = 0x0005;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SETCURSOR: u32 = 0x0020;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_DROPFILES: u32 = 0x0233;

const MIN_SIZE: (i32, i32) = (960, 640);
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;
const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
const GWL_STYLE: i32 = -16;
const SWP_FRAMECHANGED: u32 = 0x20;
const SWP_NOSIZE: u32 = 0x1;

thread_local! {
    static QUEUE: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    /// Curseur voulu dans la zone cliente (nul : celui de la classe).
    static CURSOR: Cell<HANDLE> = const { Cell::new(null_mut()) };
    /// Points par pouce du moniteur de la fenêtre (96 = échelle 1).
    static DPI: Cell<u32> = const { Cell::new(BASE_DPI) };
}

thread_local! {
    /// Events of the plugin windows, by window.
    static PLUGIN_QUEUE: RefCell<Vec<(HWND, Event)>> = const { RefCell::new(Vec::new()) };
}

fn push(ev: Event) {
    QUEUE.with(|q| q.borrow_mut().push(ev));
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn point(l: LPARAM) -> (f32, f32) {
    ((l & 0xffff) as u16 as i16 as f32, ((l >> 16) & 0xffff) as u16 as i16 as f32)
}

fn modifiers() -> Modifiers {
    // SAFETY: lecture de l'état clavier du thread courant.
    unsafe {
        Modifiers { shift: GetKeyState(0x10) < 0, ctrl: GetKeyState(0x11) < 0, alt: GetKeyState(0x12) < 0 }
    }
}

fn vk_to_key(vk: usize) -> Option<Key> {
    Some(match vk {
        0x20 => Key::Space,
        0x0D => Key::Enter,
        0x1B => Key::Escape,
        0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x2E => Key::Delete,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x70..=0x7B => Key::F((vk - 0x70 + 1) as u8),
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x6B => Key::Char('+'),
        0x6D => Key::Char('-'),
        0x30..=0x39 | 0x41..=0x5A => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ => return None,
    })
}

/// Curseur système d'une forme. Windows n'a pas de main « saisie » : on
/// reprend les quatre flèches du déplacement.
fn cursor_id(cursor: Cursor) -> usize {
    match cursor {
        Cursor::Arrow => IDC_ARROW,
        Cursor::Hand => IDC_HAND,
        Cursor::Text => IDC_IBEAM,
        Cursor::ResizeH => IDC_SIZEWE,
        Cursor::ResizeV => IDC_SIZENS,
        Cursor::ResizeNwSe => IDC_SIZENWSE,
        Cursor::Move | Cursor::Grab => IDC_SIZEALL,
        Cursor::Crosshair => IDC_CROSS,
        Cursor::NotAllowed => IDC_NO,
    }
}

/// Taille de fenêtre (cadre compris) pour une zone cliente de `w` × `h` pixels
/// logiques, à `dpi`.
fn outer_size(w: i32, h: i32, dpi: u32) -> (i32, i32) {
    let px = |v: i32| (v as i64 * dpi as i64 / BASE_DPI as i64) as i32;
    let mut r = RECT { left: 0, top: 0, right: px(w), bottom: px(h) };
    // SAFETY: RECT valide.
    unsafe { AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPEDWINDOW, 0, 0, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

fn min_window_size() -> (i32, i32) {
    outer_size(MIN_SIZE.0, MIN_SIZE.1, DPI.get())
}

/// Files dropped from Explorer (`WM_DROPFILES`): names and client position.
/// Windows reports no hovering without OLE, only the drop.
unsafe fn dropped_files(drop: HANDLE) -> Event {
    // SAFETY: `drop` comes with WM_DROPFILES and is released here.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(got as usize);
            paths.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf)));
        }
        let mut pt = POINT::default();
        DragQueryPoint(drop, &mut pt);
        DragFinish(drop);
        Event::FileDrop { x: pt.x as f32, y: pt.y as f32, paths }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let button = |m| match m {
        WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
        WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
        _ => MouseButton::Middle,
    };
    // SAFETY: appels Win32 sur la fenêtre qui reçoit le message.
    unsafe {
        match msg {
            WM_CLOSE => push(Event::CloseRequested),
            WM_SIZE => {
                if w != 1 {
                    // 1 = SIZE_MINIMIZED
                    push(Event::Resized { width: (l & 0xffff) as u32, height: ((l >> 16) & 0xffff) as u32 });
                }
            }
            WM_PAINT => {
                ValidateRect(hwnd, null());
                push(Event::Redraw);
            }
            WM_ERASEBKGND => return 1,
            // Sans cela, Windows remettrait le curseur de la classe à chaque mouvement.
            WM_SETCURSOR if (l & 0xffff) == HTCLIENT && !CURSOR.get().is_null() => {
                SetCursor(CURSOR.get());
                return 1;
            }
            WM_GETMINMAXINFO => {
                let info = &mut *(l as *mut MINMAXINFO);
                let (mw, mh) = min_window_size();
                info.ptMinTrackSize = POINT { x: mw, y: mh };
            }
            WM_MOUSEMOVE => {
                if !TRACKING.get() {
                    let mut tme =
                        TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    TrackMouseEvent(&mut tme);
                    TRACKING.set(true);
                }
                let (x, y) = point(l);
                push(Event::MouseMove { x, y });
            }
            WM_MOUSELEAVE => {
                TRACKING.set(false);
                push(Event::MouseLeave);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                SetCapture(hwnd);
                let (x, y) = point(l);
                push(Event::MouseDown { x, y, button: button(msg), mods: modifiers() });
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                ReleaseCapture();
                let (x, y) = point(l);
                push(Event::MouseUp { x, y, button: button(msg) });
            }
            WM_MOUSEWHEEL => {
                let delta = ((w >> 16) & 0xffff) as u16 as i16 as f32 / 120.0;
                let mut pt = POINT { x: (l & 0xffff) as u16 as i16 as i32, y: ((l >> 16) & 0xffff) as u16 as i16 as i32 };
                ScreenToClient(hwnd, &mut pt);
                push(Event::Wheel { x: pt.x as f32, y: pt.y as f32, delta, mods: modifiers() });
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Bit 30 : la touche était déjà enfoncée (répétition automatique).
                if (l >> 30) & 1 == 0 {
                    push(Event::Physical { code: ((l >> 16) & 0xff) as u16, down: true });
                }
                if let Some(key) = vk_to_key(w) {
                    push(Event::KeyDown { key, mods: modifiers() });
                }
                if msg == WM_SYSKEYDOWN {
                    // Laisse Windows gérer Alt+F4 et compagnie.
                    return DefWindowProcW(hwnd, msg, w, l);
                }
            }
            WM_KEYUP => {
                push(Event::Physical { code: ((l >> 16) & 0xff) as u16, down: false });
                if let Some(key) = vk_to_key(w) {
                    push(Event::KeyUp { key });
                }
            }
            WM_CHAR => {
                if let Some(ch) = char::from_u32(w as u32).filter(|c| !c.is_control()) {
                    push(Event::Text(ch));
                }
            }
            WM_KILLFOCUS => push(Event::FocusLost),
            WM_DROPFILES => push(dropped_files(w as HANDLE)),
            WM_DPICHANGED => {
                // Fenêtre passée sur un autre moniteur, ou échelle changée :
                // Windows propose le rectangle qui garde la même taille apparente.
                let dpi = (w & 0xffff) as u32;
                let r = &*(l as *const RECT);
                SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                if dpi != DPI.get() && dpi > 0 {
                    DPI.set(dpi);
                    push(Event::ScaleChanged { scale: dpi as f32 / BASE_DPI as f32 });
                }
            }
            _ => return DefWindowProcW(hwnd, msg, w, l),
        }
    }
    0
}

/// Plugin windows only report closing and resizing; the plugin's child window
/// takes the input.
unsafe extern "system" fn plugin_wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let push = |ev| PLUGIN_QUEUE.with(|q| q.borrow_mut().push((hwnd, ev)));
    match msg {
        WM_CLOSE => push(Event::CloseRequested),
        WM_SIZE if w != 1 => push(Event::Resized { width: (l & 0xffff) as u32, height: ((l >> 16) & 0xffff) as u32 }),
        // SAFETY: message of this window.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    0
}

fn plugin_style(resizable: bool) -> u32 {
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    if resizable { style } else { style & !(WS_THICKFRAME | WS_MAXIMIZEBOX) }
}

/// Frame size for a client area of `w` × `h` physical pixels.
fn plugin_outer(w: u32, h: u32, style: u32, dpi: u32) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
    // SAFETY: RECT valide.
    unsafe { AdjustWindowRectExForDpi(&mut r, style, 0, 0, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

pub(crate) struct PluginWindow {
    hwnd: HWND,
    size: (u32, u32),
    resizable: bool,
}

impl PluginWindow {
    pub(crate) fn open_plugin(o: &PluginWindowOptions) -> Result<Self, WindowError> {
        let class_name = wide("PonzikPluginWindow");
        let title = wide(&o.title);
        let owner = match o.owner {
            Some(NativeHandle::Win32(h)) => h as HWND,
            _ => null_mut(),
        };
        // SAFETY: séquence standard RegisterClass / CreateWindow ; chaînes vivantes pendant l'appel.
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: plugin_wndproc,
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW as *const u16),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: null_mut(),
            };
            RegisterClassExW(&class);
            let style = plugin_style(o.resizable);
            let dpi = if owner.is_null() { BASE_DPI } else { GetDpiForWindow(owner).max(1) };
            let (ow, oh) = plugin_outer(o.width, o.height, style, dpi);
            let (x, y) = o.position.unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT));
            // Owned window: always above its owner, minimized with it.
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                style,
                x,
                y,
                ow,
                oh,
                owner,
                null_mut(),
                instance,
                null_mut(),
            );
            if hwnd.is_null() {
                return Err(WindowError::System("CreateWindowExW failed".into()));
            }
            let dark: i32 = 1;
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const i32).cast(), 4);
            ShowWindow(hwnd, SW_SHOW);
            Ok(PluginWindow { hwnd, size: (o.width, o.height), resizable: o.resizable })
        }
    }

    pub(crate) fn native_handle(&self) -> NativeHandle {
        NativeHandle::Win32(self.hwnd as usize)
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        // SAFETY: fenêtre vivante.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / BASE_DPI as f32 }
    }

    pub(crate) fn set_plugin_size(&mut self, width: u32, height: u32) {
        if (width, height) == self.size {
            return;
        }
        self.size = (width, height);
        // SAFETY: fenêtre vivante.
        unsafe {
            let (ow, oh) = plugin_outer(width, height, plugin_style(self.resizable), GetDpiForWindow(self.hwnd).max(1));
            SetWindowPos(self.hwnd, null_mut(), 0, 0, ow, oh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub(crate) fn set_resizable(&mut self, resizable: bool) {
        if resizable == self.resizable {
            return;
        }
        self.resizable = resizable;
        // SAFETY: fenêtre vivante ; le cadre est recalculé par SWP_FRAMECHANGED.
        unsafe {
            let keep = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32 & !plugin_style(true);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, (keep | plugin_style(resizable)) as isize);
            SetWindowPos(self.hwnd, null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOSIZE);
        }
    }

    pub(crate) fn position(&mut self) -> Option<(i32, i32)> {
        let mut r = RECT::default();
        // SAFETY: fenêtre vivante, RECT local.
        (unsafe { GetWindowRect(self.hwnd, &mut r) } != 0).then_some((r.left, r.top))
    }

    pub(crate) fn poll_plugin_events(&mut self, out: &mut Vec<Event>) {
        // SAFETY: pompe de messages du thread ; les messages de la fenêtre
        // principale passent par sa procédure et restent dans sa file.
        unsafe {
            let mut msg = std::mem::zeroed::<MSG>();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let hwnd = self.hwnd;
        PLUGIN_QUEUE.with(|q| {
            q.borrow_mut().retain(|(h, ev)| {
                if *h != hwnd {
                    return true;
                }
                match *ev {
                    Event::Resized { width, height } if (width, height) == self.size => {}
                    Event::Resized { width, height } => {
                        self.size = (width, height);
                        out.push(ev.clone());
                    }
                    _ => out.push(ev.clone()),
                }
                false
            });
        });
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        let hwnd = self.hwnd;
        PLUGIN_QUEUE.with(|q| q.borrow_mut().retain(|&(h, _)| h != hwnd));
        // SAFETY: fenêtre créée par ce thread.
        unsafe { DestroyWindow(hwnd) };
    }
}

pub(crate) struct Window {
    hwnd: HWND,
    size: (u32, u32),
    cursor: Cursor,
}

impl Window {
    pub(crate) fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        let class_name = wide("PonzikWindow");
        let title = wide(title);
        // SAFETY: séquence standard RegisterClass / CreateWindow ; les chaînes
        // UTF-16 vivent jusqu'à la fin de la fonction.
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: wndproc,
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW as *const u16),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: null_mut(),
            };
            // Échoue sans conséquence si la classe existe déjà.
            RegisterClassExW(&class);
            // Échoue sans conséquence si le manifeste l'a déjà fait (ou avant Windows 10 1703).
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

            let (ow, oh) = outer_size(width as i32, height as i32, BASE_DPI);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                ow,
                oh,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            );
            if hwnd.is_null() {
                return Err(WindowError::System("CreateWindowExW failed".into()));
            }
            let dark: i32 = 1;
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const i32).cast(), 4);
            // La taille demandée est logique : mise à l'échelle du moniteur avant d'afficher.
            let dpi = GetDpiForWindow(hwnd);
            if dpi > 0 && dpi != BASE_DPI {
                DPI.set(dpi);
                let (ow, oh) = outer_size(width as i32, height as i32, dpi);
                SetWindowPos(hwnd, null_mut(), 0, 0, ow, oh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            ShowWindow(hwnd, SW_SHOW);
            // Files can be dropped here from Explorer.
            DragAcceptFiles(hwnd, 1);

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client);
            Ok(Window { hwnd, size: ((client.right - client.left) as u32, (client.bottom - client.top) as u32), cursor: Cursor::Arrow })
        }
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        DPI.get() as f32 / BASE_DPI as f32
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    pub(crate) fn poll_events(&mut self, out: &mut Vec<Event>) {
        // SAFETY: pompe de messages du thread propriétaire de la fenêtre.
        unsafe {
            let mut msg = std::mem::zeroed::<MSG>();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        QUEUE.with(|q| {
            for ev in q.borrow_mut().drain(..) {
                if let Event::Resized { width, height } = ev {
                    if (width, height) == self.size {
                        continue;
                    }
                    self.size = (width, height);
                }
                out.push(ev);
            }
        });
    }

    pub(crate) fn wait_events(&mut self, timeout: Duration, out: &mut Vec<Event>) {
        let queued = QUEUE.with(|q| !q.borrow().is_empty());
        if !queued {
            let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            // SAFETY: attente sans handle, réveillée par tout message.
            unsafe { MsgWaitForMultipleObjects(0, null(), 0, ms, QS_ALLINPUT) };
        }
        self.poll_events(out);
    }

    pub(crate) fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // lignes de haut en bas
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0],
        };
        // SAFETY: `pixels` contient width * height pixels BGRX (0x00RRGGBB en mémoire).
        unsafe {
            let hdc = GetDC(self.hwnd);
            let w = width as i32;
            let h = height as i32;
            StretchDIBits(hdc, 0, 0, w, h, 0, 0, w, h, pixels.as_ptr().cast(), &info, 0, SRCCOPY);
            ReleaseDC(self.hwnd, hdc);
        }
        Ok(())
    }

    pub(crate) fn set_cursor(&mut self, cursor: Cursor) {
        if self.cursor == cursor {
            return;
        }
        self.cursor = cursor;
        // SAFETY: curseurs système partagés, jamais libérés ; appel sur le thread de la fenêtre.
        unsafe {
            let handle = LoadCursorW(null_mut(), cursor_id(cursor) as *const u16);
            CURSOR.set(handle);
            // Appliqué tout de suite si la souris est sur la zone cliente ; sinon
            // au prochain WM_SETCURSOR.
            if TRACKING.get() {
                SetCursor(handle);
            }
        }
    }

    pub(crate) fn backend_name(&self) -> &'static str {
        "Win32 · GDI"
    }

    pub(crate) fn native_handle(&self) -> NativeHandle {
        NativeHandle::Win32(self.hwnd as usize)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: fenêtre créée par ce thread.
        unsafe { DestroyWindow(self.hwnd) };
    }
}
