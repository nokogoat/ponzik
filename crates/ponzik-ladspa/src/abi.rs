//! The LADSPA 1.1 C ABI, written by hand (public for the test plugin).

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_ulong, c_void};

pub type LADSPA_Data = f32;
pub type LADSPA_Handle = *mut c_void;

pub const LADSPA_PROPERTY_REALTIME: c_int = 0x1;
pub const LADSPA_PROPERTY_INPLACE_BROKEN: c_int = 0x2;
pub const LADSPA_PROPERTY_HARD_RT_CAPABLE: c_int = 0x4;

pub const LADSPA_PORT_INPUT: c_int = 0x1;
pub const LADSPA_PORT_OUTPUT: c_int = 0x2;
pub const LADSPA_PORT_CONTROL: c_int = 0x4;
pub const LADSPA_PORT_AUDIO: c_int = 0x8;

pub const LADSPA_HINT_BOUNDED_BELOW: c_int = 0x1;
pub const LADSPA_HINT_BOUNDED_ABOVE: c_int = 0x2;
pub const LADSPA_HINT_TOGGLED: c_int = 0x4;
pub const LADSPA_HINT_SAMPLE_RATE: c_int = 0x8;
pub const LADSPA_HINT_LOGARITHMIC: c_int = 0x10;
pub const LADSPA_HINT_INTEGER: c_int = 0x20;
pub const LADSPA_HINT_DEFAULT_MASK: c_int = 0x3C0;
pub const LADSPA_HINT_DEFAULT_NONE: c_int = 0x0;
pub const LADSPA_HINT_DEFAULT_MINIMUM: c_int = 0x40;
pub const LADSPA_HINT_DEFAULT_LOW: c_int = 0x80;
pub const LADSPA_HINT_DEFAULT_MIDDLE: c_int = 0xC0;
pub const LADSPA_HINT_DEFAULT_HIGH: c_int = 0x100;
pub const LADSPA_HINT_DEFAULT_MAXIMUM: c_int = 0x140;
pub const LADSPA_HINT_DEFAULT_0: c_int = 0x200;
pub const LADSPA_HINT_DEFAULT_1: c_int = 0x240;
pub const LADSPA_HINT_DEFAULT_100: c_int = 0x280;
pub const LADSPA_HINT_DEFAULT_440: c_int = 0x2C0;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LADSPA_PortRangeHint {
    pub hint_descriptor: c_int,
    pub lower_bound: LADSPA_Data,
    pub upper_bound: LADSPA_Data,
}

#[repr(C)]
pub struct LADSPA_Descriptor {
    pub unique_id: c_ulong,
    pub label: *const c_char,
    pub properties: c_int,
    pub name: *const c_char,
    pub maker: *const c_char,
    pub copyright: *const c_char,
    pub port_count: c_ulong,
    pub port_descriptors: *const c_int,
    pub port_names: *const *const c_char,
    pub port_range_hints: *const LADSPA_PortRangeHint,
    pub implementation_data: *mut c_void,
    pub instantiate: unsafe extern "C" fn(descriptor: *const LADSPA_Descriptor, sample_rate: c_ulong) -> LADSPA_Handle,
    pub connect_port: unsafe extern "C" fn(instance: LADSPA_Handle, port: c_ulong, data: *mut LADSPA_Data),
    pub activate: Option<unsafe extern "C" fn(instance: LADSPA_Handle)>,
    pub run: unsafe extern "C" fn(instance: LADSPA_Handle, sample_count: c_ulong),
    pub run_adding: Option<unsafe extern "C" fn(instance: LADSPA_Handle, sample_count: c_ulong)>,
    pub set_run_adding_gain: Option<unsafe extern "C" fn(instance: LADSPA_Handle, gain: LADSPA_Data)>,
    pub deactivate: Option<unsafe extern "C" fn(instance: LADSPA_Handle)>,
    pub cleanup: unsafe extern "C" fn(instance: LADSPA_Handle),
}

pub type LADSPA_Descriptor_Function = unsafe extern "C" fn(index: c_ulong) -> *const LADSPA_Descriptor;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_ladspa_h() {
        // 11 fields of a word (the int is padded), then 8 functions.
        let w = size_of::<usize>();
        if w == 8 && size_of::<c_ulong>() == 8 {
            assert_eq!(size_of::<LADSPA_Descriptor>(), 11 * 8 + 8 * 8);
        }
        assert_eq!(size_of::<LADSPA_PortRangeHint>(), 12);
    }
}
