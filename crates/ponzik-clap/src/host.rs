//! Host side of a CLAP instance: the `clap_host` callbacks and their
//! extensions (`clap.gui`, `clap.timer-support`, `clap.posix-fd-support`,
//! `clap.params`, `clap.thread-check`, `clap.log`, `clap.latency`,
//! `clap.state`), and the plugin's editor ([`ClapEditor`]) driven from the UI
//! thread, which is also the main-thread handle on the plugin's state.
//!
//! `clap_host.host_data` points to the instance's [`HostShared`], which
//! outlives the plugin. Callbacks never take the `state` lock: the plugin may
//! call them while the editor holds it around a call into the plugin.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::Instant;

use ponzik_engine::ring::{self, Consumer, Producer};
use ponzik_engine::{EditorEvent, EditorSize, ParentWindow, PluginEditor, RunLoop, StateContext};

use crate::abi::*;

/// What the plugin reported in its output events, carried from the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OutMsg {
    Param(ClapId, f64),
    Begin(ClapId),
    End(ClapId),
}

/// Capacity of the audio → UI queue of edits.
const OUT_CAPACITY: usize = 1024;

/// Window API of this platform.
#[cfg(target_os = "macos")]
const API: &[u8] = CLAP_WINDOW_API_COCOA;
#[cfg(windows)]
const API: &[u8] = CLAP_WINDOW_API_WIN32;
#[cfg(not(any(target_os = "macos", windows)))]
const API: &[u8] = CLAP_WINDOW_API_X11;

fn api_of(parent: ParentWindow) -> &'static [u8] {
    match parent {
        ParentWindow::X11(_) => CLAP_WINDOW_API_X11,
        ParentWindow::Win32(_) => CLAP_WINDOW_API_WIN32,
        ParentWindow::Cocoa(_) => CLAP_WINDOW_API_COCOA,
    }
}

struct State {
    /// Null once the plugin is destroyed.
    plugin: *const ClapPlugin,
    gui: *const ClapPluginGui,
    timer: *const ClapPluginTimerSupport,
    fd: *const ClapPluginPosixFdSupport,
    has_gui: bool,
    open: bool,
    params: *const ClapPluginParams,
    /// Exposed parameters, read again after `rescan(CLAP_PARAM_RESCAN_VALUES)`.
    ids: Vec<ClapId>,
}

/// State shared by the host callbacks, the instance (audio thread) and its editor (UI thread).
pub(crate) struct HostShared {
    main_thread: ThreadId,
    state: Mutex<State>,
    run_loop: Mutex<RunLoop>,
    events: Mutex<Vec<EditorEvent>>,
    out: Mutex<Consumer<OutMsg>>,
    /// `request_callback`: `on_main_thread` is due.
    callback: AtomicBool,
    /// `clap.params` `rescan` with new values (preset loaded in the plugin).
    rescan: AtomicBool,
}

// SAFETY: the plugin pointers are only used on the UI thread, under `state`.
unsafe impl Send for HostShared {}
unsafe impl Sync for HostShared {}

thread_local! {
    /// The current thread runs an audio-thread call of an instance.
    static AUDIO: Cell<bool> = const { Cell::new(false) };
}

/// Runs an audio-thread call (`process`, `start_processing`…): whatever thread
/// makes it (audio, worker, offline render, or the UI tearing down an instance
/// the engine gave back) is the instance's audio thread for `clap.thread-check`.
pub(crate) fn as_audio_thread<R>(f: impl FnOnce() -> R) -> R {
    let was = AUDIO.with(|a| a.replace(true));
    let r = f();
    AUDIO.with(|a| a.set(was));
    r
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl HostShared {
    /// Shared state and the audio side of the edit queue.
    pub(crate) fn new() -> (Arc<HostShared>, Producer<OutMsg>) {
        let (tx, rx) = ring::channel(OUT_CAPACITY);
        let shared = HostShared {
            main_thread: std::thread::current().id(),
            state: Mutex::new(State {
                plugin: null(),
                gui: null(),
                timer: null(),
                fd: null(),
                has_gui: false,
                open: false,
                params: null(),
                ids: Vec::new(),
            }),
            run_loop: Mutex::new(RunLoop::new()),
            events: Mutex::new(Vec::new()),
            out: Mutex::new(rx),
            callback: AtomicBool::new(false),
            rescan: AtomicBool::new(false),
        };
        (Arc::new(shared), tx)
    }

    /// The plugin is initialized: its extensions can be queried.
    pub(crate) fn attach(&self, plugin: *const ClapPlugin, ids: Vec<ClapId>) {
        let ext = |id: &[u8]| {
            // SAFETY: initialized plugin, NUL-terminated id.
            unsafe { ((*plugin).get_extension)(plugin, id.as_ptr().cast()) }
        };
        let gui = ext(CLAP_EXT_GUI).cast::<ClapPluginGui>();
        // SAFETY: extension returned by the plugin, checked for null.
        let has_gui = unsafe { gui.as_ref() }.is_some_and(|g| unsafe { (g.is_api_supported)(plugin, API.as_ptr().cast(), false) });
        let mut st = lock(&self.state);
        st.plugin = plugin;
        st.gui = gui;
        st.timer = ext(CLAP_EXT_TIMER_SUPPORT).cast();
        st.fd = ext(CLAP_EXT_POSIX_FD_SUPPORT).cast();
        st.has_gui = has_gui;
        st.params = ext(CLAP_EXT_PARAMS).cast();
        st.ids = ids;
    }

    /// The plugin is about to be destroyed: its interface goes first, and the
    /// editor turns inert.
    pub(crate) fn detach(&self) {
        let mut st = lock(&self.state);
        close_gui(&mut st);
        st.plugin = null();
        st.gui = null();
        st.timer = null();
        st.fd = null();
        st.params = null();
        st.has_gui = false;
        *lock(&self.run_loop) = RunLoop::new();
    }

    fn push(&self, e: EditorEvent) {
        lock(&self.events).push(e);
    }
}

fn close_gui(st: &mut State) {
    if !st.open {
        return;
    }
    st.open = false;
    // SAFETY: interface created by `open`, plugin alive (UI thread).
    if let Some(gui) = unsafe { st.gui.as_ref() } {
        unsafe {
            (gui.hide)(st.plugin);
            (gui.destroy)(st.plugin);
        }
    }
}

// --- clap_host -------------------------------------------------------------

fn shared<'a>(host: *const ClapHost) -> Option<&'a HostShared> {
    // SAFETY: `host_data` points to the `HostShared` kept alive by the instance.
    unsafe { host.as_ref().and_then(|h| (h.host_data as *const HostShared).as_ref()) }
}

/// `clap_host` of an instance; `shared` must outlive it.
pub(crate) fn new_host(shared: &Arc<HostShared>) -> Box<ClapHost> {
    Box::new(ClapHost {
        clap_version: CLAP_VERSION,
        host_data: Arc::as_ptr(shared) as *mut c_void,
        name: c"Ponzik".as_ptr(),
        vendor: c"Ponzik".as_ptr(),
        url: c"https://github.com/ponzik".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: host_get_extension,
        request_restart: host_noop,
        request_process: host_noop,
        request_callback: host_request_callback,
    })
}

/// Transparent wrapper making the static tables of function pointers shareable.
struct Table<T>(T);
// SAFETY: constant tables of function pointers.
unsafe impl<T> Sync for Table<T> {}

static HOST_GUI: Table<ClapHostGui> = Table(ClapHostGui {
    resize_hints_changed: gui_hints_changed,
    request_resize: gui_request_resize,
    request_show: gui_request_show,
    request_hide: gui_request_hide,
    closed: gui_closed,
});
static HOST_TIMER: Table<ClapHostTimerSupport> =
    Table(ClapHostTimerSupport { register_timer: timer_register, unregister_timer: timer_unregister });
static HOST_FD: Table<ClapHostPosixFdSupport> =
    Table(ClapHostPosixFdSupport { register_fd: fd_register, modify_fd: fd_modify, unregister_fd: fd_unregister });
static HOST_PARAMS: Table<ClapHostParams> =
    Table(ClapHostParams { rescan: params_rescan, clear: params_clear, request_flush: host_noop });
static HOST_THREAD_CHECK: Table<ClapHostThreadCheck> =
    Table(ClapHostThreadCheck { is_main_thread, is_audio_thread });
static HOST_LOG: Table<ClapHostLog> = Table(ClapHostLog { log: host_log });
static HOST_LATENCY: Table<ClapHostLatency> = Table(ClapHostLatency { changed: host_noop });
static HOST_STATE: Table<ClapHostState> = Table(ClapHostState { mark_dirty: host_noop });

unsafe extern "C" fn host_get_extension(_: *const ClapHost, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return null();
    }
    // SAFETY: NUL-terminated string from the plugin.
    let id = unsafe { CStr::from_ptr(id) }.to_bytes_with_nul();
    fn p<T>(t: &'static Table<T>) -> *const c_void {
        &t.0 as *const T as *const c_void
    }
    match id {
        _ if id == CLAP_EXT_GUI => p(&HOST_GUI),
        _ if id == CLAP_EXT_TIMER_SUPPORT => p(&HOST_TIMER),
        _ if id == CLAP_EXT_POSIX_FD_SUPPORT && cfg!(unix) => p(&HOST_FD),
        _ if id == CLAP_EXT_PARAMS => p(&HOST_PARAMS),
        _ if id == CLAP_EXT_THREAD_CHECK => p(&HOST_THREAD_CHECK),
        _ if id == CLAP_EXT_LOG => p(&HOST_LOG),
        _ if id == CLAP_EXT_LATENCY => p(&HOST_LATENCY),
        _ if id == CLAP_EXT_STATE => p(&HOST_STATE),
        _ => null(),
    }
}

unsafe extern "C" fn host_noop(_: *const ClapHost) {}

unsafe extern "C" fn host_request_callback(host: *const ClapHost) {
    if let Some(s) = shared(host) {
        s.callback.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn gui_hints_changed(host: *const ClapHost) {
    if let Some(s) = shared(host) {
        s.push(EditorEvent::HintsChanged);
    }
}

unsafe extern "C" fn gui_request_resize(host: *const ClapHost, width: u32, height: u32) -> bool {
    shared(host).map(|s| s.push(EditorEvent::Resize { width, height })).is_some()
}

unsafe extern "C" fn gui_request_show(host: *const ClapHost) -> bool {
    shared(host).map(|s| s.push(EditorEvent::Show)).is_some()
}

unsafe extern "C" fn gui_request_hide(host: *const ClapHost) -> bool {
    shared(host).map(|s| s.push(EditorEvent::Hide)).is_some()
}

unsafe extern "C" fn gui_closed(host: *const ClapHost, _was_destroyed: bool) {
    // The host closes its window, which destroys the interface (and so
    // acknowledges `was_destroyed`).
    if let Some(s) = shared(host) {
        s.push(EditorEvent::Closed);
    }
}

unsafe extern "C" fn timer_register(host: *const ClapHost, period_ms: u32, id: *mut ClapId) -> bool {
    let Some(s) = shared(host) else { return false };
    if id.is_null() {
        return false;
    }
    let t = lock(&s.run_loop).add_timer(period_ms, Instant::now());
    // SAFETY: output provided by the plugin.
    unsafe { *id = t };
    true
}

unsafe extern "C" fn timer_unregister(host: *const ClapHost, id: ClapId) -> bool {
    shared(host).is_some_and(|s| lock(&s.run_loop).remove_timer(id))
}

// The CLAP descriptor flags match those of the run loop (read, write, error).
unsafe extern "C" fn fd_register(host: *const ClapHost, fd: i32, flags: u32) -> bool {
    shared(host).is_some_and(|s| lock(&s.run_loop).add_fd(fd, flags))
}

unsafe extern "C" fn fd_modify(host: *const ClapHost, fd: i32, flags: u32) -> bool {
    shared(host).is_some_and(|s| lock(&s.run_loop).modify_fd(fd, flags))
}

unsafe extern "C" fn fd_unregister(host: *const ClapHost, fd: i32) -> bool {
    shared(host).is_some_and(|s| lock(&s.run_loop).remove_fd(fd))
}

unsafe extern "C" fn params_rescan(host: *const ClapHost, flags: u32) {
    if flags & CLAP_PARAM_RESCAN_VALUES != 0
        && let Some(s) = shared(host)
    {
        s.rescan.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn params_clear(_: *const ClapHost, _: ClapId, _: u32) {}

unsafe extern "C" fn is_main_thread(host: *const ClapHost) -> bool {
    !AUDIO.with(Cell::get) && shared(host).is_some_and(|s| std::thread::current().id() == s.main_thread)
}

unsafe extern "C" fn is_audio_thread(host: *const ClapHost) -> bool {
    AUDIO.with(Cell::get) || shared(host).is_some_and(|s| std::thread::current().id() != s.main_thread)
}

unsafe extern "C" fn host_log(_: *const ClapHost, severity: i32, msg: *const c_char) {
    // Warnings and worse (debug 0, info 1).
    if severity >= 2 && !msg.is_null() {
        // SAFETY: NUL-terminated string from the plugin.
        eprintln!("CLAP: {}", unsafe { CStr::from_ptr(msg) }.to_string_lossy());
    }
}

// --- Editor ----------------------------------------------------------------

/// `clap.gui` of an instance, as seen by the app.
pub(crate) struct ClapEditor(pub(crate) Arc<HostShared>);

impl PluginEditor for ClapEditor {
    fn has_gui(&self) -> bool {
        lock(&self.0.state).has_gui
    }

    fn is_alive(&self) -> bool {
        !lock(&self.0.state).plugin.is_null()
    }

    fn is_open(&self) -> bool {
        lock(&self.0.state).open
    }

    fn open(&self, parent: ParentWindow, scale: f64) -> Result<EditorSize, String> {
        let mut st = lock(&self.0.state);
        let plugin = st.plugin;
        // SAFETY: extension of the live plugin, checked for null; UI thread.
        let Some(gui) = (unsafe { st.gui.as_ref() }).filter(|_| !plugin.is_null()) else {
            return Err("this plugin has no interface".into());
        };
        let api = api_of(parent).as_ptr().cast::<c_char>();
        // SAFETY: documented `clap.gui` sequence, on the UI thread, plugin alive.
        unsafe {
            if !st.open {
                if !(gui.is_api_supported)(plugin, api, false) {
                    return Err("the plugin's interface does not support this window system".into());
                }
                if !(gui.create)(plugin, api, false) {
                    return Err("the plugin could not create its interface".into());
                }
                // Cocoa sizes are in points: the scale is not the host's business.
                if !matches!(parent, ParentWindow::Cocoa(_)) {
                    (gui.set_scale)(plugin, scale);
                }
                let window = ClapWindow { api, specific: parent.raw() };
                if !(gui.set_parent)(plugin, &window) {
                    (gui.destroy)(plugin);
                    return Err("the plugin could not embed its interface".into());
                }
                st.open = true;
            }
            let (mut w, mut h) = (0, 0);
            if !(gui.get_size)(plugin, &mut w, &mut h) || w == 0 || h == 0 {
                (w, h) = (640, 480);
            }
            let resizable = (gui.can_resize)(plugin);
            (gui.show)(plugin);
            Ok(EditorSize { width: w, height: h, resizable })
        }
    }

    fn close(&self) {
        close_gui(&mut lock(&self.0.state));
    }

    fn set_size(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let st = lock(&self.0.state);
        // SAFETY: extension of the live plugin; UI thread.
        let gui = unsafe { st.gui.as_ref() }.filter(|_| st.open)?;
        let (mut w, mut h) = (width.max(1), height.max(1));
        // SAFETY: interface created, outputs are locals.
        unsafe {
            if !(gui.can_resize)(st.plugin) {
                return None;
            }
            (gui.adjust_size)(st.plugin, &mut w, &mut h);
            (gui.set_size)(st.plugin, w, h).then_some((w, h))
        }
    }

    fn resizable(&self) -> bool {
        let st = lock(&self.0.state);
        // SAFETY: extension of the live plugin; UI thread.
        st.open && unsafe { st.gui.as_ref() }.is_some_and(|g| unsafe { (g.can_resize)(st.plugin) })
    }

    fn idle(&self, out: &mut Vec<EditorEvent>) {
        let s = &*self.0;
        {
            let st = lock(&s.state);
            let plugin = st.plugin;
            if !plugin.is_null() {
                // SAFETY: plugin alive, main-thread callbacks; the run loop is
                // unlocked while the plugin runs (it may add or remove entries).
                unsafe {
                    if s.callback.swap(false, Ordering::AcqRel) {
                        ((*plugin).on_main_thread)(plugin);
                    }
                    if s.rescan.swap(false, Ordering::AcqRel)
                        && let Some(params) = st.params.as_ref()
                    {
                        for &id in &st.ids {
                            let mut v = 0.0;
                            if (params.get_value)(plugin, id, &mut v) {
                                out.push(EditorEvent::Param { id, value: v as f32 });
                            }
                        }
                    }
                    if let Some(timer) = st.timer.as_ref() {
                        let due = lock(&s.run_loop).due_timers(Instant::now());
                        for id in due {
                            (timer.on_timer)(plugin, id);
                        }
                    }
                    if let Some(fd) = st.fd.as_ref() {
                        let ready = lock(&s.run_loop).ready_fds();
                        for (f, flags) in ready {
                            (fd.on_fd)(plugin, f, flags);
                        }
                    }
                }
            }
        }
        let mut rx = lock(&s.out);
        while let Some(m) = rx.pop() {
            out.push(match m {
                OutMsg::Param(id, v) => EditorEvent::Param { id, value: v as f32 },
                OutMsg::Begin(id) => EditorEvent::GestureBegin(id),
                OutMsg::End(id) => EditorEvent::GestureEnd(id),
            });
        }
        drop(rx);
        out.append(&mut lock(&s.events));
    }

    /// The plugin hears host changes through its input events: its interface
    /// follows by itself.
    fn param_changed(&self, _: u32, _: f32) {}

    fn save_state(&self, context: StateContext) -> Option<Vec<u8>> {
        let st = lock(&self.0.state);
        // Plugin alive, main-thread call (the lock keeps it alive).
        (!st.plugin.is_null()).then(|| crate::state::save(st.plugin, context)).flatten()
    }

    fn load_state(&self, data: &[u8], context: StateContext) -> Result<Vec<(u32, f32)>, String> {
        let st = lock(&self.0.state);
        if st.plugin.is_null() {
            return Err("the plugin is no longer loaded".into());
        }
        crate::state::load(st.plugin, data, context)?;
        Ok(crate::state::read_values(st.plugin, &st.ids))
    }
}
