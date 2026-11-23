//! LV2 test plugins for `ponzik-lv2` (described in `bundle/*.ttl`):
//!
//! - `urn:ponzik:test:gain`: stereo gain; refuses to start without the
//!   sample rate and block length options; a change of its `mode` goes
//!   through the worker, whose answer (`mode × 10`) shows on `worked`;
//!   reports 32 samples of latency; saves the number of blocks run as its
//!   state (`urn:ponzik:test:gain#blocks`).
//! - `urn:ponzik:test:synth`: a sine at the last MIDI note held, × level.
//! - `urn:ponzik:test:fixed`: mono copy; counts on `bad` the blocks that are
//!   not of the nominal length.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, c_char, c_void};
use std::ptr::{null, null_mut};

use ponzik_lv2::abi::*;

struct Host {
    map: *const LV2_URID_Map,
    options: *const LV2_Options_Option,
    schedule: *const LV2_Worker_Schedule,
}

unsafe fn host(features: *const *const LV2_Feature) -> Host {
    let mut h = Host { map: null(), options: null(), schedule: null() };
    let mut f = features;
    while !f.is_null() && unsafe { !(*f).is_null() } {
        let feat = unsafe { &**f };
        let uri = unsafe { CStr::from_ptr(feat.uri) }.to_bytes();
        match uri {
            b"http://lv2plug.in/ns/ext/urid#map" => h.map = feat.data.cast(),
            b"http://lv2plug.in/ns/ext/options#options" => h.options = feat.data.cast(),
            b"http://lv2plug.in/ns/ext/worker#schedule" => h.schedule = feat.data.cast(),
            _ => {}
        }
        f = unsafe { f.add(1) };
    }
    h
}

unsafe fn map(h: &Host, uri: &CStr) -> u32 {
    unsafe { ((*h.map).map)((*h.map).handle, uri.as_ptr()) }
}

/// An `Int` or `Float` option of the host.
unsafe fn option(h: &Host, key: u32) -> Option<f32> {
    let mut o = h.options;
    while !o.is_null() && unsafe { (*o).key } != 0 {
        let opt = unsafe { &*o };
        if opt.key == key && !opt.value.is_null() && opt.size == 4 {
            // Both an `i32` and an `f32` fit; the gain only asks for known types.
            let bits = unsafe { *opt.value.cast::<u32>() };
            return Some(if opt.type_ == unsafe { map(h, c"http://lv2plug.in/ns/ext/atom#Int") } {
                bits as i32 as f32
            } else {
                f32::from_bits(bits)
            });
        }
        o = unsafe { o.add(1) };
    }
    None
}

// --- Gain ------------------------------------------------------------------

struct Gain {
    ports: [*mut f32; 12],
    schedule: *const LV2_Worker_Schedule,
    last_mode: f32,
    blocks: i32,
    blocks_key: u32,
    int_type: u32,
    worked: f32,
}

unsafe extern "C" fn gain_instantiate(_: *const LV2_Descriptor, rate: f64, _: *const c_char, features: *const *const LV2_Feature) -> LV2_Handle {
    let h = unsafe { host(features) };
    if h.map.is_null() || h.options.is_null() || h.schedule.is_null() {
        return null_mut();
    }
    let sr = unsafe { option(&h, map(&h, c"http://lv2plug.in/ns/ext/parameters#sampleRate")) };
    let max = unsafe { option(&h, map(&h, c"http://lv2plug.in/ns/ext/buf-size#maxBlockLength")) };
    if sr != Some(rate as f32) || max.is_none_or(|m| m < 1.0) {
        return null_mut();
    }
    let g = Box::new(Gain {
        ports: [null_mut(); 12],
        schedule: h.schedule,
        last_mode: 0.0,
        blocks: 0,
        blocks_key: unsafe { map(&h, c"urn:ponzik:test:gain#blocks") },
        int_type: unsafe { map(&h, c"http://lv2plug.in/ns/ext/atom#Int") },
        worked: 0.0,
    });
    Box::into_raw(g).cast()
}

unsafe extern "C" fn gain_connect(i: LV2_Handle, port: u32, data: *mut c_void) {
    let g = unsafe { &mut *i.cast::<Gain>() };
    if let Some(p) = g.ports.get_mut(port as usize) {
        *p = data.cast();
    }
}

unsafe extern "C" fn gain_run(i: LV2_Handle, n: u32) {
    let g = unsafe { &mut *i.cast::<Gain>() };
    let n = n as usize;
    let p = |k: usize| g.ports[k];
    unsafe {
        let gain = if *p(7) != 0.0 { 0.0 } else { *p(4) };
        for c in 0..2 {
            let (inp, out) = (std::slice::from_raw_parts(p(c), n), std::slice::from_raw_parts_mut(p(c + 2), n));
            for k in 0..n {
                out[k] = inp[k] * gain;
            }
        }
        let mode = *p(5);
        if mode != g.last_mode {
            g.last_mode = mode;
            let s = &*g.schedule;
            (s.schedule_work)(s.handle, 4, (&mode as *const f32).cast());
        }
        *p(9) = g.worked;
        *p(10) = 32.0;
    }
    g.blocks += 1;
}

unsafe extern "C" fn gain_cleanup(i: LV2_Handle) {
    drop(unsafe { Box::from_raw(i.cast::<Gain>()) });
}

unsafe extern "C" fn work(_: LV2_Handle, respond: LV2_Worker_Respond_Function, h: *mut c_void, size: u32, data: *const c_void) -> u32 {
    if size != 4 {
        return LV2_WORKER_ERR_UNKNOWN;
    }
    // Not real time: the worker may take its time.
    std::thread::sleep(std::time::Duration::from_millis(2));
    let v = unsafe { *data.cast::<f32>() } * 10.0;
    unsafe { respond(h, 4, (&v as *const f32).cast()) }
}

unsafe extern "C" fn work_response(i: LV2_Handle, size: u32, body: *const c_void) -> u32 {
    let g = unsafe { &mut *i.cast::<Gain>() };
    if size == 4 {
        g.worked = unsafe { *body.cast::<f32>() };
    }
    LV2_WORKER_SUCCESS
}

unsafe extern "C" fn save(i: LV2_Handle, store: LV2_State_Store_Function, h: *mut c_void, _: u32, _: *const *const LV2_Feature) -> u32 {
    let g = unsafe { &*i.cast::<Gain>() };
    unsafe { store(h, g.blocks_key, (&g.blocks as *const i32).cast(), 4, g.int_type, LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE) }
}

unsafe extern "C" fn restore(i: LV2_Handle, retrieve: LV2_State_Retrieve_Function, h: *mut c_void, _: u32, _: *const *const LV2_Feature) -> u32 {
    let g = unsafe { &mut *i.cast::<Gain>() };
    let (mut size, mut ty, mut flags) = (0usize, 0u32, 0u32);
    let v = unsafe { retrieve(h, g.blocks_key, &mut size, &mut ty, &mut flags) };
    if v.is_null() || size != 4 || ty != g.int_type {
        return LV2_STATE_ERR_NO_PROPERTY;
    }
    g.blocks = unsafe { *v.cast::<i32>() };
    LV2_STATE_SUCCESS
}

struct Tables {
    worker: LV2_Worker_Interface,
    state: LV2_State_Interface,
}
// SAFETY: function pointers only.
unsafe impl Sync for Tables {}

static TABLES: Tables = Tables {
    worker: LV2_Worker_Interface { work, work_response, end_run: None },
    state: LV2_State_Interface { save, restore },
};

unsafe extern "C" fn gain_extension(uri: *const c_char) -> *const c_void {
    match unsafe { CStr::from_ptr(uri) }.to_bytes() {
        b"http://lv2plug.in/ns/ext/worker#interface" => (&TABLES.worker as *const LV2_Worker_Interface).cast(),
        b"http://lv2plug.in/ns/ext/state#interface" => (&TABLES.state as *const LV2_State_Interface).cast(),
        _ => null(),
    }
}

// --- Synth -----------------------------------------------------------------

struct Synth {
    midi_port: *const LV2_Atom_Sequence,
    out: [*mut f32; 2],
    level: *const f32,
    midi: u32,
    rate: f32,
    note: Option<u8>,
    vel: f32,
    phase: f32,
}

unsafe extern "C" fn synth_instantiate(_: *const LV2_Descriptor, rate: f64, _: *const c_char, features: *const *const LV2_Feature) -> LV2_Handle {
    let h = unsafe { host(features) };
    if h.map.is_null() {
        return null_mut();
    }
    let s = Box::new(Synth {
        midi_port: null(),
        out: [null_mut(); 2],
        level: null(),
        midi: unsafe { map(&h, c"http://lv2plug.in/ns/ext/midi#MidiEvent") },
        rate: rate as f32,
        note: None,
        vel: 0.0,
        phase: 0.0,
    });
    Box::into_raw(s).cast()
}

unsafe extern "C" fn synth_connect(i: LV2_Handle, port: u32, data: *mut c_void) {
    let s = unsafe { &mut *i.cast::<Synth>() };
    match port {
        0 => s.midi_port = data.cast(),
        1 => s.out[0] = data.cast(),
        2 => s.out[1] = data.cast(),
        3 => s.level = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn synth_run(i: LV2_Handle, n: u32) {
    let s = unsafe { &mut *i.cast::<Synth>() };
    let n = n as usize;
    // Events of the sequence, all at their time (the test only checks pitch and level).
    unsafe {
        let seq = &*s.midi_port;
        let base = s.midi_port.cast::<u8>();
        let end = 8 + seq.atom.size as usize;
        let mut at = 16usize;
        while at + 16 <= end {
            let ev = &*base.add(at).cast::<LV2_Atom_Event>();
            let body = std::slice::from_raw_parts(base.add(at + 16), ev.body.size as usize);
            if ev.body.type_ == s.midi && body.len() >= 3 {
                match body[0] & 0xF0 {
                    0x90 if body[2] > 0 => {
                        s.note = Some(body[1]);
                        s.vel = body[2] as f32 / 127.0;
                    }
                    0x80 | 0x90 if s.note == Some(body[1]) => s.note = None,
                    _ => {}
                }
            }
            at += 16 + atom_pad(ev.body.size) as usize;
        }
        let (l, r) = (std::slice::from_raw_parts_mut(s.out[0], n), std::slice::from_raw_parts_mut(s.out[1], n));
        let level = *s.level;
        match s.note {
            Some(k) => {
                let f = 440.0 * 2f32.powf((k as f32 - 69.0) / 12.0);
                for k in 0..n {
                    let v = (s.phase * std::f32::consts::TAU).sin() * level * s.vel;
                    s.phase = (s.phase + f / s.rate).fract();
                    l[k] = v;
                    r[k] = v;
                }
            }
            None => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
    }
}

unsafe extern "C" fn synth_cleanup(i: LV2_Handle) {
    drop(unsafe { Box::from_raw(i.cast::<Synth>()) });
}

// --- Fixed -----------------------------------------------------------------

struct Fixed {
    ports: [*mut f32; 3],
    nominal: u32,
    bad: f32,
}

unsafe extern "C" fn fixed_instantiate(_: *const LV2_Descriptor, _: f64, _: *const c_char, features: *const *const LV2_Feature) -> LV2_Handle {
    let h = unsafe { host(features) };
    if h.map.is_null() || h.options.is_null() {
        return null_mut();
    }
    let Some(nominal) = (unsafe { option(&h, map(&h, c"http://lv2plug.in/ns/ext/buf-size#nominalBlockLength")) }) else {
        return null_mut();
    };
    Box::into_raw(Box::new(Fixed { ports: [null_mut(); 3], nominal: nominal as u32, bad: 0.0 })).cast()
}

unsafe extern "C" fn fixed_connect(i: LV2_Handle, port: u32, data: *mut c_void) {
    let f = unsafe { &mut *i.cast::<Fixed>() };
    if let Some(p) = f.ports.get_mut(port as usize) {
        *p = data.cast();
    }
}

unsafe extern "C" fn fixed_run(i: LV2_Handle, n: u32) {
    let f = unsafe { &mut *i.cast::<Fixed>() };
    if n != f.nominal {
        f.bad += 1.0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(f.ports[0], f.ports[1], n as usize);
        *f.ports[2] = f.bad;
    }
}

unsafe extern "C" fn fixed_cleanup(i: LV2_Handle) {
    drop(unsafe { Box::from_raw(i.cast::<Fixed>()) });
}

// --- Descriptors -----------------------------------------------------------

struct Descriptors([LV2_Descriptor; 3]);
// SAFETY: constant strings and function pointers.
unsafe impl Sync for Descriptors {}

static DESCRIPTORS: Descriptors = Descriptors([
    LV2_Descriptor {
        uri: c"urn:ponzik:test:gain".as_ptr(),
        instantiate: gain_instantiate,
        connect_port: gain_connect,
        activate: None,
        run: gain_run,
        deactivate: None,
        cleanup: gain_cleanup,
        extension_data: Some(gain_extension),
    },
    LV2_Descriptor {
        uri: c"urn:ponzik:test:synth".as_ptr(),
        instantiate: synth_instantiate,
        connect_port: synth_connect,
        activate: None,
        run: synth_run,
        deactivate: None,
        cleanup: synth_cleanup,
        extension_data: None,
    },
    LV2_Descriptor {
        uri: c"urn:ponzik:test:fixed".as_ptr(),
        instantiate: fixed_instantiate,
        connect_port: fixed_connect,
        activate: None,
        run: fixed_run,
        deactivate: None,
        cleanup: fixed_cleanup,
        extension_data: None,
    },
]);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn lv2_descriptor(index: u32) -> *const LV2_Descriptor {
    DESCRIPTORS.0.get(index as usize).map_or(null(), |d| d as *const LV2_Descriptor)
}
