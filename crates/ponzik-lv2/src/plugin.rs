//! An LV2 instance wrapped in the `Instrument`, `Effect` and `NoteFx` traits.
//!
//! Everything that allocates (port buffers, atom sequences, the worker and
//! its queues, parameter descriptions) is made when the instance is
//! created; `run`, `connect_port`, notes and parameter changes never
//! allocate. Control ports are `f32` values the plugin reads in place;
//! MIDI goes into a preallocated atom sequence.
//!
//! A plugin that needs a fixed block length runs on blocks of
//! [`FIXED_BLOCK`] samples through a FIFO, which delays it by as much.

use std::ffi::{CStr, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use ponzik_engine::dylib::DynLib;
use ponzik_engine::note_fx::NoteBuffer;
use ponzik_engine::ring::Consumer;
use ponzik_engine::{Controller, Editor, Effect, Instrument, NoteFx, ParamInfo, Params, Unit};

use crate::abi::*;
use crate::atom::AtomBuf;
use crate::host::{LOG, Options, Shared, Worker, lock, urids};
use crate::state::{Blob, property_of};
use crate::world::{PluginDesc, PortKind};

/// Longest block given to `run`; longer ones are cut.
pub const MAX_BLOCK: usize = 4096;
/// Block length of the plugins that need a fixed one (`buf-size:fixedBlockLength`).
pub const FIXED_BLOCK: usize = 256;
/// Smallest atom sequence buffer, in bytes.
const ATOM_BYTES: usize = 16 * 1024;
/// Sample rate the ranges of sample-rate ports are shown at.
const DISPLAY_RATE: f32 = 48_000.0;
/// Room for the sidechain signal of one block.
const SIDECHAIN: usize = 8192;

// --- Shared libraries ----------------------------------------------------------

static LIBS: Mutex<Vec<(PathBuf, Weak<DynLib>)>> = Mutex::new(Vec::new());

/// Loads a plugin or interface binary once per path while something uses it.
pub(crate) fn load_library(path: &Path) -> Result<Arc<DynLib>, String> {
    let key = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut libs = lock(&LIBS);
    libs.retain(|(_, w)| w.strong_count() > 0);
    if let Some(lib) = libs.iter().find(|(p, _)| *p == key).and_then(|(_, w)| w.upgrade()) {
        return Ok(lib);
    }
    let lib = Arc::new(DynLib::open(&key)?);
    libs.push((key, Arc::downgrade(&lib)));
    Ok(lib)
}

/// The descriptor of `uri` in a plugin binary.
pub(crate) fn find_descriptor(lib: &DynLib, uri: &str) -> Result<*const LV2_Descriptor, String> {
    let f = lib.symbol(c"lv2_descriptor");
    if f.is_null() {
        return Err("no lv2_descriptor in the plugin's library".into());
    }
    // SAFETY: the symbol has the documented signature.
    let f: LV2_Descriptor_Function = unsafe { std::mem::transmute(f) };
    for i in 0..4096 {
        // SAFETY: documented discovery call; null ends the list.
        let d = unsafe { f(i) };
        if d.is_null() {
            break;
        }
        // SAFETY: descriptor from the plugin, URI NUL-terminated.
        let u = unsafe { (*d).uri };
        if !u.is_null() && unsafe { CStr::from_ptr(u) }.to_bytes() == uri.as_bytes() {
            return Ok(d);
        }
    }
    Err(format!("{uri} is not in its library"))
}

/// URIs a plugin binary declares.
pub(crate) fn library_uris(path: &Path) -> Result<Vec<String>, String> {
    let lib = load_library(path)?;
    let f = lib.symbol(c"lv2_descriptor");
    if f.is_null() {
        return Err("no lv2_descriptor in the plugin's library".into());
    }
    // SAFETY: as in `find_descriptor`.
    let f: LV2_Descriptor_Function = unsafe { std::mem::transmute(f) };
    let mut out = Vec::new();
    for i in 0..4096 {
        // SAFETY: idem.
        let d = unsafe { f(i) };
        if d.is_null() {
            break;
        }
        let u = unsafe { (*d).uri };
        if !u.is_null() {
            out.push(unsafe { CStr::from_ptr(u) }.to_string_lossy().into_owned());
        }
    }
    Ok(out)
}

// --- Model (one per kind) ---------------------------------------------------

/// How a control value shows in Ponzik.
#[derive(Clone, Debug)]
pub(crate) enum Conv {
    /// Ponzik value = port value × `scale` (× the sample rate for a sample-rate port).
    Linear { scale: f32, integer: bool, sample_rate: bool, min: f32, max: f32 },
    /// Ponzik value = index in the list of port values.
    Choice(Vec<f32>),
}

impl Conv {
    pub(crate) fn to_port(&self, v: f32, sr: f32) -> f32 {
        match self {
            Conv::Linear { scale, integer, sample_rate, min, max } => {
                let p = if *sample_rate { v / sr.max(1.0) } else { v / scale };
                let p = if p.is_finite() { p.clamp(*min, *max) } else { *min };
                if *integer { p.round() } else { p }
            }
            Conv::Choice(values) => values.get((v.max(0.0).round() as usize).min(values.len().saturating_sub(1))).copied().unwrap_or(0.0),
        }
    }

    pub(crate) fn to_host(&self, p: f32, sr: f32) -> f32 {
        match self {
            Conv::Linear { scale, sample_rate, .. } => {
                if *sample_rate {
                    p * sr
                } else {
                    p * scale
                }
            }
            Conv::Choice(values) => {
                // The nearest value (a toggle is on for anything not zero).
                let mut best = 0;
                for (i, v) in values.iter().enumerate() {
                    if (p - v).abs() < (p - values[best]).abs() {
                        best = i;
                    }
                }
                if values.len() == 2 && values[0] == 0.0 && p != 0.0 { 1.0 } else { best as f32 }
            }
        }
    }
}

/// What every instance of a plugin shares: its description and parameters.
pub(crate) struct Model {
    pub desc: PluginDesc,
    pub kind: &'static str,
    pub infos: &'static [ParamInfo],
    /// Port index (the parameter id) and conversion of each entry of `infos`.
    pub convs: Vec<(u32, Conv)>,
}

impl Model {
    pub(crate) fn param(&self, id: u32) -> Option<usize> {
        self.convs.binary_search_by_key(&id, |c| c.0).ok()
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn unit_of(uri: &str) -> (Unit, f32) {
    match uri {
        UNITS_DB => (Unit::Db, 1.0),
        UNITS_HZ => (Unit::Hz, 1.0),
        UNITS_KHZ => (Unit::Hz, 1000.0),
        UNITS_S => (Unit::Seconds, 1.0),
        UNITS_MS => (Unit::Seconds, 0.001),
        UNITS_SEMITONE => (Unit::Semitones, 1.0),
        UNITS_CENT => (Unit::Cents, 1.0),
        _ => (Unit::None, 1.0),
    }
}

/// Ports the host drives itself instead of showing them.
fn host_driven(designation: &str) -> bool {
    matches!(designation, LV2_LATENCY | LV2_FREE_WHEELING | TIME_BPM | PARAM_SAMPLE_RATE)
}

fn build_model(kind: String, desc: PluginDesc) -> Model {
    let mut infos = Vec::new();
    let mut convs = Vec::new();
    for p in &desc.ports {
        if p.kind != PortKind::Control || !p.input || p.not_on_gui || host_driven(&p.designation) {
            continue;
        }
        let name = leak(p.name.clone());
        let (mut lo, mut hi) = (p.min.unwrap_or(0.0), p.max.unwrap_or(1.0));
        if hi < lo {
            std::mem::swap(&mut lo, &mut hi);
        }
        if hi == lo {
            hi = lo + 1.0;
        }
        let def = p.default.unwrap_or(lo).clamp(lo, hi);
        if p.toggled {
            static OFF_ON: [&str; 2] = ["Off", "On"];
            infos.push(ParamInfo::choice(p.index, name, &OFF_ON, usize::from(def != 0.0)));
            convs.push((p.index, Conv::Choice(vec![0.0, if hi > 0.0 { hi } else { 1.0 }])));
            continue;
        }
        if p.enumeration && p.scale_points.len() >= 2 {
            let labels: Vec<&'static str> = p.scale_points.iter().map(|s| leak(s.1.clone())).collect();
            let values: Vec<f32> = p.scale_points.iter().map(|s| s.0).collect();
            let conv = Conv::Choice(values);
            let d = conv.to_host(def, DISPLAY_RATE) as usize;
            infos.push(ParamInfo::choice(p.index, name, Box::leak(labels.into_boxed_slice()), d));
            convs.push((p.index, conv));
            continue;
        }
        let (unit, scale) = if p.sample_rate { (Unit::Hz, DISPLAY_RATE) } else { unit_of(&p.unit) };
        let conv = Conv::Linear { scale, integer: p.integer, sample_rate: p.sample_rate, min: lo, max: hi };
        let mut info = ParamInfo::new(p.index, name, lo * scale, hi * scale, def * scale, unit);
        if p.logarithmic && lo > 0.0 {
            info = info.log();
        }
        infos.push(info);
        convs.push((p.index, conv));
    }
    Model { kind: leak(kind), infos: Box::leak(infos.into_boxed_slice()), convs, desc }
}

static MODELS: Mutex<Vec<Arc<Model>>> = Mutex::new(Vec::new());

/// The model of `uri` in `bundle`, read once per process.
pub(crate) fn model(bundle: &Path, uri: &str) -> Result<Arc<Model>, String> {
    let kind = crate::kind_for(uri);
    if let Some(m) = lock(&MODELS).iter().find(|m| m.kind == kind && m.desc.bundle == bundle) {
        return Ok(m.clone());
    }
    let mut g = crate::ttl::Graph::new();
    crate::world::load_bundle(&mut g, bundle)?;
    let desc = crate::world::describe(&g, uri, bundle)?;
    if let Some(why) = desc.unsupported() {
        return Err(format!("{}: {why}", desc.name));
    }
    let mut models = lock(&MODELS);
    // Another thread may have been quicker: one leak per kind and bundle.
    if let Some(m) = models.iter().find(|m| m.kind == kind && m.desc.bundle == bundle) {
        return Ok(m.clone());
    }
    let m = Arc::new(build_model(kind, desc));
    models.push(m.clone());
    Ok(m)
}

// --- Instance ------------------------------------------------------------------

/// Buffers of one port.
enum PortBuf {
    Audio(Box<[f32]>),
    Atom(AtomBuf),
    Control,
    None,
}

/// The FIFO of a plugin that runs on fixed blocks.
struct Fixed {
    pos: usize,
    input: [Box<[f32]>; 2],
    output: [Box<[f32]>; 2],
}

/// Features handed to `instantiate`, and what they point to.
struct Features {
    options: Box<Options>,
    worker: Option<Worker>,
    list: Vec<LV2_Feature>,
    ptrs: Vec<*const LV2_Feature>,
}

impl Features {
    fn new(model: &Model, sample_rate: f32, block: (usize, usize), seq: usize) -> Result<Box<Features>, String> {
        let wants = |f: &str| model.desc.required.iter().chain(&model.desc.optional).any(|r| r == f);
        let worker = if wants(WORKER_SCHEDULE) && model.desc.has_extension(WORKER_INTERFACE) { Some(Worker::new()?) } else { None };
        let mut f = Box::new(Features {
            options: Options::new(sample_rate, block.0, block.1, block.1.min(1024), seq),
            worker,
            list: Vec::new(),
            ptrs: Vec::new(),
        });
        // Static feature names, so the pointers stay valid.
        let mut list = vec![
            LV2_Feature { uri: c"http://lv2plug.in/ns/ext/urid#map".as_ptr(), data: (&crate::urid::FEATURES.map as *const LV2_URID_Map) as *mut c_void },
            LV2_Feature { uri: c"http://lv2plug.in/ns/ext/urid#unmap".as_ptr(), data: (&crate::urid::FEATURES.unmap as *const LV2_URID_Unmap) as *mut c_void },
            LV2_Feature { uri: c"http://lv2plug.in/ns/ext/options#options".as_ptr(), data: f.options.list.as_ptr() as *mut c_void },
            LV2_Feature { uri: c"http://lv2plug.in/ns/ext/log#log".as_ptr(), data: (&LOG.0 as *const LV2_Log_Log) as *mut c_void },
            LV2_Feature { uri: c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength".as_ptr(), data: null_mut() },
            LV2_Feature { uri: c"http://lv2plug.in/ns/lv2core#isLive".as_ptr(), data: null_mut() },
        ];
        if block.0 == block.1 {
            list.push(LV2_Feature { uri: c"http://lv2plug.in/ns/ext/buf-size#fixedBlockLength".as_ptr(), data: null_mut() });
            list.push(LV2_Feature { uri: c"http://lv2plug.in/ns/ext/buf-size#powerOf2BlockLength".as_ptr(), data: null_mut() });
        }
        if let Some(w) = &mut f.worker {
            list.push(LV2_Feature { uri: c"http://lv2plug.in/ns/ext/worker#schedule".as_ptr(), data: (&mut *w.schedule as *mut LV2_Worker_Schedule).cast() });
        }
        f.list = list;
        f.ptrs = f.list.iter().map(|x| x as *const LV2_Feature).chain(std::iter::once(null())).collect();
        Ok(f)
    }
}

/// An LV2 instance; common to the instrument, effect and note effect wrappers.
pub(crate) struct Core {
    model: Arc<Model>,
    descriptor: *const LV2_Descriptor,
    handle: LV2_Handle,
    features: Box<Features>,
    sample_rate: f32,
    active: bool,
    /// Value of every port (only control ports use theirs).
    controls: Box<[f32]>,
    /// Ponzik value of each parameter (same order as `model.infos`).
    host_values: Box<[f32]>,
    bufs: Vec<PortBuf>,
    /// Main audio inputs and outputs (port indices), at most two each.
    ins: Vec<usize>,
    outs: Vec<usize>,
    side: Vec<usize>,
    sidechain: [Box<[f32]>; 2],
    has_sidechain: bool,
    midi_in: Option<usize>,
    midi_out: Option<usize>,
    atom_ins: Vec<usize>,
    atom_outs: Vec<usize>,
    latency_port: Option<usize>,
    bpm_port: Option<usize>,
    state_iface: *const LV2_State_Interface,
    worker_iface: *const LV2_Worker_Interface,
    fixed: Option<Fixed>,
    from_ui: Consumer<(u32, f32)>,
    /// Bytes of an interface atom, read on the audio thread.
    scratch: Box<[u8]>,
    held: u128,
    latency: usize,
    shared: Arc<Shared>,
    _lib: Arc<DynLib>,
}

// SAFETY: the instance is used by one thread at a time (UI, then the engine);
// what the editor touches goes through `Shared`.
unsafe impl Send for Core {}

fn bundle_dir(bundle: &Path) -> std::ffi::CString {
    let mut s = bundle.to_string_lossy().into_owned();
    if !s.ends_with(std::path::MAIN_SEPARATOR) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    cstr(&s)
}

impl Core {
    pub(crate) fn new(model: Arc<Model>) -> Result<Core, String> {
        let lib = load_library(&model.desc.binary)?;
        let descriptor = find_descriptor(&lib, &model.desc.uri)?;
        let fixed = model.desc.required.iter().any(|f| f == BUF_FIXED || f == BUF_POWER_OF_2);
        let block = if fixed { (FIXED_BLOCK, FIXED_BLOCK) } else { (1, MAX_BLOCK) };
        let atom_bytes = model.desc.ports.iter().map(|p| p.min_size).max().unwrap_or(0).max(ATOM_BYTES);
        let features = Features::new(&model, 48_000.0, block, atom_bytes)?;
        let n = model.desc.ports.len();
        let frames = if fixed { FIXED_BLOCK } else { MAX_BLOCK };
        let mut controls = vec![0f32; n.max(1)].into_boxed_slice();
        let mut bufs = Vec::with_capacity(n);
        let (mut ins, mut outs, mut side, mut atom_ins, mut atom_outs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut midi_in, mut midi_out, mut latency_port, mut bpm_port) = (None, None, None, None);
        for p in &model.desc.ports {
            let i = p.index as usize;
            bufs.push(match p.kind {
                PortKind::Audio | PortKind::Cv => {
                    if p.kind == PortKind::Audio {
                        if p.input && p.side_chain {
                            side.push(i);
                        } else if p.input {
                            ins.push(i);
                        } else {
                            outs.push(i);
                        }
                    }
                    PortBuf::Audio(vec![0f32; frames].into_boxed_slice())
                }
                PortKind::Control => {
                    let lo = p.min.unwrap_or(0.0);
                    let hi = p.max.unwrap_or(lo.max(1.0));
                    controls[i] = p.default.unwrap_or(lo).clamp(lo.min(hi), hi.max(lo));
                    if p.designation == LV2_FREE_WHEELING {
                        controls[i] = 0.0;
                    }
                    if !p.input && (p.reports_latency || p.designation == LV2_LATENCY) {
                        latency_port = Some(i);
                    }
                    if p.input && p.designation == TIME_BPM {
                        bpm_port = Some(i);
                        controls[i] = 120.0;
                    }
                    PortBuf::Control
                }
                PortKind::Atom => {
                    let mut b = AtomBuf::new(atom_bytes.max(p.min_size));
                    if p.input {
                        atom_ins.push(i);
                        if p.midi && midi_in.is_none() {
                            midi_in = Some(i);
                        }
                        b.clear_input(urids().sequence);
                    } else {
                        atom_outs.push(i);
                        if p.midi && midi_out.is_none() {
                            midi_out = Some(i);
                        }
                    }
                    PortBuf::Atom(b)
                }
                PortKind::Event | PortKind::Unknown => PortBuf::None,
            });
        }
        ins.truncate(2);
        outs.truncate(2);
        side.truncate(2);
        let host_values = model.infos.iter().map(|i| i.default).collect();
        let (shared, from_ui) = Shared::new(model.clone(), n);
        let fixed = fixed.then(|| Fixed {
            pos: 0,
            input: [vec![0f32; FIXED_BLOCK].into(), vec![0f32; FIXED_BLOCK].into()],
            output: [vec![0f32; FIXED_BLOCK].into(), vec![0f32; FIXED_BLOCK].into()],
        });
        let mut core = Core {
            model,
            descriptor,
            handle: null_mut(),
            features,
            sample_rate: 48_000.0,
            active: false,
            controls,
            host_values,
            bufs,
            ins,
            outs,
            side,
            sidechain: [vec![0f32; SIDECHAIN].into(), vec![0f32; SIDECHAIN].into()],
            has_sidechain: false,
            midi_in,
            midi_out,
            atom_ins,
            atom_outs,
            latency_port,
            bpm_port,
            state_iface: null(),
            worker_iface: null(),
            fixed,
            from_ui,
            scratch: vec![0u8; crate::host::UI_ATOM_BYTES].into_boxed_slice(),
            held: 0,
            latency: 0,
            shared,
            _lib: lib,
        };
        core.instantiate(48_000.0)?;
        // The plugin's default state (`state:state` in its description).
        if !core.model.desc.default_state.is_empty() && !core.state_iface.is_null() {
            let props: Vec<_> = core.model.desc.default_state.iter().filter_map(|(k, v)| property_of(k, v)).collect();
            // SAFETY: interface of the live instance; nothing runs yet.
            if let Err(e) = crate::state::restore_props(unsafe { &*core.state_iface }, core.handle, &props) {
                eprintln!("LV2 {}: default state: {e}", core.model.desc.name);
            }
        }
        core.publish_all();
        Ok(core)
    }

    pub(crate) fn kind(&self) -> &'static str {
        self.model.kind
    }

    pub(crate) fn infos(&self) -> &'static [ParamInfo] {
        self.model.infos
    }

    fn extension<T>(&self, uri: &CStr) -> *const T {
        // SAFETY: descriptor from the library, kept loaded.
        match unsafe { (*self.descriptor).extension_data } {
            Some(f) => unsafe { f(uri.as_ptr()) }.cast(),
            None => null(),
        }
    }

    /// Creates the instance at `sample_rate` and connects every port.
    fn instantiate(&mut self, sample_rate: f32) -> Result<(), String> {
        self.features.options.sample_rate = sample_rate;
        let bundle = bundle_dir(&self.model.desc.bundle);
        // SAFETY: descriptor from the library; features NUL-terminated and alive
        // as long as the instance (owned by `self`).
        let handle = unsafe { ((*self.descriptor).instantiate)(self.descriptor, sample_rate as f64, bundle.as_ptr(), self.features.ptrs.as_ptr()) };
        if handle.is_null() {
            return Err(format!("{} could not be instantiated", self.model.desc.name));
        }
        self.handle = handle;
        self.sample_rate = sample_rate;
        self.shared.sample_rate.store(sample_rate.to_bits(), Ordering::Relaxed);
        self.state_iface = self.extension(c"http://lv2plug.in/ns/ext/state#interface");
        self.worker_iface = self.extension(c"http://lv2plug.in/ns/ext/worker#interface");
        let ctl = self.controls.as_mut_ptr();
        for (i, b) in self.bufs.iter_mut().enumerate() {
            let ptr: *mut c_void = match b {
                PortBuf::Audio(a) => a.as_mut_ptr().cast(),
                // SAFETY: `i` < number of ports = length of `controls`.
                PortBuf::Control => unsafe { ctl.add(i) }.cast(),
                PortBuf::Atom(a) => a.as_ptr(),
                PortBuf::None => null_mut(),
            };
            // SAFETY: buffers owned by `self`, sized for the longest block.
            unsafe { ((*self.descriptor).connect_port)(handle, i as u32, ptr) };
        }
        if let (Some(w), Some(iface)) = (&self.features.worker, unsafe { self.worker_iface.as_ref() }) {
            w.attach(handle, iface);
        }
        let mut inner = lock(&self.shared.inner);
        inner.instance = handle;
        inner.descriptor = self.descriptor;
        Ok(())
    }

    fn activate(&mut self) {
        if !self.active && !self.handle.is_null() {
            // SAFETY: instance connected.
            if let Some(f) = unsafe { (*self.descriptor).activate } {
                unsafe { f(self.handle) };
            }
            self.active = true;
        }
    }

    fn deactivate(&mut self) {
        if self.active {
            // SAFETY: activated instance.
            if let Some(f) = unsafe { (*self.descriptor).deactivate } {
                unsafe { f(self.handle) };
            }
            self.active = false;
        }
    }

    /// Destroys the instance (its interface first).
    fn destroy(&mut self) {
        if self.handle.is_null() {
            return;
        }
        {
            let mut inner = lock(&self.shared.inner);
            crate::ui::close(&self.shared, &mut inner);
            inner.instance = null_mut();
        }
        self.deactivate();
        if let Some(w) = &self.features.worker {
            w.detach();
        }
        // SAFETY: no more calls on this instance.
        unsafe { ((*self.descriptor).cleanup)(self.handle) };
        self.handle = null_mut();
        self.state_iface = null();
        self.worker_iface = null();
    }

    /// The control values by symbol and the plugin's properties.
    pub(crate) fn blob(&self) -> Blob {
        let ports = self
            .model
            .desc
            .ports
            .iter()
            .filter(|p| p.kind == PortKind::Control && p.input)
            .map(|p| (p.symbol.clone(), self.controls[p.index as usize]))
            .collect();
        // SAFETY: state interface of the live instance (save may run beside `run`).
        let props = unsafe { self.state_iface.as_ref() }
            .and_then(|i| crate::state::save_props(i, self.handle).map_err(|e| eprintln!("LV2 {}: {e}", self.model.desc.name)).ok())
            .unwrap_or_default();
        Blob { ports, props }
    }

    /// Applies a state on the instance this thread owns (not running).
    pub(crate) fn apply_blob(&mut self, blob: &Blob) -> Result<(), String> {
        for p in &self.model.desc.ports {
            if p.kind == PortKind::Control
                && p.input
                && !host_driven(&p.designation)
                && let Some(v) = blob.port(&p.symbol)
            {
                self.controls[p.index as usize] = v;
            }
        }
        let mut result = Ok(());
        if !blob.props.is_empty()
            && let Some(i) = unsafe { self.state_iface.as_ref() }
        {
            result = crate::state::restore_props(i, self.handle, &blob.props);
        }
        self.publish_all();
        result
    }

    /// Ponzik values from the control values, and the values the editor sees.
    fn publish_all(&mut self) {
        let sr = self.sample_rate;
        for (k, (port, conv)) in self.model.convs.iter().enumerate() {
            self.host_values[k] = conv.to_host(self.controls[*port as usize], sr);
        }
        for (i, v) in self.controls.iter().enumerate() {
            if let Some(a) = self.shared.values.get(i) {
                a.store(v.to_bits(), Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate && sample_rate > 0.0 {
            // A new instance at the new rate, with the state of the old one.
            let blob = self.blob();
            self.destroy();
            if let Err(e) = self.instantiate(sample_rate) {
                eprintln!("LV2: {e}");
                return;
            }
            let before = self.host_values.clone();
            if let Err(e) = self.apply_blob(&blob) {
                eprintln!("LV2 {}: {e}", self.model.desc.name);
            }
            // A frequency stays a frequency: sample-rate ports follow the new rate.
            for (k, (port, conv)) in self.model.convs.iter().enumerate() {
                if let Conv::Linear { sample_rate: true, .. } = conv {
                    self.controls[*port as usize] = conv.to_port(before[k], sample_rate);
                    self.host_values[k] = before[k];
                }
            }
            self.publish_all();
        }
        if let Some(f) = &mut self.fixed {
            f.pos = 0;
            f.input.iter_mut().chain(f.output.iter_mut()).for_each(|b| b.fill(0.0));
        }
        self.activate();
    }

    /// Current value of the control port `symbol` (inputs and outputs).
    pub(crate) fn port_value(&self, symbol: &str) -> Option<f32> {
        let p = self.model.desc.port(symbol).filter(|p| p.kind == PortKind::Control)?;
        Some(self.controls[p.index as usize])
    }

    pub(crate) fn get_param(&self, id: u32) -> f32 {
        self.model.param(id).map_or(0.0, |k| self.host_values[k])
    }

    pub(crate) fn set_param(&mut self, id: u32, value: f32) {
        let Some(k) = self.model.param(id) else { return };
        let port = self.model.convs[k].1.to_port(value, self.sample_rate);
        self.controls[id as usize] = port;
        self.host_values[k] = value;
    }

    /// Time of the next event in the current block.
    fn event_time(&self) -> i64 {
        self.fixed.as_ref().map_or(0, |f| f.pos as i64)
    }

    pub(crate) fn midi(&mut self, data: [u8; 3], len: usize) {
        let Some(port) = self.midi_in else { return };
        let t = self.event_time();
        let midi = urids().midi;
        if let PortBuf::Atom(b) = &mut self.bufs[port] {
            // Full: the event is lost.
            let _ = b.push(t, midi, &data[..len.min(3)]);
        }
    }

    pub(crate) fn note_on(&mut self, key: u8, velocity: f32) {
        let key = key.min(127);
        let vel = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
        self.midi([0x90, key, vel], 3);
        self.held |= 1 << key;
    }

    pub(crate) fn note_off(&mut self, key: u8) {
        let key = key.min(127);
        self.midi([0x80, key, 0], 3);
        self.held &= !(1 << key);
    }

    pub(crate) fn controller(&mut self, ctl: Controller) {
        match ctl {
            Controller::PitchBend(b) => {
                let v = ((b.clamp(-1.0, 1.0) * 8192.0).round() as i32 + 8192).clamp(0, 16_383) as u16;
                self.midi([0xE0, (v & 0x7F) as u8, (v >> 7) as u8], 3);
            }
            Controller::Cc(cc, v) => self.midi([0xB0, cc.min(127), (v.clamp(0.0, 1.0) * 127.0).round() as u8], 3),
        }
    }

    pub(crate) fn all_notes_off(&mut self) {
        for key in 0..128u8 {
            if self.held & (1 << key) != 0 {
                self.note_off(key);
            }
        }
        self.midi([0xB0, 123, 0], 3);
    }

    pub(crate) fn set_tempo(&mut self, bpm: f32) {
        if let Some(p) = self.bpm_port {
            self.controls[p] = bpm;
        }
    }

    pub(crate) fn latency(&self) -> usize {
        self.latency + if self.fixed.is_some() { FIXED_BLOCK } else { 0 }
    }

    fn audio(&mut self, port: usize) -> &mut [f32] {
        match &mut self.bufs[port] {
            PortBuf::Audio(a) => a,
            _ => &mut [],
        }
    }

    /// One `run` of `n` samples, the input buffers filled. False when the
    /// instance cannot run now (inactive, or the UI thread restoring a state).
    fn run(&mut self, n: usize) -> bool {
        if !self.active || self.handle.is_null() || !self.shared.try_enter_audio() {
            return false;
        }
        let u = urids();
        let sr = self.sample_rate;
        // Values written by the plugin's interface.
        while let Some((port, v)) = self.from_ui.pop() {
            if let Some(c) = self.controls.get_mut(port as usize) {
                *c = v;
            }
            if let Some(k) = self.model.param(port) {
                self.host_values[k] = self.model.convs[k].1.to_host(v, sr);
            }
        }
        // Atoms written by the interface: port, type, body.
        while let Some(len) = self.shared.atoms_in.pop(&mut self.scratch) {
            if len < 8 {
                continue;
            }
            let port = u32::from_le_bytes(self.scratch[..4].try_into().unwrap_or_default()) as usize;
            let ty = u32::from_le_bytes(self.scratch[4..8].try_into().unwrap_or_default());
            let t = self.fixed.as_ref().map_or(0, |f| f.pos as i64);
            if let Some(PortBuf::Atom(b)) = self.bufs.get_mut(port) {
                let _ = b.push(t, ty, &self.scratch[8..len]);
            }
        }
        for &i in &self.atom_outs {
            if let PortBuf::Atom(b) = &mut self.bufs[i] {
                b.prepare_output(u.chunk);
            }
        }
        if self.has_sidechain {
            for (k, &port) in self.side.iter().enumerate() {
                let src = &self.sidechain[k.min(1)];
                if let PortBuf::Audio(a) = &mut self.bufs[port] {
                    let m = n.min(a.len()).min(src.len());
                    a[..m].copy_from_slice(&src[..m]);
                }
            }
        }
        // SAFETY: active instance, every port connected to a buffer of at least `n`.
        unsafe { ((*self.descriptor).run)(self.handle, n as u32) };
        for &i in &self.atom_ins {
            if let PortBuf::Atom(b) = &mut self.bufs[i] {
                b.clear_input(u.sequence);
            }
        }
        // What the interface sees.
        for (i, v) in self.controls.iter().enumerate() {
            self.shared.values[i].store(v.to_bits(), Ordering::Relaxed);
        }
        if self.shared.ui_open.load(Ordering::Relaxed) {
            for &i in &self.atom_outs {
                if let PortBuf::Atom(b) = &self.bufs[i] {
                    let port = (i as u32).to_le_bytes();
                    b.for_each_atom(u.sequence, |atom| {
                        let _ = self.shared.atoms_out.push(&[&port, atom]);
                    });
                }
            }
        }
        if let Some(p) = self.latency_port {
            self.latency = self.controls[p].clamp(0.0, 1_000_000.0) as usize;
        }
        if let (Some(w), Some(iface)) = (&mut self.features.worker, unsafe { self.worker_iface.as_ref() }) {
            w.deliver(self.handle, iface);
        }
        self.shared.leave_audio();
        true
    }

    /// Main inputs from `left` and `right` (`n` samples from `at`).
    fn fill_inputs(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len();
        match self.ins.len() {
            0 => {}
            1 => {
                let p = self.ins[0];
                self.audio(p)[..n].iter_mut().zip(left.iter().zip(right)).for_each(|(m, (a, b))| *m = 0.5 * (a + b));
            }
            _ => {
                let (a, b) = (self.ins[0], self.ins[1]);
                self.audio(a)[..n].copy_from_slice(left);
                self.audio(b)[..n].copy_from_slice(right);
            }
        }
    }

    /// Main outputs into `left` and `right`, added or replacing.
    fn read_outputs(&mut self, left: &mut [f32], right: &mut [f32], add: bool) {
        let n = left.len();
        let (a, b) = match self.outs.len() {
            0 => return,
            1 => (self.outs[0], self.outs[0]),
            _ => (self.outs[0], self.outs[1]),
        };
        let (bufs, (l, r)) = (&self.bufs, (left, right));
        let get = |p: usize| match &bufs[p] {
            PortBuf::Audio(x) => &x[..n],
            _ => &[][..],
        };
        let (sa, sb) = (get(a), get(b));
        if sa.len() < n || sb.len() < n {
            return;
        }
        if add {
            l.iter_mut().zip(sa).for_each(|(o, v)| *o += v);
            r.iter_mut().zip(sb).for_each(|(o, v)| *o += v);
        } else {
            l.copy_from_slice(sa);
            r.copy_from_slice(sb);
        }
    }

    /// Runs the plugin over a stereo block: `add` for an instrument (output
    /// added), otherwise in place (an effect; dry when it cannot run).
    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32], add: bool) {
        if self.fixed.is_none() {
            for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
                if !add {
                    self.fill_inputs(l, r);
                }
                if self.run(l.len()) {
                    self.read_outputs(l, r, add);
                }
            }
            return;
        }
        // Fixed blocks: in through the FIFO, out one block later.
        let n = left.len();
        let mut i = 0;
        while i < n {
            let Some(f) = self.fixed.as_mut() else { return };
            let pos = f.pos;
            let take = (FIXED_BLOCK - pos).min(n - i);
            f.input[0][pos..pos + take].copy_from_slice(&left[i..i + take]);
            f.input[1][pos..pos + take].copy_from_slice(&right[i..i + take]);
            if add {
                left[i..i + take].iter_mut().zip(&f.output[0][pos..pos + take]).for_each(|(o, v)| *o += v);
                right[i..i + take].iter_mut().zip(&f.output[1][pos..pos + take]).for_each(|(o, v)| *o += v);
            } else {
                left[i..i + take].copy_from_slice(&f.output[0][pos..pos + take]);
                right[i..i + take].copy_from_slice(&f.output[1][pos..pos + take]);
            }
            f.pos += take;
            i += take;
            if f.pos == FIXED_BLOCK {
                let Some(mut f) = self.fixed.take() else { return };
                if !add {
                    self.fill_inputs(&f.input[0], &f.input[1]);
                }
                f.pos = 0;
                let [ol, or] = &mut f.output;
                if add {
                    ol.fill(0.0);
                    or.fill(0.0);
                } else {
                    // Dry if the plugin cannot run.
                    ol.copy_from_slice(&f.input[0]);
                    or.copy_from_slice(&f.input[1]);
                }
                if self.run(FIXED_BLOCK) {
                    self.read_outputs(ol, or, add);
                }
                self.fixed = Some(f);
            }
        }
    }

    pub(crate) fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
        if self.side.is_empty() {
            return;
        }
        let n = left.len().min(SIDECHAIN);
        self.sidechain[0][..n].copy_from_slice(&left[..n]);
        self.sidechain[1][..n].copy_from_slice(&right[..n]);
        self.has_sidechain = true;
    }

    /// MIDI the plugin wrote on its MIDI output, after the last `run`.
    pub(crate) fn midi_out(&self, mut f: impl FnMut(i64, &[u8])) {
        let Some(port) = self.midi_out else { return };
        let u = urids();
        if let PortBuf::Atom(b) = &self.bufs[port] {
            b.for_each(u.sequence, |t, ty, body| {
                if ty == u.midi {
                    f(t, body);
                }
            });
        }
    }

    pub(crate) fn editor(&self) -> Editor {
        Arc::new(crate::ui::Lv2Editor(self.shared.clone()))
    }

    pub(crate) fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let blob = Blob::decode(data)?;
        self.apply_blob(&blob)
    }

    /// The plugin's presets (from every bundle found), with their values in
    /// Ponzik's units.
    pub(crate) fn presets(&self) -> Vec<(String, Vec<(u32, f32)>)> {
        let presets = crate::world::presets(&self.model.desc.uri, &crate::bundles_with(&self.model.desc.bundle));
        presets
            .into_iter()
            .map(|p| {
                let values = p
                    .values
                    .iter()
                    .filter_map(|(sym, v)| {
                        let port = self.model.desc.port(sym)?.index;
                        let k = self.model.param(port)?;
                        Some((port, self.model.convs[k].1.to_host(*v, self.sample_rate)))
                    })
                    .collect();
                (p.name, values)
            })
            .collect()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.destroy();
    }
}

// --- Wrappers ------------------------------------------------------------------

/// An LV2 instrument: MIDI in, stereo out added to the render.
pub struct Lv2Instrument(pub(crate) Core);

/// An LV2 effect: stereo in place.
pub struct Lv2Effect(pub(crate) Core);

/// An LV2 MIDI effect (MIDI in and out, no audio), as a note effect.
pub struct Lv2NoteFx {
    core: Core,
    /// Samples since the last `run`.
    elapsed: u32,
    silence: [Box<[f32]>; 2],
}

macro_rules! impl_params {
    ($t:ty, $($f:tt)+) => {
        impl Params for $t {
            fn params(&self) -> &'static [ParamInfo] {
                self.$($f)+.infos()
            }

            fn get_param(&self, id: u32) -> f32 {
                self.$($f)+.get_param(id)
            }

            fn set_param(&mut self, id: u32, value: f32) {
                self.$($f)+.set_param(id, value);
            }
        }
    };
}

impl_params!(Lv2Instrument, 0);
impl_params!(Lv2Effect, 0);
impl_params!(Lv2NoteFx, core);

impl Lv2Instrument {
    pub(crate) fn new(model: Arc<Model>) -> Result<Self, String> {
        Core::new(model).map(Lv2Instrument)
    }

    /// Presets of the plugin: name and values.
    pub fn presets(&self) -> Vec<(String, Vec<(u32, f32)>)> {
        self.0.presets()
    }

    /// Value of a control port, outputs included (meters, latency).
    pub fn port_value(&self, symbol: &str) -> Option<f32> {
        self.0.port_value(symbol)
    }
}

impl Lv2Effect {
    pub(crate) fn new(model: Arc<Model>) -> Result<Self, String> {
        Core::new(model).map(Lv2Effect)
    }

    pub fn presets(&self) -> Vec<(String, Vec<(u32, f32)>)> {
        self.0.presets()
    }

    /// Value of a control port, outputs included (meters, latency).
    pub fn port_value(&self, symbol: &str) -> Option<f32> {
        self.0.port_value(symbol)
    }
}

impl Instrument for Lv2Instrument {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.0.set_sample_rate(sample_rate);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.0.note_on(note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        self.0.note_off(note);
    }

    fn controller(&mut self, ctl: Controller) {
        self.0.controller(ctl);
    }

    fn all_notes_off(&mut self) {
        self.0.all_notes_off();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.0.process(left, right, true);
    }

    fn editor(&self) -> Option<Editor> {
        Some(self.0.editor())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.load_state(data)
    }
}

impl Effect for Lv2Effect {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.0.set_sample_rate(sample_rate);
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.0.set_tempo(bpm);
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.0.set_sidechain(left, right);
    }

    /// Read from the plugin's latency port after each block.
    fn latency(&self) -> usize {
        self.0.latency()
    }

    /// A plugin does not say how long it may stay quiet before sound comes
    /// back out: the smart disable gives it a generous margin.
    fn tail(&self) -> f32 {
        ponzik_engine::mixer::PLUGIN_TAIL_SECONDS
    }

    fn editor(&self) -> Option<Editor> {
        Some(self.0.editor())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.load_state(data)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.0.process(left, right, false);
    }
}

impl Lv2NoteFx {
    pub(crate) fn new(model: Arc<Model>) -> Result<Self, String> {
        Ok(Lv2NoteFx { core: Core::new(model)?, elapsed: 0, silence: [vec![0f32; 64].into(), vec![0f32; 64].into()] })
    }

    /// Runs the plugin over the time elapsed and turns its MIDI output into notes.
    fn pump(&mut self, out: &mut NoteBuffer) {
        let n = (self.elapsed as usize).clamp(1, 64);
        self.elapsed = 0;
        let [l, r] = &mut self.silence;
        self.core.process(&mut l[..n], &mut r[..n], true);
        self.core.midi_out(|_, msg| match msg {
            [s, k, v, ..] if s & 0xF0 == 0x90 && *v > 0 => {
                out.note_on(*k & 127, *v as f32 / 127.0);
            }
            [s, k, ..] if s & 0xF0 == 0x80 || s & 0xF0 == 0x90 => {
                out.note_off(*k & 127);
            }
            _ => {}
        });
    }
}

impl NoteFx for Lv2NoteFx {
    fn kind(&self) -> &'static str {
        self.core.kind()
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.core.set_sample_rate(sample_rate);
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.core.set_tempo(bpm);
    }

    fn reset(&mut self) {
        self.core.all_notes_off();
        let mut sink = NoteBuffer::default();
        self.pump(&mut sink);
    }

    fn note_on(&mut self, key: u8, vel: f32, _out: &mut NoteBuffer) {
        self.core.note_on(key, vel);
    }

    fn note_off(&mut self, key: u8, _out: &mut NoteBuffer) {
        self.core.note_off(key);
    }

    fn advance(&mut self, frames: u32) {
        self.elapsed = self.elapsed.saturating_add(frames);
    }

    /// The plugin runs at every instant something happens, and every 64
    /// samples in between, so what it delays comes out in time.
    fn tick(&mut self, out: &mut NoteBuffer) {
        self.pump(out);
    }

    fn next_event(&self) -> Option<u32> {
        Some(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_follow_the_port() {
        let sr = Conv::Linear { scale: DISPLAY_RATE, integer: false, sample_rate: true, min: 0.0, max: 0.5 };
        assert_eq!(sr.to_port(4_410.0, 44_100.0), 0.1);
        assert_eq!(sr.to_port(40_000.0, 44_100.0), 0.5, "clamped to the port's range");
        assert_eq!(sr.to_host(0.1, 44_100.0), 4_410.0);
        let ms = Conv::Linear { scale: 0.001, integer: true, sample_rate: false, min: 1.0, max: 500.0 };
        assert_eq!(ms.to_port(0.0204, 48_000.0), 20.0);
        assert!((ms.to_host(20.0, 48_000.0) - 0.02).abs() < 1e-6);
        let choice = Conv::Choice(vec![1.0, 2.0, 4.0]);
        assert_eq!(choice.to_port(2.0, 1.0), 4.0);
        assert_eq!(choice.to_port(9.0, 1.0), 4.0);
        assert_eq!(choice.to_host(2.2, 1.0), 1.0);
        let toggle = Conv::Choice(vec![0.0, 1.0]);
        assert_eq!(toggle.to_host(0.3, 1.0), 1.0, "anything but zero is on");
        assert_eq!(toggle.to_host(0.0, 1.0), 0.0);
    }
}
