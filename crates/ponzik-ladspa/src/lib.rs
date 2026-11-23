//! LADSPA plugin host for Ponzik: effects only, shown with Ponzik's own
//! knobs. The C ABI is written by hand ([`abi`]); libraries are loaded with
//! `dlopen` / `LoadLibraryExW`.
//!
//! Port hints become parameter descriptions: bounds, toggled, integer,
//! logarithmic, sample rate (bounds in fractions of it, shown in hertz)
//! and the default hints. The state of a LADSPA plugin is its parameter
//! values, which the project already keeps.
//!
//! Kind in a project: `ladspa:<path>#<unique id>` (see [`kind_for`]).

pub mod abi;

use std::ffi::{CStr, c_int, c_ulong};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, Weak};

use ponzik_engine::dylib::DynLib;
use ponzik_engine::{Effect, ParamInfo, Params, Unit};

use crate::abi::*;

pub const KIND_PREFIX: &str = "ladspa:";
/// Longest block given to `run`; longer ones are cut.
pub const MAX_BLOCK: usize = 4096;
/// Sample rate the ranges of sample-rate ports are shown at.
const DISPLAY_RATE: f32 = 48_000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    /// The unique id, in decimal.
    pub id: String,
    pub label: String,
    pub name: String,
    pub vendor: String,
    pub path: PathBuf,
}

impl PluginInfo {
    pub fn kind(&self) -> String {
        kind_for(&self.path, &self.id)
    }
}

pub fn kind_for(path: &Path, id: &str) -> String {
    format!("{KIND_PREFIX}{}#{id}", path.display())
}

/// `(path, unique id)` of a kind, cut at the last `#`.
pub fn parse_kind(kind: &str) -> Option<(&Path, &str)> {
    let (path, id) = kind.strip_prefix(KIND_PREFIX)?.rsplit_once('#')?;
    (!path.is_empty() && !id.is_empty()).then_some((Path::new(path), id))
}

/// `$LADSPA_PATH` if set, otherwise the platform's folders.
pub fn search_paths() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("LADSPA_PATH").filter(|v| !v.is_empty()) {
        return std::env::split_paths(&v).collect();
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut dirs = Vec::new();
    if cfg!(windows) {
        dirs.extend(env("COMMONPROGRAMFILES").map(|p| p.join("LADSPA")));
    } else if cfg!(target_os = "macos") {
        dirs.extend(env("HOME").map(|p| p.join("Library/Audio/Plug-Ins/LADSPA")));
        dirs.push("/Library/Audio/Plug-Ins/LADSPA".into());
    } else {
        dirs.extend(env("HOME").map(|p| p.join(".ladspa")));
        dirs.push("/usr/local/lib/ladspa".into());
        dirs.push("/usr/lib/ladspa".into());
        dirs.push("/usr/lib64/ladspa".into());
    }
    dirs
}

/// A shared library, by its extension.
pub fn is_library(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "so" | "dll" | "dylib"))
}

/// Libraries of the search folders (one level of subfolders), without duplicates.
pub fn plugin_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.is_file() && is_library(&p) {
                out.push(p);
            } else if p.is_dir() && depth < 2 {
                walk(&p, depth + 1, out);
            }
        }
    }
    for dir in search_paths() {
        let mut found = Vec::new();
        walk(&dir, 0, &mut found);
        for f in found {
            let c = f.canonicalize().unwrap_or_else(|_| f.clone());
            if !seen.contains(&c) {
                seen.push(c);
                out.push(f);
            }
        }
    }
    out
}

static LIBS: Mutex<Vec<(PathBuf, Weak<DynLib>)>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn load(path: &Path) -> Result<Arc<DynLib>, String> {
    let key = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut libs = lock(&LIBS);
    libs.retain(|(_, w)| w.strong_count() > 0);
    if let Some(l) = libs.iter().find(|(p, _)| *p == key).and_then(|(_, w)| w.upgrade()) {
        return Ok(l);
    }
    let lib = Arc::new(DynLib::open(&key)?);
    libs.push((key, Arc::downgrade(&lib)));
    Ok(lib)
}

fn c_str(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: LADSPA strings are NUL-terminated.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Descriptors of a library.
fn descriptors(lib: &DynLib) -> Result<Vec<&'static LADSPA_Descriptor>, String> {
    let f = lib.symbol(c"ladspa_descriptor");
    if f.is_null() {
        return Err("no ladspa_descriptor in this library".into());
    }
    // SAFETY: the symbol has the documented signature.
    let f: LADSPA_Descriptor_Function = unsafe { std::mem::transmute(f) };
    let mut out = Vec::new();
    for i in 0..4096 {
        // SAFETY: discovery call; null ends the list. Descriptors are static
        // in the library, which stays mapped (see `DynLib`).
        match unsafe { f(i).as_ref() } {
            Some(d) => out.push(d),
            None => break,
        }
    }
    Ok(out)
}

struct Port {
    kind: c_int,
    name: String,
    hint: LADSPA_PortRangeHint,
}

fn ports(d: &LADSPA_Descriptor) -> Vec<Port> {
    (0..d.port_count as usize)
        .map(|i| {
            // SAFETY: arrays of `port_count` entries.
            unsafe {
                Port {
                    kind: *d.port_descriptors.add(i),
                    name: c_str(*d.port_names.add(i)),
                    hint: if d.port_range_hints.is_null() {
                        LADSPA_PortRangeHint { hint_descriptor: 0, lower_bound: 0.0, upper_bound: 1.0 }
                    } else {
                        *d.port_range_hints.add(i)
                    },
                }
            }
        })
        .collect()
}

fn is(p: &Port, flags: c_int) -> bool {
    p.kind & flags == flags
}

/// Why a plugin cannot be an effect in Ponzik, `None` if it can.
fn unsupported(ports: &[Port]) -> Option<&'static str> {
    if !ports.iter().any(|p| is(p, LADSPA_PORT_AUDIO | LADSPA_PORT_OUTPUT)) {
        return Some("has no audio output");
    }
    None
}

/// The plugins of a library (loaded here: the app does it in a child
/// process), and those it cannot host with why.
pub fn scan_file(path: &Path) -> Result<ScanResult, String> {
    let lib = load(path)?;
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    for d in descriptors(&lib)? {
        let name = c_str(d.name);
        if let Some(why) = unsupported(&ports(d)) {
            skipped.push((name, why.to_string()));
            continue;
        }
        ok.push(PluginInfo { id: d.unique_id.to_string(), label: c_str(d.label), name, vendor: c_str(d.maker), path: path.to_path_buf() });
    }
    Ok((ok, skipped))
}

// --- Parameters ----------------------------------------------------------------

/// Default value of a port from its hints (LADSPA 1.1).
pub fn default_value(h: &LADSPA_PortRangeHint, sample_rate: f32) -> f32 {
    let d = h.hint_descriptor;
    let sr = if d & LADSPA_HINT_SAMPLE_RATE != 0 { sample_rate } else { 1.0 };
    let (lo, hi) = (h.lower_bound * sr, h.upper_bound * sr);
    let log = d & LADSPA_HINT_LOGARITHMIC != 0 && lo > 0.0 && hi > 0.0;
    let mix = |w: f32| if log { (lo.ln() * (1.0 - w) + hi.ln() * w).exp() } else { lo * (1.0 - w) + hi * w };
    let v = match d & LADSPA_HINT_DEFAULT_MASK {
        LADSPA_HINT_DEFAULT_MINIMUM => lo,
        LADSPA_HINT_DEFAULT_LOW => mix(0.25),
        LADSPA_HINT_DEFAULT_MIDDLE => mix(0.5),
        LADSPA_HINT_DEFAULT_HIGH => mix(0.75),
        LADSPA_HINT_DEFAULT_MAXIMUM => hi,
        LADSPA_HINT_DEFAULT_0 => 0.0,
        LADSPA_HINT_DEFAULT_1 => 1.0,
        LADSPA_HINT_DEFAULT_100 => 100.0,
        LADSPA_HINT_DEFAULT_440 => 440.0,
        // No default: zero within the bounds.
        _ => match (d & LADSPA_HINT_BOUNDED_BELOW != 0, d & LADSPA_HINT_BOUNDED_ABOVE != 0) {
            (true, true) => 0f32.clamp(lo.min(hi), hi.max(lo)),
            (true, false) => lo.max(0.0),
            (false, true) => hi.min(0.0),
            (false, false) => 0.0,
        },
    };
    if d & LADSPA_HINT_INTEGER != 0 { v.round() } else { v }
}

#[derive(Clone, Copy, Debug)]
struct Conv {
    port: usize,
    /// Bounds in fractions of the sample rate (the value itself is absolute).
    sample_rate: bool,
    integer: bool,
    toggled: bool,
    bounds: (Option<f32>, Option<f32>),
}

impl Conv {
    fn port_value(self, v: f32, sr: f32) -> f32 {
        if self.toggled {
            return if v >= 0.5 { 1.0 } else { 0.0 };
        }
        let k = if self.sample_rate { sr } else { 1.0 };
        let mut v = if self.integer { v.round() } else { v };
        if let Some(lo) = self.bounds.0 {
            v = v.max(lo * k);
        }
        if let Some(hi) = self.bounds.1 {
            v = v.min(hi * k);
        }
        v
    }
}

/// Plugins of a library and the ones skipped, with why.
pub type ScanResult = (Vec<PluginInfo>, Vec<(String, String)>);

struct Model {
    kind: &'static str,
    name: String,
    infos: &'static [ParamInfo],
    convs: Vec<Conv>,
    defaults: Vec<f32>,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

static MODELS: Mutex<Vec<(String, Arc<Model>)>> = Mutex::new(Vec::new());

/// The name of a plugin already loaded in this process (no file read).
pub fn loaded_name(kind: &str) -> Option<String> {
    lock(&MODELS).iter().find(|(k, _)| k == kind).map(|(_, m)| m.name.clone())
}

fn model(kind: String, name: String, ports: &[Port]) -> Arc<Model> {
    let mut models = lock(&MODELS);
    if let Some((_, m)) = models.iter().find(|(k, _)| *k == kind) {
        return m.clone();
    }
    let mut infos = Vec::new();
    let mut convs = Vec::new();
    let mut defaults = Vec::new();
    for (i, p) in ports.iter().enumerate() {
        if !is(p, LADSPA_PORT_CONTROL | LADSPA_PORT_INPUT) {
            continue;
        }
        let h = p.hint.hint_descriptor;
        let sample_rate = h & LADSPA_HINT_SAMPLE_RATE != 0;
        let sr = if sample_rate { DISPLAY_RATE } else { 1.0 };
        let def = default_value(&p.hint, DISPLAY_RATE);
        let name = leak(p.name.clone());
        let bounds = (
            (h & LADSPA_HINT_BOUNDED_BELOW != 0).then_some(p.hint.lower_bound),
            (h & LADSPA_HINT_BOUNDED_ABOVE != 0).then_some(p.hint.upper_bound),
        );
        let conv = Conv { port: i, sample_rate, integer: h & LADSPA_HINT_INTEGER != 0, toggled: h & LADSPA_HINT_TOGGLED != 0, bounds };
        if conv.toggled {
            static OFF_ON: [&str; 2] = ["Off", "On"];
            infos.push(ParamInfo::choice(i as u32, name, &OFF_ON, usize::from(def != 0.0)));
        } else {
            let lo = if h & LADSPA_HINT_BOUNDED_BELOW != 0 { p.hint.lower_bound * sr } else { def.min(0.0) };
            let mut hi = if h & LADSPA_HINT_BOUNDED_ABOVE != 0 { p.hint.upper_bound * sr } else { def.max(lo + 1.0).max(1.0) };
            if hi <= lo {
                hi = lo + 1.0;
            }
            let unit = if sample_rate { Unit::Hz } else { Unit::None };
            let mut info = ParamInfo::new(i as u32, name, lo, hi, def.clamp(lo, hi), unit);
            if h & LADSPA_HINT_LOGARITHMIC != 0 && lo > 0.0 {
                info = info.log();
            }
            infos.push(info);
        }
        convs.push(conv);
        defaults.push(def);
    }
    let m = Arc::new(Model { kind: leak(kind.clone()), name: name.clone(), infos: Box::leak(infos.into_boxed_slice()), convs, defaults });
    models.push((kind, m.clone()));
    m
}

// --- Instance --------------------------------------------------------------------

/// A LADSPA effect: stereo in place through copy buffers.
pub struct LadspaEffect {
    model: Arc<Model>,
    descriptor: &'static LADSPA_Descriptor,
    handle: LADSPA_Handle,
    sample_rate: f32,
    active: bool,
    /// Value of every port (control ports only use theirs).
    controls: Box<[f32]>,
    /// Ponzik value of each parameter.
    values: Box<[f32]>,
    audio: Vec<Box<[f32]>>,
    /// Audio buffers of each port (`usize::MAX`: none).
    audio_of: Vec<usize>,
    ins: Vec<usize>,
    outs: Vec<usize>,
    latency_port: Option<usize>,
    _lib: Arc<DynLib>,
}

// SAFETY: the instance is used by one thread at a time (UI, then the engine).
unsafe impl Send for LadspaEffect {}

impl LadspaEffect {
    pub fn new(path: &Path, id: &str) -> Result<LadspaEffect, String> {
        let lib = load(path)?;
        let descriptor = descriptors(&lib)?
            .into_iter()
            .find(|d| d.unique_id.to_string() == id)
            .ok_or_else(|| format!("LADSPA plugin {id} not found in {}", path.display()))?;
        let ports = ports(descriptor);
        if let Some(why) = unsupported(&ports) {
            return Err(format!("{}: {why}", c_str(descriptor.name)));
        }
        let model = model(kind_for(path, id), c_str(descriptor.name), &ports);
        let mut audio = Vec::new();
        let mut audio_of = Vec::new();
        let (mut ins, mut outs, mut latency_port) = (Vec::new(), Vec::new(), None);
        for (i, p) in ports.iter().enumerate() {
            if is(p, LADSPA_PORT_AUDIO) {
                audio_of.push(audio.len());
                if is(p, LADSPA_PORT_INPUT) {
                    ins.push(audio.len());
                } else {
                    outs.push(audio.len());
                }
                audio.push(vec![0f32; MAX_BLOCK].into_boxed_slice());
            } else {
                audio_of.push(usize::MAX);
                if is(p, LADSPA_PORT_CONTROL | LADSPA_PORT_OUTPUT) && p.name.eq_ignore_ascii_case("latency") {
                    latency_port = Some(i);
                }
            }
        }
        ins.truncate(2);
        outs.truncate(2);
        let mut controls = vec![0f32; ports.len().max(1)].into_boxed_slice();
        for (c, d) in model.convs.iter().zip(&model.defaults) {
            controls[c.port] = c.port_value(*d, DISPLAY_RATE);
        }
        let values = model.infos.iter().map(|i| i.default).collect();
        let mut fx = LadspaEffect {
            model,
            descriptor,
            handle: null_mut(),
            sample_rate: 0.0,
            active: false,
            controls,
            values,
            audio,
            audio_of,
            ins,
            outs,
            latency_port,
            _lib: lib,
        };
        fx.instantiate(DISPLAY_RATE)?;
        Ok(fx)
    }

    fn instantiate(&mut self, sample_rate: f32) -> Result<(), String> {
        let d = self.descriptor;
        // SAFETY: descriptor of the loaded library.
        let h = unsafe { (d.instantiate)(d, sample_rate.round() as c_ulong) };
        if h.is_null() {
            return Err(format!("{} could not be instantiated", c_str(d.name)));
        }
        self.handle = h;
        self.sample_rate = sample_rate;
        // Control values follow the rate (frequencies stay frequencies).
        for (k, c) in self.model.convs.iter().enumerate() {
            self.controls[c.port] = c.port_value(self.values[k], sample_rate);
        }
        let ctl = self.controls.as_mut_ptr();
        for i in 0..d.port_count as usize {
            let ptr = match self.audio_of[i] {
                usize::MAX => unsafe { ctl.add(i) },
                a => self.audio[a].as_mut_ptr(),
            };
            // SAFETY: buffers owned by `self`, sized for the longest block.
            unsafe { (d.connect_port)(h, i as c_ulong, ptr) };
        }
        Ok(())
    }

    fn destroy(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: live instance, no more calls after `cleanup`.
        unsafe {
            if self.active
                && let Some(f) = self.descriptor.deactivate
            {
                f(self.handle);
            }
            (self.descriptor.cleanup)(self.handle);
        }
        self.active = false;
        self.handle = null_mut();
    }

    /// Value of a control port by name (outputs included).
    pub fn port_value(&self, name: &str) -> Option<f32> {
        let ports = ports(self.descriptor);
        let i = ports.iter().position(|p| is(p, LADSPA_PORT_CONTROL) && p.name == name)?;
        Some(self.controls[i])
    }
}

impl Drop for LadspaEffect {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl Params for LadspaEffect {
    fn params(&self) -> &'static [ParamInfo] {
        self.model.infos
    }

    fn get_param(&self, id: u32) -> f32 {
        self.model.convs.iter().position(|c| c.port == id as usize).map_or(0.0, |k| self.values[k])
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(k) = self.model.convs.iter().position(|c| c.port == id as usize) else { return };
        let v = self.model.infos[k].clamp(value);
        self.values[k] = v;
        let c = self.model.convs[k];
        self.controls[c.port] = c.port_value(v, self.sample_rate);
    }
}

impl Effect for LadspaEffect {
    fn kind(&self) -> &'static str {
        self.model.kind
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate > 0.0 && sample_rate != self.sample_rate {
            self.destroy();
            if let Err(e) = self.instantiate(sample_rate) {
                eprintln!("LADSPA: {e}");
                return;
            }
        }
        if !self.active && !self.handle.is_null() {
            // SAFETY: connected instance.
            if let Some(f) = self.descriptor.activate {
                unsafe { f(self.handle) };
            }
            self.active = true;
        }
    }

    fn latency(&self) -> usize {
        self.latency_port.map_or(0, |p| self.controls[p].clamp(0.0, 1_000_000.0) as usize)
    }

    fn tail(&self) -> f32 {
        ponzik_engine::mixer::PLUGIN_TAIL_SECONDS
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.active {
            return;
        }
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            let n = l.len();
            match self.ins.len() {
                0 => {}
                1 => self.audio[self.ins[0]][..n].iter_mut().zip(l.iter().zip(r.iter())).for_each(|(m, (a, b))| *m = 0.5 * (a + b)),
                _ => {
                    let (a, b) = (self.ins[0], self.ins[1]);
                    self.audio[a][..n].copy_from_slice(l);
                    self.audio[b][..n].copy_from_slice(r);
                }
            }
            // SAFETY: active instance, every port connected to a buffer of `MAX_BLOCK`.
            unsafe { (self.descriptor.run)(self.handle, n as c_ulong) };
            let (a, b) = match self.outs.len() {
                0 => return,
                1 => (self.outs[0], self.outs[0]),
                _ => (self.outs[0], self.outs[1]),
            };
            l.copy_from_slice(&self.audio[a][..n]);
            r.copy_from_slice(&self.audio[b][..n]);
        }
    }
}

pub fn create_effect(path: &Path, id: &str) -> Result<Box<dyn Effect>, String> {
    Ok(Box::new(LadspaEffect::new(path, id)?))
}

pub fn effect_from_kind(kind: &str) -> Result<Box<dyn Effect>, String> {
    let (path, id) = parse_kind(kind).ok_or_else(|| format!("invalid LADSPA kind: {kind}"))?;
    create_effect(path, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(d: c_int, lo: f32, hi: f32) -> LADSPA_PortRangeHint {
        LADSPA_PortRangeHint { hint_descriptor: d, lower_bound: lo, upper_bound: hi }
    }

    #[test]
    fn defaults_follow_the_hints() {
        let b = LADSPA_HINT_BOUNDED_BELOW | LADSPA_HINT_BOUNDED_ABOVE;
        assert_eq!(default_value(&hint(b | LADSPA_HINT_DEFAULT_MINIMUM, 1.0, 9.0), 1.0), 1.0);
        assert_eq!(default_value(&hint(b | LADSPA_HINT_DEFAULT_LOW, 0.0, 8.0), 1.0), 2.0);
        assert_eq!(default_value(&hint(b | LADSPA_HINT_DEFAULT_HIGH, 0.0, 8.0), 1.0), 6.0);
        let mid_log = default_value(&hint(b | LADSPA_HINT_DEFAULT_MIDDLE | LADSPA_HINT_LOGARITHMIC, 10.0, 1000.0), 1.0);
        assert!((mid_log - 100.0).abs() < 1e-3);
        assert_eq!(default_value(&hint(b | LADSPA_HINT_DEFAULT_440, 0.0, 1.0), 1.0), 440.0);
        // A fraction of the sample rate.
        let f = default_value(&hint(b | LADSPA_HINT_DEFAULT_MAXIMUM | LADSPA_HINT_SAMPLE_RATE, 0.0, 0.5), 48_000.0);
        assert_eq!(f, 24_000.0);
        assert_eq!(default_value(&hint(b | LADSPA_HINT_DEFAULT_MIDDLE | LADSPA_HINT_INTEGER, 1.0, 8.0), 1.0), 5.0);
        assert_eq!(default_value(&hint(LADSPA_HINT_BOUNDED_BELOW, 2.0, 0.0), 1.0), 2.0);
    }

    #[test]
    fn kinds_round_trip() {
        let k = kind_for(Path::new("/usr/lib/ladspa/a#b.so"), "1049");
        assert_eq!(parse_kind(&k), Some((Path::new("/usr/lib/ladspa/a#b.so"), "1049")));
        assert!(effect_from_kind("ladspa:/nonexistent.so#1").is_err());
        assert!(is_library(Path::new("x.SO")) && !is_library(Path::new("x.ttl")));
    }
}
