//! Fenêtre Win32 : FFI directe vers user32 / gdi32 / dwmapi, sans crate `windows`.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::Duration;

use crate::{Event, Key, Modifiers, MouseButton, WindowError};

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
    fn AdjustWindowRectEx(rect: *mut RECT, style: u32, menu: i32, ex_style: u32) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn ValidateRect(hwnd: HWND, rect: *const RECT) -> i32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> i32;
    fn ScreenToClient(hwnd: HWND, pt: *mut POINT) -> i32;
    fn TrackMouseEvent(tme: *mut TRACKMOUSEEVENT) -> i32;
    fn GetKeyState(vk: i32) -> i16;
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
const WS_VISIBLE: u32 = 0x1000_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;
const CS_HREDRAW: u32 = 0x2;
const CS_VREDRAW: u32 = 0x1;
const IDC_ARROW: usize = 32512;
const PM_REMOVE: u32 = 1;
const QS_ALLINPUT: u32 = 0x04FF;
const SRCCOPY: u32 = 0x00CC_0020;
const TME_LEAVE: u32 = 0x2;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;

const WM_SIZE: u32 = 0x0005;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
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

const MIN_SIZE: (i32, i32) = (960, 640);

thread_local! {
    static QUEUE: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
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
        0x30..=0x39 | 0x41..=0x5A => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ => return None,
    })
}

fn min_window_size() -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: MIN_SIZE.0, bottom: MIN_SIZE.1 };
    // SAFETY: RECT valide.
    unsafe { AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, 0, 0) };
    (r.right - r.left, r.bottom - r.top)
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
                if let Some(key) = vk_to_key(w) {
                    push(Event::KeyDown { key, mods: modifiers() });
                }
                if msg == WM_SYSKEYDOWN {
                    // Laisse Windows gérer Alt+F4 et compagnie.
                    return DefWindowProcW(hwnd, msg, w, l);
                }
            }
            WM_KEYUP => {
                if let Some(key) = vk_to_key(w) {
                    push(Event::KeyUp { key });
                }
            }
            WM_KILLFOCUS => push(Event::FocusLost),
            _ => return DefWindowProcW(hwnd, msg, w, l),
        }
    }
    0
}

pub(crate) struct Window {
    hwnd: HWND,
    size: (u32, u32),
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

            let mut r = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, 0, 0);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                r.right - r.left,
                r.bottom - r.top,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            );
            if hwnd.is_null() {
                return Err(WindowError::System("CreateWindowExW a échoué".into()));
            }
            let dark: i32 = 1;
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const i32).cast(), 4);

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client);
            Ok(Window { hwnd, size: ((client.right - client.left) as u32, (client.bottom - client.top) as u32) })
        }
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

    pub(crate) fn backend_name(&self) -> &'static str {
        "Win32 · GDI"
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: fenêtre créée par ce thread.
        unsafe { DestroyWindow(self.hwnd) };
    }
}
