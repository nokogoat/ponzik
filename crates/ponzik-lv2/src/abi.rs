//! The LV2 C ABI written by hand: core descriptor, features, URID, atoms,
//! options, log, worker, state and UI, with the URIs Ponzik uses.
//!
//! Public so that the in-repo test plugin can be written against it.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

pub type LV2_Handle = *mut c_void;
pub type LV2_URID = u32;

#[repr(C)]
pub struct LV2_Feature {
    pub uri: *const c_char,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct LV2_Descriptor {
    pub uri: *const c_char,
    pub instantiate: unsafe extern "C" fn(
        descriptor: *const LV2_Descriptor,
        sample_rate: f64,
        bundle_path: *const c_char,
        features: *const *const LV2_Feature,
    ) -> LV2_Handle,
    pub connect_port: unsafe extern "C" fn(instance: LV2_Handle, port: u32, data: *mut c_void),
    pub activate: Option<unsafe extern "C" fn(instance: LV2_Handle)>,
    pub run: unsafe extern "C" fn(instance: LV2_Handle, sample_count: u32),
    pub deactivate: Option<unsafe extern "C" fn(instance: LV2_Handle)>,
    pub cleanup: unsafe extern "C" fn(instance: LV2_Handle),
    pub extension_data: Option<unsafe extern "C" fn(uri: *const c_char) -> *const c_void>,
}

pub type LV2_Descriptor_Function = unsafe extern "C" fn(index: u32) -> *const LV2_Descriptor;

// --- URID ------------------------------------------------------------------

#[repr(C)]
pub struct LV2_URID_Map {
    pub handle: *mut c_void,
    pub map: unsafe extern "C" fn(handle: *mut c_void, uri: *const c_char) -> LV2_URID,
}

#[repr(C)]
pub struct LV2_URID_Unmap {
    pub handle: *mut c_void,
    pub unmap: unsafe extern "C" fn(handle: *mut c_void, urid: LV2_URID) -> *const c_char,
}

// --- Atoms -----------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom {
    pub size: u32,
    pub type_: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom_Sequence_Body {
    pub unit: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom_Sequence {
    pub atom: LV2_Atom,
    pub body: LV2_Atom_Sequence_Body,
}

/// An event of a sequence; its body (`body.size` bytes) follows, then
/// padding to 8 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom_Event {
    /// Time in frames (the only unit Ponzik writes).
    pub frames: i64,
    pub body: LV2_Atom,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom_Int {
    pub atom: LV2_Atom,
    pub body: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LV2_Atom_Float {
    pub atom: LV2_Atom,
    pub body: f32,
}

/// Size of an atom with its body, padded to 8 bytes.
pub const fn atom_pad(size: u32) -> u32 {
    (size + 7) & !7
}

// --- Options ---------------------------------------------------------------

pub const LV2_OPTIONS_INSTANCE: u32 = 0;

#[repr(C)]
pub struct LV2_Options_Option {
    pub context: u32,
    pub subject: u32,
    pub key: LV2_URID,
    pub size: u32,
    pub type_: LV2_URID,
    pub value: *const c_void,
}

#[repr(C)]
pub struct LV2_Options_Interface {
    pub get: unsafe extern "C" fn(instance: LV2_Handle, options: *mut LV2_Options_Option) -> u32,
    pub set: unsafe extern "C" fn(instance: LV2_Handle, options: *const LV2_Options_Option) -> u32,
}

// --- Log -------------------------------------------------------------------

/// `printf` is variadic in C: Ponzik's takes only the fixed arguments and
/// prints the format as it is; plugins mostly go through `vprintf`.
#[repr(C)]
pub struct LV2_Log_Log {
    pub handle: *mut c_void,
    pub printf: unsafe extern "C" fn(handle: *mut c_void, type_: LV2_URID, fmt: *const c_char) -> c_int,
    pub vprintf: unsafe extern "C" fn(handle: *mut c_void, type_: LV2_URID, fmt: *const c_char, ap: *mut c_void) -> c_int,
}

// --- Worker ----------------------------------------------------------------

pub const LV2_WORKER_SUCCESS: u32 = 0;
pub const LV2_WORKER_ERR_UNKNOWN: u32 = 1;
pub const LV2_WORKER_ERR_NO_SPACE: u32 = 2;

pub type LV2_Worker_Respond_Function = unsafe extern "C" fn(handle: *mut c_void, size: u32, data: *const c_void) -> u32;

#[repr(C)]
pub struct LV2_Worker_Interface {
    pub work: unsafe extern "C" fn(
        instance: LV2_Handle,
        respond: LV2_Worker_Respond_Function,
        handle: *mut c_void,
        size: u32,
        data: *const c_void,
    ) -> u32,
    pub work_response: unsafe extern "C" fn(instance: LV2_Handle, size: u32, body: *const c_void) -> u32,
    pub end_run: Option<unsafe extern "C" fn(instance: LV2_Handle) -> u32>,
}

#[repr(C)]
pub struct LV2_Worker_Schedule {
    pub handle: *mut c_void,
    pub schedule_work: unsafe extern "C" fn(handle: *mut c_void, size: u32, data: *const c_void) -> u32,
}

// --- State -----------------------------------------------------------------

pub const LV2_STATE_IS_POD: u32 = 1;
pub const LV2_STATE_IS_PORTABLE: u32 = 2;
pub const LV2_STATE_SUCCESS: u32 = 0;
pub const LV2_STATE_ERR_UNKNOWN: u32 = 1;
pub const LV2_STATE_ERR_NO_PROPERTY: u32 = 5;

pub type LV2_State_Store_Function =
    unsafe extern "C" fn(handle: *mut c_void, key: u32, value: *const c_void, size: usize, type_: u32, flags: u32) -> u32;
pub type LV2_State_Retrieve_Function =
    unsafe extern "C" fn(handle: *mut c_void, key: u32, size: *mut usize, type_: *mut u32, flags: *mut u32) -> *const c_void;

#[repr(C)]
pub struct LV2_State_Interface {
    pub save: unsafe extern "C" fn(
        instance: LV2_Handle,
        store: LV2_State_Store_Function,
        handle: *mut c_void,
        flags: u32,
        features: *const *const LV2_Feature,
    ) -> u32,
    pub restore: unsafe extern "C" fn(
        instance: LV2_Handle,
        retrieve: LV2_State_Retrieve_Function,
        handle: *mut c_void,
        flags: u32,
        features: *const *const LV2_Feature,
    ) -> u32,
}

// --- UI --------------------------------------------------------------------

pub type LV2UI_Write_Function =
    unsafe extern "C" fn(controller: *mut c_void, port_index: u32, buffer_size: u32, port_protocol: u32, buffer: *const c_void);

#[repr(C)]
pub struct LV2UI_Descriptor {
    pub uri: *const c_char,
    pub instantiate: unsafe extern "C" fn(
        descriptor: *const LV2UI_Descriptor,
        plugin_uri: *const c_char,
        bundle_path: *const c_char,
        write_function: LV2UI_Write_Function,
        controller: *mut c_void,
        widget: *mut *mut c_void,
        features: *const *const LV2_Feature,
    ) -> *mut c_void,
    pub cleanup: unsafe extern "C" fn(ui: *mut c_void),
    pub port_event: Option<unsafe extern "C" fn(ui: *mut c_void, port_index: u32, buffer_size: u32, format: u32, buffer: *const c_void)>,
    pub extension_data: Option<unsafe extern "C" fn(uri: *const c_char) -> *const c_void>,
}

pub type LV2UI_DescriptorFunction = unsafe extern "C" fn(index: u32) -> *const LV2UI_Descriptor;

/// `ui:resize`, both ways: the host's (the UI asks for a size) and the UI's
/// (the host resizes it).
#[repr(C)]
pub struct LV2UI_Resize {
    pub handle: *mut c_void,
    pub ui_resize: unsafe extern "C" fn(handle: *mut c_void, width: c_int, height: c_int) -> c_int,
}

#[repr(C)]
pub struct LV2UI_Idle_Interface {
    pub idle: unsafe extern "C" fn(ui: *mut c_void) -> c_int,
}

#[repr(C)]
pub struct LV2UI_Touch {
    pub handle: *mut c_void,
    pub touch: unsafe extern "C" fn(handle: *mut c_void, port_index: u32, grabbed: bool),
}

#[repr(C)]
pub struct LV2UI_Port_Map {
    pub handle: *mut c_void,
    pub port_index: unsafe extern "C" fn(handle: *mut c_void, symbol: *const c_char) -> u32,
}

/// `data-access`: the plugin's `extension_data`, for the UI.
#[repr(C)]
pub struct LV2_Extension_Data_Feature {
    pub data_access: Option<unsafe extern "C" fn(uri: *const c_char) -> *const c_void>,
}

// --- URIs ------------------------------------------------------------------

macro_rules! uris {
    ($($name:ident = $uri:literal;)*) => {
        $(pub const $name: &str = $uri;)*
    };
}

uris! {
    LV2_CORE = "http://lv2plug.in/ns/lv2core#";
    LV2_PLUGIN = "http://lv2plug.in/ns/lv2core#Plugin";
    LV2_INSTRUMENT_PLUGIN = "http://lv2plug.in/ns/lv2core#InstrumentPlugin";
    LV2_MIDI_PLUGIN = "http://lv2plug.in/ns/lv2core#MIDIPlugin";
    LV2_BINARY = "http://lv2plug.in/ns/lv2core#binary";
    LV2_PORT = "http://lv2plug.in/ns/lv2core#port";
    LV2_INDEX = "http://lv2plug.in/ns/lv2core#index";
    LV2_SYMBOL = "http://lv2plug.in/ns/lv2core#symbol";
    LV2_NAME = "http://lv2plug.in/ns/lv2core#name";
    LV2_DEFAULT = "http://lv2plug.in/ns/lv2core#default";
    LV2_MINIMUM = "http://lv2plug.in/ns/lv2core#minimum";
    LV2_MAXIMUM = "http://lv2plug.in/ns/lv2core#maximum";
    LV2_PORT_PROPERTY = "http://lv2plug.in/ns/lv2core#portProperty";
    LV2_SCALE_POINT = "http://lv2plug.in/ns/lv2core#scalePoint";
    LV2_DESIGNATION = "http://lv2plug.in/ns/lv2core#designation";
    LV2_REQUIRED_FEATURE = "http://lv2plug.in/ns/lv2core#requiredFeature";
    LV2_OPTIONAL_FEATURE = "http://lv2plug.in/ns/lv2core#optionalFeature";
    LV2_EXTENSION_DATA = "http://lv2plug.in/ns/lv2core#extensionData";
    LV2_PROJECT = "http://lv2plug.in/ns/lv2core#project";
    LV2_INPUT_PORT = "http://lv2plug.in/ns/lv2core#InputPort";
    LV2_OUTPUT_PORT = "http://lv2plug.in/ns/lv2core#OutputPort";
    LV2_AUDIO_PORT = "http://lv2plug.in/ns/lv2core#AudioPort";
    LV2_CONTROL_PORT = "http://lv2plug.in/ns/lv2core#ControlPort";
    LV2_CV_PORT = "http://lv2plug.in/ns/lv2core#CVPort";
    LV2_TOGGLED = "http://lv2plug.in/ns/lv2core#toggled";
    LV2_INTEGER = "http://lv2plug.in/ns/lv2core#integer";
    LV2_ENUMERATION = "http://lv2plug.in/ns/lv2core#enumeration";
    LV2_SAMPLE_RATE = "http://lv2plug.in/ns/lv2core#sampleRate";
    LV2_CONNECTION_OPTIONAL = "http://lv2plug.in/ns/lv2core#connectionOptional";
    LV2_REPORTS_LATENCY = "http://lv2plug.in/ns/lv2core#reportsLatency";
    LV2_IS_SIDE_CHAIN = "http://lv2plug.in/ns/lv2core#isSideChain";
    LV2_LATENCY = "http://lv2plug.in/ns/lv2core#latency";
    LV2_FREE_WHEELING = "http://lv2plug.in/ns/lv2core#freeWheeling";
    LV2_ENABLED = "http://lv2plug.in/ns/lv2core#enabled";
    LV2_HARD_RT_CAPABLE = "http://lv2plug.in/ns/lv2core#hardRTCapable";
    LV2_IN_PLACE_BROKEN = "http://lv2plug.in/ns/lv2core#inPlaceBroken";
    LV2_IS_LIVE = "http://lv2plug.in/ns/lv2core#isLive";
    PPROPS_LOGARITHMIC = "http://lv2plug.in/ns/ext/port-props#logarithmic";
    PPROPS_NOT_ON_GUI = "http://lv2plug.in/ns/ext/port-props#notOnGUI";
    PPROPS_EXPENSIVE = "http://lv2plug.in/ns/ext/port-props#expensive";
    PPROPS_SUPPORTS_STRICT_BOUNDS = "http://lv2plug.in/ns/ext/port-props#supportsStrictBounds";
    RDF_VALUE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#value";
    RDFS_LABEL = "http://www.w3.org/2000/01/rdf-schema#label";
    RDFS_SEE_ALSO = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
    DOAP_NAME = "http://usefulinc.com/ns/doap#name";
    DOAP_MAINTAINER = "http://usefulinc.com/ns/doap#maintainer";
    DOAP_DEVELOPER = "http://usefulinc.com/ns/doap#developer";
    FOAF_NAME = "http://xmlns.com/foaf/0.1/name";
    UNITS_UNIT = "http://lv2plug.in/ns/extensions/units#unit";
    UNITS_DB = "http://lv2plug.in/ns/extensions/units#db";
    UNITS_HZ = "http://lv2plug.in/ns/extensions/units#hz";
    UNITS_KHZ = "http://lv2plug.in/ns/extensions/units#khz";
    UNITS_S = "http://lv2plug.in/ns/extensions/units#s";
    UNITS_MS = "http://lv2plug.in/ns/extensions/units#ms";
    UNITS_SEMITONE = "http://lv2plug.in/ns/extensions/units#semitone12TET";
    UNITS_CENT = "http://lv2plug.in/ns/extensions/units#cent";
    UNITS_BPM = "http://lv2plug.in/ns/extensions/units#bpm";
    PSET_PRESET = "http://lv2plug.in/ns/ext/presets#Preset";
    PSET_VALUE = "http://lv2plug.in/ns/ext/presets#value";
    LV2_APPLIES_TO = "http://lv2plug.in/ns/lv2core#appliesTo";
    STATE_STATE = "http://lv2plug.in/ns/ext/state#state";
    STATE_INTERFACE = "http://lv2plug.in/ns/ext/state#interface";
    STATE_LOAD_DEFAULT = "http://lv2plug.in/ns/ext/state#loadDefaultState";
    STATE_THREAD_SAFE_RESTORE = "http://lv2plug.in/ns/ext/state#threadSafeRestore";
    ATOM_ATOM_PORT = "http://lv2plug.in/ns/ext/atom#AtomPort";
    ATOM_BUFFER_TYPE = "http://lv2plug.in/ns/ext/atom#bufferType";
    ATOM_SUPPORTS = "http://lv2plug.in/ns/ext/atom#supports";
    ATOM_SEQUENCE = "http://lv2plug.in/ns/ext/atom#Sequence";
    ATOM_CHUNK = "http://lv2plug.in/ns/ext/atom#Chunk";
    ATOM_INT = "http://lv2plug.in/ns/ext/atom#Int";
    ATOM_LONG = "http://lv2plug.in/ns/ext/atom#Long";
    ATOM_FLOAT = "http://lv2plug.in/ns/ext/atom#Float";
    ATOM_DOUBLE = "http://lv2plug.in/ns/ext/atom#Double";
    ATOM_BOOL = "http://lv2plug.in/ns/ext/atom#Bool";
    ATOM_STRING = "http://lv2plug.in/ns/ext/atom#String";
    ATOM_PATH = "http://lv2plug.in/ns/ext/atom#Path";
    ATOM_URID = "http://lv2plug.in/ns/ext/atom#URID";
    ATOM_EVENT_TRANSFER = "http://lv2plug.in/ns/ext/atom#eventTransfer";
    ATOM_BEAT_TIME = "http://lv2plug.in/ns/ext/atom#beatTime";
    ATOM_FRAME_TIME = "http://lv2plug.in/ns/ext/atom#frameTime";
    MIDI_EVENT = "http://lv2plug.in/ns/ext/midi#MidiEvent";
    EVENT_EVENT_PORT = "http://lv2plug.in/ns/ext/event#EventPort";
    RSZ_MINIMUM_SIZE = "http://lv2plug.in/ns/ext/resize-port#minimumSize";
    URID_MAP = "http://lv2plug.in/ns/ext/urid#map";
    URID_UNMAP = "http://lv2plug.in/ns/ext/urid#unmap";
    OPTIONS_OPTIONS = "http://lv2plug.in/ns/ext/options#options";
    OPTIONS_INTERFACE = "http://lv2plug.in/ns/ext/options#interface";
    BUF_BOUNDED = "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
    BUF_FIXED = "http://lv2plug.in/ns/ext/buf-size#fixedBlockLength";
    BUF_POWER_OF_2 = "http://lv2plug.in/ns/ext/buf-size#powerOf2BlockLength";
    BUF_COARSE = "http://lv2plug.in/ns/ext/buf-size#coarseBlockLength";
    BUF_MIN_BLOCK = "http://lv2plug.in/ns/ext/buf-size#minBlockLength";
    BUF_MAX_BLOCK = "http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
    BUF_NOMINAL_BLOCK = "http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
    BUF_SEQUENCE_SIZE = "http://lv2plug.in/ns/ext/buf-size#sequenceSize";
    PARAM_SAMPLE_RATE = "http://lv2plug.in/ns/ext/parameters#sampleRate";
    LOG_LOG = "http://lv2plug.in/ns/ext/log#log";
    LOG_ERROR = "http://lv2plug.in/ns/ext/log#Error";
    LOG_WARNING = "http://lv2plug.in/ns/ext/log#Warning";
    LOG_NOTE = "http://lv2plug.in/ns/ext/log#Note";
    LOG_TRACE = "http://lv2plug.in/ns/ext/log#Trace";
    WORKER_SCHEDULE = "http://lv2plug.in/ns/ext/worker#schedule";
    WORKER_INTERFACE = "http://lv2plug.in/ns/ext/worker#interface";
    TIME_BPM = "http://lv2plug.in/ns/ext/time#beatsPerMinute";
    UI_UI = "http://lv2plug.in/ns/extensions/ui#ui";
    UI_BINARY = "http://lv2plug.in/ns/extensions/ui#binary";
    UI_X11 = "http://lv2plug.in/ns/extensions/ui#X11UI";
    UI_WINDOWS = "http://lv2plug.in/ns/extensions/ui#WindowsUI";
    UI_COCOA = "http://lv2plug.in/ns/extensions/ui#CocoaUI";
    UI_PARENT = "http://lv2plug.in/ns/extensions/ui#parent";
    UI_RESIZE = "http://lv2plug.in/ns/extensions/ui#resize";
    UI_IDLE = "http://lv2plug.in/ns/extensions/ui#idleInterface";
    UI_NO_USER_RESIZE = "http://lv2plug.in/ns/extensions/ui#noUserResize";
    UI_FIXED_SIZE = "http://lv2plug.in/ns/extensions/ui#fixedSize";
    UI_TOUCH = "http://lv2plug.in/ns/extensions/ui#touch";
    UI_PORT_MAP = "http://lv2plug.in/ns/extensions/ui#portMap";
    UI_UPDATE_RATE = "http://lv2plug.in/ns/extensions/ui#updateRate";
    UI_SCALE_FACTOR = "http://lv2plug.in/ns/extensions/ui#scaleFactor";
    UI_MAKE_RESIDENT = "http://lv2plug.in/ns/extensions/ui#makeResident";
    UI_MAKE_SO_RESIDENT = "http://lv2plug.in/ns/extensions/ui#makeSONameResident";
    INSTANCE_ACCESS = "http://lv2plug.in/ns/ext/instance-access";
    DATA_ACCESS = "http://lv2plug.in/ns/ext/data-access";
}

/// NUL-terminated copy of a URI for the C side.
pub fn cstr(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s.replace('\0', "")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_c_headers() {
        assert_eq!(size_of::<LV2_Atom>(), 8);
        assert_eq!(size_of::<LV2_Atom_Sequence>(), 16);
        assert_eq!(size_of::<LV2_Atom_Event>(), 16);
        assert_eq!(size_of::<LV2_Feature>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<LV2_Descriptor>(), size_of::<[usize; 8]>());
        assert_eq!(size_of::<LV2UI_Descriptor>(), 5 * size_of::<usize>());
        assert_eq!(size_of::<LV2_Options_Option>(), if size_of::<usize>() == 8 { 32 } else { 24 });
        assert_eq!(atom_pad(3), 8);
        assert_eq!(atom_pad(16), 16);
    }
}
