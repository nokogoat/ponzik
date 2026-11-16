//! Plugin CLAP minimal pour les tests de `ponzik-clap` : un effet « gain »
//! (un paramètre) et un instrument « sinus » monophonique.
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

unsafe extern "C" fn init(_: *const c_char) -> bool {
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
}

unsafe extern "C" fn create_plugin(
    _: *const ClapPluginFactory,
    _: *const ClapHost,
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
                t.freq = 440.0 * 2f64.powf((e.key as f64 - 69.0) / 12.0);
                t.amp = e.velocity;
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
    let n = pr.frames_count as usize;
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
    } else {
        null()
    }
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
