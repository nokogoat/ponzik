//! Instance de plugin VST 3 enveloppée dans les traits `Instrument` et `Effect`.
//!
//! Tout ce qui alloue (tampons audio, listes d'événements, files de paramètres,
//! fiches) est préparé à la création ; `render`, `process`, `note_on` et
//! `set_param` n'allouent jamais.
//!
//! Les paramètres sont exposés sous leur forme normalisée VST 3 (`[0, 1]`),
//! ou en pas entiers `0..=stepCount` pour les paramètres discrets.

use std::path::Path;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};

use ponzik_engine::ring::Producer;
use ponzik_engine::{Controller, Editor, Effect, Instrument, NoteExpression, ParamInfo, Params, TuningRamps, Unit};

use crate::abi::*;
use crate::com::*;
use crate::editor::{ComponentHandler, Vst3Editor, Vst3Shared, to_norm, to_value};
use crate::library::Module;

/// Taille de bloc maximale annoncée au plugin ; les blocs plus longs sont découpés.
pub const MAX_BLOCK: usize = 4096;
/// Capacité des listes d'événements : au-delà, les événements du bloc sont perdus.
const EVENT_CAPACITY: usize = 1024;
/// Samples between two values of a gliding note's tuning.
const RAMP_STEP: usize = 32;
/// Files de sortie réservées en plus des paramètres connus.
const EXTRA_QUEUES: usize = 16;

// --- Tampons audio ---------------------------------------------------------

/// Tampons d'un sens (entrées ou sorties) : un `AudioBusBuffers` par bus.
struct Buses {
    data: Vec<Vec<f32>>,
    ptrs: Vec<Vec<*mut f32>>,
    buffers: Vec<AudioBusBuffers>,
}

impl Buses {
    fn new(channels: &[u32]) -> Self {
        let mut data = Vec::new();
        let mut ptrs = Vec::new();
        for &n in channels {
            let mut bus = Vec::new();
            for _ in 0..n {
                let mut buf = vec![0.0f32; MAX_BLOCK];
                bus.push(buf.as_mut_ptr());
                data.push(buf);
            }
            ptrs.push(bus);
        }
        // Les tampons du tas ne bougent pas quand `data` et `ptrs` sont déplacés.
        let buffers = ptrs
            .iter_mut()
            .map(|p| AudioBusBuffers {
                num_channels: p.len() as i32,
                silence_flags: 0,
                channel_buffers32: p.as_mut_ptr(),
            })
            .collect();
        Buses { data, ptrs, buffers }
    }

    /// Canaux du bus principal (vide s'il n'existe pas).
    fn main(&mut self) -> &mut [Vec<f32>] {
        let n = self.ptrs.first().map_or(0, Vec::len);
        &mut self.data[..n]
    }

    fn clear(&mut self, frames: usize) {
        for d in &mut self.data {
            d[..frames].fill(0.0);
        }
        for b in &mut self.buffers {
            b.silence_flags = 0;
        }
    }

    fn as_mut_ptr(&mut self) -> *mut AudioBusBuffers {
        if self.buffers.is_empty() { null_mut() } else { self.buffers.as_mut_ptr() }
    }
}

// --- Fiches de paramètres partagées ---------------------------------------

/// Chaînes et fiches fuitées une fois par type de plugin (`kind`).
type Leaked = (&'static str, &'static [ParamInfo]);
static LEAKED: Mutex<Vec<Leaked>> = Mutex::new(Vec::new());

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// --- Instance --------------------------------------------------------------

struct Param {
    id: ParamId,
    /// `stepCount` VST 3 : 0 pour un paramètre continu.
    steps: i32,
    value: f32,
}

impl Param {
    fn normalize(&self, v: f32) -> f64 {
        to_norm(self.steps, v)
    }

    fn denormalize(&self, n: f64) -> f32 {
        to_value(self.steps, n)
    }
}

/// Instance VST 3 active ; commune aux enveloppes instrument et effet.
struct Core {
    component: Com<IComponent>,
    processor: Com<IAudioProcessor>,
    controller: Option<Com<IEditController>>,
    /// Contrôleur distinct du composant (initialisé et terminé à part).
    separate: bool,
    /// Points de connexion composant ↔ contrôleur, à déconnecter avant la fin.
    connection: Option<(Com<IConnectionPoint>, Com<IConnectionPoint>)>,
    kind: &'static str,
    infos: &'static [ParamInfo],
    /// Triés par identifiant.
    values: Vec<Param>,
    in_changes: Owned<ParamChanges>,
    out_changes: Owned<ParamChanges>,
    in_events: Owned<EventList>,
    out_events: Owned<EventList>,
    inputs: Buses,
    outputs: Buses,
    context: Box<ProcessContext>,
    has_events: bool,
    /// Parameter driven by each MIDI controller (`IMidiMapping`): CCs 0 to
    /// 127, then aftertouch (128) and pitch bend ([`K_PITCH_BEND`]).
    midi_map: Box<[Option<ParamId>; 130]>,
    /// Tuning of each key (slides, portamento), sent every [`RAMP_STEP`].
    tuning: Box<TuningRamps>,
    sample_rate: f32,
    /// `IAudioProcessor::getLatencySamples`, lu à l'activation.
    latency: usize,
    held: u128,
    active: bool,
    processing: bool,
    /// Changements de sortie du processeur, vers le contrôleur (thread de l'interface).
    edits: Producer<(ParamId, f64)>,
    shared: Arc<Vst3Shared>,
    // Détruits après le plugin (ordre de déclaration).
    _handler: Owned<ComponentHandler>,
    _host: Owned<HostApplication>,
    _module: Arc<Module>,
}

// SAFETY : l'instance n'est utilisée que par un thread à la fois (interface puis
// moteur), ce que VST 3 tolère pour les appels de configuration et de traitement.
unsafe impl Send for Core {}

macro_rules! call {
    ($com:expr, $vtbl:ty, $method:ident $(, $arg:expr)*) => {{
        let c = &$com;
        // SAFETY : interface vivante, table du bon type, arguments valides pendant l'appel.
        unsafe { (c.vtbl::<$vtbl>().$method)(c.as_ptr() $(, $arg)*) }
    }};
}

fn create<I: Interface>(module: &Module, cid: &Tuid) -> Option<Com<I>> {
    let f = module.factory();
    let mut out = null_mut();
    let r = call!(f, IPluginFactoryVtbl, create_instance, cid.as_ptr().cast(), I::IID.as_ptr().cast(), &mut out);
    // SAFETY : la fabrique renvoie une référence comptée sur `I`.
    if r == K_RESULT_OK { unsafe { Com::from_raw(out) } } else { None }
}

fn initialize<I: Interface>(obj: &Com<I>, host: &Owned<HostApplication>) -> bool {
    call!(obj, IPluginBaseVtbl, initialize, host.as_ptr()) == K_RESULT_OK
}

fn terminate<I: Interface>(obj: &Com<I>) {
    call!(obj, IPluginBaseVtbl, terminate);
}

/// Canaux de chaque bus audio dans un sens.
fn bus_channels(component: &Com<IComponent>, dir: i32) -> Vec<u32> {
    let count = call!(component, IComponentVtbl, get_bus_count, K_AUDIO, dir).clamp(0, 64);
    (0..count)
        .map(|i| {
            // SAFETY : structure C sans invariant, remplie par le plugin.
            let mut info: BusInfo = unsafe { std::mem::zeroed() };
            if call!(component, IComponentVtbl, get_bus_info, K_AUDIO, dir, i, &mut info) == K_RESULT_OK {
                info.channel_count.clamp(0, 64) as u32
            } else {
                0
            }
        })
        .collect()
}

/// Demande la stéréo sur les bus principaux, garde la disposition proposée ailleurs.
fn arrange_buses(component: &Com<IComponent>, processor: &Com<IAudioProcessor>) {
    let arrangements = |dir: i32| -> Vec<SpeakerArrangement> {
        let n = call!(component, IComponentVtbl, get_bus_count, K_AUDIO, dir).clamp(0, 64);
        (0..n)
            .map(|i| {
                let mut a = K_STEREO;
                if i > 0 {
                    call!(processor, IAudioProcessorVtbl, get_bus_arrangement, dir, i, &mut a);
                }
                a
            })
            .collect()
    };
    let (mut ins, mut outs) = (arrangements(K_INPUT), arrangements(K_OUTPUT));
    let ptr = |v: &mut Vec<u64>| if v.is_empty() { null_mut() } else { v.as_mut_ptr() };
    let (pi, po) = (ptr(&mut ins), ptr(&mut outs));
    // Refus possible : le plugin garde alors sa disposition, lue ensuite dans `getBusInfo`.
    call!(processor, IAudioProcessorVtbl, set_bus_arrangements, pi, ins.len() as i32, po, outs.len() as i32);
}

/// Transmet l'état du composant au contrôleur séparé.
fn sync_controller(component: &Com<IComponent>, controller: &Com<IEditController>) {
    let mut stream = Owned::new(MemoryStream::default());
    if call!(component, IComponentVtbl, get_state, stream.as_ptr()) != K_RESULT_OK {
        return;
    }
    stream.get().pos = 0;
    call!(controller, IEditControllerVtbl, set_component_state, stream.as_ptr());
}

impl Core {
    fn new(bundle: &Path, class_id: &str, effect: bool) -> Result<Core, String> {
        let cid =
            crate::parse_class_id(class_id).ok_or_else(|| format!("identifiant de classe invalide : {class_id}"))?;
        let module = Module::load(bundle)?;
        let host = Owned::new(HostApplication);
        let component = create::<IComponent>(&module, &cid)
            .ok_or_else(|| format!("class \"{class_id}\" not found in {}", bundle.display()))?;
        if !initialize(&component, &host) {
            return Err(format!("initialization of \"{class_id}\" failed"));
        }
        let Some(processor) = component.query::<IAudioProcessor>() else {
            terminate(&component);
            return Err(format!("\"{class_id}\" is not an audio processor"));
        };
        if call!(processor, IAudioProcessorVtbl, can_process_sample_size, K_SAMPLE32) != K_RESULT_OK {
            drop(processor);
            terminate(&component);
            return Err(format!("\"{class_id}\" does not process 32-bit audio"));
        }

        // Contrôleur : intégré au composant, ou classe séparée à connecter.
        let mut separate = false;
        let controller = component.query::<IEditController>().or_else(|| {
            let mut ccid: Tuid = [0; 16];
            if call!(component, IComponentVtbl, get_controller_class_id, &mut ccid) != K_RESULT_OK || ccid == [0; 16] {
                return None;
            }
            let c = create::<IEditController>(&module, &ccid)?;
            if !initialize(&c, &host) {
                return None;
            }
            separate = true;
            Some(c)
        });
        let mut connection = None;
        if let Some(ctrl) = controller.as_ref().filter(|_| separate) {
            if let (Some(a), Some(b)) = (component.query::<IConnectionPoint>(), ctrl.query::<IConnectionPoint>()) {
                call!(a, IConnectionPointVtbl, connect, b.as_ptr());
                call!(b, IConnectionPointVtbl, connect, a.as_ptr());
                connection = Some((a, b));
            }
            sync_controller(&component, ctrl);
        }
        let (shared, edits) = Vst3Shared::new();
        let handler = Owned::new(ComponentHandler(shared.clone()));
        if let Some(ctrl) = &controller {
            call!(ctrl, IEditControllerVtbl, set_component_handler, handler.as_ptr());
        }

        // Bus : stéréo demandée, bus principaux et entrée d'événements activés.
        arrange_buses(&component, &processor);
        let (ins, outs) = (bus_channels(&component, K_INPUT), bus_channels(&component, K_OUTPUT));
        if !ins.is_empty() {
            call!(component, IComponentVtbl, activate_bus, K_AUDIO, K_INPUT, 0, 1);
        }
        if !outs.is_empty() {
            call!(component, IComponentVtbl, activate_bus, K_AUDIO, K_OUTPUT, 0, 1);
        }
        let has_events = call!(component, IComponentVtbl, get_bus_count, K_EVENT, K_INPUT) > 0;
        if has_events {
            call!(component, IComponentVtbl, activate_bus, K_EVENT, K_INPUT, 0, 1);
        }
        if effect && ins.is_empty() {
            // Un effet sans entrée ne peut rien traiter : on le signale tôt.
            eprintln!("VST 3: \"{class_id}\" has no audio input");
        }

        let mut midi_map: Box<[Option<ParamId>; 130]> = Box::new([None; 130]);
        if let Some(mapping) = controller.as_ref().and_then(|c| c.query::<IMidiMapping>()) {
            for (n, slot) in midi_map.iter_mut().enumerate() {
                let mut id: ParamId = 0;
                if call!(mapping, IMidiMappingVtbl, get_midi_controller_assignment, 0, 0, n as i16, &mut id) == K_RESULT_OK {
                    *slot = Some(id);
                }
            }
        }
        let kind = crate::kind_for(bundle, class_id);
        let (kind, infos) = shared_infos(kind, controller.as_ref());
        let mut values: Vec<Param> = Vec::with_capacity(infos.len());
        if let Some(ctrl) = &controller {
            for_each_param(ctrl, |p| {
                if infos.iter().any(|i| i.id == p.id) {
                    let n = call!(ctrl, IEditControllerVtbl, get_param_normalized, p.id);
                    let mut v = Param { id: p.id, steps: p.step_count.max(0), value: 0.0 };
                    v.value = v.denormalize(n);
                    values.push(v);
                }
            });
        }
        values.sort_by_key(|p| p.id);
        values.dedup_by_key(|p| p.id);
        shared.attach(component.clone(), controller.clone(), values.iter().map(|p| (p.id, p.steps)).collect());
        let context = Box::new(ProcessContext {
            state: K_TEMPO_VALID | K_CONT_TIME_VALID,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            ..ProcessContext::default()
        });
        Ok(Core {
            component,
            processor,
            controller,
            separate,
            connection,
            kind,
            infos,
            in_changes: ParamChanges::new(values.len() + EXTRA_QUEUES),
            out_changes: ParamChanges::new(values.len() + EXTRA_QUEUES),
            values,
            in_events: EventList::new(EVENT_CAPACITY),
            out_events: EventList::new(EVENT_CAPACITY),
            inputs: Buses::new(&ins),
            outputs: Buses::new(&outs),
            context,
            has_events,
            midi_map,
            tuning: Box::default(),
            sample_rate: 48_000.0,
            held: 0,
            latency: 0,
            active: false,
            processing: false,
            edits,
            shared,
            _handler: handler,
            _host: host,
            _module: module,
        })
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.stop();
        let mut setup = ProcessSetup {
            process_mode: K_REALTIME,
            symbolic_sample_size: K_SAMPLE32,
            max_samples_per_block: MAX_BLOCK as i32,
            sample_rate: sample_rate as f64,
        };
        self.context.sample_rate = sample_rate as f64;
        if call!(self.processor, IAudioProcessorVtbl, setup_processing, &mut setup) != K_RESULT_OK {
            return;
        }
        self.active = call!(self.component, IComponentVtbl, set_active, 1) == K_RESULT_OK;
        // Valable une fois le composant actif (`setupProcessing` puis `setActive`).
        self.latency =
            if self.active { call!(self.processor, IAudioProcessorVtbl, get_latency_samples) as usize } else { 0 };
    }

    fn stop(&mut self) {
        if self.processing {
            call!(self.processor, IAudioProcessorVtbl, set_processing, 0);
        }
        if self.active {
            call!(self.component, IComponentVtbl, set_active, 0);
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
        let n = p.normalize(value);
        self.in_changes.get().add(id, 0, n);
    }

    /// Événement de note à la position `offset` du prochain bloc.
    fn note(&mut self, on: bool, key: u8, velocity: f32, offset: i32) {
        let key = key.min(127);
        if !self.has_events {
            return;
        }
        let velocity = velocity.clamp(0.0, 1.0);
        // The key is the note's id: note expressions address it.
        let (kind, data) = if on {
            self.tuning.reset(key);
            let note_on = NoteOnEvent { channel: 0, pitch: key as i16, tuning: 0.0, velocity, length: 0, note_id: key as i32 };
            (K_NOTE_ON_EVENT, EventData { note_on })
        } else {
            let note_off = NoteOffEvent { channel: 0, pitch: key as i16, velocity, note_id: key as i32, tuning: 0.0 };
            (K_NOTE_OFF_EVENT, EventData { note_off })
        };
        let e = Event { bus_index: 0, sample_offset: offset, ppq_position: 0.0, flags: K_IS_LIVE, kind, data };
        self.in_events.get().push(e);
        if on {
            self.held |= 1 << key;
        } else {
            self.held &= !(1 << key);
        }
    }

    fn all_notes_off(&mut self) {
        for key in 0..128u8 {
            if self.held & (1 << key) != 0 {
                self.note(false, key, 0.0, 0);
            }
        }
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let key = key.min(127);
        match expr {
            NoteExpression::Tuning { semitones, secs } => {
                if let Some(v) = self.tuning.set(key, semitones, secs, self.sample_rate) {
                    let _ = self.in_events.get().push(expression_event(K_TUNING_TYPE_ID, key, tuning_norm(v), 0));
                }
            }
            NoteExpression::Pan(p) => {
                let v = 0.5 * (p.clamp(-1.0, 1.0) as f64 + 1.0);
                let _ = self.in_events.get().push(expression_event(K_PAN_TYPE_ID, key, v, 0));
            }
        }
    }

    /// Pitch bend and CCs: to the parameter the plugin maps them to, if any.
    fn controller(&mut self, ctl: Controller) {
        let (n, v) = match ctl {
            Controller::PitchBend(b) => (K_PITCH_BEND as usize, 0.5 * (b.clamp(-1.0, 1.0) as f64 + 1.0)),
            Controller::Cc(cc, v) => (usize::from(cc.min(127)), v.clamp(0.0, 1.0) as f64),
        };
        if let Some(id) = self.midi_map[n] {
            self.in_changes.get().add(id, 0, v);
        }
    }

    /// Values of the gliding notes over the next `frames` samples.
    fn ramp_tuning(&mut self, frames: usize) {
        if !self.has_events || !self.tuning.is_busy() {
            return;
        }
        let Self { tuning, in_events, .. } = self;
        let events = in_events.get();
        tuning.advance(frames, RAMP_STEP, |off, key, v| {
            // A full list drops the step: the next one corrects it.
            let _ = events.push(expression_event(K_TUNING_TYPE_ID, key, tuning_norm(v), off as i32));
        });
    }

    /// Un appel `process` sur `frames` échantillons, tampons déjà remplis.
    fn run(&mut self, frames: usize) -> bool {
        if !self.active {
            self.in_events.get().clear();
            return false;
        }
        if !self.processing {
            // `kNotImplemented` est une réponse admise : le plugin traite sans préparation.
            let r = call!(self.processor, IAudioProcessorVtbl, set_processing, 1);
            self.processing = r == K_RESULT_OK || r == K_NOT_IMPLEMENTED;
            if !self.processing {
                return false;
            }
        }
        self.outputs.clear(frames);
        let mut data = ProcessData {
            process_mode: K_REALTIME,
            symbolic_sample_size: K_SAMPLE32,
            num_samples: frames as i32,
            num_inputs: self.inputs.buffers.len() as i32,
            num_outputs: self.outputs.buffers.len() as i32,
            inputs: self.inputs.as_mut_ptr(),
            outputs: self.outputs.as_mut_ptr(),
            input_parameter_changes: self.in_changes.as_ptr(),
            output_parameter_changes: self.out_changes.as_ptr(),
            input_events: if self.has_events { self.in_events.as_ptr() } else { null_mut() },
            output_events: self.out_events.as_ptr(),
            process_context: &mut *self.context,
        };
        let status = call!(self.processor, IAudioProcessorVtbl, process, &mut data);
        self.in_events.get().clear();
        self.out_events.get().clear();
        self.in_changes.get().clear();
        self.context.continous_time_samples += frames as i64;
        self.context.project_time_samples += frames as i64;
        let out = self.out_changes.get();
        for q in out.queues() {
            let Some(n) = q.last() else { continue };
            if let Ok(i) = self.values.binary_search_by_key(&q.id(), |p| p.id) {
                let v = self.values[i].denormalize(n);
                self.values[i].value = v;
            }
            // File pleine : le contrôleur ne verra pas ce changement.
            let _ = self.edits.push((q.id(), n));
        }
        out.clear();
        status == K_RESULT_OK || status == K_RESULT_FALSE
    }
}

/// Normalized tuning of a note expression.
fn tuning_norm(semitones: f32) -> f64 {
    (0.5 + semitones as f64 / 240.0).clamp(0.0, 1.0)
}

fn expression_event(type_id: u32, key: u8, value: f64, offset: i32) -> Event {
    let expression = NoteExpressionValueEvent { type_id, note_id: key as i32, value };
    Event { bus_index: 0, sample_offset: offset, ppq_position: 0.0, flags: K_IS_LIVE, kind: K_NOTE_EXPRESSION_VALUE_EVENT, data: EventData { expression } }
}

impl Drop for Core {
    fn drop(&mut self) {
        // La vue d'abord ; l'éditeur rend ensuite toutes ses références.
        self.shared.detach();
        self.stop();
        if let Some(ctrl) = &self.controller {
            call!(ctrl, IEditControllerVtbl, set_component_handler, null_mut());
        }
        if let Some((a, b)) = self.connection.take() {
            call!(a, IConnectionPointVtbl, disconnect, b.as_ptr());
            call!(b, IConnectionPointVtbl, disconnect, a.as_ptr());
        }
        if let Some(ctrl) = self.controller.take() {
            if self.separate {
                terminate(&ctrl);
            }
        }
        terminate(&self.component);
        // Les références restantes (processeur, composant) sont rendues par les champs.
    }
}

/// Parcourt les fiches VST 3 du contrôleur.
fn for_each_param(ctrl: &Com<IEditController>, mut f: impl FnMut(&ParameterInfo)) {
    for i in 0..call!(ctrl, IEditControllerVtbl, get_parameter_count).max(0) {
        // SAFETY : structure C sans invariant, remplie par le plugin.
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if call!(ctrl, IEditControllerVtbl, get_parameter_info, i, &mut info) == K_RESULT_OK {
            f(&info);
        }
    }
}

/// Texte affiché par le plugin pour une valeur normalisée.
fn value_text(ctrl: &Com<IEditController>, id: ParamId, n: f64) -> String {
    let mut s: String128 = [0; 128];
    if call!(ctrl, IEditControllerVtbl, get_param_string_by_value, id, n, s.as_mut_ptr()) == K_RESULT_OK {
        read_wstr(&s)
    } else {
        String::new()
    }
}

/// Fiches de paramètres et `kind` fuités, réutilisés pour chaque instance du même plugin.
fn shared_infos(kind: String, ctrl: Option<&Com<IEditController>>) -> Leaked {
    let mut leaked = LEAKED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&l) = leaked.iter().find(|(k, _)| *k == kind) {
        return l;
    }
    let mut infos: Vec<ParamInfo> = Vec::new();
    if let Some(ctrl) = ctrl {
        for_each_param(ctrl, |p| {
            if p.flags & (K_IS_HIDDEN | K_IS_READ_ONLY) != 0
                || p.id == K_NO_PARAM_ID
                || infos.iter().any(|i| i.id == p.id)
            {
                return;
            }
            let name = leak(read_wstr(&p.title));
            let def = p.default_normalized_value.clamp(0.0, 1.0);
            let steps = p.step_count.max(0);
            let info = if steps == 0 {
                ParamInfo::new(p.id, name, 0.0, 1.0, def as f32, Unit::Percent)
            } else if p.flags & K_IS_LIST != 0 && steps <= 256 {
                let choices: Vec<&'static str> =
                    (0..=steps).map(|k| leak(value_text(ctrl, p.id, k as f64 / steps as f64))).collect();
                ParamInfo::choice(
                    p.id,
                    name,
                    Box::leak(choices.into_boxed_slice()),
                    (def * steps as f64).round() as usize,
                )
            } else {
                ParamInfo::new(p.id, name, 0.0, steps as f32, (def * steps as f64).round() as f32, Unit::None)
            };
            infos.push(info);
        });
    }
    let l = (leak(kind), &*Box::leak(infos.into_boxed_slice()));
    leaked.push(l);
    l
}

// --- Enveloppes ------------------------------------------------------------

/// Instrument VST 3 (entrée de notes, sortie stéréo ajoutée au rendu).
pub struct Vst3Instrument(Core);

/// Effet VST 3 (traitement stéréo sur place via des tampons de copie).
pub struct Vst3Effect(Core);

impl Vst3Instrument {
    pub(crate) fn new(bundle: &Path, class_id: &str) -> Result<Self, String> {
        Core::new(bundle, class_id, false).map(Vst3Instrument)
    }

    /// Note jouée à la position `offset` (en échantillons) du prochain `render`.
    pub fn note_at(&mut self, offset: u32, note: u8, velocity: f32, on: bool) {
        let offset = (offset as usize).min(MAX_BLOCK - 1) as i32;
        self.0.note(on, note, velocity, offset);
    }
}

impl Vst3Effect {
    pub(crate) fn new(bundle: &Path, class_id: &str) -> Result<Self, String> {
        Core::new(bundle, class_id, true).map(Vst3Effect)
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

impl_params!(Vst3Instrument);
impl_params!(Vst3Effect);

impl Core {
    fn editor(&self) -> Option<Editor> {
        Some(Arc::new(Vst3Editor(self.shared.clone())))
    }

    /// Restores a state on the instance it owns (UI thread, before it goes to
    /// the engine), then reads every parameter value back.
    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        crate::state::load(&self.component, self.controller.as_ref(), data)?;
        let steps: Vec<(ParamId, i32)> = self.values.iter().map(|p| (p.id, p.steps)).collect();
        for (id, v) in crate::state::read_values(self.controller.as_ref(), &steps) {
            if let Some(i) = self.param_index(id) {
                self.values[i].value = v;
            }
        }
        Ok(())
    }
}

impl Instrument for Vst3Instrument {
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
        self.0.note(true, note, velocity, 0);
    }

    fn note_off(&mut self, note: u8) {
        self.0.note(false, note, 0.0, 0);
    }

    fn supports_expression(&self) -> bool {
        self.0.has_events
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        if self.0.has_events {
            self.0.note_expression(key, expr);
        }
    }

    fn controller(&mut self, ctl: Controller) {
        self.0.controller(ctl);
    }

    fn all_notes_off(&mut self) {
        self.0.all_notes_off();
    }

    fn editor(&self) -> Option<Editor> {
        self.0.editor()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.load_state(data)
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let core = &mut self.0;
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            let n = l.len();
            core.inputs.clear(n);
            core.ramp_tuning(n);
            if !core.run(n) {
                return;
            }
            let (a, b) = match core.outputs.main() {
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

impl Effect for Vst3Effect {
    /// A plugin does not say how long it may stay quiet before sound comes
    /// back out: the smart disable gives it a generous margin.
    fn tail(&self) -> f32 {
        ponzik_engine::mixer::PLUGIN_TAIL_SECONDS
    }

    fn kind(&self) -> &'static str {
        self.0.kind
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.0.set_sample_rate(sample_rate);
    }

    /// Lue à l'activation : un `restartComponent(kLatencyChanged)` ultérieur n'est pas suivi.
    fn latency(&self) -> usize {
        self.0.latency
    }

    fn editor(&self) -> Option<Editor> {
        self.0.editor()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.load_state(data)
    }

    /// VST 3 n'a pas de `reset` : on arrête le traitement, relancé au bloc suivant.
    fn reset(&mut self) {
        if self.0.processing {
            call!(self.0.processor, IAudioProcessorVtbl, set_processing, 0);
            self.0.processing = false;
        }
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.0.context.tempo = bpm as f64;
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let core = &mut self.0;
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            let n = l.len();
            core.inputs.clear(n);
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
