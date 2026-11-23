//! The editor of an LV2 instance: its own interface embedded in a host
//! window (`ui:X11UI`, `ui:WindowsUI`, `ui:CocoaUI` through `ui:parent`,
//! with `ui:idleInterface`, `ui:resize`, `ui:touch` and `ui:portMap`), and
//! the main-thread handle on the plugin's state.
//!
//! The interface writes control values and atoms through queues to the
//! audio thread; it hears the control values the audio thread publishes and
//! the atoms the plugin writes on its output ports, every `idle`.

use std::ffi::{c_char, c_int, c_void};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ponzik_engine::dylib::DynLib;
use ponzik_engine::{EditorEvent, EditorSize, ParentWindow, PluginEditor, StateContext};

use crate::abi::*;
use crate::host::{Inner, LOG, Shared, lock, urids};
use crate::state::Blob;
use crate::world::PortKind;

/// Features given to the interface, and what they point to (boxed: stable).
struct UiFeatures {
    resize: LV2UI_Resize,
    touch: LV2UI_Touch,
    port_map: LV2UI_Port_Map,
    data_access: LV2_Extension_Data_Feature,
    rate: f32,
    scale: f32,
    sample_rate: f32,
    options: [LV2_Options_Option; 4],
    list: Vec<LV2_Feature>,
    ptrs: Vec<*const LV2_Feature>,
}

pub(crate) struct UiInstance {
    desc: *const LV2UI_Descriptor,
    handle: *mut c_void,
    idle: *const LV2UI_Idle_Interface,
    resize: *const LV2UI_Resize,
    fixed_size: bool,
    /// Last control value sent to the interface, per port.
    sent: Vec<Option<u32>>,
    /// Atoms read from the plugin, 8-byte aligned.
    scratch: Box<[u64]>,
    size: (u32, u32),
    _features: Box<UiFeatures>,
    _lib: Arc<DynLib>,
}

fn shared<'a>(h: *mut c_void) -> Option<&'a Shared> {
    // SAFETY: the handles given to the interface point to the instance's
    // `Shared`, which outlives the interface.
    unsafe { (h as *const Shared).as_ref() }
}

unsafe extern "C" fn write_port(controller: *mut c_void, port: u32, size: u32, protocol: u32, buffer: *const c_void) {
    let Some(s) = shared(controller) else { return };
    if buffer.is_null() || port as usize >= s.values.len() {
        return;
    }
    if protocol == 0 && size == 4 {
        // SAFETY: a float, as the protocol says.
        let v = unsafe { *buffer.cast::<f32>() };
        if !v.is_finite() {
            return;
        }
        let _ = lock(&s.to_audio).push((port, v));
        s.values[port as usize].store(v.to_bits(), Ordering::Relaxed);
        if let Some(k) = s.model.param(port) {
            s.push(EditorEvent::Param { id: port, value: s.model.convs[k].1.to_host(v, s.sample_rate()) });
        }
    } else if protocol == urids().event_transfer && size >= 8 {
        // SAFETY: an atom of `size` bytes.
        let atom = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
        let _ = s.atoms_in.push(&[&port.to_le_bytes(), &atom[4..8], &atom[8..]]);
    }
}

unsafe extern "C" fn host_resize(handle: *mut c_void, width: c_int, height: c_int) -> c_int {
    let Some(s) = shared(handle) else { return 1 };
    if width > 0 && height > 0 {
        s.push(EditorEvent::Resize { width: width as u32, height: height as u32 });
    }
    0
}

unsafe extern "C" fn touch(handle: *mut c_void, port: u32, grabbed: bool) {
    if let Some(s) = shared(handle) {
        s.push(if grabbed { EditorEvent::GestureBegin(port) } else { EditorEvent::GestureEnd(port) });
    }
}

unsafe extern "C" fn port_index(handle: *mut c_void, symbol: *const c_char) -> u32 {
    let Some(s) = shared(handle) else { return u32::MAX };
    if symbol.is_null() {
        return u32::MAX;
    }
    // SAFETY: NUL-terminated string from the interface.
    let sym = unsafe { std::ffi::CStr::from_ptr(symbol) }.to_string_lossy();
    s.model.desc.port(&sym).map_or(u32::MAX, |p| p.index)
}

/// Closes the interface (UI thread, `inner` locked).
pub(crate) fn close(s: &Shared, inner: &mut Inner) {
    let Some(ui) = inner.ui.take() else { return };
    s.ui_open.store(false, Ordering::Release);
    // SAFETY: interface created by `open`, never used after.
    unsafe { ((*ui.desc).cleanup)(ui.handle) };
    s.atoms_out.clear();
}

fn ui_descriptor(lib: &DynLib, uri: &str) -> Result<*const LV2UI_Descriptor, String> {
    let f = lib.symbol(c"lv2ui_descriptor");
    if f.is_null() {
        return Err("no lv2ui_descriptor in the interface's library".into());
    }
    // SAFETY: the symbol has the documented signature.
    let f: LV2UI_DescriptorFunction = unsafe { std::mem::transmute(f) };
    for i in 0..256 {
        // SAFETY: discovery call, null ends the list.
        let d = unsafe { f(i) };
        if d.is_null() {
            break;
        }
        let u = unsafe { (*d).uri };
        if !u.is_null() && unsafe { std::ffi::CStr::from_ptr(u) }.to_bytes() == uri.as_bytes() {
            return Ok(d);
        }
    }
    Err(format!("{uri} is not in its library"))
}

fn ui_extension<T>(desc: *const LV2UI_Descriptor, uri: &std::ffi::CStr) -> *const T {
    // SAFETY: descriptor of a loaded interface library.
    match unsafe { (*desc).extension_data } {
        Some(f) => unsafe { f(uri.as_ptr()) }.cast(),
        None => null(),
    }
}

/// Sends the control values that changed (all of them after `open`) and
/// the plugin's atoms to the interface.
fn send_updates(s: &Shared, ui: &mut UiInstance) {
    // SAFETY: live interface.
    let Some(port_event) = (unsafe { (*ui.desc).port_event }) else {
        s.atoms_out.clear();
        return;
    };
    for p in &s.model.desc.ports {
        if p.kind != PortKind::Control {
            continue;
        }
        let i = p.index as usize;
        let bits = s.values[i].load(Ordering::Relaxed);
        if ui.sent[i] != Some(bits) {
            ui.sent[i] = Some(bits);
            let v = f32::from_bits(bits);
            // SAFETY: a float for a control port.
            unsafe { port_event(ui.handle, p.index, 4, 0, (&v as *const f32).cast()) };
        }
    }
    let et = urids().event_transfer;
    let bytes = ui.scratch.len() * 8;
    loop {
        // SAFETY: the words are plain bytes.
        let buf = unsafe { std::slice::from_raw_parts_mut(ui.scratch.as_mut_ptr().cast::<u8>(), bytes) };
        let Some(n) = s.atoms_out.pop(buf) else { break };
        if n < 12 {
            continue;
        }
        let port = u32::from_le_bytes(buf[..4].try_into().unwrap_or_default());
        // The atom right after the port, realigned on 8 bytes.
        buf.copy_within(4..n, 8);
        // SAFETY: `n - 4` bytes of atom, aligned.
        unsafe { port_event(ui.handle, port, (n - 4) as u32, et, buf.as_ptr().add(8).cast()) };
    }
}

/// An LV2 instance's editor, as the app sees it.
pub(crate) struct Lv2Editor(pub(crate) Arc<Shared>);

impl Lv2Editor {
    fn open_ui(&self, inner: &mut Inner, parent: ParentWindow, scale: f64) -> Result<UiInstance, String> {
        let s = &*self.0;
        let desc = s.model.desc.native_ui().ok_or("this plugin has no interface for this platform")?.clone();
        let lib = crate::plugin::load_library(&desc.binary)?;
        let d = ui_descriptor(&lib, &desc.uri)?;
        let me = Arc::as_ptr(&self.0) as *mut c_void;
        let u = urids();
        let mut f = Box::new(UiFeatures {
            resize: LV2UI_Resize { handle: me, ui_resize: host_resize },
            touch: LV2UI_Touch { handle: me, touch },
            port_map: LV2UI_Port_Map { handle: me, port_index },
            // SAFETY: descriptor of the live instance.
            data_access: LV2_Extension_Data_Feature { data_access: unsafe { (*inner.descriptor).extension_data } },
            rate: 30.0,
            scale: scale as f32,
            sample_rate: s.sample_rate(),
            options: std::array::from_fn(|_| LV2_Options_Option { context: 0, subject: 0, key: 0, size: 0, type_: 0, value: null() }),
            list: Vec::new(),
            ptrs: Vec::new(),
        });
        let opt = |key, type_, value: *const f32| LV2_Options_Option { context: LV2_OPTIONS_INSTANCE, subject: 0, key, size: 4, type_, value: value.cast() };
        f.options[0] = opt(u.update_rate, u.float, &f.rate);
        f.options[1] = opt(u.scale_factor, u.float, &f.scale);
        f.options[2] = opt(u.sample_rate, u.float, &f.sample_rate);
        let feature = |uri: &'static std::ffi::CStr, data: *mut c_void| LV2_Feature { uri: uri.as_ptr(), data };
        f.list = vec![
            feature(c"http://lv2plug.in/ns/extensions/ui#parent", parent.raw() as *mut c_void),
            feature(c"http://lv2plug.in/ns/extensions/ui#resize", (&mut f.resize as *mut LV2UI_Resize).cast()),
            feature(c"http://lv2plug.in/ns/extensions/ui#idleInterface", null_mut()),
            feature(c"http://lv2plug.in/ns/extensions/ui#touch", (&mut f.touch as *mut LV2UI_Touch).cast()),
            feature(c"http://lv2plug.in/ns/extensions/ui#portMap", (&mut f.port_map as *mut LV2UI_Port_Map).cast()),
            feature(c"http://lv2plug.in/ns/ext/urid#map", (&crate::urid::FEATURES.map as *const LV2_URID_Map) as *mut c_void),
            feature(c"http://lv2plug.in/ns/ext/urid#unmap", (&crate::urid::FEATURES.unmap as *const LV2_URID_Unmap) as *mut c_void),
            feature(c"http://lv2plug.in/ns/ext/options#options", f.options.as_ptr() as *mut c_void),
            feature(c"http://lv2plug.in/ns/ext/log#log", (&LOG.0 as *const LV2_Log_Log) as *mut c_void),
            feature(c"http://lv2plug.in/ns/ext/instance-access", inner.instance),
            feature(c"http://lv2plug.in/ns/ext/data-access", (&mut f.data_access as *mut LV2_Extension_Data_Feature).cast()),
        ];
        f.ptrs = f.list.iter().map(|x| x as *const LV2_Feature).chain(std::iter::once(null())).collect();
        let plugin_uri = cstr(&s.model.desc.uri);
        let dir = desc.binary.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut bundle = dir.to_string_lossy().into_owned();
        if !bundle.ends_with(std::path::MAIN_SEPARATOR) {
            bundle.push(std::path::MAIN_SEPARATOR);
        }
        let bundle = cstr(&bundle);
        let mut widget: *mut c_void = null_mut();
        // Sizes asked while it is created are the first size.
        lock(&s.events).retain(|e| !matches!(e, EditorEvent::Resize { .. }));
        // SAFETY: documented call on the UI thread; features alive as long as the interface.
        let handle = unsafe { ((*d).instantiate)(d, plugin_uri.as_ptr(), bundle.as_ptr(), write_port, me, &mut widget, f.ptrs.as_ptr()) };
        if handle.is_null() {
            return Err("the plugin could not create its interface".into());
        }
        let mut size = (0, 0);
        lock(&s.events).retain(|e| match e {
            EditorEvent::Resize { width, height } => {
                size = (*width, *height);
                false
            }
            _ => true,
        });
        if size.0 == 0 {
            size = (640, 480);
        }
        Ok(UiInstance {
            desc: d,
            handle,
            idle: ui_extension(d, c"http://lv2plug.in/ns/extensions/ui#idleInterface"),
            resize: ui_extension(d, c"http://lv2plug.in/ns/extensions/ui#resize"),
            fixed_size: desc.fixed_size,
            sent: vec![None; s.values.len()],
            scratch: vec![0u64; crate::host::UI_ATOM_BYTES / 8 + 2].into_boxed_slice(),
            size,
            _features: f,
            _lib: lib,
        })
    }
}

impl PluginEditor for Lv2Editor {
    fn has_gui(&self) -> bool {
        self.0.model.desc.native_ui().is_some_and(|u| u.binary.is_file())
    }

    fn is_alive(&self) -> bool {
        !lock(&self.0.inner).instance.is_null()
    }

    fn is_open(&self) -> bool {
        lock(&self.0.inner).ui.is_some()
    }

    fn open(&self, parent: ParentWindow, scale: f64) -> Result<EditorSize, String> {
        let mut inner = lock(&self.0.inner);
        if inner.instance.is_null() {
            return Err("the plugin is no longer loaded".into());
        }
        if inner.ui.is_none() {
            let mut ui = self.open_ui(&mut inner, parent, scale)?;
            send_updates(&self.0, &mut ui);
            self.0.ui_open.store(true, Ordering::Release);
            inner.ui = Some(ui);
        }
        let ui = inner.ui.as_ref().ok_or("no interface")?;
        Ok(EditorSize { width: ui.size.0, height: ui.size.1, resizable: !ui.fixed_size && !ui.resize.is_null() })
    }

    fn close(&self) {
        let mut inner = lock(&self.0.inner);
        close(&self.0, &mut inner);
    }

    fn set_size(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let mut inner = lock(&self.0.inner);
        let ui = inner.ui.as_mut()?;
        // SAFETY: the interface's own resize extension.
        let r = unsafe { ui.resize.as_ref() }.filter(|_| !ui.fixed_size)?;
        let (w, h) = (width.max(1), height.max(1));
        // SAFETY: live interface.
        (unsafe { (r.ui_resize)(r.handle, w as c_int, h as c_int) } == 0).then(|| {
            ui.size = (w, h);
            (w, h)
        })
    }

    fn resizable(&self) -> bool {
        lock(&self.0.inner).ui.as_ref().is_some_and(|u| !u.fixed_size && !u.resize.is_null())
    }

    fn idle(&self, out: &mut Vec<EditorEvent>) {
        let s = &*self.0;
        let mut closed = false;
        {
            let mut inner = lock(&s.inner);
            if let Some(ui) = inner.ui.as_mut() {
                send_updates(s, ui);
                // SAFETY: the interface's idle extension, UI thread.
                if let Some(idle) = unsafe { ui.idle.as_ref() } {
                    closed = unsafe { (idle.idle)(ui.handle) } != 0;
                }
            }
            if closed {
                close(s, &mut inner);
            }
        }
        out.append(&mut lock(&s.events));
        if closed {
            out.push(EditorEvent::Closed);
        }
    }

    /// The audio thread publishes the new value: the interface hears it at
    /// the next `idle`.
    fn param_changed(&self, _: u32, _: f32) {}

    fn save_state(&self, _: StateContext) -> Option<Vec<u8>> {
        let s = &*self.0;
        let inner = lock(&s.inner);
        if inner.instance.is_null() {
            return None;
        }
        let ports = s
            .model
            .desc
            .ports
            .iter()
            .filter(|p| p.kind == PortKind::Control && p.input)
            .map(|p| (p.symbol.clone(), s.value(p.index)))
            .collect();
        // SAFETY: descriptor of the live instance (kept alive by the lock);
        // `save` may run beside `run`.
        let iface: *const LV2_State_Interface = match unsafe { (*inner.descriptor).extension_data } {
            Some(f) => unsafe { f(c"http://lv2plug.in/ns/ext/state#interface".as_ptr()) }.cast(),
            None => null(),
        };
        let props = match unsafe { iface.as_ref() } {
            Some(i) => crate::state::save_props(i, inner.instance).ok()?,
            None => Vec::new(),
        };
        Some(Blob { ports, props }.encode())
    }

    fn load_state(&self, data: &[u8], _: StateContext) -> Result<Vec<(u32, f32)>, String> {
        let s = &*self.0;
        let blob = Blob::decode(data)?;
        let inner = lock(&s.inner);
        if inner.instance.is_null() {
            return Err("the plugin is no longer loaded".into());
        }
        let mut values = Vec::new();
        {
            let mut tx = lock(&s.to_audio);
            for (port, conv) in &s.model.convs {
                let Some(sym) = s.model.desc.ports.get(*port as usize).map(|p| &p.symbol) else { continue };
                if let Some(v) = blob.port(sym) {
                    let _ = tx.push((*port, v));
                    values.push((*port, conv.to_host(v, s.sample_rate())));
                }
            }
        }
        // SAFETY: descriptor of the live instance.
        let iface: *const LV2_State_Interface = match unsafe { (*inner.descriptor).extension_data } {
            Some(f) => unsafe { f(c"http://lv2plug.in/ns/ext/state#interface".as_ptr()) }.cast(),
            None => null(),
        };
        if let Some(i) = unsafe { iface.as_ref() }
            && !blob.props.is_empty()
        {
            // Restore is an instantiation-class call: `run` waits outside the gate.
            s.enter_ui();
            let r = crate::state::restore_props(i, inner.instance, &blob.props);
            s.leave_ui();
            r?;
        }
        Ok(values)
    }
}
