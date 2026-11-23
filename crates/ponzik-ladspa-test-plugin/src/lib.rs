//! LADSPA test plugins for `ponzik-ladspa`:
//!
//! - 4201 `ponzik_gain`: stereo gain (0 to 2, default 1), `Invert`
//!   (toggled), `Steps` (integer 1 to 8, default middle), `Tone` (a
//!   fraction of the sample rate, logarithmic, unused) and a `latency`
//!   output of 16 samples.
//! - 4202 `ponzik_lowpass`: mono one-pole lowpass, its cutoff a fraction
//!   of the sample rate (default middle, logarithmic).
//!
//! `PONZIK_TEST_PLUGIN_CRASH` makes `ladspa_descriptor` abort, for the scan tests.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_ulong};
use std::ptr::{null, null_mut};

use ponzik_ladspa::abi::*;

struct Gain {
    ports: [*mut f32; 9],
}

unsafe extern "C" fn gain_instantiate(_: *const LADSPA_Descriptor, _: c_ulong) -> LADSPA_Handle {
    Box::into_raw(Box::new(Gain { ports: [null_mut(); 9] })).cast()
}

unsafe extern "C" fn gain_connect(h: LADSPA_Handle, port: c_ulong, data: *mut f32) {
    let g = unsafe { &mut *h.cast::<Gain>() };
    if let Some(p) = g.ports.get_mut(port as usize) {
        *p = data;
    }
}

unsafe extern "C" fn gain_run(h: LADSPA_Handle, n: c_ulong) {
    let g = unsafe { &mut *h.cast::<Gain>() };
    let n = n as usize;
    unsafe {
        let mut gain = *g.ports[4];
        if *g.ports[5] > 0.0 {
            gain = -gain;
        }
        for c in 0..2 {
            let inp = std::slice::from_raw_parts(g.ports[c], n);
            let out = std::slice::from_raw_parts_mut(g.ports[c + 2], n);
            for k in 0..n {
                out[k] = inp[k] * gain;
            }
        }
        *g.ports[8] = 16.0;
    }
}

unsafe extern "C" fn cleanup<T>(h: LADSPA_Handle) {
    drop(unsafe { Box::from_raw(h.cast::<T>()) });
}

struct Lowpass {
    ports: [*mut f32; 3],
    rate: f32,
    z: f32,
}

unsafe extern "C" fn lp_instantiate(_: *const LADSPA_Descriptor, rate: c_ulong) -> LADSPA_Handle {
    Box::into_raw(Box::new(Lowpass { ports: [null_mut(); 3], rate: rate as f32, z: 0.0 })).cast()
}

unsafe extern "C" fn lp_connect(h: LADSPA_Handle, port: c_ulong, data: *mut f32) {
    let l = unsafe { &mut *h.cast::<Lowpass>() };
    if let Some(p) = l.ports.get_mut(port as usize) {
        *p = data;
    }
}

unsafe extern "C" fn lp_activate(h: LADSPA_Handle) {
    unsafe { (*h.cast::<Lowpass>()).z = 0.0 };
}

unsafe extern "C" fn lp_run(h: LADSPA_Handle, n: c_ulong) {
    let l = unsafe { &mut *h.cast::<Lowpass>() };
    let n = n as usize;
    unsafe {
        let fc = (*l.ports[2]).clamp(1.0, l.rate * 0.49);
        let a = 1.0 - (-std::f32::consts::TAU * fc / l.rate).exp();
        let inp = std::slice::from_raw_parts(l.ports[0], n);
        let out = std::slice::from_raw_parts_mut(l.ports[1], n);
        for k in 0..n {
            l.z += a * (inp[k] - l.z);
            out[k] = l.z;
        }
    }
}

const IN: c_int = LADSPA_PORT_INPUT | LADSPA_PORT_AUDIO;
const OUT: c_int = LADSPA_PORT_OUTPUT | LADSPA_PORT_AUDIO;
const CTL: c_int = LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL;
const CTL_OUT: c_int = LADSPA_PORT_OUTPUT | LADSPA_PORT_CONTROL;
const BOUNDED: c_int = LADSPA_HINT_BOUNDED_BELOW | LADSPA_HINT_BOUNDED_ABOVE;

const fn hint(d: c_int, lower_bound: f32, upper_bound: f32) -> LADSPA_PortRangeHint {
    LADSPA_PortRangeHint { hint_descriptor: d, lower_bound, upper_bound }
}

struct Strings<const N: usize>([*const c_char; N]);
// SAFETY: pointers to static strings.
unsafe impl<const N: usize> Sync for Strings<N> {}

static GAIN_PORTS: [c_int; 9] = [IN, IN, OUT, OUT, CTL, CTL, CTL, CTL, CTL_OUT];
static GAIN_NAMES: Strings<9> = Strings([
    c"In L".as_ptr(),
    c"In R".as_ptr(),
    c"Out L".as_ptr(),
    c"Out R".as_ptr(),
    c"Gain".as_ptr(),
    c"Invert".as_ptr(),
    c"Steps".as_ptr(),
    c"Tone".as_ptr(),
    c"latency".as_ptr(),
]);
static GAIN_HINTS: [LADSPA_PortRangeHint; 9] = [
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(BOUNDED | LADSPA_HINT_DEFAULT_1, 0.0, 2.0),
    hint(LADSPA_HINT_TOGGLED | LADSPA_HINT_DEFAULT_0, 0.0, 0.0),
    hint(BOUNDED | LADSPA_HINT_INTEGER | LADSPA_HINT_DEFAULT_MIDDLE, 1.0, 8.0),
    hint(BOUNDED | LADSPA_HINT_SAMPLE_RATE | LADSPA_HINT_LOGARITHMIC | LADSPA_HINT_DEFAULT_LOW, 0.001, 0.5),
    hint(0, 0.0, 0.0),
];

static LP_PORTS: [c_int; 3] = [IN, OUT, CTL];
static LP_NAMES: Strings<3> = Strings([c"In".as_ptr(), c"Out".as_ptr(), c"Cutoff".as_ptr()]);
static LP_HINTS: [LADSPA_PortRangeHint; 3] = [
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(BOUNDED | LADSPA_HINT_SAMPLE_RATE | LADSPA_HINT_LOGARITHMIC | LADSPA_HINT_DEFAULT_MIDDLE, 0.0005, 0.45),
];

struct Descriptors([LADSPA_Descriptor; 2]);
// SAFETY: static strings, arrays and functions.
unsafe impl Sync for Descriptors {}

static DESCRIPTORS: Descriptors = Descriptors([
    LADSPA_Descriptor {
        unique_id: 4201,
        label: c"ponzik_gain".as_ptr(),
        properties: LADSPA_PROPERTY_HARD_RT_CAPABLE,
        name: c"Ponzik Test Gain".as_ptr(),
        maker: c"Ponzik".as_ptr(),
        copyright: c"GPL".as_ptr(),
        port_count: 9,
        port_descriptors: GAIN_PORTS.as_ptr(),
        port_names: GAIN_NAMES.0.as_ptr(),
        port_range_hints: GAIN_HINTS.as_ptr(),
        implementation_data: null_mut(),
        instantiate: gain_instantiate,
        connect_port: gain_connect,
        activate: None,
        run: gain_run,
        run_adding: None,
        set_run_adding_gain: None,
        deactivate: None,
        cleanup: cleanup::<Gain>,
    },
    LADSPA_Descriptor {
        unique_id: 4202,
        label: c"ponzik_lowpass".as_ptr(),
        properties: LADSPA_PROPERTY_HARD_RT_CAPABLE,
        name: c"Ponzik Test Lowpass".as_ptr(),
        maker: c"Ponzik".as_ptr(),
        copyright: c"GPL".as_ptr(),
        port_count: 3,
        port_descriptors: LP_PORTS.as_ptr(),
        port_names: LP_NAMES.0.as_ptr(),
        port_range_hints: LP_HINTS.as_ptr(),
        implementation_data: null_mut(),
        instantiate: lp_instantiate,
        connect_port: lp_connect,
        activate: Some(lp_activate),
        run: lp_run,
        run_adding: None,
        set_run_adding_gain: None,
        deactivate: None,
        cleanup: cleanup::<Lowpass>,
    },
]);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladspa_descriptor(index: c_ulong) -> *const LADSPA_Descriptor {
    if std::env::var_os("PONZIK_TEST_PLUGIN_CRASH").is_some() {
        std::process::abort();
    }
    DESCRIPTORS.0.get(index as usize).map_or(null(), |d| d as *const LADSPA_Descriptor)
}
