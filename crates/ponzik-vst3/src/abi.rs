//! Sous-ensemble de l'ABI VST 3, recopié à la main depuis les en-têtes officiels
//! (`pluginterfaces/base/{funknown,ipluginbase,ibstream}.h`,
//! `pluginterfaces/vst/{ivstcomponent,ivstaudioprocessor,ivsteditcontroller,
//! ivstparameterchanges,ivstevents,ivstmessage,ivstattributes,ivsthostapplication,
//! ivstprocesscontext}.h`, `pluginterfaces/gui/iplugview{,contentscalesupport}.h`).
//!
//! Chaque interface C++ est une structure dont le premier champ pointe sur sa
//! table virtuelle ; les tables héritées sont imbriquées en tête (`base`). Les
//! appels suivent `PLUGIN_API` : `__stdcall` sous Windows, convention C ailleurs,
//! soit `extern "system"`. L'alignement par défaut de `repr(C)` correspond au
//! `#pragma pack(16)` des en-têtes sur les plateformes 64 bits.

use std::ffi::{c_char, c_void};

pub type TResult = i32;
pub type TBool = u8;
pub type ParamId = u32;
pub type ParamValue = f64;
pub type SpeakerArrangement = u64;
/// Chaîne UTF-16 de 128 unités (`String128`).
pub type String128 = [u16; 128];

/// Identifiant d'interface ou de classe (`TUID`), dans l'ordre mémoire de la plateforme.
pub type Tuid = [u8; 16];

/// Équivalent de `INLINE_UID(l1, l2, l3, l4)`.
///
/// Sous Windows (`COM_COMPATIBLE`), la disposition est celle d'un GUID COM :
/// `l1` en petit-boutiste, les deux moitiés de `l2` en petit-boutiste, puis `l3`
/// et `l4` en gros-boutiste. Ailleurs, les quatre mots sont en gros-boutiste.
pub const fn uid(l1: u32, l2: u32, l3: u32, l4: u32) -> Tuid {
    let (a, b, c, d) = (l1.to_be_bytes(), l2.to_be_bytes(), l3.to_be_bytes(), l4.to_be_bytes());
    if cfg!(windows) {
        [a[3], a[2], a[1], a[0], b[1], b[0], b[3], b[2], c[0], c[1], c[2], c[3], d[0], d[1], d[2], d[3]]
    } else {
        [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3], c[0], c[1], c[2], c[3], d[0], d[1], d[2], d[3]]
    }
}

/// Inverse de [`uid`] : les quatre mots `l1..l4`, indépendants de la plateforme.
pub const fn uid_words(t: &Tuid) -> [u32; 4] {
    const fn be(t: &Tuid, i: usize) -> u32 {
        u32::from_be_bytes([t[i], t[i + 1], t[i + 2], t[i + 3]])
    }
    let l1 = if cfg!(windows) { u32::from_le_bytes([t[0], t[1], t[2], t[3]]) } else { be(t, 0) };
    let l2 = if cfg!(windows) { u32::from_be_bytes([t[5], t[4], t[7], t[6]]) } else { be(t, 4) };
    [l1, l2, be(t, 8), be(t, 12)]
}

// --- funknown.h : codes de retour -------------------------------------------

#[cfg(windows)]
mod results {
    use super::TResult;
    pub const K_NO_INTERFACE: TResult = 0x8000_4002_u32 as i32;
    pub const K_RESULT_OK: TResult = 0;
    pub const K_RESULT_FALSE: TResult = 1;
    pub const K_INVALID_ARGUMENT: TResult = 0x8007_0057_u32 as i32;
    pub const K_NOT_IMPLEMENTED: TResult = 0x8000_4001_u32 as i32;
    pub const K_INTERNAL_ERROR: TResult = 0x8000_4005_u32 as i32;
    pub const K_NOT_INITIALIZED: TResult = 0x8000_FFFF_u32 as i32;
    pub const K_OUT_OF_MEMORY: TResult = 0x8007_000E_u32 as i32;
}

#[cfg(not(windows))]
mod results {
    use super::TResult;
    pub const K_NO_INTERFACE: TResult = -1;
    pub const K_RESULT_OK: TResult = 0;
    pub const K_RESULT_FALSE: TResult = 1;
    pub const K_INVALID_ARGUMENT: TResult = 2;
    pub const K_NOT_IMPLEMENTED: TResult = 3;
    pub const K_INTERNAL_ERROR: TResult = 4;
    pub const K_NOT_INITIALIZED: TResult = 5;
    pub const K_OUT_OF_MEMORY: TResult = 6;
}

pub use results::*;
pub const K_RESULT_TRUE: TResult = K_RESULT_OK;

// --- Identifiants d'interfaces ------------------------------------------------

pub const FUNKNOWN_IID: Tuid = uid(0x00000000, 0x00000000, 0xC0000000, 0x00000046);
pub const IPLUGIN_BASE_IID: Tuid = uid(0x22888DDB, 0x156E45AE, 0x8358B348, 0x08190625);
pub const IPLUGIN_FACTORY_IID: Tuid = uid(0x7A4D811C, 0x52114A1F, 0xAED9D2EE, 0x0B43BF9F);
pub const IPLUGIN_FACTORY2_IID: Tuid = uid(0x0007B650, 0xF24B4C0B, 0xA464EDB9, 0xF00B2ABB);
pub const IPLUGIN_FACTORY3_IID: Tuid = uid(0x4555A2AB, 0xC1234E57, 0x9B122910, 0x36878931);
pub const IBSTREAM_IID: Tuid = uid(0xC3BF6EA2, 0x30994752, 0x9B6BF990, 0x1EE33E9B);
pub const ICOMPONENT_IID: Tuid = uid(0xE831FF31, 0xF2D54301, 0x928EBBEE, 0x25697802);
pub const IAUDIO_PROCESSOR_IID: Tuid = uid(0x42043F99, 0xB7DA453C, 0xA569E79D, 0x9AAEC33D);
pub const IEDIT_CONTROLLER_IID: Tuid = uid(0xDCD7BBE3, 0x7742448D, 0xA874AACC, 0x979C759E);
pub const ICOMPONENT_HANDLER_IID: Tuid = uid(0x93A0BEA3, 0x0BD045DB, 0x8E890B0C, 0xC1E46AC6);
pub const ICONNECTION_POINT_IID: Tuid = uid(0x70A4156F, 0x6E6E4026, 0x989148BF, 0xAA60D8D1);
pub const IMESSAGE_IID: Tuid = uid(0x936F033B, 0xC6C047DB, 0xBB0882F8, 0x13C1E613);
pub const IATTRIBUTE_LIST_IID: Tuid = uid(0x1E5F0AEB, 0xCC7F4533, 0xA2544011, 0x38AD5EE4);
pub const IHOST_APPLICATION_IID: Tuid = uid(0x58E595CC, 0xDB2D4969, 0x8B6AAF8C, 0x36A664E5);
pub const IPARAM_VALUE_QUEUE_IID: Tuid = uid(0x01263A18, 0xED074F6F, 0x98C9D356, 0x4686F9BA);
pub const IPARAMETER_CHANGES_IID: Tuid = uid(0xA4779663, 0x0BB64A56, 0xB44384A8, 0x466FEB9D);
pub const IEVENT_LIST_IID: Tuid = uid(0x3A2C4214, 0x346349FE, 0xB2C4F397, 0xB9695A44);
pub const IPLUG_VIEW_IID: Tuid = uid(0x5BC32507, 0xD06049EA, 0xA6151B52, 0x2B755B29);
pub const IPLUG_FRAME_IID: Tuid = uid(0x367FAF01, 0xAFA94693, 0x8D4DA2A0, 0xED0882A3);
pub const IPLUG_VIEW_CONTENT_SCALE_SUPPORT_IID: Tuid = uid(0x65ED9690, 0x8AC44525, 0x8AADEF7A, 0x72EA703F);
/// `Steinberg::Linux::IRunLoop`, obtenu par `queryInterface` sur l'`IPlugFrame`.
pub const IRUN_LOOP_IID: Tuid = uid(0x18C35366, 0x97764F1A, 0x9C5B8385, 0x7A871389);
pub const IEVENT_HANDLER_IID: Tuid = uid(0x561E65C9, 0x13A0496F, 0x813A2C35, 0x654D7983);
pub const IMIDI_MAPPING_IID: Tuid = uid(0xDF0FF9F7, 0x49B74669, 0xB63AB732, 0x7ADBF5E5);
pub const ITIMER_HANDLER_IID: Tuid = uid(0x10BDD94F, 0x41424774, 0x821FAD8F, 0xECA72CA9);

// --- Constantes ---------------------------------------------------------------

/// Catégorie de classe d'un processeur audio (`kVstAudioEffectClass`).
pub const K_VST_AUDIO_EFFECT_CLASS: &str = "Audio Module Class";
/// Catégorie de classe d'un contrôleur séparé (`kVstComponentControllerClass`).
pub const K_VST_COMPONENT_CONTROLLER_CLASS: &str = "Component Controller Class";

pub const K_AUDIO: i32 = 0;
pub const K_EVENT: i32 = 1;
pub const K_INPUT: i32 = 0;
pub const K_OUTPUT: i32 = 1;
pub const K_MAIN: i32 = 0;
pub const K_AUX: i32 = 1;
pub const K_DEFAULT_ACTIVE: u32 = 1;

pub const K_REALTIME: i32 = 0;
pub const K_OFFLINE: i32 = 2;
pub const K_SAMPLE32: i32 = 0;
pub const K_SAMPLE64: i32 = 1;

pub const K_SPEAKER_L: u64 = 1 << 0;
pub const K_SPEAKER_R: u64 = 1 << 1;
pub const K_SPEAKER_M: u64 = 1 << 19;
pub const K_STEREO: SpeakerArrangement = K_SPEAKER_L | K_SPEAKER_R;
pub const K_MONO: SpeakerArrangement = K_SPEAKER_M;

pub const K_NO_PARAM_ID: ParamId = 0xFFFF_FFFF;
pub const K_CAN_AUTOMATE: i32 = 1 << 0;
pub const K_IS_READ_ONLY: i32 = 1 << 1;
pub const K_IS_LIST: i32 = 1 << 3;
pub const K_IS_HIDDEN: i32 = 1 << 4;
pub const K_IS_BYPASS: i32 = 1 << 16;

pub const K_IS_LIVE: u16 = 1 << 0;
pub const K_NOTE_ON_EVENT: u16 = 0;
pub const K_NOTE_OFF_EVENT: u16 = 1;
pub const K_NOTE_EXPRESSION_VALUE_EVENT: u16 = 4;
/// Note expression types: tuning (normalized 0.5 + semitones / 240) and pan.
pub const K_TUNING_TYPE_ID: u32 = 2;
pub const K_PAN_TYPE_ID: u32 = 3;
/// `IMidiMapping` controller numbers past the 128 CCs.
pub const K_PITCH_BEND: i16 = 129;

pub const K_IB_SEEK_SET: i32 = 0;
pub const K_IB_SEEK_CUR: i32 = 1;
pub const K_IB_SEEK_END: i32 = 2;

pub const K_PLAYING: u32 = 1 << 1;
pub const K_TEMPO_VALID: u32 = 1 << 10;
pub const K_CONT_TIME_VALID: u32 = 1 << 17;

pub const K_MANY_INSTANCES: i32 = 0x7FFF_FFFF;

/// `IComponentHandler::restartComponent` : les valeurs des paramètres ont changé.
pub const K_PARAM_VALUES_CHANGED: i32 = 1 << 2;

/// Types de fenêtres de `IPlugView::isPlatformTypeSupported` / `attached`.
pub const K_PLATFORM_TYPE_HWND: &[u8] = b"HWND\0";
pub const K_PLATFORM_TYPE_NSVIEW: &[u8] = b"NSView\0";
pub const K_PLATFORM_TYPE_X11_EMBED_WINDOW_ID: &[u8] = b"X11EmbedWindowID\0";
/// Nom de la vue principale pour `IEditController::createView`.
pub const VIEW_TYPE_EDITOR: &[u8] = b"editor\0";

// --- funknown.h ---------------------------------------------------------------

#[repr(C)]
pub struct FUnknownVtbl {
    pub query_interface:
        unsafe extern "system" fn(this: *mut c_void, iid: *const Tuid, obj: *mut *mut c_void) -> TResult,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

/// Déclare une interface : un pointeur de table virtuelle en tête d'objet.
macro_rules! interface {
    ($($name:ident => $vtbl:ident, $iid:expr;)*) => {$(
        #[repr(C)]
        pub struct $name {
            pub vtbl: *const $vtbl,
        }

        impl Interface for $name {
            const IID: Tuid = $iid;
        }
    )*};
}

/// Interface COM dont on connaît l'identifiant.
pub trait Interface {
    const IID: Tuid;
}

interface! {
    FUnknown => FUnknownVtbl, FUNKNOWN_IID;
    IPluginBase => IPluginBaseVtbl, IPLUGIN_BASE_IID;
    IPluginFactory => IPluginFactoryVtbl, IPLUGIN_FACTORY_IID;
    IPluginFactory2 => IPluginFactory2Vtbl, IPLUGIN_FACTORY2_IID;
    IPluginFactory3 => IPluginFactory3Vtbl, IPLUGIN_FACTORY3_IID;
    IBStream => IBStreamVtbl, IBSTREAM_IID;
    IComponent => IComponentVtbl, ICOMPONENT_IID;
    IAudioProcessor => IAudioProcessorVtbl, IAUDIO_PROCESSOR_IID;
    IEditController => IEditControllerVtbl, IEDIT_CONTROLLER_IID;
    IMidiMapping => IMidiMappingVtbl, IMIDI_MAPPING_IID;
    IComponentHandler => IComponentHandlerVtbl, ICOMPONENT_HANDLER_IID;
    IConnectionPoint => IConnectionPointVtbl, ICONNECTION_POINT_IID;
    IMessage => IMessageVtbl, IMESSAGE_IID;
    IAttributeList => IAttributeListVtbl, IATTRIBUTE_LIST_IID;
    IHostApplication => IHostApplicationVtbl, IHOST_APPLICATION_IID;
    IParamValueQueue => IParamValueQueueVtbl, IPARAM_VALUE_QUEUE_IID;
    IParameterChanges => IParameterChangesVtbl, IPARAMETER_CHANGES_IID;
    IEventList => IEventListVtbl, IEVENT_LIST_IID;
    IPlugView => IPlugViewVtbl, IPLUG_VIEW_IID;
    IPlugFrame => IPlugFrameVtbl, IPLUG_FRAME_IID;
    IPlugViewContentScaleSupport => IPlugViewContentScaleSupportVtbl, IPLUG_VIEW_CONTENT_SCALE_SUPPORT_IID;
    IRunLoop => IRunLoopVtbl, IRUN_LOOP_IID;
    IEventHandler => IEventHandlerVtbl, IEVENT_HANDLER_IID;
    ITimerHandler => ITimerHandlerVtbl, ITIMER_HANDLER_IID;
}

// --- ipluginbase.h ------------------------------------------------------------

#[repr(C)]
pub struct IPluginBaseVtbl {
    pub unknown: FUnknownVtbl,
    pub initialize: unsafe extern "system" fn(this: *mut c_void, context: *mut FUnknown) -> TResult,
    pub terminate: unsafe extern "system" fn(this: *mut c_void) -> TResult,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PFactoryInfo {
    pub vendor: [u8; 64],
    pub url: [u8; 256],
    pub email: [u8; 128],
    pub flags: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfo {
    pub cid: Tuid,
    pub cardinality: i32,
    pub category: [u8; 32],
    pub name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfo2 {
    pub cid: Tuid,
    pub cardinality: i32,
    pub category: [u8; 32],
    pub name: [u8; 64],
    pub class_flags: u32,
    pub sub_categories: [u8; 128],
    pub vendor: [u8; 64],
    pub version: [u8; 64],
    pub sdk_version: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfoW {
    pub cid: Tuid,
    pub cardinality: i32,
    pub category: [u8; 32],
    pub name: [u16; 64],
    pub class_flags: u32,
    pub sub_categories: [u8; 128],
    pub vendor: [u16; 64],
    pub version: [u16; 64],
    pub sdk_version: [u16; 64],
}

#[repr(C)]
pub struct IPluginFactoryVtbl {
    pub unknown: FUnknownVtbl,
    pub get_factory_info: unsafe extern "system" fn(this: *mut c_void, info: *mut PFactoryInfo) -> TResult,
    pub count_classes: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_class_info: unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut PClassInfo) -> TResult,
    pub create_instance: unsafe extern "system" fn(
        this: *mut c_void,
        cid: *const c_char,
        iid: *const c_char,
        obj: *mut *mut c_void,
    ) -> TResult,
}

#[repr(C)]
pub struct IPluginFactory2Vtbl {
    pub base: IPluginFactoryVtbl,
    pub get_class_info2: unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut PClassInfo2) -> TResult,
}

#[repr(C)]
pub struct IPluginFactory3Vtbl {
    pub base: IPluginFactory2Vtbl,
    pub get_class_info_unicode:
        unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut PClassInfoW) -> TResult,
    pub set_host_context: unsafe extern "system" fn(this: *mut c_void, context: *mut FUnknown) -> TResult,
}

/// `GetPluginFactory`, exporté par tout module VST 3.
pub type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;
/// `ModuleEntry` (Linux), reçoit la poignée `dlopen`.
pub type ModuleEntryFn = unsafe extern "system" fn(handle: *mut c_void) -> bool;
/// `ModuleExit` (Linux), `InitDll` et `ExitDll` (Windows).
pub type ModuleHookFn = unsafe extern "system" fn() -> bool;

// --- ibstream.h ---------------------------------------------------------------

#[repr(C)]
pub struct IBStreamVtbl {
    pub unknown: FUnknownVtbl,
    pub read: unsafe extern "system" fn(this: *mut c_void, buffer: *mut c_void, bytes: i32, read: *mut i32) -> TResult,
    pub write:
        unsafe extern "system" fn(this: *mut c_void, buffer: *mut c_void, bytes: i32, written: *mut i32) -> TResult,
    pub seek: unsafe extern "system" fn(this: *mut c_void, pos: i64, mode: i32, result: *mut i64) -> TResult,
    pub tell: unsafe extern "system" fn(this: *mut c_void, pos: *mut i64) -> TResult,
}

// --- ivstcomponent.h ----------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BusInfo {
    pub media_type: i32,
    pub direction: i32,
    pub channel_count: i32,
    pub name: String128,
    pub bus_type: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RoutingInfo {
    pub media_type: i32,
    pub bus_index: i32,
    pub channel: i32,
}

#[repr(C)]
pub struct IComponentVtbl {
    pub base: IPluginBaseVtbl,
    pub get_controller_class_id: unsafe extern "system" fn(this: *mut c_void, cid: *mut Tuid) -> TResult,
    pub set_io_mode: unsafe extern "system" fn(this: *mut c_void, mode: i32) -> TResult,
    pub get_bus_count: unsafe extern "system" fn(this: *mut c_void, kind: i32, dir: i32) -> i32,
    pub get_bus_info:
        unsafe extern "system" fn(this: *mut c_void, kind: i32, dir: i32, index: i32, bus: *mut BusInfo) -> TResult,
    pub get_routing_info:
        unsafe extern "system" fn(this: *mut c_void, input: *mut RoutingInfo, output: *mut RoutingInfo) -> TResult,
    pub activate_bus:
        unsafe extern "system" fn(this: *mut c_void, kind: i32, dir: i32, index: i32, state: TBool) -> TResult,
    pub set_active: unsafe extern "system" fn(this: *mut c_void, state: TBool) -> TResult,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut IBStream) -> TResult,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut IBStream) -> TResult,
}

// --- ivstaudioprocessor.h -----------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessSetup {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub max_samples_per_block: i32,
    pub sample_rate: f64,
}

/// Tampons d'un bus ; seule la variante 32 bits de l'union est utilisée.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBusBuffers {
    pub num_channels: i32,
    pub silence_flags: u64,
    pub channel_buffers32: *mut *mut f32,
}

#[repr(C)]
pub struct ProcessData {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub num_samples: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub inputs: *mut AudioBusBuffers,
    pub outputs: *mut AudioBusBuffers,
    pub input_parameter_changes: *mut IParameterChanges,
    pub output_parameter_changes: *mut IParameterChanges,
    pub input_events: *mut IEventList,
    pub output_events: *mut IEventList,
    pub process_context: *mut ProcessContext,
}

#[repr(C)]
pub struct IAudioProcessorVtbl {
    pub unknown: FUnknownVtbl,
    pub set_bus_arrangements: unsafe extern "system" fn(
        this: *mut c_void,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> TResult,
    pub get_bus_arrangement:
        unsafe extern "system" fn(this: *mut c_void, dir: i32, index: i32, arr: *mut SpeakerArrangement) -> TResult,
    pub can_process_sample_size: unsafe extern "system" fn(this: *mut c_void, size: i32) -> TResult,
    pub get_latency_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub setup_processing: unsafe extern "system" fn(this: *mut c_void, setup: *mut ProcessSetup) -> TResult,
    pub set_processing: unsafe extern "system" fn(this: *mut c_void, state: TBool) -> TResult,
    pub process: unsafe extern "system" fn(this: *mut c_void, data: *mut ProcessData) -> TResult,
    pub get_tail_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

// --- ivstprocesscontext.h -----------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Chord {
    pub key_note: u8,
    pub root_note: u8,
    pub chord_mask: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameRate {
    pub frames_per_second: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ProcessContext {
    pub state: u32,
    pub sample_rate: f64,
    pub project_time_samples: i64,
    pub system_time: i64,
    pub continous_time_samples: i64,
    pub project_time_music: f64,
    pub bar_position_music: f64,
    pub cycle_start_music: f64,
    pub cycle_end_music: f64,
    pub tempo: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub chord: Chord,
    pub smpte_offset_subframes: i32,
    pub frame_rate: FrameRate,
    pub samples_to_next_clock: i32,
}

// --- ivsteditcontroller.h -----------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParameterInfo {
    pub id: ParamId,
    pub title: String128,
    pub short_title: String128,
    pub units: String128,
    pub step_count: i32,
    pub default_normalized_value: ParamValue,
    pub unit_id: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct IEditControllerVtbl {
    pub base: IPluginBaseVtbl,
    pub set_component_state: unsafe extern "system" fn(this: *mut c_void, state: *mut IBStream) -> TResult,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut IBStream) -> TResult,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut IBStream) -> TResult,
    pub get_parameter_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_parameter_info:
        unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut ParameterInfo) -> TResult,
    pub get_param_string_by_value:
        unsafe extern "system" fn(this: *mut c_void, id: ParamId, value: ParamValue, out: *mut u16) -> TResult,
    pub get_param_value_by_string:
        unsafe extern "system" fn(this: *mut c_void, id: ParamId, text: *const u16, value: *mut ParamValue) -> TResult,
    pub normalized_param_to_plain:
        unsafe extern "system" fn(this: *mut c_void, id: ParamId, v: ParamValue) -> ParamValue,
    pub plain_param_to_normalized:
        unsafe extern "system" fn(this: *mut c_void, id: ParamId, v: ParamValue) -> ParamValue,
    pub get_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: ParamId) -> ParamValue,
    pub set_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: ParamId, v: ParamValue) -> TResult,
    pub set_component_handler: unsafe extern "system" fn(this: *mut c_void, handler: *mut IComponentHandler) -> TResult,
    pub create_view: unsafe extern "system" fn(this: *mut c_void, name: *const c_char) -> *mut c_void,
}

#[repr(C)]
pub struct IComponentHandlerVtbl {
    pub unknown: FUnknownVtbl,
    pub begin_edit: unsafe extern "system" fn(this: *mut c_void, id: ParamId) -> TResult,
    pub perform_edit: unsafe extern "system" fn(this: *mut c_void, id: ParamId, value: ParamValue) -> TResult,
    pub end_edit: unsafe extern "system" fn(this: *mut c_void, id: ParamId) -> TResult,
    pub restart_component: unsafe extern "system" fn(this: *mut c_void, flags: i32) -> TResult,
}

// --- ivstmessage.h, ivstattributes.h -----------------------------------------

/// `IMidiMapping`: the parameter a MIDI controller drives.
#[repr(C)]
pub struct IMidiMappingVtbl {
    pub unknown: FUnknownVtbl,
    pub get_midi_controller_assignment:
        unsafe extern "system" fn(this: *mut c_void, bus_index: i32, channel: i16, controller: i16, id: *mut ParamId) -> TResult,
}

#[repr(C)]
pub struct IConnectionPointVtbl {
    pub unknown: FUnknownVtbl,
    pub connect: unsafe extern "system" fn(this: *mut c_void, other: *mut IConnectionPoint) -> TResult,
    pub disconnect: unsafe extern "system" fn(this: *mut c_void, other: *mut IConnectionPoint) -> TResult,
    pub notify: unsafe extern "system" fn(this: *mut c_void, message: *mut IMessage) -> TResult,
}

#[repr(C)]
pub struct IMessageVtbl {
    pub unknown: FUnknownVtbl,
    pub get_message_id: unsafe extern "system" fn(this: *mut c_void) -> *const c_char,
    pub set_message_id: unsafe extern "system" fn(this: *mut c_void, id: *const c_char),
    pub get_attributes: unsafe extern "system" fn(this: *mut c_void) -> *mut IAttributeList,
}

#[repr(C)]
pub struct IAttributeListVtbl {
    pub unknown: FUnknownVtbl,
    pub set_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: i64) -> TResult,
    pub get_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut i64) -> TResult,
    pub set_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: f64) -> TResult,
    pub get_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut f64) -> TResult,
    pub set_string: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, text: *const u16) -> TResult,
    pub get_string:
        unsafe extern "system" fn(this: *mut c_void, id: *const c_char, text: *mut u16, size_bytes: u32) -> TResult,
    pub set_binary:
        unsafe extern "system" fn(this: *mut c_void, id: *const c_char, data: *const c_void, size: u32) -> TResult,
    pub get_binary: unsafe extern "system" fn(
        this: *mut c_void,
        id: *const c_char,
        data: *mut *const c_void,
        size: *mut u32,
    ) -> TResult,
}

// --- ivsthostapplication.h ----------------------------------------------------

#[repr(C)]
pub struct IHostApplicationVtbl {
    pub unknown: FUnknownVtbl,
    pub get_name: unsafe extern "system" fn(this: *mut c_void, name: *mut u16) -> TResult,
    pub create_instance: unsafe extern "system" fn(
        this: *mut c_void,
        cid: *const Tuid,
        iid: *const Tuid,
        obj: *mut *mut c_void,
    ) -> TResult,
}

// --- ivstparameterchanges.h ---------------------------------------------------

#[repr(C)]
pub struct IParamValueQueueVtbl {
    pub unknown: FUnknownVtbl,
    pub get_parameter_id: unsafe extern "system" fn(this: *mut c_void) -> ParamId,
    pub get_point_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_point:
        unsafe extern "system" fn(this: *mut c_void, index: i32, offset: *mut i32, value: *mut ParamValue) -> TResult,
    pub add_point:
        unsafe extern "system" fn(this: *mut c_void, offset: i32, value: ParamValue, index: *mut i32) -> TResult,
}

#[repr(C)]
pub struct IParameterChangesVtbl {
    pub unknown: FUnknownVtbl,
    pub get_parameter_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_parameter_data: unsafe extern "system" fn(this: *mut c_void, index: i32) -> *mut IParamValueQueue,
    pub add_parameter_data:
        unsafe extern "system" fn(this: *mut c_void, id: *const ParamId, index: *mut i32) -> *mut IParamValueQueue,
}

// --- ivstevents.h -------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NoteOnEvent {
    pub channel: i16,
    pub pitch: i16,
    pub tuning: f32,
    pub velocity: f32,
    pub length: i32,
    pub note_id: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NoteOffEvent {
    pub channel: i16,
    pub pitch: i16,
    pub velocity: f32,
    pub note_id: i32,
    pub tuning: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NoteExpressionValueEvent {
    pub type_id: u32,
    pub note_id: i32,
    pub value: f64,
}

/// Union des charges utiles ; `raw` en fixe la taille (24 octets) et l'alignement.
#[repr(C)]
#[derive(Clone, Copy)]
pub union EventData {
    pub note_on: NoteOnEvent,
    pub note_off: NoteOffEvent,
    pub expression: NoteExpressionValueEvent,
    pub raw: [u64; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Event {
    pub bus_index: i32,
    pub sample_offset: i32,
    pub ppq_position: f64,
    pub flags: u16,
    pub kind: u16,
    pub data: EventData,
}

#[repr(C)]
pub struct IEventListVtbl {
    pub unknown: FUnknownVtbl,
    pub get_event_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_event: unsafe extern "system" fn(this: *mut c_void, index: i32, event: *mut Event) -> TResult,
    pub add_event: unsafe extern "system" fn(this: *mut c_void, event: *mut Event) -> TResult,
}

// --- gui/iplugview.h, iplugviewcontentscalesupport.h ---------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ViewRect {
    pub fn sized(width: u32, height: u32) -> Self {
        ViewRect { left: 0, top: 0, right: width as i32, bottom: height as i32 }
    }

    pub fn width(&self) -> u32 {
        (self.right - self.left).max(0) as u32
    }

    pub fn height(&self) -> u32 {
        (self.bottom - self.top).max(0) as u32
    }
}

#[repr(C)]
pub struct IPlugViewVtbl {
    pub unknown: FUnknownVtbl,
    pub is_platform_type_supported: unsafe extern "system" fn(this: *mut c_void, kind: *const c_char) -> TResult,
    pub attached: unsafe extern "system" fn(this: *mut c_void, parent: *mut c_void, kind: *const c_char) -> TResult,
    pub removed: unsafe extern "system" fn(this: *mut c_void) -> TResult,
    pub on_wheel: unsafe extern "system" fn(this: *mut c_void, distance: f32) -> TResult,
    pub on_key_down: unsafe extern "system" fn(this: *mut c_void, key: u16, code: i16, modifiers: i16) -> TResult,
    pub on_key_up: unsafe extern "system" fn(this: *mut c_void, key: u16, code: i16, modifiers: i16) -> TResult,
    pub get_size: unsafe extern "system" fn(this: *mut c_void, size: *mut ViewRect) -> TResult,
    pub on_size: unsafe extern "system" fn(this: *mut c_void, size: *mut ViewRect) -> TResult,
    pub on_focus: unsafe extern "system" fn(this: *mut c_void, state: TBool) -> TResult,
    pub set_frame: unsafe extern "system" fn(this: *mut c_void, frame: *mut IPlugFrame) -> TResult,
    pub can_resize: unsafe extern "system" fn(this: *mut c_void) -> TResult,
    pub check_size_constraint: unsafe extern "system" fn(this: *mut c_void, rect: *mut ViewRect) -> TResult,
}

#[repr(C)]
pub struct IPlugFrameVtbl {
    pub unknown: FUnknownVtbl,
    pub resize_view:
        unsafe extern "system" fn(this: *mut c_void, view: *mut IPlugView, size: *mut ViewRect) -> TResult,
}

#[repr(C)]
pub struct IPlugViewContentScaleSupportVtbl {
    pub unknown: FUnknownVtbl,
    pub set_content_scale_factor: unsafe extern "system" fn(this: *mut c_void, factor: f32) -> TResult,
}

// --- gui/iplugview.h, `Steinberg::Linux` ---------------------------------------

#[repr(C)]
pub struct IRunLoopVtbl {
    pub unknown: FUnknownVtbl,
    pub register_event_handler:
        unsafe extern "system" fn(this: *mut c_void, handler: *mut IEventHandler, fd: i32) -> TResult,
    pub unregister_event_handler: unsafe extern "system" fn(this: *mut c_void, handler: *mut IEventHandler) -> TResult,
    pub register_timer: unsafe extern "system" fn(this: *mut c_void, handler: *mut ITimerHandler, ms: u64) -> TResult,
    pub unregister_timer: unsafe extern "system" fn(this: *mut c_void, handler: *mut ITimerHandler) -> TResult,
}

#[repr(C)]
pub struct IEventHandlerVtbl {
    pub unknown: FUnknownVtbl,
    pub on_fd_is_set: unsafe extern "system" fn(this: *mut c_void, fd: i32),
}

#[repr(C)]
pub struct ITimerHandlerVtbl {
    pub unknown: FUnknownVtbl,
    pub on_timer: unsafe extern "system" fn(this: *mut c_void),
}

// Tailles des en-têtes C++ sur les plateformes 64 bits.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<PClassInfo>() == 116);
    assert!(size_of::<PClassInfo2>() == 440);
    assert!(size_of::<PClassInfoW>() == 696);
    assert!(size_of::<BusInfo>() == 276);
    assert!(size_of::<AudioBusBuffers>() == 24);
    assert!(size_of::<ProcessSetup>() == 24);
    assert!(size_of::<ProcessData>() == 80);
    assert!(size_of::<ParameterInfo>() == 792);
    assert!(size_of::<Event>() == 48);
    assert!(size_of::<ProcessContext>() == 112);
    assert!(size_of::<ViewRect>() == 16);
};

/// Copie une chaîne ASCII/UTF-8 dans un tableau C de taille fixe (tronquée, terminée par zéro).
pub fn copy_cstr(dst: &mut [u8], src: &str) {
    let n = src.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&src.as_bytes()[..n]);
    dst[n..].fill(0);
}

/// Copie une chaîne dans un tableau UTF-16 de taille fixe (tronquée, terminée par zéro).
pub fn copy_wstr(dst: &mut [u16], src: &str) {
    let mut n = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s;
        n += 1;
    }
    let n = n.min(dst.len().saturating_sub(1));
    dst[n..].fill(0);
}

/// Lit une chaîne C de taille fixe.
pub fn read_cstr(src: &[u8]) -> String {
    let n = src.iter().position(|&c| c == 0).unwrap_or(src.len());
    String::from_utf8_lossy(&src[..n]).into_owned()
}

/// Lit une chaîne UTF-16 de taille fixe.
pub fn read_wstr(src: &[u16]) -> String {
    let n = src.iter().position(|&c| c == 0).unwrap_or(src.len());
    String::from_utf16_lossy(&src[..n])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_layout() {
        let t = uid(0x01020304, 0x05060708, 0x090A0B0C, 0x0D0E0F10);
        if cfg!(windows) {
            assert_eq!(t, [4, 3, 2, 1, 6, 5, 8, 7, 9, 10, 11, 12, 13, 14, 15, 16]);
        } else {
            assert_eq!(t, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        }
        assert_eq!(uid_words(&t), [0x01020304, 0x05060708, 0x090A0B0C, 0x0D0E0F10]);
        // FUnknown est le GUID COM d'IUnknown {00000000-0000-0000-C000-000000000046}.
        if cfg!(windows) {
            assert_eq!(FUNKNOWN_IID, [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
        }
    }

    #[test]
    fn fixed_strings() {
        let mut a = [0xFFu8; 4];
        copy_cstr(&mut a, "abcdef");
        assert_eq!(read_cstr(&a), "abc");
        let mut w = [0xFFFFu16; 8];
        copy_wstr(&mut w, "été");
        assert_eq!(read_wstr(&w), "été");
    }
}
