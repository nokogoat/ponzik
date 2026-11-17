//! Plugin CLAP minimal pour les tests de `ponzik-clap` : un effet « gain »
//! (un paramètre) et un instrument « sinus » monophonique.
//!
//! Le gain a une interface (`clap.gui`) réduite à sa comptabilité : taille,
//! échelle, parent, visibilité. Créée, elle arme une minuterie
//! (`clap.timer-support`) dont le premier tick simule une édition du gain à
//! 0,25 (gestes et valeur sortent au `process` suivant), et, sous Unix, un tube
//! surveillé (`clap.posix-fd-support`) dont la lecture demande à l'hôte un
//! redimensionnement à 420 × 260 et de relire les valeurs (`rescan`).
//!
//! State: the gain saves (`clap.state-context`) its value, a hidden count of
//! processed blocks and the context it was saved for; the sine (plain
//! `clap.state`) saves the last key it played. Neither is a parameter: only a
//! state round trip brings them back.
//!
//! Construire avec `cargo build -p ponzik-clap-test-plugin` avant
//! `cargo test -p ponzik-clap`.

use std::ffi::{CStr, c_char, c_void};
use std::ptr::null;

use ponzik_clap::abi::*;

/// Enveloppe rendant partageables les statiques contenant des pointeurs bruts.
#[repr(transparent)]
struct Shared<T>(T);
// SAFETY : données constantes, jamais modifiées.
unsafe impl<T> Sync for Shared<T> {}

pub const GAIN_ID: &CStr = c"ponzik.test.gain";
pub const SINE_ID: &CStr = c"ponzik.test.sine";
const GAIN_PARAM: ClapId = 7;

static GAIN_FEATURES: Shared<[*const c_char; 2]> = Shared([c"audio-effect".as_ptr(), null()]);
static SINE_FEATURES: Shared<[*const c_char; 3]> = Shared([c"instrument".as_ptr(), c"synthesizer".as_ptr(), null()]);

const fn descriptor(id: &CStr, name: &CStr, features: *const *const c_char) -> ClapPluginDescriptor {
    let empty = c"".as_ptr();
    ClapPluginDescriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"Ponzik".as_ptr(),
        url: empty,
        manual_url: empty,
        support_url: empty,
        version: c"1.0".as_ptr(),
        description: empty,
        features,
    }
}

static GAIN_DESC: Shared<ClapPluginDescriptor> = Shared(descriptor(GAIN_ID, c"Gain de test", GAIN_FEATURES.0.as_ptr()));
static SINE_DESC: Shared<ClapPluginDescriptor> =
    Shared(descriptor(SINE_ID, c"Sinus de test", SINE_FEATURES.0.as_ptr()));

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static clap_entry: ClapPluginEntry = ClapPluginEntry { clap_version: CLAP_VERSION, init, deinit, get_factory };

/// With `PONZIK_TEST_PLUGIN_CRASH` set, loading the plugin crashes the
/// process: the host's scan must survive it.
unsafe extern "C" fn init(_: *const c_char) -> bool {
    if std::env::var_os("PONZIK_TEST_PLUGIN_CRASH").is_some() {
        std::process::abort();
    }
    true
}

unsafe extern "C" fn deinit() {}

static FACTORY: ClapPluginFactory = ClapPluginFactory { get_plugin_count, get_plugin_descriptor, create_plugin };

unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    // SAFETY : chaîne C fournie par l'hôte.
    if unsafe { CStr::from_ptr(id) }.to_bytes_with_nul() == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const ClapPluginFactory as *const c_void
    } else {
        null()
    }
}

unsafe extern "C" fn get_plugin_count(_: *const ClapPluginFactory) -> u32 {
    2
}

unsafe extern "C" fn get_plugin_descriptor(_: *const ClapPluginFactory, i: u32) -> *const ClapPluginDescriptor {
    match i {
        0 => &GAIN_DESC.0,
        1 => &SINE_DESC.0,
        _ => null(),
    }
}

/// Instance : `clap` en tête pour convertir `*const ClapPlugin` en `*mut Test`.
#[repr(C)]
struct Test {
    clap: ClapPlugin,
    instrument: bool,
    sample_rate: f64,
    gain: f64,
    key: i16,
    freq: f64,
    amp: f64,
    phase: f64,
    /// Hidden state: blocks processed (gain), last key played (sine).
    blocks: u32,
    last_key: i16,
    host: *const ClapHost,
    gui: Gui,
}

/// Interface de test : rien n'est dessiné, tout est noté.
#[derive(Default)]
struct Gui {
    created: bool,
    shown: bool,
    scale: f64,
    size: (u32, u32),
    parent: usize,
    timer: Option<ClapId>,
    ticks: u32,
    /// Tube (lecture, écriture) surveillé par l'hôte.
    #[cfg_attr(not(unix), allow(dead_code))]
    pipe: Option<(i32, i32)>,
    /// Édition simulée, sortie au prochain `process`.
    edit: Option<f64>,
}

/// Valeur que l'interface de test donne au gain au premier tick.
pub const GUI_EDIT: f64 = 0.25;
/// Taille demandée à l'hôte quand le tube devient lisible.
pub const GUI_REQUEST: (u32, u32) = (420, 260);
/// Taille de l'interface à l'échelle 1.
pub const GUI_SIZE: (u32, u32) = (300, 200);

unsafe extern "C" fn create_plugin(
    _: *const ClapPluginFactory,
    host: *const ClapHost,
    id: *const c_char,
) -> *const ClapPlugin {
    // SAFETY : chaîne C fournie par l'hôte.
    let id = unsafe { CStr::from_ptr(id) };
    let (desc, instrument) = match id {
        id if id == GAIN_ID => (&GAIN_DESC.0, false),
        id if id == SINE_ID => (&SINE_DESC.0, true),
        _ => return null(),
    };
    let t = Box::new(Test {
        clap: ClapPlugin {
            desc,
            plugin_data: std::ptr::null_mut(),
            init: p_init,
            destroy,
            activate,
            deactivate: p_noop,
            start_processing: p_init,
            stop_processing: p_noop,
            reset: p_noop,
            process,
            get_extension,
            on_main_thread: p_noop,
        },
        instrument,
        sample_rate: 44_100.0,
        gain: 1.0,
        key: -1,
        freq: 0.0,
        amp: 0.0,
        phase: 0.0,
        blocks: 0,
        last_key: -1,
        host,
        gui: Gui::default(),
    });
    Box::into_raw(t) as *const ClapPlugin
}

fn me<'a>(p: *const ClapPlugin) -> &'a mut Test {
    // SAFETY : `p` vient de `create_plugin`, l'hôte ne l'utilise que d'un thread à la fois.
    unsafe { &mut *(p as *mut Test) }
}

unsafe extern "C" fn p_init(_: *const ClapPlugin) -> bool {
    true
}

unsafe extern "C" fn p_noop(_: *const ClapPlugin) {}

unsafe extern "C" fn destroy(p: *const ClapPlugin) {
    // Un hôte correct a détruit l'interface avant le plugin.
    if me(p).gui.created {
        eprintln!("ponzik-clap-test-plugin : plugin détruit avant son interface");
        std::process::abort();
    }
    // SAFETY : allocation de `create_plugin`.
    drop(unsafe { Box::from_raw(p as *mut Test) });
}

unsafe extern "C" fn activate(p: *const ClapPlugin, sample_rate: f64, _: u32, _: u32) -> bool {
    me(p).sample_rate = sample_rate;
    true
}

fn handle_events(t: &mut Test, events: *const ClapInputEvents) {
    // SAFETY : liste valide pendant l'appel ; le type annoncé garantit la structure.
    let list = unsafe { &*events };
    for i in 0..unsafe { (list.size)(list) } {
        let h = unsafe { (list.get)(list, i) };
        match unsafe { (*h).kind } {
            CLAP_EVENT_PARAM_VALUE => {
                let e = unsafe { &*(h as *const ClapEventParamValue) };
                if e.param_id == GAIN_PARAM {
                    t.gain = e.value;
                }
            }
            CLAP_EVENT_NOTE_ON => {
                let e = unsafe { &*(h as *const ClapEventNote) };
                t.key = e.key;
                t.last_key = e.key;
                t.freq = 440.0 * 2f64.powf((e.key as f64 - 69.0) / 12.0);
                t.amp = e.velocity;
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                let e = unsafe { &*(h as *const ClapEventNoteExpression) };
                if e.expression_id == CLAP_NOTE_EXPRESSION_TUNING && (e.key == -1 || e.key == t.key) {
                    t.freq = 440.0 * 2f64.powf((t.key as f64 + e.value - 69.0) / 12.0);
                }
            }
            CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                let e = unsafe { &*(h as *const ClapEventNote) };
                if e.key == -1 || e.key == t.key {
                    t.amp = 0.0;
                }
            }
            _ => {}
        }
    }
}

unsafe extern "C" fn process(p: *const ClapPlugin, process: *const ClapProcess) -> ClapProcessStatus {
    let t = me(p);
    // SAFETY : structure et tampons fournis par l'hôte pour la durée de l'appel.
    let pr = unsafe { &*process };
    handle_events(t, pr.in_events);
    t.blocks += 1;
    let n = pr.frames_count as usize;
    if let Some(v) = t.gui.edit.take() {
        t.gain = v;
        push_edit(pr.out_events, v);
    }
    let out = unsafe { &*pr.audio_outputs };
    let outs = unsafe { std::slice::from_raw_parts(out.data32, out.channel_count as usize) };
    if t.instrument {
        let step = std::f64::consts::TAU * t.freq / t.sample_rate;
        for i in 0..n {
            let s = (t.amp * t.phase.sin()) as f32;
            t.phase = (t.phase + step) % std::f64::consts::TAU;
            outs.iter().for_each(|&c| unsafe { *c.add(i) = s });
        }
    } else {
        let input = unsafe { &*pr.audio_inputs };
        let ins = unsafe { std::slice::from_raw_parts(input.data32, input.channel_count as usize) };
        for (&o, &i) in outs.iter().zip(ins) {
            for k in 0..n {
                unsafe { *o.add(k) = *i.add(k) * t.gain as f32 };
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

// --- Extensions ------------------------------------------------------------

unsafe extern "C" fn get_extension(p: *const ClapPlugin, id: *const c_char) -> *const c_void {
    // SAFETY : chaîne C fournie par l'hôte.
    let id = unsafe { CStr::from_ptr(id) }.to_bytes_with_nul();
    let instrument = me(p).instrument;
    if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const ClapPluginAudioPorts as *const c_void
    } else if id == CLAP_EXT_PARAMS && !instrument {
        &PARAMS as *const ClapPluginParams as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS && instrument {
        &NOTE_PORTS as *const ClapPluginNotePorts as *const c_void
    } else if id == CLAP_EXT_GUI && !instrument {
        &GUI as *const ClapPluginGui as *const c_void
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        &TIMER as *const ClapPluginTimerSupport as *const c_void
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        &FD as *const ClapPluginPosixFdSupport as *const c_void
    } else if id == CLAP_EXT_LATENCY && !instrument {
        &LATENCY as *const ClapPluginLatency as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const ClapPluginState as *const c_void
    } else if id == CLAP_EXT_STATE_CONTEXT && !instrument {
        &STATE_CONTEXT as *const ClapPluginStateContext as *const c_void
    } else {
        null()
    }
}

// --- State -----------------------------------------------------------------

/// Gain state: `PZTG`, gain (f64), blocks (u32), context (u32), little-endian.
pub const GAIN_STATE_MAGIC: &[u8; 4] = b"PZTG";
/// Size of the gain state.
pub const GAIN_STATE_LEN: usize = 4 + 8 + 4 + 4;

static STATE: ClapPluginState = ClapPluginState { save: state_save, load: state_load };
static STATE_CONTEXT: ClapPluginStateContext = ClapPluginStateContext { save: context_save, load: context_load };

fn write_all(out: *const ClapOStream, mut data: &[u8]) -> bool {
    // Small writes on purpose: the host must append, not overwrite.
    while !data.is_empty() {
        let n = data.len().min(5);
        // SAFETY: stream provided by the host for the call.
        let w = unsafe { ((*out).write)(out, data.as_ptr().cast(), n as u64) };
        if w <= 0 {
            return false;
        }
        data = &data[w as usize..];
    }
    true
}

fn read_all(input: *const ClapIStream) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    let mut buf = [0u8; 3];
    loop {
        // SAFETY: stream provided by the host for the call.
        let n = unsafe { ((*input).read)(input, buf.as_mut_ptr().cast(), buf.len() as u64) };
        match n {
            0 => return Some(v),
            n if n < 0 => return None,
            n => v.extend_from_slice(&buf[..n as usize]),
        }
    }
}

fn save_with(p: *const ClapPlugin, out: *const ClapOStream, context: u32) -> bool {
    let t = me(p);
    if t.instrument {
        return write_all(out, &t.last_key.to_le_bytes());
    }
    let mut b = Vec::with_capacity(GAIN_STATE_LEN);
    b.extend_from_slice(GAIN_STATE_MAGIC);
    b.extend_from_slice(&t.gain.to_le_bytes());
    b.extend_from_slice(&t.blocks.to_le_bytes());
    b.extend_from_slice(&context.to_le_bytes());
    write_all(out, &b)
}

fn load_with(p: *const ClapPlugin, input: *const ClapIStream) -> bool {
    let t = me(p);
    let Some(b) = read_all(input) else { return false };
    if t.instrument {
        let Ok(k) = <[u8; 2]>::try_from(b.as_slice()) else { return false };
        t.last_key = i16::from_le_bytes(k);
        return true;
    }
    if b.len() != GAIN_STATE_LEN || &b[..4] != GAIN_STATE_MAGIC {
        return false;
    }
    t.gain = f64::from_le_bytes(b[4..12].try_into().unwrap());
    t.blocks = u32::from_le_bytes(b[12..16].try_into().unwrap());
    true
}

unsafe extern "C" fn state_save(p: *const ClapPlugin, out: *const ClapOStream) -> bool {
    save_with(p, out, 0)
}

unsafe extern "C" fn state_load(p: *const ClapPlugin, input: *const ClapIStream) -> bool {
    load_with(p, input)
}

unsafe extern "C" fn context_save(p: *const ClapPlugin, out: *const ClapOStream, context: u32) -> bool {
    save_with(p, out, context)
}

unsafe extern "C" fn context_load(p: *const ClapPlugin, input: *const ClapIStream, _: u32) -> bool {
    load_with(p, input)
}

/// Latence annoncée par le gain : seulement déclarée (le signal n'est pas
/// retardé), pour vérifier que l'hôte la lit.
pub const GAIN_LATENCY: u32 = 64;

static LATENCY: ClapPluginLatency = ClapPluginLatency { get: latency_get };

unsafe extern "C" fn latency_get(_: *const ClapPlugin) -> u32 {
    GAIN_LATENCY
}

fn copy_name(dst: &mut [c_char], src: &CStr) {
    for (d, &s) in dst.iter_mut().zip(src.to_bytes_with_nul()) {
        *d = s as c_char;
    }
}

static AUDIO_PORTS: ClapPluginAudioPorts = ClapPluginAudioPorts { count: ap_count, get: ap_get };

unsafe extern "C" fn ap_count(p: *const ClapPlugin, is_input: bool) -> u32 {
    if is_input && me(p).instrument { 0 } else { 1 }
}

unsafe extern "C" fn ap_get(p: *const ClapPlugin, i: u32, is_input: bool, info: *mut ClapAudioPortInfo) -> bool {
    if i != 0 || unsafe { ap_count(p, is_input) } == 0 {
        return false;
    }
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.id = 0;
    copy_name(&mut info.name, c"Principal");
    info.flags = 1;
    info.channel_count = 2;
    info.port_type = c"stereo".as_ptr();
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

static NOTE_PORTS: ClapPluginNotePorts = ClapPluginNotePorts { count: np_count, get: np_get };

unsafe extern "C" fn np_count(_: *const ClapPlugin, is_input: bool) -> u32 {
    is_input as u32
}

unsafe extern "C" fn np_get(_: *const ClapPlugin, i: u32, is_input: bool, info: *mut ClapNotePortInfo) -> bool {
    if i != 0 || !is_input {
        return false;
    }
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    copy_name(&mut info.name, c"Notes");
    true
}

static PARAMS: ClapPluginParams =
    ClapPluginParams { count: pa_count, get_info: pa_info, get_value: pa_value, value_to_text, text_to_value, flush };

unsafe extern "C" fn pa_count(_: *const ClapPlugin) -> u32 {
    1
}

unsafe extern "C" fn pa_info(_: *const ClapPlugin, i: u32, info: *mut ClapParamInfo) -> bool {
    if i != 0 {
        return false;
    }
    // SAFETY : structure fournie par l'hôte.
    let info = unsafe { &mut *info };
    info.id = GAIN_PARAM;
    info.flags = 0;
    info.cookie = std::ptr::null_mut();
    copy_name(&mut info.name, c"Gain");
    copy_name(&mut info.module, c"");
    info.min_value = 0.0;
    info.max_value = 2.0;
    info.default_value = 1.0;
    true
}

unsafe extern "C" fn pa_value(p: *const ClapPlugin, id: ClapId, out: *mut f64) -> bool {
    if id != GAIN_PARAM {
        return false;
    }
    // SAFETY : sortie fournie par l'hôte.
    unsafe { *out = me(p).gain };
    true
}

unsafe extern "C" fn value_to_text(_: *const ClapPlugin, _: ClapId, _: f64, _: *mut c_char, _: u32) -> bool {
    false
}

unsafe extern "C" fn text_to_value(_: *const ClapPlugin, _: ClapId, _: *const c_char, _: *mut f64) -> bool {
    false
}

unsafe extern "C" fn flush(p: *const ClapPlugin, input: *const ClapInputEvents, _: *const ClapOutputEvents) {
    handle_events(me(p), input);
}

// --- Interface (comptabilité seulement) -------------------------------------

/// Édition simulée : geste, valeur, fin de geste.
fn push_edit(out: *const ClapOutputEvents, value: f64) {
    let header = |kind: u16, size: usize| ClapEventHeader {
        size: size as u32,
        time: 0,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        kind,
        flags: 0,
    };
    let gesture = |kind| ClapEventParamGesture {
        header: header(kind, size_of::<ClapEventParamGesture>()),
        param_id: GAIN_PARAM,
    };
    let begin = gesture(CLAP_EVENT_PARAM_GESTURE_BEGIN);
    let end = gesture(CLAP_EVENT_PARAM_GESTURE_END);
    let param = ClapEventParamValue {
        header: header(CLAP_EVENT_PARAM_VALUE, size_of::<ClapEventParamValue>()),
        param_id: GAIN_PARAM,
        cookie: std::ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    };
    // SAFETY : file fournie par l'hôte pendant `process`.
    unsafe {
        let list = &*out;
        (list.try_push)(list, &begin.header);
        (list.try_push)(list, &param.header);
        (list.try_push)(list, &end.header);
    }
}

/// Extension de l'hôte, nulle s'il ne la propose pas.
fn host_ext<T>(t: &Test, id: &[u8]) -> Option<&'static T> {
    // SAFETY : hôte fourni à la création, vivant tant que le plugin l'est.
    unsafe { ((*t.host).get_extension)(t.host, id.as_ptr().cast()).cast::<T>().as_ref() }
}

#[cfg(target_os = "macos")]
const API: &CStr = c"cocoa";
#[cfg(windows)]
const API: &CStr = c"win32";
#[cfg(not(any(target_os = "macos", windows)))]
const API: &CStr = c"x11";

#[cfg(unix)]
mod sys {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        pub fn pipe(fds: *mut c_int) -> c_int;
        pub fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
        pub fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
        pub fn close(fd: c_int) -> c_int;
    }
}

static GUI: ClapPluginGui = ClapPluginGui {
    is_api_supported: gui_api_supported,
    get_preferred_api: gui_preferred_api,
    create: gui_create,
    destroy: gui_destroy,
    set_scale: gui_set_scale,
    get_size: gui_get_size,
    can_resize: gui_can_resize,
    get_resize_hints: gui_resize_hints,
    adjust_size: gui_adjust_size,
    set_size: gui_set_size,
    set_parent: gui_set_parent,
    set_transient: gui_set_transient,
    suggest_title: gui_suggest_title,
    show: gui_show,
    hide: gui_hide,
};

unsafe extern "C" fn gui_api_supported(_: *const ClapPlugin, api: *const c_char, floating: bool) -> bool {
    // SAFETY : chaîne C fournie par l'hôte.
    !floating && unsafe { CStr::from_ptr(api) } == API
}

unsafe extern "C" fn gui_preferred_api(_: *const ClapPlugin, api: *mut *const c_char, floating: *mut bool) -> bool {
    // SAFETY : sorties fournies par l'hôte.
    unsafe {
        *api = API.as_ptr();
        *floating = false;
    }
    true
}

unsafe extern "C" fn gui_create(p: *const ClapPlugin, api: *const c_char, floating: bool) -> bool {
    let t = me(p);
    // SAFETY : chaîne C fournie par l'hôte.
    if t.gui.created || floating || unsafe { CStr::from_ptr(api) } != API {
        return false;
    }
    // L'interface se crée sur le thread principal de l'hôte.
    let main = host_ext::<ClapHostThreadCheck>(t, CLAP_EXT_THREAD_CHECK);
    if !main.is_some_and(|c| unsafe { (c.is_main_thread)(t.host) }) {
        return false;
    }
    t.gui = Gui { created: true, scale: 1.0, size: GUI_SIZE, ..Gui::default() };
    if let Some(timer) = host_ext::<ClapHostTimerSupport>(t, CLAP_EXT_TIMER_SUPPORT) {
        let mut id = CLAP_INVALID_ID;
        // SAFETY : extension de l'hôte, sortie locale.
        if unsafe { (timer.register_timer)(t.host, 1, &mut id) } {
            t.gui.timer = Some(id);
        }
    }
    #[cfg(unix)]
    if let Some(fd) = host_ext::<ClapHostPosixFdSupport>(t, CLAP_EXT_POSIX_FD_SUPPORT) {
        let mut fds = [0; 2];
        // SAFETY : tube neuf, un octet écrit pour le rendre lisible.
        unsafe {
            if sys::pipe(fds.as_mut_ptr()) == 0 {
                sys::write(fds[1], b"!".as_ptr().cast(), 1);
                if (fd.register_fd)(t.host, fds[0], CLAP_POSIX_FD_READ) {
                    t.gui.pipe = Some((fds[0], fds[1]));
                } else {
                    sys::close(fds[0]);
                    sys::close(fds[1]);
                }
            }
        }
    }
    true
}

unsafe extern "C" fn gui_destroy(p: *const ClapPlugin) {
    let t = me(p);
    if let (Some(id), Some(timer)) = (t.gui.timer, host_ext::<ClapHostTimerSupport>(t, CLAP_EXT_TIMER_SUPPORT)) {
        // SAFETY : minuterie enregistrée par `gui_create`.
        unsafe { (timer.unregister_timer)(t.host, id) };
    }
    #[cfg(unix)]
    if let Some((r, w)) = t.gui.pipe {
        if let Some(fd) = host_ext::<ClapHostPosixFdSupport>(t, CLAP_EXT_POSIX_FD_SUPPORT) {
            // SAFETY : descripteur enregistré par `gui_create`.
            unsafe { (fd.unregister_fd)(t.host, r) };
        }
        // SAFETY : tube ouvert par `gui_create`.
        unsafe {
            sys::close(r);
            sys::close(w);
        }
    }
    t.gui = Gui::default();
}

unsafe extern "C" fn gui_set_scale(p: *const ClapPlugin, scale: f64) -> bool {
    let g = &mut me(p).gui;
    g.scale = scale;
    g.size = ((GUI_SIZE.0 as f64 * scale) as u32, (GUI_SIZE.1 as f64 * scale) as u32);
    true
}

unsafe extern "C" fn gui_get_size(p: *const ClapPlugin, w: *mut u32, h: *mut u32) -> bool {
    let g = &me(p).gui;
    // SAFETY : sorties fournies par l'hôte.
    unsafe { (*w, *h) = g.size };
    g.created
}

unsafe extern "C" fn gui_can_resize(_: *const ClapPlugin) -> bool {
    true
}

unsafe extern "C" fn gui_resize_hints(_: *const ClapPlugin, hints: *mut ClapGuiResizeHints) -> bool {
    // SAFETY : sortie fournie par l'hôte.
    unsafe {
        *hints = ClapGuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, ..Default::default() };
    }
    true
}

/// Au moins 100 × 80, par pas de 10 pixels.
unsafe extern "C" fn gui_adjust_size(_: *const ClapPlugin, w: *mut u32, h: *mut u32) -> bool {
    // SAFETY : entrées-sorties fournies par l'hôte.
    unsafe {
        *w = (*w).max(100) / 10 * 10;
        *h = (*h).max(80) / 10 * 10;
    }
    true
}

unsafe extern "C" fn gui_set_size(p: *const ClapPlugin, w: u32, h: u32) -> bool {
    let g = &mut me(p).gui;
    if !g.created || w % 10 != 0 || h % 10 != 0 {
        return false;
    }
    g.size = (w, h);
    true
}

unsafe extern "C" fn gui_set_parent(p: *const ClapPlugin, window: *const ClapWindow) -> bool {
    let g = &mut me(p).gui;
    // SAFETY : fenêtre fournie par l'hôte.
    let w = unsafe { &*window };
    if !g.created || w.specific == 0 || unsafe { CStr::from_ptr(w.api) } != API {
        return false;
    }
    g.parent = w.specific;
    true
}

unsafe extern "C" fn gui_set_transient(_: *const ClapPlugin, _: *const ClapWindow) -> bool {
    false
}

unsafe extern "C" fn gui_suggest_title(_: *const ClapPlugin, _: *const c_char) {}

unsafe extern "C" fn gui_show(p: *const ClapPlugin) -> bool {
    let g = &mut me(p).gui;
    g.shown = g.created && g.parent != 0;
    g.shown
}

unsafe extern "C" fn gui_hide(p: *const ClapPlugin) -> bool {
    me(p).gui.shown = false;
    true
}

static TIMER: ClapPluginTimerSupport = ClapPluginTimerSupport { on_timer };

unsafe extern "C" fn on_timer(p: *const ClapPlugin, id: ClapId) {
    let g = &mut me(p).gui;
    if g.timer == Some(id) {
        g.ticks += 1;
        if g.ticks == 1 {
            g.edit = Some(GUI_EDIT);
        }
    }
}

static FD: ClapPluginPosixFdSupport = ClapPluginPosixFdSupport { on_fd };

unsafe extern "C" fn on_fd(p: *const ClapPlugin, fd: i32, flags: u32) {
    let t = me(p);
    #[cfg(unix)]
    if t.gui.pipe.is_some_and(|(r, _)| r == fd) && flags & CLAP_POSIX_FD_READ != 0 {
        let mut b = [0u8; 1];
        // SAFETY : tube lisible ouvert par `gui_create`.
        unsafe { sys::read(fd, b.as_mut_ptr().cast(), 1) };
        if let Some(gui) = host_ext::<ClapHostGui>(t, CLAP_EXT_GUI) {
            // SAFETY : extension de l'hôte, appelée sur son thread principal.
            unsafe { (gui.request_resize)(t.host, GUI_REQUEST.0, GUI_REQUEST.1) };
        }
        // Comme après un preset chargé dans l'interface : l'hôte relit les valeurs.
        if let Some(params) = host_ext::<ClapHostParams>(t, CLAP_EXT_PARAMS) {
            // SAFETY : extension de l'hôte, appelée sur son thread principal.
            unsafe { (params.rescan)(t.host, CLAP_PARAM_RESCAN_VALUES) };
        }
    }
    #[cfg(not(unix))]
    let _ = (t, fd, flags);
}
