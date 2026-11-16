//! Instance de plugin CLAP enveloppée dans les traits `Instrument` et `Effect`.
//!
//! Tout ce qui alloue (tampons audio, files d'événements, fiches de paramètres)
//! est préparé à la création ; `render`, `process`, `note_on` et `set_param`
//! n'allouent jamais.

use std::ffi::{c_char, c_void};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use ponzik_engine::ring::Producer;
use ponzik_engine::{Editor, Effect, Instrument, ParamInfo, Params, Unit};

use crate::abi::*;
use crate::host::{ClapEditor, HostShared, OutMsg, as_audio_thread, new_host};
use crate::library::Library;

/// Taille de bloc maximale annoncée au plugin ; les blocs plus longs sont découpés.
pub const MAX_BLOCK: usize = 4096;
/// Capacité des files d'événements : au-delà, les événements du bloc sont perdus.
const EVENT_CAPACITY: usize = 1024;

// --- Files d'événements ----------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
union Event {
    header: ClapEventHeader,
    note: ClapEventNote,
    param: ClapEventParamValue,
    midi: ClapEventMidi,
}

/// File d'entrée de taille fixe ; `list.ctx` pointe sur la structure elle-même.
struct InEvents {
    list: ClapInputEvents,
    events: Vec<Event>,
}

unsafe extern "C" fn in_size(list: *const ClapInputEvents) -> u32 {
    // SAFETY : `ctx` pointe sur l'`InEvents` propriétaire, vivant pendant `process`.
    unsafe { (*((*list).ctx as *const InEvents)).events.len() as u32 }
}

unsafe extern "C" fn in_get(list: *const ClapInputEvents, index: u32) -> *const ClapEventHeader {
    // SAFETY : idem ; l'indice est vérifié.
    let ev = unsafe { &*((*list).ctx as *const InEvents) };
    ev.events.get(index as usize).map_or(null(), |e| e as *const Event as *const ClapEventHeader)
}

impl InEvents {
    fn new() -> Box<Self> {
        let mut b = Box::new(InEvents {
            list: ClapInputEvents { ctx: null_mut(), size: in_size, get: in_get },
            events: Vec::with_capacity(EVENT_CAPACITY),
        });
        b.list.ctx = &mut *b as *mut InEvents as *mut c_void;
        b
    }

    fn push(&mut self, e: Event) {
        if self.events.len() < EVENT_CAPACITY {
            self.events.push(e);
        }
    }
}

/// File de sortie : on ne garde que les changements de paramètres et les gestes.
struct OutEvents {
    list: ClapOutputEvents,
    params: Vec<OutMsg>,
}

unsafe extern "C" fn out_push(list: *const ClapOutputEvents, e: *const ClapEventHeader) -> bool {
    // SAFETY : `ctx` pointe sur l'`OutEvents` propriétaire, `e` sur un événement valide.
    let (out, h) = unsafe { (&mut *((*list).ctx as *mut OutEvents), &*e) };
    if h.space_id != CLAP_CORE_EVENT_SPACE_ID || out.params.len() >= EVENT_CAPACITY {
        return true;
    }
    // SAFETY : le type annoncé garantit la structure.
    let msg = match h.kind {
        CLAP_EVENT_PARAM_VALUE => {
            let p = unsafe { &*(e as *const ClapEventParamValue) };
            OutMsg::Param(p.param_id, p.value)
        }
        CLAP_EVENT_PARAM_GESTURE_BEGIN => OutMsg::Begin(unsafe { (*(e as *const ClapEventParamGesture)).param_id }),
        CLAP_EVENT_PARAM_GESTURE_END => OutMsg::End(unsafe { (*(e as *const ClapEventParamGesture)).param_id }),
        _ => return true,
    };
    out.params.push(msg);
    true
}

impl OutEvents {
    fn new() -> Box<Self> {
        let mut b = Box::new(OutEvents {
            list: ClapOutputEvents { ctx: null_mut(), try_push: out_push },
            params: Vec::with_capacity(EVENT_CAPACITY),
        });
        b.list.ctx = &mut *b as *mut OutEvents as *mut c_void;
        b
    }
}

fn header(kind: u16, size: usize) -> ClapEventHeader {
    ClapEventHeader { size: size as u32, time: 0, space_id: CLAP_CORE_EVENT_SPACE_ID, kind, flags: CLAP_EVENT_IS_LIVE }
}

// --- Tampons audio ---------------------------------------------------------

/// Tampons d'un sens (entrées ou sorties) : un `clap_audio_buffer` par port.
struct Ports {
    data: Vec<Vec<f32>>,
    ptrs: Vec<Vec<*mut f32>>,
    buffers: Vec<ClapAudioBuffer>,
}

impl Ports {
    fn new(channels: &[u32]) -> Self {
        let mut data = Vec::new();
        let mut ptrs = Vec::new();
        for &n in channels {
            let mut port = Vec::new();
            for _ in 0..n {
                let mut buf = vec![0.0f32; MAX_BLOCK];
                port.push(buf.as_mut_ptr());
                data.push(buf);
            }
            ptrs.push(port);
        }
        // Les tampons du tas ne bougent pas quand `data` et `ptrs` sont déplacés.
        let buffers = ptrs
            .iter_mut()
            .map(|p| ClapAudioBuffer {
                data32: p.as_mut_ptr(),
                data64: null_mut(),
                channel_count: p.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        Ports { data, ptrs, buffers }
    }

    /// Canaux du port principal (vide s'il n'existe pas).
    fn main(&mut self) -> &mut [Vec<f32>] {
        let n = self.ptrs.first().map_or(0, Vec::len);
        &mut self.data[..n]
    }

    fn clear(&mut self, frames: usize) {
        for d in &mut self.data {
            d[..frames].fill(0.0);
        }
    }
}

// --- Fiches de paramètres partagées ---------------------------------------

/// Chaînes et fiches fuitées une fois par type de plugin (`kind`).
type Leaked = (&'static str, &'static [ParamInfo]);
static LEAKED: Mutex<Vec<Leaked>> = Mutex::new(Vec::new());

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn fixed_str(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

// --- Instance --------------------------------------------------------------

struct Param {
    id: ClapId,
    cookie: *mut c_void,
    value: f32,
}

/// Instance CLAP active ; commune aux enveloppes instrument et effet.
struct Core {
    plugin: *const ClapPlugin,
    kind: &'static str,
    infos: &'static [ParamInfo],
    /// Triés par identifiant CLAP.
    values: Vec<Param>,
    in_events: Box<InEvents>,
    out_events: Box<OutEvents>,
    inputs: Ports,
    outputs: Ports,
    midi_dialect: bool,
    held: u128,
    steady_time: i64,
    /// Latence annoncée par `clap.latency`, lue à l'activation.
    latency: usize,
    active: bool,
    processing: bool,
    /// Éditions de l'interface du plugin, vers l'interface de Ponzik.
    edits: Producer<OutMsg>,
    // Détruits après le plugin (ordre de déclaration).
    shared: Arc<HostShared>,
    _host: Box<ClapHost>,
    _lib: Arc<Library>,
}

// SAFETY : l'instance n'est utilisée que par un thread à la fois (interface puis
// moteur), ce que CLAP autorise pour les appels du thread principal et audio.
unsafe impl Send for Core {}

fn ext<T>(plugin: *const ClapPlugin, id: &[u8]) -> *const T {
    // SAFETY : plugin initialisé, identifiant terminé par un zéro.
    unsafe { ((*plugin).get_extension)(plugin, id.as_ptr().cast()) }.cast()
}

fn port_channels(plugin: *const ClapPlugin, is_input: bool, fallback: u32) -> Vec<u32> {
    let ap = ext::<ClapPluginAudioPorts>(plugin, CLAP_EXT_AUDIO_PORTS);
    // SAFETY : extension renvoyée par le plugin, pointeur vérifié.
    let Some(ap) = (unsafe { ap.as_ref() }) else {
        return if fallback > 0 { vec![fallback] } else { Vec::new() };
    };
    // SAFETY : appels documentés de `clap.audio-ports`, sur une structure locale.
    let count = unsafe { (ap.count)(plugin, is_input) };
    (0..count)
        .map(|i| {
            let mut info: ClapAudioPortInfo = unsafe { std::mem::zeroed() };
            if unsafe { (ap.get)(plugin, i, is_input, &mut info) } { info.channel_count.min(64) } else { 0 }
        })
        .collect()
}

fn prefers_midi(plugin: *const ClapPlugin) -> bool {
    let np = ext::<ClapPluginNotePorts>(plugin, CLAP_EXT_NOTE_PORTS);
    // SAFETY : extension renvoyée par le plugin, pointeur vérifié.
    let Some(np) = (unsafe { np.as_ref() }) else { return false };
    let mut info: ClapNotePortInfo = unsafe { std::mem::zeroed() };
    // SAFETY : appels documentés de `clap.note-ports`.
    if unsafe { (np.count)(plugin, true) } == 0 || !unsafe { (np.get)(plugin, 0, true, &mut info) } {
        return false;
    }
    info.supported_dialects & CLAP_NOTE_DIALECT_CLAP == 0 && info.supported_dialects & CLAP_NOTE_DIALECT_MIDI != 0
}

impl Core {
    fn new(path: &Path, id: &str, effect: bool) -> Result<Core, String> {
        let lib = Library::load(path)?;
        let factory = lib.factory().ok_or_else(|| format!("{}: no plugin factory", path.display()))?;
        let cid = std::ffi::CString::new(id).map_err(|_| "identifiant invalide".to_string())?;
        let (shared, edits) = HostShared::new();
        let host = new_host(&shared);
        // SAFETY : fabrique valide ; l'hôte vit plus longtemps que le plugin.
        let plugin = unsafe { (factory.create_plugin)(factory, &*host, cid.as_ptr()) };
        if plugin.is_null() {
            return Err(format!("plugin \"{id}\" not found in {}", path.display()));
        }
        // SAFETY : plugin fraîchement créé.
        if !unsafe { ((*plugin).init)(plugin) } {
            unsafe { ((*plugin).destroy)(plugin) };
            return Err(format!("initialization of \"{id}\" failed"));
        }
        shared.attach(plugin);
        let params_ext = ext::<ClapPluginParams>(plugin, CLAP_EXT_PARAMS);
        let kind = crate::kind_for(path, id);
        let (kind, infos) = shared_infos(kind, plugin, params_ext);
        let cookies = cookies(plugin, params_ext);
        let mut values: Vec<Param> = infos
            .iter()
            .map(|p| {
                let mut v = p.default as f64;
                if !params_ext.is_null() {
                    // SAFETY : extension non nulle, sortie locale.
                    unsafe { ((*params_ext).get_value)(plugin, p.id, &mut v) };
                }
                let cookie = cookies.binary_search_by_key(&p.id, |c| c.0).map_or(null_mut(), |i| cookies[i].1);
                Param { id: p.id, cookie, value: v as f32 }
            })
            .collect();
        values.sort_by_key(|p| p.id);
        Ok(Core {
            plugin,
            kind,
            infos,
            values,
            in_events: InEvents::new(),
            out_events: OutEvents::new(),
            inputs: Ports::new(&port_channels(plugin, true, if effect { 2 } else { 0 })),
            outputs: Ports::new(&port_channels(plugin, false, 2)),
            midi_dialect: prefers_midi(plugin),
            held: 0,
            steady_time: 0,
            latency: 0,
            active: false,
            processing: false,
            edits,
            shared,
            _host: host,
            _lib: lib,
        })
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.stop();
        // SAFETY : plugin initialisé et inactif.
        self.active = unsafe { ((*self.plugin).activate)(self.plugin, sample_rate as f64, 1, MAX_BLOCK as u32) };
        self.latency = 0;
        if self.active {
            let ext = ext::<ClapPluginLatency>(self.plugin, CLAP_EXT_LATENCY);
            // SAFETY : extension renvoyée par le plugin, pointeur vérifié ; plugin
            // actif et appel depuis le thread principal, comme l'exige `clap.latency`.
            if let Some(ext) = unsafe { ext.as_ref() } {
                self.latency = unsafe { (ext.get)(self.plugin) } as usize;
            }
        }
    }

    fn stop(&mut self) {
        // SAFETY : appels symétriques de `start_processing` / `activate`.
        unsafe {
            if self.processing {
                as_audio_thread(|| ((*self.plugin).stop_processing)(self.plugin));
            }
            if self.active {
                ((*self.plugin).deactivate)(self.plugin);
            }
        }
        self.processing = false;
        self.active = false;
    }

    fn param_index(&self, id: u32) -> Option<usize> {
        self.values.binary_search_by_key(&id, |p| p.id).ok()
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(i) = self.param_index(id) else { return };
        let p = &mut self.values[i];
        p.value = value;
        let param = ClapEventParamValue {
            header: header(CLAP_EVENT_PARAM_VALUE, size_of::<ClapEventParamValue>()),
            param_id: id,
            cookie: p.cookie,
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value: value as f64,
        };
        self.in_events.push(Event { param });
    }

    fn note(&mut self, kind: u16, key: u8, velocity: f32) {
        let key = key.min(127);
        let e = if self.midi_dialect {
            let status = if kind == CLAP_EVENT_NOTE_ON { 0x90 } else { 0x80 };
            let midi = ClapEventMidi {
                header: header(CLAP_EVENT_MIDI, size_of::<ClapEventMidi>()),
                port_index: 0,
                data: [status, key, (velocity.clamp(0.0, 1.0) * 127.0).round() as u8],
            };
            Event { midi }
        } else {
            let note = ClapEventNote {
                header: header(kind, size_of::<ClapEventNote>()),
                note_id: -1,
                port_index: 0,
                channel: 0,
                key: key as i16,
                velocity: velocity.clamp(0.0, 1.0) as f64,
            };
            Event { note }
        };
        self.in_events.push(e);
        if kind == CLAP_EVENT_NOTE_ON {
            self.held |= 1 << key;
        } else {
            self.held &= !(1 << key);
        }
    }

    fn all_notes_off(&mut self) {
        let kind = if self.midi_dialect { CLAP_EVENT_NOTE_OFF } else { CLAP_EVENT_NOTE_CHOKE };
        for key in 0..128u8 {
            if self.held & (1 << key) != 0 {
                self.note(kind, key, 0.0);
            }
        }
    }

    /// Un appel `process` sur `frames` échantillons, tampons déjà remplis.
    fn run(&mut self, frames: usize) -> bool {
        if !self.active {
            self.in_events.events.clear();
            return false;
        }
        if !self.processing {
            // SAFETY : plugin actif, thread audio.
            self.processing = as_audio_thread(|| unsafe { ((*self.plugin).start_processing)(self.plugin) });
            if !self.processing {
                return false;
            }
        }
        self.outputs.clear(frames);
        let process = ClapProcess {
            steady_time: self.steady_time,
            frames_count: frames as u32,
            transport: null(),
            audio_inputs: self.inputs.buffers.as_ptr(),
            audio_outputs: self.outputs.buffers.as_mut_ptr(),
            audio_inputs_count: self.inputs.buffers.len() as u32,
            audio_outputs_count: self.outputs.buffers.len() as u32,
            in_events: &self.in_events.list,
            out_events: &self.out_events.list,
        };
        // SAFETY : tous les pointeurs de `process` vivent pendant l'appel.
        let status = as_audio_thread(|| unsafe { ((*self.plugin).process)(self.plugin, &process) });
        self.in_events.events.clear();
        self.steady_time += frames as i64;
        for k in 0..self.out_events.params.len() {
            let msg = self.out_events.params[k];
            if let OutMsg::Param(id, v) = msg
                && let Some(i) = self.param_index(id)
            {
                self.values[i].value = v as f32;
            }
            // File pleine : l'interface de Ponzik ne suivra pas cette édition.
            let _ = self.edits.push(msg);
        }
        self.out_events.params.clear();
        status != CLAP_PROCESS_ERROR
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // L'interface du plugin d'abord, puis l'éditeur devient inerte.
        self.shared.detach();
        self.stop();
        // SAFETY : plus aucun appel après la destruction.
        unsafe { ((*self.plugin).destroy)(self.plugin) };
    }
}

/// Parcourt les fiches CLAP du plugin.
fn for_each_info(plugin: *const ClapPlugin, ext: *const ClapPluginParams, mut f: impl FnMut(&ClapParamInfo)) {
    // SAFETY : extension renvoyée par le plugin, pointeur vérifié.
    let Some(ext) = (unsafe { ext.as_ref() }) else { return };
    // SAFETY : appels documentés de `clap.params`, sur une structure locale.
    for i in 0..unsafe { (ext.count)(plugin) } {
        let mut info: ClapParamInfo = unsafe { std::mem::zeroed() };
        if unsafe { (ext.get_info)(plugin, i, &mut info) } {
            f(&info);
        }
    }
}

/// Cookies des paramètres de cette instance, triés par identifiant.
fn cookies(plugin: *const ClapPlugin, ext: *const ClapPluginParams) -> Vec<(ClapId, *mut c_void)> {
    let mut v = Vec::new();
    for_each_info(plugin, ext, |i| v.push((i.id, i.cookie)));
    v.sort_by_key(|c| c.0);
    v
}

/// Fiches de paramètres et `kind` fuités, réutilisés pour chaque instance du même plugin.
fn shared_infos(kind: String, plugin: *const ClapPlugin, ext: *const ClapPluginParams) -> Leaked {
    let mut leaked = LEAKED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&l) = leaked.iter().find(|(k, _)| *k == kind) {
        return l;
    }
    let mut infos = Vec::new();
    for_each_info(plugin, ext, |info| {
        if info.flags & CLAP_PARAM_IS_HIDDEN != 0 || info.id == CLAP_INVALID_ID {
            return;
        }
        let (min, max, def) = (info.min_value as f32, info.max_value as f32, info.default_value as f32);
        let mut p = ParamInfo::new(info.id, leak(fixed_str(&info.name)), min, max, def, Unit::None);
        let module = fixed_str(&info.module);
        if !module.is_empty() {
            p = p.group(leak(module));
        }
        infos.push(p);
    });
    let l = (leak(kind), &*Box::leak(infos.into_boxed_slice()));
    leaked.push(l);
    l
}

// --- Enveloppes ------------------------------------------------------------

/// Instrument CLAP (entrée de notes, sortie stéréo ajoutée au rendu).
pub struct ClapInstrument(Core);

/// Effet CLAP (traitement stéréo sur place via des tampons de copie).
pub struct ClapEffect(Core);

impl ClapInstrument {
    pub(crate) fn new(path: &Path, id: &str) -> Result<Self, String> {
        Core::new(path, id, false).map(ClapInstrument)
    }
}

impl ClapEffect {
    pub(crate) fn new(path: &Path, id: &str) -> Result<Self, String> {
        Core::new(path, id, true).map(ClapEffect)
    }
}

macro_rules! impl_params {
    ($t:ty) => {
        impl Params for $t {
            fn params(&self) -> &'static [ParamInfo] {
                self.0.infos
            }

            fn get_param(&self, id: u32) -> f32 {
                self.0.param_index(id).map_or(0.0, |i| self.0.values[i].value)
            }

            fn set_param(&mut self, id: u32, value: f32) {
                self.0.set_param(id, value);
            }
        }
    };
}

impl_params!(ClapInstrument);
impl_params!(ClapEffect);

impl Core {
    fn editor(&self) -> Option<Editor> {
        Some(Arc::new(ClapEditor(self.shared.clone())))
    }
}

impl Instrument for ClapInstrument {
    fn kind(&self) -> &'static str {
        self.0.kind
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.0.set_sample_rate(sample_rate);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.0.note(CLAP_EVENT_NOTE_ON, note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        self.0.note(CLAP_EVENT_NOTE_OFF, note, 0.0);
    }

    fn all_notes_off(&mut self) {
        self.0.all_notes_off();
    }

    fn editor(&self) -> Option<Editor> {
        self.0.editor()
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let core = &mut self.0;
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            let n = l.len();
            if !core.run(n) {
                return;
            }
            let out = core.outputs.main();
            let (a, b) = match out {
                [] => return,
                [m] => (&m[..n], &m[..n]),
                [a, b, ..] => (&a[..n], &b[..n]),
            };
            for i in 0..n {
                l[i] += a[i];
                r[i] += b[i];
            }
        }
    }
}

impl Effect for ClapEffect {
    fn kind(&self) -> &'static str {
        self.0.kind
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.0.set_sample_rate(sample_rate);
    }

    /// Lue à l'activation : un changement demandé ensuite par le plugin
    /// (`request_restart`) n'est pas suivi.
    fn latency(&self) -> usize {
        self.0.latency
    }

    fn editor(&self) -> Option<Editor> {
        self.0.editor()
    }

    fn reset(&mut self) {
        if self.0.active {
            // SAFETY : plugin actif.
            as_audio_thread(|| unsafe { ((*self.0.plugin).reset)(self.0.plugin) });
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let core = &mut self.0;
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            let n = l.len();
            match core.inputs.main() {
                [] => {}
                [m] => m[..n].iter_mut().zip(l.iter().zip(r.iter())).for_each(|(m, (a, b))| *m = 0.5 * (a + b)),
                [a, b, ..] => {
                    a[..n].copy_from_slice(l);
                    b[..n].copy_from_slice(r);
                }
            }
            if !core.run(n) {
                return;
            }
            match core.outputs.main() {
                [] => {}
                [m] => {
                    l.copy_from_slice(&m[..n]);
                    r.copy_from_slice(&m[..n]);
                }
                [a, b, ..] => {
                    l.copy_from_slice(&a[..n]);
                    r.copy_from_slice(&b[..n]);
                }
            }
        }
    }
}
