//! Sous-ensemble de l'ABI C de CLAP 1.x, recopié à la main depuis les en-têtes
//! officiels (`entry.h`, `plugin-factory.h`, `plugin.h`, `host.h`, `process.h`,
//! `events.h`, `audio-buffer.h`, `ext/params.h`, `ext/audio-ports.h`,
//! `ext/note-ports.h`, `ext/latency.h`). Les noms suivent ceux du C, en casse Rust.

use std::ffi::{c_char, c_void};

pub type ClapId = u32;
pub const CLAP_INVALID_ID: ClapId = u32::MAX;
pub const CLAP_NAME_SIZE: usize = 256;
pub const CLAP_PATH_SIZE: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ClapVersion {
    pub major: u32,
    pub minor: u32,
    pub revision: u32,
}

pub const CLAP_VERSION: ClapVersion = ClapVersion { major: 1, minor: 2, revision: 0 };

// --- entry.h ---------------------------------------------------------------

/// Symbole exporté `clap_entry`.
#[repr(C)]
pub struct ClapPluginEntry {
    pub clap_version: ClapVersion,
    pub init: unsafe extern "C" fn(plugin_path: *const c_char) -> bool,
    pub deinit: unsafe extern "C" fn(),
    pub get_factory: unsafe extern "C" fn(factory_id: *const c_char) -> *const c_void,
}

// --- factory/plugin-factory.h ---------------------------------------------

pub const CLAP_PLUGIN_FACTORY_ID: &[u8] = b"clap.plugin-factory\0";

#[repr(C)]
pub struct ClapPluginFactory {
    pub get_plugin_count: unsafe extern "C" fn(factory: *const ClapPluginFactory) -> u32,
    pub get_plugin_descriptor:
        unsafe extern "C" fn(factory: *const ClapPluginFactory, index: u32) -> *const ClapPluginDescriptor,
    pub create_plugin: unsafe extern "C" fn(
        factory: *const ClapPluginFactory,
        host: *const ClapHost,
        plugin_id: *const c_char,
    ) -> *const ClapPlugin,
}

// --- plugin.h --------------------------------------------------------------

#[repr(C)]
pub struct ClapPluginDescriptor {
    pub clap_version: ClapVersion,
    pub id: *const c_char,
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub url: *const c_char,
    pub manual_url: *const c_char,
    pub support_url: *const c_char,
    pub version: *const c_char,
    pub description: *const c_char,
    /// Tableau terminé par un pointeur nul (« instrument », « audio-effect »…).
    pub features: *const *const c_char,
}

#[repr(C)]
pub struct ClapPlugin {
    pub desc: *const ClapPluginDescriptor,
    pub plugin_data: *mut c_void,
    pub init: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    pub destroy: unsafe extern "C" fn(plugin: *const ClapPlugin),
    pub activate: unsafe extern "C" fn(plugin: *const ClapPlugin, sample_rate: f64, min: u32, max: u32) -> bool,
    pub deactivate: unsafe extern "C" fn(plugin: *const ClapPlugin),
    pub start_processing: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    pub stop_processing: unsafe extern "C" fn(plugin: *const ClapPlugin),
    pub reset: unsafe extern "C" fn(plugin: *const ClapPlugin),
    pub process: unsafe extern "C" fn(plugin: *const ClapPlugin, process: *const ClapProcess) -> ClapProcessStatus,
    pub get_extension: unsafe extern "C" fn(plugin: *const ClapPlugin, id: *const c_char) -> *const c_void,
    pub on_main_thread: unsafe extern "C" fn(plugin: *const ClapPlugin),
}

// --- host.h ----------------------------------------------------------------

#[repr(C)]
pub struct ClapHost {
    pub clap_version: ClapVersion,
    pub host_data: *mut c_void,
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub url: *const c_char,
    pub version: *const c_char,
    pub get_extension: unsafe extern "C" fn(host: *const ClapHost, extension_id: *const c_char) -> *const c_void,
    pub request_restart: unsafe extern "C" fn(host: *const ClapHost),
    pub request_process: unsafe extern "C" fn(host: *const ClapHost),
    pub request_callback: unsafe extern "C" fn(host: *const ClapHost),
}

// --- process.h / audio-buffer.h -------------------------------------------

pub type ClapProcessStatus = i32;
pub const CLAP_PROCESS_ERROR: ClapProcessStatus = 0;
pub const CLAP_PROCESS_CONTINUE: ClapProcessStatus = 1;

#[repr(C)]
pub struct ClapAudioBuffer {
    pub data32: *mut *mut f32,
    pub data64: *mut *mut f64,
    pub channel_count: u32,
    pub latency: u32,
    pub constant_mask: u64,
}

#[repr(C)]
pub struct ClapProcess {
    pub steady_time: i64,
    pub frames_count: u32,
    /// `clap_event_transport_t*`, toujours nul ici.
    pub transport: *const c_void,
    pub audio_inputs: *const ClapAudioBuffer,
    pub audio_outputs: *mut ClapAudioBuffer,
    pub audio_inputs_count: u32,
    pub audio_outputs_count: u32,
    pub in_events: *const ClapInputEvents,
    pub out_events: *const ClapOutputEvents,
}

// --- events.h --------------------------------------------------------------

pub const CLAP_CORE_EVENT_SPACE_ID: u16 = 0;
pub const CLAP_EVENT_IS_LIVE: u32 = 1;
pub const CLAP_EVENT_NOTE_ON: u16 = 0;
pub const CLAP_EVENT_NOTE_OFF: u16 = 1;
pub const CLAP_EVENT_NOTE_CHOKE: u16 = 2;
pub const CLAP_EVENT_PARAM_VALUE: u16 = 5;
pub const CLAP_EVENT_MIDI: u16 = 10;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapEventHeader {
    pub size: u32,
    pub time: u32,
    pub space_id: u16,
    pub kind: u16,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapEventNote {
    pub header: ClapEventHeader,
    pub note_id: i32,
    pub port_index: i16,
    pub channel: i16,
    pub key: i16,
    pub velocity: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapEventParamValue {
    pub header: ClapEventHeader,
    pub param_id: ClapId,
    pub cookie: *mut c_void,
    pub note_id: i32,
    pub port_index: i16,
    pub channel: i16,
    pub key: i16,
    pub value: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapEventMidi {
    pub header: ClapEventHeader,
    pub port_index: u16,
    pub data: [u8; 3],
}

#[repr(C)]
pub struct ClapInputEvents {
    pub ctx: *mut c_void,
    pub size: unsafe extern "C" fn(list: *const ClapInputEvents) -> u32,
    pub get: unsafe extern "C" fn(list: *const ClapInputEvents, index: u32) -> *const ClapEventHeader,
}

#[repr(C)]
pub struct ClapOutputEvents {
    pub ctx: *mut c_void,
    pub try_push: unsafe extern "C" fn(list: *const ClapOutputEvents, event: *const ClapEventHeader) -> bool,
}

// --- ext/params.h ----------------------------------------------------------

pub const CLAP_EXT_PARAMS: &[u8] = b"clap.params\0";
pub const CLAP_PARAM_IS_STEPPED: u32 = 1 << 0;
pub const CLAP_PARAM_IS_HIDDEN: u32 = 1 << 2;

#[repr(C)]
pub struct ClapParamInfo {
    pub id: ClapId,
    pub flags: u32,
    pub cookie: *mut c_void,
    pub name: [c_char; CLAP_NAME_SIZE],
    pub module: [c_char; CLAP_PATH_SIZE],
    pub min_value: f64,
    pub max_value: f64,
    pub default_value: f64,
}

#[repr(C)]
pub struct ClapPluginParams {
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin) -> u32,
    pub get_info: unsafe extern "C" fn(plugin: *const ClapPlugin, index: u32, info: *mut ClapParamInfo) -> bool,
    pub get_value: unsafe extern "C" fn(plugin: *const ClapPlugin, id: ClapId, out: *mut f64) -> bool,
    pub value_to_text:
        unsafe extern "C" fn(plugin: *const ClapPlugin, id: ClapId, value: f64, out: *mut c_char, cap: u32) -> bool,
    pub text_to_value:
        unsafe extern "C" fn(plugin: *const ClapPlugin, id: ClapId, text: *const c_char, out: *mut f64) -> bool,
    pub flush:
        unsafe extern "C" fn(plugin: *const ClapPlugin, input: *const ClapInputEvents, out: *const ClapOutputEvents),
}

// --- ext/audio-ports.h -----------------------------------------------------

pub const CLAP_EXT_AUDIO_PORTS: &[u8] = b"clap.audio-ports\0";

#[repr(C)]
pub struct ClapAudioPortInfo {
    pub id: ClapId,
    pub name: [c_char; CLAP_NAME_SIZE],
    pub flags: u32,
    pub channel_count: u32,
    pub port_type: *const c_char,
    pub in_place_pair: ClapId,
}

#[repr(C)]
pub struct ClapPluginAudioPorts {
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin, is_input: bool) -> u32,
    pub get: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        index: u32,
        is_input: bool,
        info: *mut ClapAudioPortInfo,
    ) -> bool,
}

// --- ext/note-ports.h ------------------------------------------------------

pub const CLAP_EXT_NOTE_PORTS: &[u8] = b"clap.note-ports\0";
pub const CLAP_NOTE_DIALECT_CLAP: u32 = 1 << 0;
pub const CLAP_NOTE_DIALECT_MIDI: u32 = 1 << 1;

#[repr(C)]
pub struct ClapNotePortInfo {
    pub id: ClapId,
    pub supported_dialects: u32,
    pub preferred_dialect: u32,
    pub name: [c_char; CLAP_NAME_SIZE],
}

#[repr(C)]
pub struct ClapPluginNotePorts {
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin, is_input: bool) -> u32,
    pub get: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        index: u32,
        is_input: bool,
        info: *mut ClapNotePortInfo,
    ) -> bool,
}

// --- ext/latency.h ---------------------------------------------------------

pub const CLAP_EXT_LATENCY: &[u8] = b"clap.latency\0";

/// Latence du plugin, en échantillons ; lue sur le thread principal, plugin actif.
#[repr(C)]
pub struct ClapPluginLatency {
    pub get: unsafe extern "C" fn(plugin: *const ClapPlugin) -> u32,
}
