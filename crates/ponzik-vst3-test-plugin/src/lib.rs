//! Plugin VST 3 minimal pour les tests de `ponzik-vst3` : un effet « gain »
//! (un paramètre, contrôleur séparé relié par `IConnectionPoint`) et un
//! instrument « sinus » monophonique (composant et contrôleur en un seul objet).
//!
//! Construire avec `cargo build -p ponzik-vst3-test-plugin` avant
//! `cargo test -p ponzik-vst3`.
//!
//! Le module vérifie aussi l'hôte : `GetPluginFactory` échoue avant
//! `ModuleEntry`, une libération en trop ou un objet encore vivant à
//! `ModuleExit` interrompt le processus. Le gain reste muet tant que le
//! contrôleur ne s'est pas annoncé au processeur par un `IMessage`.

use std::ffi::{CStr, c_char, c_void};
use std::mem::offset_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ponzik_vst3::abi::*;

pub const GAIN_CID: Tuid = uid(0x504F4E5A, 0x494B5633, 0x4741494E, 0x00000001);
pub const GAIN_CONTROLLER_CID: Tuid = uid(0x504F4E5A, 0x494B5633, 0x4741494E, 0x00000002);
pub const SINE_CID: Tuid = uid(0x504F4E5A, 0x494B5633, 0x53494E45, 0x00000003);
const GAIN_PARAM: ParamId = 7;
const HELLO: &CStr = c"ponzik.bonjour";

/// Objets vivants (fabrique comprise), vérifiés à la sortie du module.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static ENTERED: AtomicBool = AtomicBool::new(false);

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("ponzik-vst3-test-plugin : {what}");
        std::process::abort();
    }
}

// --- Points d'entrée du module ---------------------------------------------

/// # Safety
/// Appelé par l'hôte après le chargement.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    if !ENTERED.load(Ordering::SeqCst) {
        return null_mut();
    }
    FACTORY_REFS.fetch_add(1, Ordering::SeqCst);
    &FACTORY as *const Factory as *mut IPluginFactory
}

fn enter() -> bool {
    ENTERED.store(true, Ordering::SeqCst);
    true
}

fn exit() -> bool {
    check(FACTORY_REFS.load(Ordering::SeqCst) == 0, "fabrique encore référencée à la sortie du module");
    check(LIVE.load(Ordering::SeqCst) == 0, "objets encore vivants à la sortie du module");
    ENTERED.store(false, Ordering::SeqCst);
    true
}

/// # Safety
/// Appelé par l'hôte avec la poignée `dlopen`.
#[cfg(not(windows))]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ModuleEntry(_: *mut c_void) -> bool {
    enter()
}

/// # Safety
/// Appelé par l'hôte avant `dlclose`.
#[cfg(not(windows))]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ModuleExit() -> bool {
    exit()
}

/// # Safety
/// Appelé par l'hôte après le chargement.
#[cfg(windows)]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn InitDll() -> bool {
    enter()
}

/// # Safety
/// Appelé par l'hôte avant `FreeLibrary`.
#[cfg(windows)]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ExitDll() -> bool {
    exit()
}

// --- Fabrique --------------------------------------------------------------

#[repr(C)]
struct Factory {
    vtbl: &'static IPluginFactory2Vtbl,
}

static FACTORY_REFS: AtomicU32 = AtomicU32::new(0);
static FACTORY: Factory = Factory {
    vtbl: &IPluginFactory2Vtbl {
        base: IPluginFactoryVtbl {
            unknown: FUnknownVtbl { query_interface: f_query, add_ref: f_add_ref, release: f_release },
            get_factory_info: f_info,
            count_classes: f_count,
            get_class_info: f_class_info,
            create_instance: f_create,
        },
        get_class_info2: f_class_info2,
    },
};

unsafe extern "system" fn f_query(this: *mut c_void, iid: *const Tuid, obj: *mut *mut c_void) -> TResult {
    // SAFETY : arguments fournis par l'hôte.
    let iid = unsafe { *iid };
    let ok = [FUNKNOWN_IID, IPLUGIN_FACTORY_IID, IPLUGIN_FACTORY2_IID].contains(&iid);
    unsafe { *obj = if ok { this } else { null_mut() } };
    if ok {
        FACTORY_REFS.fetch_add(1, Ordering::SeqCst);
        K_RESULT_OK
    } else {
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn f_add_ref(_: *mut c_void) -> u32 {
    FACTORY_REFS.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn f_release(_: *mut c_void) -> u32 {
    let prev = FACTORY_REFS.fetch_sub(1, Ordering::SeqCst);
    check(prev > 0, "fabrique libérée en trop");
    prev - 1
}

unsafe extern "system" fn f_info(_: *mut c_void, info: *mut PFactoryInfo) -> TResult {
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    copy_cstr(&mut info.vendor, "Ponzik");
    copy_cstr(&mut info.url, "");
    copy_cstr(&mut info.email, "");
    info.flags = 0;
    K_RESULT_OK
}

struct ClassDef {
    cid: Tuid,
    category: &'static str,
    name: &'static str,
    sub: &'static str,
}

const CLASSES: [ClassDef; 3] = [
    ClassDef { cid: GAIN_CID, category: K_VST_AUDIO_EFFECT_CLASS, name: "Gain de test", sub: "Fx" },
    ClassDef {
        cid: GAIN_CONTROLLER_CID,
        category: K_VST_COMPONENT_CONTROLLER_CLASS,
        name: "Gain de test (contrôleur)",
        sub: "",
    },
    ClassDef { cid: SINE_CID, category: K_VST_AUDIO_EFFECT_CLASS, name: "Sinus de test", sub: "Instrument|Synth" },
];

unsafe extern "system" fn f_count(_: *mut c_void) -> i32 {
    CLASSES.len() as i32
}

unsafe extern "system" fn f_class_info(_: *mut c_void, i: i32, info: *mut PClassInfo) -> TResult {
    let Some(c) = CLASSES.get(i as usize) else { return K_INVALID_ARGUMENT };
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.cid = c.cid;
    info.cardinality = K_MANY_INSTANCES;
    copy_cstr(&mut info.category, c.category);
    copy_cstr(&mut info.name, c.name);
    K_RESULT_OK
}

unsafe extern "system" fn f_class_info2(_: *mut c_void, i: i32, info: *mut PClassInfo2) -> TResult {
    let Some(c) = CLASSES.get(i as usize) else { return K_INVALID_ARGUMENT };
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.cid = c.cid;
    info.cardinality = K_MANY_INSTANCES;
    copy_cstr(&mut info.category, c.category);
    copy_cstr(&mut info.name, c.name);
    info.class_flags = 0;
    copy_cstr(&mut info.sub_categories, c.sub);
    copy_cstr(&mut info.vendor, "Ponzik");
    copy_cstr(&mut info.version, "1.0.0");
    copy_cstr(&mut info.sdk_version, "VST 3.8.1");
    K_RESULT_OK
}

unsafe extern "system" fn f_create(
    _: *mut c_void,
    cid: *const c_char,
    iid: *const c_char,
    obj: *mut *mut c_void,
) -> TResult {
    // SAFETY : identifiants de 16 octets et sortie fournis par l'hôte.
    let (cid, iid) = unsafe { (*(cid as *const Tuid), &*(iid as *const Tuid)) };
    let class = match cid {
        GAIN_CID => Class::Gain,
        GAIN_CONTROLLER_CID => Class::GainController,
        SINE_CID => Class::Sine,
        _ => {
            unsafe { *obj = null_mut() };
            return K_NO_INTERFACE;
        }
    };
    let p = Plugin::new(class);
    // SAFETY : objet neuf ; `queryInterface` prend sa propre référence, on rend la nôtre.
    unsafe {
        let r = query::<0>(p.cast(), iid, obj);
        release::<0>(p.cast());
        r
    }
}

// --- Objet plugin ----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Gain,
    GainController,
    Sine,
}

/// Un objet par instance ; une table par interface, comme l'héritage multiple C++.
#[repr(C)]
struct Plugin {
    component: &'static IComponentVtbl,
    processor: &'static IAudioProcessorVtbl,
    controller: &'static IEditControllerVtbl,
    connection: &'static IConnectionPointVtbl,
    refs: AtomicU32,
    class: Class,
    host: *mut IHostApplication,
    peer: *mut IConnectionPoint,
    greeted: bool,
    sample_rate: f64,
    /// Gain réel du processeur (0..2).
    gain: f64,
    /// Valeur normalisée vue par le contrôleur.
    norm: f64,
    key: i16,
    freq: f64,
    amp: f64,
    phase: f64,
}

const COMPONENT: usize = offset_of!(Plugin, component);
const PROCESSOR: usize = offset_of!(Plugin, processor);
const CONTROLLER: usize = offset_of!(Plugin, controller);
const CONNECTION: usize = offset_of!(Plugin, connection);

const fn unknown<const OFF: usize>() -> FUnknownVtbl {
    FUnknownVtbl { query_interface: query::<OFF>, add_ref: add_ref::<OFF>, release: release::<OFF> }
}

static COMPONENT_VTBL: IComponentVtbl = IComponentVtbl {
    base: IPluginBaseVtbl {
        unknown: unknown::<COMPONENT>(),
        initialize: initialize::<COMPONENT>,
        terminate: terminate::<COMPONENT>,
    },
    get_controller_class_id,
    set_io_mode,
    get_bus_count,
    get_bus_info,
    get_routing_info,
    activate_bus,
    set_active,
    set_state: component_set_state,
    get_state: component_get_state,
};

static PROCESSOR_VTBL: IAudioProcessorVtbl = IAudioProcessorVtbl {
    unknown: unknown::<PROCESSOR>(),
    set_bus_arrangements,
    get_bus_arrangement,
    can_process_sample_size,
    get_latency_samples: latency_samples,
    setup_processing,
    set_processing,
    process,
    get_tail_samples: zero,
};

static CONTROLLER_VTBL: IEditControllerVtbl = IEditControllerVtbl {
    base: IPluginBaseVtbl {
        unknown: unknown::<CONTROLLER>(),
        initialize: initialize::<CONTROLLER>,
        terminate: terminate::<CONTROLLER>,
    },
    set_component_state,
    set_state: stream_ok,
    get_state: stream_ok,
    get_parameter_count,
    get_parameter_info,
    get_param_string_by_value,
    get_param_value_by_string,
    normalized_param_to_plain,
    plain_param_to_normalized,
    get_param_normalized,
    set_param_normalized,
    set_component_handler,
    create_view,
};

static CONNECTION_VTBL: IConnectionPointVtbl =
    IConnectionPointVtbl { unknown: unknown::<CONNECTION>(), connect, disconnect, notify };

impl Plugin {
    fn new(class: Class) -> *mut Plugin {
        LIVE.fetch_add(1, Ordering::SeqCst);
        Box::into_raw(Box::new(Plugin {
            component: &COMPONENT_VTBL,
            processor: &PROCESSOR_VTBL,
            controller: &CONTROLLER_VTBL,
            connection: &CONNECTION_VTBL,
            refs: AtomicU32::new(1),
            class,
            host: null_mut(),
            peer: null_mut(),
            greeted: false,
            sample_rate: 44_100.0,
            gain: 1.0,
            // Volontairement faux tant que l'hôte n'a pas transmis l'état du composant.
            norm: 0.0,
            key: -1,
            freq: 0.0,
            amp: 0.0,
            phase: 0.0,
        }))
    }

    /// Décalages des interfaces exposées ; la première sert d'identité (FUnknown).
    fn interfaces(&self) -> &'static [(Tuid, usize)] {
        match self.class {
            Class::Gain => &[
                (IPLUGIN_BASE_IID, COMPONENT),
                (ICOMPONENT_IID, COMPONENT),
                (IAUDIO_PROCESSOR_IID, PROCESSOR),
                (ICONNECTION_POINT_IID, CONNECTION),
            ],
            Class::GainController => &[
                (IPLUGIN_BASE_IID, CONTROLLER),
                (IEDIT_CONTROLLER_IID, CONTROLLER),
                (ICONNECTION_POINT_IID, CONNECTION),
            ],
            Class::Sine => &[
                (IPLUGIN_BASE_IID, COMPONENT),
                (ICOMPONENT_IID, COMPONENT),
                (IAUDIO_PROCESSOR_IID, PROCESSOR),
                (IEDIT_CONTROLLER_IID, CONTROLLER),
            ],
        }
    }

    fn release_links(&mut self) {
        for p in
            [std::mem::replace(&mut self.host, null_mut()).cast(), std::mem::replace(&mut self.peer, null_mut()).cast()]
        {
            unref(p);
        }
    }
}

/// `release` sur une interface quelconque (ignore le pointeur nul).
fn unref(p: *mut c_void) {
    if !p.is_null() {
        // SAFETY : référence possédée sur un objet COM vivant.
        unsafe { ((**(p as *const *const FUnknownVtbl)).release)(p) };
    }
}

fn me<'a, const OFF: usize>(this: *mut c_void) -> &'a mut Plugin {
    // SAFETY : `this` désigne le champ `OFF` d'un `Plugin` vivant ; l'hôte
    // n'appelle qu'un thread à la fois.
    unsafe { &mut *((this as *mut u8).sub(OFF) as *mut Plugin) }
}

unsafe extern "system" fn query<const OFF: usize>(
    this: *mut c_void,
    iid: *const Tuid,
    obj: *mut *mut c_void,
) -> TResult {
    let p = me::<OFF>(this);
    // SAFETY : identifiant fourni par l'hôte.
    let iid = unsafe { *iid };
    let ifaces = p.interfaces();
    let found = if iid == FUNKNOWN_IID { Some(ifaces[0].1) } else { ifaces.iter().find(|i| i.0 == iid).map(|i| i.1) };
    let base = p as *mut Plugin as *mut u8;
    // SAFETY : sortie fournie par l'hôte ; décalage d'un champ de `Plugin`.
    unsafe { *obj = found.map_or(null_mut(), |off| base.add(off).cast()) };
    if found.is_some() {
        p.refs.fetch_add(1, Ordering::SeqCst);
        K_RESULT_OK
    } else {
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn add_ref<const OFF: usize>(this: *mut c_void) -> u32 {
    me::<OFF>(this).refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release<const OFF: usize>(this: *mut c_void) -> u32 {
    let p = me::<OFF>(this);
    let prev = p.refs.fetch_sub(1, Ordering::SeqCst);
    check(prev > 0, "objet libéré en trop");
    if prev == 1 {
        p.release_links();
        // SAFETY : dernière référence d'un objet de `Plugin::new`.
        drop(unsafe { Box::from_raw(p as *mut Plugin) });
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
    prev - 1
}

unsafe extern "system" fn initialize<const OFF: usize>(this: *mut c_void, context: *mut FUnknown) -> TResult {
    let p = me::<OFF>(this);
    if context.is_null() || !p.host.is_null() {
        return K_RESULT_FALSE;
    }
    let mut host = null_mut();
    // SAFETY : contexte fourni par l'hôte.
    let r = unsafe { ((*(*context).vtbl).query_interface)(context.cast(), &IHOST_APPLICATION_IID, &mut host) };
    if r != K_RESULT_OK {
        return K_RESULT_FALSE;
    }
    p.host = host.cast();
    K_RESULT_OK
}

unsafe extern "system" fn terminate<const OFF: usize>(this: *mut c_void) -> TResult {
    me::<OFF>(this).release_links();
    K_RESULT_OK
}

// --- IComponent ------------------------------------------------------------

fn component<'a>(this: *mut c_void) -> &'a mut Plugin {
    me::<COMPONENT>(this)
}

unsafe extern "system" fn get_controller_class_id(this: *mut c_void, cid: *mut Tuid) -> TResult {
    if component(this).class != Class::Gain {
        return K_RESULT_FALSE;
    }
    // SAFETY : sortie fournie par l'hôte.
    unsafe { *cid = GAIN_CONTROLLER_CID };
    K_RESULT_OK
}

unsafe extern "system" fn set_io_mode(_: *mut c_void, _: i32) -> TResult {
    K_RESULT_OK
}

/// Bus : (audio entrées, audio sorties, événements entrées).
fn buses(p: &Plugin) -> (i32, i32, i32) {
    if p.class == Class::Sine { (0, 1, 1) } else { (1, 1, 0) }
}

unsafe extern "system" fn get_bus_count(this: *mut c_void, kind: i32, dir: i32) -> i32 {
    let (ai, ao, ei) = buses(component(this));
    match (kind, dir) {
        (K_AUDIO, K_INPUT) => ai,
        (K_AUDIO, K_OUTPUT) => ao,
        (K_EVENT, K_INPUT) => ei,
        _ => 0,
    }
}

unsafe extern "system" fn get_bus_info(this: *mut c_void, kind: i32, dir: i32, i: i32, bus: *mut BusInfo) -> TResult {
    if i != 0 || unsafe { get_bus_count(this, kind, dir) } == 0 {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : structure fournie par l'hôte.
    let bus = unsafe { &mut *bus };
    bus.media_type = kind;
    bus.direction = dir;
    bus.channel_count = if kind == K_AUDIO { 2 } else { 16 };
    copy_wstr(&mut bus.name, if kind == K_AUDIO { "Principal" } else { "Notes" });
    bus.bus_type = K_MAIN;
    bus.flags = K_DEFAULT_ACTIVE;
    K_RESULT_OK
}

unsafe extern "system" fn get_routing_info(_: *mut c_void, _: *mut RoutingInfo, _: *mut RoutingInfo) -> TResult {
    K_NOT_IMPLEMENTED
}

unsafe extern "system" fn activate_bus(this: *mut c_void, kind: i32, dir: i32, i: i32, _: TBool) -> TResult {
    if i == 0 && unsafe { get_bus_count(this, kind, dir) } > 0 { K_RESULT_OK } else { K_INVALID_ARGUMENT }
}

unsafe extern "system" fn set_active(this: *mut c_void, state: TBool) -> TResult {
    let p = component(this);
    if state == 0 {
        (p.amp, p.phase, p.key) = (0.0, 0.0, -1);
    }
    K_RESULT_OK
}

fn read_f64(stream: *mut IBStream) -> Option<f64> {
    let mut b = [0u8; 8];
    let mut n = 0;
    // SAFETY : flux fourni par l'hôte.
    let r = unsafe { ((*(*stream).vtbl).read)(stream.cast(), b.as_mut_ptr().cast(), 8, &mut n) };
    (r == K_RESULT_OK && n == 8).then(|| f64::from_le_bytes(b))
}

unsafe extern "system" fn component_set_state(this: *mut c_void, stream: *mut IBStream) -> TResult {
    match read_f64(stream) {
        Some(g) => {
            component(this).gain = g;
            K_RESULT_OK
        }
        None => K_RESULT_FALSE,
    }
}

unsafe extern "system" fn component_get_state(this: *mut c_void, stream: *mut IBStream) -> TResult {
    let mut b = component(this).gain.to_le_bytes();
    let mut n = 0;
    // SAFETY : flux fourni par l'hôte.
    unsafe { ((*(*stream).vtbl).write)(stream.cast(), b.as_mut_ptr().cast(), 8, &mut n) }
}

// --- IAudioProcessor -------------------------------------------------------

fn processor<'a>(this: *mut c_void) -> &'a mut Plugin {
    me::<PROCESSOR>(this)
}

unsafe extern "system" fn set_bus_arrangements(
    this: *mut c_void,
    ins: *mut SpeakerArrangement,
    n_ins: i32,
    outs: *mut SpeakerArrangement,
    n_outs: i32,
) -> TResult {
    let (ai, ao, _) = buses(processor(this));
    let all_stereo = |p: *mut SpeakerArrangement, n: i32| {
        // SAFETY : tableau de `n` dispositions fourni par l'hôte.
        (0..n as usize).all(|i| unsafe { *p.add(i) } == K_STEREO)
    };
    if n_ins == ai && n_outs == ao && all_stereo(ins, n_ins) && all_stereo(outs, n_outs) {
        K_RESULT_OK
    } else {
        K_RESULT_FALSE
    }
}

unsafe extern "system" fn get_bus_arrangement(_: *mut c_void, _: i32, i: i32, arr: *mut SpeakerArrangement) -> TResult {
    if i != 0 {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : sortie fournie par l'hôte.
    unsafe { *arr = K_STEREO };
    K_RESULT_OK
}

unsafe extern "system" fn can_process_sample_size(_: *mut c_void, size: i32) -> TResult {
    if size == K_SAMPLE32 { K_RESULT_OK } else { K_RESULT_FALSE }
}

unsafe extern "system" fn zero(_: *mut c_void) -> u32 {
    0
}

/// Latence annoncée : seulement déclarée (le signal n'est pas retardé), pour
/// vérifier que l'hôte la lit.
pub const LATENCY: u32 = 32;

unsafe extern "system" fn latency_samples(_: *mut c_void) -> u32 {
    LATENCY
}

unsafe extern "system" fn setup_processing(this: *mut c_void, setup: *mut ProcessSetup) -> TResult {
    // SAFETY : structure fournie par l'hôte.
    processor(this).sample_rate = unsafe { (*setup).sample_rate };
    K_RESULT_OK
}

unsafe extern "system" fn set_processing(_: *mut c_void, _: TBool) -> TResult {
    K_RESULT_OK
}

/// Dernière valeur du gain transmise dans le bloc.
fn read_changes(p: &mut Plugin, changes: *mut IParameterChanges) {
    if changes.is_null() {
        return;
    }
    // SAFETY : liste et files fournies par l'hôte pendant `process`.
    unsafe {
        let vt = &*(*changes).vtbl;
        for i in 0..(vt.get_parameter_count)(changes.cast()) {
            let q = (vt.get_parameter_data)(changes.cast(), i);
            if q.is_null() {
                continue;
            }
            let qv = &*(*q).vtbl;
            let n = (qv.get_point_count)(q.cast());
            let (mut offset, mut value) = (0, 0.0);
            if (qv.get_parameter_id)(q.cast()) == GAIN_PARAM
                && n > 0
                && (qv.get_point)(q.cast(), n - 1, &mut offset, &mut value) == K_RESULT_OK
            {
                p.gain = 2.0 * value;
            }
        }
    }
}

fn apply_event(p: &mut Plugin, e: &Event) {
    // SAFETY : le type annonce la variante de l'union.
    unsafe {
        match e.kind {
            K_NOTE_ON_EVENT => {
                let n = e.data.note_on;
                p.key = n.pitch;
                p.freq = 440.0 * 2f64.powf((n.pitch as f64 - 69.0) / 12.0);
                p.amp = n.velocity as f64;
            }
            K_NOTE_OFF_EVENT if e.data.note_off.pitch == p.key => p.amp = 0.0,
            _ => {}
        }
    }
}

unsafe extern "system" fn process(this: *mut c_void, data: *mut ProcessData) -> TResult {
    let p = processor(this);
    // SAFETY : structure et tampons fournis par l'hôte pour la durée de l'appel.
    let d = unsafe { &mut *data };
    read_changes(p, d.input_parameter_changes);
    let n = d.num_samples.max(0) as usize;
    if n == 0 || d.num_outputs < 1 {
        return K_RESULT_OK;
    }
    let out = unsafe { &*d.outputs };
    let outs = unsafe { std::slice::from_raw_parts(out.channel_buffers32, out.num_channels as usize) };
    if p.class == Class::Sine {
        let events = d.input_events;
        let count = if events.is_null() { 0 } else { unsafe { ((*(*events).vtbl).get_event_count)(events.cast()) } };
        let mut next = 0;
        let mut e: Event = unsafe { std::mem::zeroed() };
        let step = std::f64::consts::TAU * p.freq / p.sample_rate;
        let mut step = step;
        for i in 0..n {
            while next < count {
                if unsafe { ((*(*events).vtbl).get_event)(events.cast(), next, &mut e) } != K_RESULT_OK
                    || e.sample_offset as usize > i
                {
                    break;
                }
                apply_event(p, &e);
                step = std::f64::consts::TAU * p.freq / p.sample_rate;
                next += 1;
            }
            let s = (p.amp * p.phase.sin()) as f32;
            p.phase = (p.phase + step) % std::f64::consts::TAU;
            outs.iter().for_each(|&c| unsafe { *c.add(i) = s });
        }
    } else {
        let input = unsafe { &*d.inputs };
        let ins = unsafe { std::slice::from_raw_parts(input.channel_buffers32, input.num_channels as usize) };
        let gain = if p.greeted { p.gain as f32 } else { 0.0 };
        for (&o, &i) in outs.iter().zip(ins) {
            for k in 0..n {
                unsafe { *o.add(k) = *i.add(k) * gain };
            }
        }
    }
    K_RESULT_OK
}

// --- IEditController -------------------------------------------------------

fn controller<'a>(this: *mut c_void) -> &'a mut Plugin {
    me::<CONTROLLER>(this)
}

unsafe extern "system" fn set_component_state(this: *mut c_void, stream: *mut IBStream) -> TResult {
    let p = controller(this);
    if p.class == Class::GainController
        && let Some(g) = read_f64(stream)
    {
        p.norm = g / 2.0;
    }
    K_RESULT_OK
}

unsafe extern "system" fn stream_ok(_: *mut c_void, _: *mut IBStream) -> TResult {
    K_RESULT_OK
}

unsafe extern "system" fn get_parameter_count(this: *mut c_void) -> i32 {
    (controller(this).class == Class::GainController) as i32
}

unsafe extern "system" fn get_parameter_info(this: *mut c_void, i: i32, info: *mut ParameterInfo) -> TResult {
    if i != 0 || controller(this).class != Class::GainController {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.id = GAIN_PARAM;
    copy_wstr(&mut info.title, "Gain");
    copy_wstr(&mut info.short_title, "Gain");
    copy_wstr(&mut info.units, "");
    info.step_count = 0;
    info.default_normalized_value = 0.5;
    info.unit_id = 0;
    info.flags = K_CAN_AUTOMATE;
    K_RESULT_OK
}

unsafe extern "system" fn get_param_string_by_value(
    _: *mut c_void,
    id: ParamId,
    v: ParamValue,
    out: *mut u16,
) -> TResult {
    if id != GAIN_PARAM {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : `String128` fourni par l'hôte.
    copy_wstr(unsafe { std::slice::from_raw_parts_mut(out, 128) }, &format!("{:.2}", 2.0 * v));
    K_RESULT_OK
}

unsafe extern "system" fn get_param_value_by_string(_: *mut c_void, _: ParamId, _: *const u16, _: *mut f64) -> TResult {
    K_RESULT_FALSE
}

unsafe extern "system" fn normalized_param_to_plain(_: *mut c_void, _: ParamId, v: ParamValue) -> ParamValue {
    2.0 * v
}

unsafe extern "system" fn plain_param_to_normalized(_: *mut c_void, _: ParamId, v: ParamValue) -> ParamValue {
    v / 2.0
}

unsafe extern "system" fn get_param_normalized(this: *mut c_void, id: ParamId) -> ParamValue {
    if id == GAIN_PARAM { controller(this).norm } else { 0.0 }
}

unsafe extern "system" fn set_param_normalized(this: *mut c_void, id: ParamId, v: ParamValue) -> TResult {
    if id != GAIN_PARAM {
        return K_INVALID_ARGUMENT;
    }
    controller(this).norm = v;
    K_RESULT_OK
}

unsafe extern "system" fn set_component_handler(_: *mut c_void, _: *mut IComponentHandler) -> TResult {
    K_RESULT_OK
}

unsafe extern "system" fn create_view(_: *mut c_void, _: *const c_char) -> *mut c_void {
    null_mut()
}

// --- IConnectionPoint ------------------------------------------------------

fn connection<'a>(this: *mut c_void) -> &'a mut Plugin {
    me::<CONNECTION>(this)
}

unsafe extern "system" fn connect(this: *mut c_void, other: *mut IConnectionPoint) -> TResult {
    let p = connection(this);
    if other.is_null() || !p.peer.is_null() {
        return K_RESULT_FALSE;
    }
    // SAFETY : interface fournie par l'hôte ; on garde une référence.
    unsafe { ((*(*other).vtbl).unknown.add_ref)(other.cast()) };
    p.peer = other;
    if p.class == Class::GainController {
        greet(p);
    }
    K_RESULT_OK
}

/// Annonce le contrôleur au processeur par un message alloué via l'hôte.
fn greet(p: &mut Plugin) {
    if p.host.is_null() {
        return;
    }
    let mut msg: *mut IMessage = null_mut();
    // SAFETY : hôte et pair retenus ; message possédé puis rendu.
    unsafe {
        let host = p.host;
        let r = ((*(*host).vtbl).create_instance)(
            host.cast(),
            &IMESSAGE_IID,
            &IMESSAGE_IID,
            &mut msg as *mut *mut IMessage as *mut *mut c_void,
        );
        if r != K_RESULT_OK || msg.is_null() {
            return;
        }
        let mv = &*(*msg).vtbl;
        (mv.set_message_id)(msg.cast(), HELLO.as_ptr());
        let attrs = (mv.get_attributes)(msg.cast());
        if !attrs.is_null() {
            ((*(*attrs).vtbl).set_float)(attrs.cast(), c"gain".as_ptr(), p.norm * 2.0);
        }
        ((*(*p.peer).vtbl).notify)(p.peer.cast(), msg);
        (mv.unknown.release)(msg.cast());
    }
}

unsafe extern "system" fn disconnect(this: *mut c_void, other: *mut IConnectionPoint) -> TResult {
    let p = connection(this);
    if other.is_null() || other != p.peer {
        return K_RESULT_FALSE;
    }
    unref(std::mem::replace(&mut p.peer, null_mut()).cast());
    K_RESULT_OK
}

unsafe extern "system" fn notify(this: *mut c_void, msg: *mut IMessage) -> TResult {
    let p = connection(this);
    if msg.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : message fourni par le pair pendant l'appel.
    unsafe {
        let mv = &*(*msg).vtbl;
        let id = (mv.get_message_id)(msg.cast());
        if id.is_null() || CStr::from_ptr(id) != HELLO {
            return K_RESULT_FALSE;
        }
        let attrs = (mv.get_attributes)(msg.cast());
        let mut gain = 0.0;
        if !attrs.is_null() && ((*(*attrs).vtbl).get_float)(attrs.cast(), c"gain".as_ptr(), &mut gain) == K_RESULT_OK {
            p.greeted = true;
        }
    }
    K_RESULT_OK
}
