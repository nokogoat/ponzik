//! Editor of a VST 3 instance: `IEditController::createView("editor")` →
//! `IPlugView`, embedded into a host window, with the host objects it talks
//! to: `IPlugFrame` (resize requests), `Steinberg::Linux::IRunLoop` (timers
//! and file descriptors, reached through the frame) and `IComponentHandler`
//! (edits made in the interface: `beginEdit` / `performEdit` / `endEdit`).
//!
//! Everything here runs on the UI thread, except the queue of the
//! processor's output parameter changes, fed by the audio thread.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use ponzik_engine::editor::FD_READ;
use ponzik_engine::ring::{self, Consumer, Producer};
use ponzik_engine::{EditorEvent, EditorSize, ParentWindow, PluginEditor, RunLoop, StateContext};

use crate::abi::*;
use crate::com::*;

/// Capacity of the audio → UI queue of output parameter changes.
const OUT_CAPACITY: usize = 1024;

/// Normalized VST 3 value → value of the instance's `ParamInfo` (`steps`:
/// `stepCount`, 0 for a continuous parameter).
pub(crate) fn to_value(steps: i32, n: f64) -> f32 {
    let n = n.clamp(0.0, 1.0);
    if steps > 0 { (n * steps as f64).round() as f32 } else { n as f32 }
}

/// Inverse of [`to_value`].
pub(crate) fn to_norm(steps: i32, v: f32) -> f64 {
    let v = if steps > 0 { v.round() as f64 / steps as f64 } else { v as f64 };
    v.clamp(0.0, 1.0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `release` on any interface of the plugin.
fn unref(p: *mut c_void) {
    if !p.is_null() {
        // SAFETY: owned reference on a live COM object.
        unsafe { ((**(p as *const *const FUnknownVtbl)).release)(p) };
    }
}

/// `addRef` on any interface of the plugin.
fn addref(p: *mut c_void) {
    if !p.is_null() {
        // SAFETY: live COM object.
        unsafe { ((**(p as *const *const FUnknownVtbl)).add_ref)(p) };
    }
}

struct ViewState {
    /// For the plugin's state; given back with the controller at `detach`.
    component: Option<Com<IComponent>>,
    controller: Option<Com<IEditController>>,
    view: Option<Com<IPlugView>>,
    frame: Owned<PlugFrame>,
}

/// Run loop of the instance and the plugin's handlers (one reference held each).
#[derive(Default)]
struct LoopState {
    lp: RunLoop,
    timers: Vec<(u32, *mut ITimerHandler)>,
    fds: Vec<(i32, *mut IEventHandler)>,
}

impl LoopState {
    fn clear(&mut self) {
        for (_, h) in self.timers.drain(..) {
            unref(h.cast());
        }
        for (_, h) in self.fds.drain(..) {
            unref(h.cast());
        }
        self.lp = RunLoop::new();
    }
}

/// State shared by the instance, its host objects and its editor.
pub(crate) struct Vst3Shared {
    state: Mutex<ViewState>,
    run_loop: Mutex<LoopState>,
    events: Mutex<Vec<EditorEvent>>,
    /// Size the plugin asked for (`resizeView`), applied by the next `set_size`.
    requested: Mutex<Option<(u32, u32)>>,
    out: Mutex<Consumer<(ParamId, f64)>>,
    /// `(id, stepCount)` of the exposed parameters, sorted by id.
    steps: OnceLock<Vec<(ParamId, i32)>>,
    /// Last value sent by `performEdit`, so it is not echoed back to the controller.
    edited: Mutex<Vec<(ParamId, f64)>>,
    /// `restartComponent(kParamValuesChanged)`: every value is to be read again.
    reread: AtomicBool,
    alive: AtomicBool,
}

// SAFETY: the plugin pointers are only used on the UI thread, under the locks.
unsafe impl Send for Vst3Shared {}
unsafe impl Sync for Vst3Shared {}

impl Vst3Shared {
    pub(crate) fn new() -> (Arc<Vst3Shared>, Producer<(ParamId, f64)>) {
        let (tx, rx) = ring::channel(OUT_CAPACITY);
        let shared = Arc::new_cyclic(|weak: &Weak<Vst3Shared>| {
            let run_loop = Owned::new(RunLoopObj(weak.clone()));
            Vst3Shared {
                state: Mutex::new(ViewState {
                    component: None,
                    controller: None,
                    view: None,
                    frame: Owned::new(PlugFrame { shared: weak.clone(), run_loop }),
                }),
                run_loop: Mutex::new(LoopState::default()),
                events: Mutex::new(Vec::new()),
                requested: Mutex::new(None),
                out: Mutex::new(rx),
                steps: OnceLock::new(),
                edited: Mutex::new(Vec::new()),
                reread: AtomicBool::new(false),
                alive: AtomicBool::new(true),
            }
        });
        (shared, tx)
    }

    /// The controller is ready; `steps` lists the exposed parameters.
    pub(crate) fn attach(
        &self,
        component: Com<IComponent>,
        controller: Option<Com<IEditController>>,
        mut steps: Vec<(ParamId, i32)>,
    ) {
        steps.sort_by_key(|s| s.0);
        let _ = self.steps.set(steps);
        let mut st = lock(&self.state);
        st.component = Some(component);
        st.controller = controller;
    }

    fn steps(&self) -> &[(ParamId, i32)] {
        self.steps.get().map_or(&[], Vec::as_slice)
    }

    /// The instance is about to be terminated: interface first, then every
    /// reference on the plugin is given back.
    pub(crate) fn detach(&self) {
        self.alive.store(false, Ordering::Release);
        let mut st = lock(&self.state);
        close_view(&mut st);
        st.controller = None;
        st.component = None;
        drop(st);
        lock(&self.run_loop).clear();
    }

    fn steps_of(&self, id: ParamId) -> i32 {
        let steps = self.steps.get().map_or(&[][..], Vec::as_slice);
        steps.binary_search_by_key(&id, |s| s.0).map_or(0, |i| steps[i].1)
    }

    fn push(&self, e: EditorEvent) {
        lock(&self.events).push(e);
    }
}

fn close_view(st: &mut ViewState) {
    if let Some(view) = st.view.take() {
        call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.removed)(p) });
        call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.set_frame)(p, null_mut()) });
    }
}

/// Calls a method of `com` through its table.
fn call<I: Interface, V, R>(com: &Com<I>, f: impl FnOnce(&V, *mut c_void) -> R) -> R {
    // SAFETY: live interface whose table is `V` (chosen by the caller).
    f(unsafe { com.vtbl::<V>() }, com.as_ptr())
}

fn platform_of(parent: ParentWindow) -> &'static [u8] {
    match parent {
        ParentWindow::X11(_) => K_PLATFORM_TYPE_X11_EMBED_WINDOW_ID,
        ParentWindow::Win32(_) => K_PLATFORM_TYPE_HWND,
        ParentWindow::Cocoa(_) => K_PLATFORM_TYPE_NSVIEW,
    }
}

// --- IComponentHandler -------------------------------------------------------

/// Receives the edits made in the plugin's interface.
pub(crate) struct ComponentHandler(pub(crate) Arc<Vst3Shared>);

impl HostObject for ComponentHandler {
    type Vtbl = IComponentHandlerVtbl;
    const VTBL: &'static IComponentHandlerVtbl = &IComponentHandlerVtbl {
        unknown: unknown_vtbl::<Self>(),
        begin_edit: handler_begin,
        perform_edit: handler_perform,
        end_edit: handler_end,
        restart_component: handler_restart,
    };
    const IIDS: &'static [Tuid] = &[ICOMPONENT_HANDLER_IID];
}

fn handler<'a>(this: *mut c_void) -> &'a Vst3Shared {
    // SAFETY: `this` is an `Obj<ComponentHandler>` (called through its table only).
    unsafe { &Obj::<ComponentHandler>::data(this).0 }
}

unsafe extern "system" fn handler_begin(this: *mut c_void, id: ParamId) -> TResult {
    handler(this).push(EditorEvent::GestureBegin(id));
    K_RESULT_OK
}

unsafe extern "system" fn handler_perform(this: *mut c_void, id: ParamId, n: ParamValue) -> TResult {
    let s = handler(this);
    let value = to_value(s.steps_of(id), n);
    let mut edited = lock(&s.edited);
    match edited.iter_mut().find(|e| e.0 == id) {
        Some(e) => e.1 = n,
        None => edited.push((id, n)),
    }
    drop(edited);
    s.push(EditorEvent::Param { id, value });
    K_RESULT_OK
}

unsafe extern "system" fn handler_end(this: *mut c_void, id: ParamId) -> TResult {
    handler(this).push(EditorEvent::GestureEnd(id));
    K_RESULT_OK
}

unsafe extern "system" fn handler_restart(this: *mut c_void, flags: i32) -> TResult {
    if flags & K_PARAM_VALUES_CHANGED != 0 {
        handler(this).reread.store(true, Ordering::Release);
    }
    K_RESULT_OK
}

// --- IPlugFrame --------------------------------------------------------------

/// Frame of the view: resize requests, and the run loop on Linux.
pub(crate) struct PlugFrame {
    shared: Weak<Vst3Shared>,
    run_loop: Owned<RunLoopObj>,
}

impl HostObject for PlugFrame {
    type Vtbl = IPlugFrameVtbl;
    const VTBL: &'static IPlugFrameVtbl = &IPlugFrameVtbl {
        unknown: FUnknownVtbl { query_interface: frame_query, add_ref, release: release::<Self> },
        resize_view: frame_resize,
    };
    const IIDS: &'static [Tuid] = &[IPLUG_FRAME_IID];
}

/// `queryInterface` of the frame: `IRunLoop` is a separate host object.
unsafe extern "system" fn frame_query(this: *mut c_void, iid: *const Tuid, obj: *mut *mut c_void) -> TResult {
    if obj.is_null() || iid.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY: 16-byte id and output provided by the plugin; `this` is an `Obj<PlugFrame>`.
    unsafe {
        let iid = *iid;
        let found: *mut c_void = if iid == FUNKNOWN_IID || iid == IPLUG_FRAME_IID {
            this
        } else if iid == IRUN_LOOP_IID && cfg!(all(unix, not(target_os = "macos"))) {
            Obj::<PlugFrame>::data(this).run_loop.as_ptr()
        } else {
            null_mut()
        };
        *obj = found;
        if found.is_null() {
            return K_NO_INTERFACE;
        }
        add_ref(found);
    }
    K_RESULT_OK
}

unsafe extern "system" fn frame_resize(this: *mut c_void, _: *mut IPlugView, size: *mut ViewRect) -> TResult {
    // SAFETY: `this` is an `Obj<PlugFrame>`; rectangle provided by the plugin.
    let (Some(s), Some(r)) = (unsafe { Obj::<PlugFrame>::data(this) }.shared.upgrade(), unsafe { size.as_ref() }) else {
        return K_INVALID_ARGUMENT;
    };
    let (width, height) = (r.width(), r.height());
    *lock(&s.requested) = Some((width, height));
    s.push(EditorEvent::Resize { width, height });
    K_RESULT_TRUE
}

// --- Steinberg::Linux::IRunLoop --------------------------------------------------

pub(crate) struct RunLoopObj(Weak<Vst3Shared>);

impl HostObject for RunLoopObj {
    type Vtbl = IRunLoopVtbl;
    const VTBL: &'static IRunLoopVtbl = &IRunLoopVtbl {
        unknown: unknown_vtbl::<Self>(),
        register_event_handler: loop_register_fd,
        unregister_event_handler: loop_unregister_fd,
        register_timer: loop_register_timer,
        unregister_timer: loop_unregister_timer,
    };
    const IIDS: &'static [Tuid] = &[IRUN_LOOP_IID];
}

fn run_loop_of(this: *mut c_void) -> Option<Arc<Vst3Shared>> {
    // SAFETY: `this` is an `Obj<RunLoopObj>` (called through its table only).
    unsafe { Obj::<RunLoopObj>::data(this) }.0.upgrade()
}

unsafe extern "system" fn loop_register_fd(this: *mut c_void, h: *mut IEventHandler, fd: i32) -> TResult {
    let Some(s) = run_loop_of(this) else { return K_INTERNAL_ERROR };
    if h.is_null() || fd < 0 {
        return K_INVALID_ARGUMENT;
    }
    let mut l = lock(&s.run_loop);
    l.lp.add_fd(fd, FD_READ);
    addref(h.cast());
    l.fds.push((fd, h));
    K_RESULT_TRUE
}

unsafe extern "system" fn loop_unregister_fd(this: *mut c_void, h: *mut IEventHandler) -> TResult {
    let Some(s) = run_loop_of(this) else { return K_INTERNAL_ERROR };
    let mut l = lock(&s.run_loop);
    let (gone, kept): (Vec<_>, Vec<_>) = l.fds.drain(..).partition(|e| e.1 == h);
    l.fds = kept;
    for (fd, _) in &gone {
        if !l.fds.iter().any(|e| e.0 == *fd) {
            l.lp.remove_fd(*fd);
        }
    }
    drop(l);
    for (_, h) in &gone {
        unref(h.cast());
    }
    if gone.is_empty() { K_INVALID_ARGUMENT } else { K_RESULT_TRUE }
}

unsafe extern "system" fn loop_register_timer(this: *mut c_void, h: *mut ITimerHandler, ms: u64) -> TResult {
    let Some(s) = run_loop_of(this) else { return K_INTERNAL_ERROR };
    if h.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let mut l = lock(&s.run_loop);
    let id = l.lp.add_timer(ms.min(u32::MAX as u64) as u32, Instant::now());
    addref(h.cast());
    l.timers.push((id, h));
    K_RESULT_TRUE
}

unsafe extern "system" fn loop_unregister_timer(this: *mut c_void, h: *mut ITimerHandler) -> TResult {
    let Some(s) = run_loop_of(this) else { return K_INTERNAL_ERROR };
    let mut l = lock(&s.run_loop);
    let (gone, kept): (Vec<_>, Vec<_>) = l.timers.drain(..).partition(|e| e.1 == h);
    l.timers = kept;
    for (id, _) in &gone {
        l.lp.remove_timer(*id);
    }
    drop(l);
    for (_, h) in &gone {
        unref(h.cast());
    }
    if gone.is_empty() { K_INVALID_ARGUMENT } else { K_RESULT_TRUE }
}

// --- Editor -------------------------------------------------------------------

pub(crate) struct Vst3Editor(pub(crate) Arc<Vst3Shared>);

impl Vst3Editor {
    /// Fires the due timers and ready descriptors, without any lock held
    /// while the plugin runs (each handler is kept alive during its call).
    fn pump(&self) {
        let s = &*self.0;
        let due: Vec<*mut ITimerHandler> = {
            let mut l = lock(&s.run_loop);
            let ids = l.lp.due_timers(Instant::now());
            l.timers.iter().filter(|t| ids.contains(&t.0)).map(|t| t.1).inspect(|h| addref(h.cast())).collect()
        };
        for h in due {
            // SAFETY: handler kept alive by the reference just taken.
            unsafe { ((*(*h).vtbl).on_timer)(h.cast()) };
            unref(h.cast());
        }
        let ready: Vec<(i32, *mut IEventHandler)> = {
            let l = lock(&s.run_loop);
            let fds = l.lp.ready_fds();
            l.fds.iter().filter(|e| fds.iter().any(|f| f.0 == e.0)).copied().inspect(|e| addref(e.1.cast())).collect()
        };
        for (fd, h) in ready {
            // SAFETY: handler kept alive by the reference just taken.
            unsafe { ((*(*h).vtbl).on_fd_is_set)(h.cast(), fd) };
            unref(h.cast());
        }
    }
}

impl PluginEditor for Vst3Editor {
    fn has_gui(&self) -> bool {
        lock(&self.0.state).controller.is_some()
    }

    fn is_alive(&self) -> bool {
        self.0.alive.load(Ordering::Acquire)
    }

    fn is_open(&self) -> bool {
        lock(&self.0.state).view.is_some()
    }

    fn open(&self, parent: ParentWindow, scale: f64) -> Result<EditorSize, String> {
        let mut st = lock(&self.0.state);
        let Some(ctrl) = st.controller.clone() else { return Err("this plugin has no interface".into()) };
        if st.view.is_none() {
            let raw = call(&ctrl, |v: &IEditControllerVtbl, p| unsafe { (v.create_view)(p, VIEW_TYPE_EDITOR.as_ptr().cast()) });
            // SAFETY: `createView` returns an owned reference or null.
            let Some(view) = (unsafe { Com::<IPlugView>::from_raw(raw) }) else {
                return Err("this plugin has no interface".into());
            };
            let platform = platform_of(parent).as_ptr().cast();
            if call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.is_platform_type_supported)(p, platform) }) != K_RESULT_OK {
                return Err("the plugin's interface does not support this window system".into());
            }
            let frame = st.frame.as_ptr::<IPlugFrame>();
            call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.set_frame)(p, frame) });
            // Cocoa sizes are in points: the scale is not the host's business.
            if !matches!(parent, ParentWindow::Cocoa(_))
                && let Some(sc) = view.query::<IPlugViewContentScaleSupport>()
            {
                call(&sc, |v: &IPlugViewContentScaleSupportVtbl, p| unsafe { (v.set_content_scale_factor)(p, scale as f32) });
            }
            let handle = parent.raw() as *mut c_void;
            if call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.attached)(p, handle, platform) }) != K_RESULT_OK {
                call(&view, |v: &IPlugViewVtbl, p| unsafe { (v.set_frame)(p, null_mut()) });
                return Err("the plugin could not embed its interface".into());
            }
            st.view = Some(view);
        }
        let view = st.view.as_ref().expect("view just created");
        let mut r = ViewRect::default();
        let (mut width, mut height) = (640, 480);
        if call(view, |v: &IPlugViewVtbl, p| unsafe { (v.get_size)(p, &mut r) }) == K_RESULT_OK && r.width() > 0 && r.height() > 0 {
            (width, height) = (r.width(), r.height());
        }
        let resizable = call(view, |v: &IPlugViewVtbl, p| unsafe { (v.can_resize)(p) }) == K_RESULT_TRUE;
        Ok(EditorSize { width, height, resizable })
    }

    fn close(&self) {
        close_view(&mut lock(&self.0.state));
        *lock(&self.0.requested) = None;
    }

    fn set_size(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let st = lock(&self.0.state);
        let view = st.view.as_ref()?;
        let mut r = ViewRect::sized(width.max(1), height.max(1));
        // The size the plugin asked for is taken as is; the user's goes
        // through its constraints.
        let asked = lock(&self.0.requested).take_if(|s| *s == (width, height)).is_some();
        if !asked {
            if call(view, |v: &IPlugViewVtbl, p| unsafe { (v.can_resize)(p) }) != K_RESULT_TRUE {
                return None;
            }
            call(view, |v: &IPlugViewVtbl, p| unsafe { (v.check_size_constraint)(p, &mut r) });
        }
        call(view, |v: &IPlugViewVtbl, p| unsafe { (v.on_size)(p, &mut r) });
        Some((r.width(), r.height()))
    }

    fn resizable(&self) -> bool {
        let st = lock(&self.0.state);
        st.view.as_ref().is_some_and(|view| call(view, |v: &IPlugViewVtbl, p| unsafe { (v.can_resize)(p) }) == K_RESULT_TRUE)
    }

    fn idle(&self, out: &mut Vec<EditorEvent>) {
        let s = &*self.0;
        if !self.is_alive() {
            out.append(&mut lock(&s.events));
            return;
        }
        self.pump();
        let ctrl = lock(&s.state).controller.clone();
        if let Some(ctrl) = ctrl {
            // Values changed inside the plugin (preset loaded in its interface).
            if s.reread.swap(false, Ordering::AcqRel) {
                for &(id, steps) in s.steps.get().map_or(&[][..], Vec::as_slice) {
                    let n = call(&ctrl, |v: &IEditControllerVtbl, p| unsafe { (v.get_param_normalized)(p, id) });
                    out.push(EditorEvent::Param { id, value: to_value(steps, n) });
                }
            }
            // The processor's output changes (meters, internal moves) reach the controller.
            let mut rx = lock(&s.out);
            while let Some((id, n)) = rx.pop() {
                call(&ctrl, |v: &IEditControllerVtbl, p| unsafe { (v.set_param_normalized)(p, id, n) });
            }
        }
        out.append(&mut lock(&s.events));
    }

    fn param_changed(&self, id: u32, value: f32) {
        let s = &*self.0;
        let n = to_norm(s.steps_of(id), value);
        {
            let mut edited = lock(&s.edited);
            if let Some(i) = edited.iter().position(|e| e.0 == id) {
                let echo = (edited[i].1 - n).abs() < 1e-6;
                edited.swap_remove(i);
                if echo {
                    return;
                }
            }
        }
        if let Some(ctrl) = lock(&s.state).controller.as_ref() {
            call(ctrl, |v: &IEditControllerVtbl, p| unsafe { (v.set_param_normalized)(p, id, n) });
        }
    }

    fn save_state(&self, _: StateContext) -> Option<Vec<u8>> {
        // References taken out of the lock: the plugin may call the host back.
        let (component, controller) = {
            let st = lock(&self.0.state);
            (st.component.clone()?, st.controller.clone())
        };
        crate::state::save(&component, controller.as_ref())
    }

    fn load_state(&self, data: &[u8], _: StateContext) -> Result<Vec<(u32, f32)>, String> {
        let (component, controller) = {
            let st = lock(&self.0.state);
            (st.component.clone().ok_or("the plugin is no longer loaded")?, st.controller.clone())
        };
        crate::state::load(&component, controller.as_ref(), data)?;
        Ok(crate::state::read_values(controller.as_ref(), self.0.steps()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_values_roundtrip() {
        assert_eq!(to_value(0, 0.25), 0.25);
        assert_eq!(to_value(4, 0.5), 2.0);
        assert_eq!(to_norm(4, 3.0), 0.75);
        assert_eq!(to_norm(0, 2.0), 1.0);
        assert_eq!(to_value(3, to_norm(3, 2.0)), 2.0);
    }

    #[test]
    fn frame_hands_out_the_run_loop() {
        let (shared, _tx) = Vst3Shared::new();
        let frame = lock(&shared.state).frame.as_ptr::<c_void>();
        let mut out = null_mut();
        // SAFETY: host objects of this test.
        unsafe {
            assert_eq!(frame_query(frame, &IPLUG_FRAME_IID, &mut out), K_RESULT_OK);
            assert_eq!(out, frame);
            release::<PlugFrame>(out);
            let r = frame_query(frame, &IRUN_LOOP_IID, &mut out);
            if cfg!(all(unix, not(target_os = "macos"))) {
                assert_eq!(r, K_RESULT_OK);
                assert!(!out.is_null() && out != frame);
                release::<RunLoopObj>(out);
            } else {
                assert_eq!(r, K_NO_INTERFACE);
            }
            assert_eq!(frame_query(frame, &IEVENT_LIST_IID, &mut out), K_NO_INTERFACE);
        }
        let mut rect = ViewRect::sized(300, 120);
        // SAFETY: frame of this test, rectangle local.
        assert_eq!(unsafe { frame_resize(frame, null_mut(), &mut rect) }, K_RESULT_TRUE);
        assert_eq!(*lock(&shared.events), [EditorEvent::Resize { width: 300, height: 120 }]);
    }
}
