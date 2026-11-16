//! Pilote WASAPI : interfaces COM déclarées à la main, sans crate `windows`.
//!
//! Mode partagé piloté par événement. Quand Windows le permet (10 et plus,
//! `IAudioClient3`), le flux est ouvert en « faible latence partagée » : la
//! plus petite période du moteur audio qui tient la latence visée, sans mode
//! exclusif ni pilote tiers (ASIO4ALL). Tout ce qui touche COM vit sur le thread
//! audio, ouvert dès l'ouverture : le périphérique reste réservé jusqu'au
//! lancement et l'appartement COM du thread appelant (souvent STA côté
//! interface) n'est jamais touché.
//!
//! En duplex, la capture garde son thread et dépose ses trames dans une file
//! sans verrou que le thread de sortie relit avant chaque rendu.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use crate::{
    Attachment, AudioError, AudioThread, CaptureFn, CaptureSource, DeviceInfo, Duplex, InputFifo, OutputConfig, RingConsumer,
    SampleKind, ring,
};

type HRESULT = i32;
type HANDLE = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}

/// PROPVARIANT : type, réservé, puis l'union (16 octets sur 64 bits).
#[repr(C)]
struct PropVariant {
    vt: u16,
    reserved: [u16; 3],
    data: [u64; 2],
}

const CLSID_MM_DEVICE_ENUMERATOR: GUID =
    GUID { data1: 0xBCDE_0395, data2: 0xE52F, data3: 0x467C, data4: [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E] };
const IID_IMM_DEVICE_ENUMERATOR: GUID =
    GUID { data1: 0xA956_64D2, data2: 0x9614, data3: 0x4F35, data4: [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6] };
const IID_IAUDIO_CLIENT: GUID =
    GUID { data1: 0x1CB9_AD4C, data2: 0xDBFA, data3: 0x4C32, data4: [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2] };
const IID_IAUDIO_CLIENT3: GUID =
    GUID { data1: 0x7ED4_EE07, data2: 0x8E67, data3: 0x4CD4, data4: [0x8C, 0x1A, 0x2B, 0x7A, 0x59, 0x87, 0xAD, 0x42] };
const IID_IAUDIO_RENDER_CLIENT: GUID =
    GUID { data1: 0xF294_ACFC, data2: 0x3146, data3: 0x4483, data4: [0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2] };
const IID_IAUDIO_CAPTURE_CLIENT: GUID =
    GUID { data1: 0xC8AD_BD64, data2: 0xE71E, data3: 0x48A0, data4: [0xA4, 0xDE, 0x18, 0x5C, 0x39, 0x5C, 0xD3, 0x17] };
const SUBTYPE_PCM: GUID =
    GUID { data1: 0x0000_0001, data2: 0x0000, data3: 0x0010, data4: [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71] };
const SUBTYPE_IEEE_FLOAT: GUID =
    GUID { data1: 0x0000_0003, data2: 0x0000, data3: 0x0010, data4: [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71] };
const PKEY_DEVICE_FRIENDLY_NAME: PropertyKey = PropertyKey {
    fmtid: GUID { data1: 0xA45C_254E, data2: 0xDF1C, data3: 0x4EFD, data4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0] },
    pid: 14,
};

const COINIT_MULTITHREADED: u32 = 0;
const CLSCTX_ALL: u32 = 0x17;
const E_RENDER: i32 = 0;
const E_CAPTURE: i32 = 1;
const E_CONSOLE: i32 = 0;
const DEVICE_STATE_ACTIVE: u32 = 1;
const STGM_READ: u32 = 0;
const VT_LPWSTR: u16 = 31;
const AUDCLNT_SHAREMODE_SHARED: i32 = 0;
const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;
const AUDCLNT_S_BUFFER_EMPTY: HRESULT = 0x0889_0001;
const E_NOTFOUND: HRESULT = 0x8007_0490_u32 as i32;
const AUDCLNT_E_DEVICE_INVALIDATED: HRESULT = 0x8889_0004_u32 as i32;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// Tampon demandé, en unités de 100 ns.
fn buffer_hns() -> i64 {
    i64::from(crate::target_latency_ms()) * 10_000
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct WaveFormatEx {
    tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct WaveFormatExtensible {
    format: WaveFormatEx,
    valid_bits_per_sample: u16,
    channel_mask: u32,
    sub_format: GUID,
}

// Les vtables sont tronquées après la dernière méthode utilisée ;
// `usize` réserve la place des méthodes qu'on n'appelle pas.

#[repr(C)]
struct IUnknownVtbl {
    QueryInterface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    AddRef: usize,
    Release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct IMMDeviceEnumeratorVtbl {
    base: IUnknownVtbl,
    EnumAudioEndpoints: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> HRESULT,
    GetDefaultAudioEndpoint: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> HRESULT,
    GetDevice: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IMMDeviceCollectionVtbl {
    base: IUnknownVtbl,
    GetCount: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    Item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IMMDeviceVtbl {
    base: IUnknownVtbl,
    Activate: unsafe extern "system" fn(*mut c_void, *const GUID, u32, *mut c_void, *mut *mut c_void) -> HRESULT,
    OpenPropertyStore: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    GetId: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
}

#[repr(C)]
struct IPropertyStoreVtbl {
    base: IUnknownVtbl,
    GetCount: usize,
    GetAt: usize,
    GetValue: unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut PropVariant) -> HRESULT,
}

#[repr(C)]
struct IAudioClientVtbl {
    base: IUnknownVtbl,
    Initialize:
        unsafe extern "system" fn(*mut c_void, i32, u32, i64, i64, *const WaveFormatEx, *const GUID) -> HRESULT,
    GetBufferSize: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    GetStreamLatency: unsafe extern "system" fn(*mut c_void, *mut i64) -> HRESULT,
    GetCurrentPadding: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    IsFormatSupported: usize,
    GetMixFormat: unsafe extern "system" fn(*mut c_void, *mut *mut WaveFormatEx) -> HRESULT,
    GetDevicePeriod: usize,
    Start: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    Stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    Reset: usize,
    SetEventHandle: unsafe extern "system" fn(*mut c_void, HANDLE) -> HRESULT,
    GetService: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

/// `IAudioClient3` (Windows 10) : périodes du moteur partagé plus courtes que 10 ms.
#[repr(C)]
struct IAudioClient3Vtbl {
    base: IAudioClientVtbl,
    IsOffloadCapable: usize,
    SetClientProperties: usize,
    GetBufferSizeLimits: usize,
    GetSharedModeEnginePeriod:
        unsafe extern "system" fn(*mut c_void, *const WaveFormatEx, *mut u32, *mut u32, *mut u32, *mut u32) -> HRESULT,
    GetCurrentSharedModeEnginePeriod: usize,
    InitializeSharedAudioStream: unsafe extern "system" fn(*mut c_void, u32, u32, *const WaveFormatEx, *const GUID) -> HRESULT,
}

#[repr(C)]
struct IAudioRenderClientVtbl {
    base: IUnknownVtbl,
    GetBuffer: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u8) -> HRESULT,
    ReleaseBuffer: unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT,
}

#[repr(C)]
struct IAudioCaptureClientVtbl {
    base: IUnknownVtbl,
    GetBuffer: unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32, *mut u64, *mut u64) -> HRESULT,
    ReleaseBuffer: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    GetNextPacketSize: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> HRESULT;
    fn CoUninitialize();
    fn CoCreateInstance(
        clsid: *const GUID,
        outer: *mut c_void,
        clsctx: u32,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> HRESULT;
    fn CoTaskMemFree(p: *mut c_void);
    fn PropVariantClear(pv: *mut PropVariant) -> HRESULT;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attrs: *mut c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> HANDLE;
    fn WaitForSingleObject(handle: HANDLE, millis: u32) -> u32;
    fn CloseHandle(handle: HANDLE) -> i32;
}

#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> HANDLE;
    fn AvRevertMmThreadCharacteristics(handle: HANDLE) -> i32;
}

fn hr(code: HRESULT, call: &str) -> Result<(), AudioError> {
    match code {
        c if c >= 0 => Ok(()),
        AUDCLNT_E_DEVICE_INVALIDATED => Err(AudioError::System(format!("{call} : périphérique débranché ou changé"))),
        c => Err(AudioError::System(format!("{call} a échoué (HRESULT 0x{:08X})", c as u32))),
    }
}

/// Pointeur d'interface COM ; `Release` à la destruction.
struct Com<V> {
    ptr: *mut *const V,
}

impl<V> Com<V> {
    /// SAFETY: `raw` doit être une interface COM valide dont la vtable commence par `V`.
    unsafe fn from_raw(raw: *mut c_void) -> Self {
        Self { ptr: raw.cast() }
    }

    fn this(&self) -> *mut c_void {
        self.ptr.cast()
    }

    fn vtbl(&self) -> &V {
        // SAFETY: invariant de `from_raw`.
        unsafe { &**self.ptr }
    }

    /// Autre interface du même objet.
    ///
    /// SAFETY: la vtable de l'interface `iid` doit commencer par `W`.
    unsafe fn query<W>(&self, iid: &GUID) -> Option<Com<W>> {
        let mut raw = null_mut();
        // SAFETY: toute vtable COM commence par IUnknown.
        unsafe {
            let unknown = self.ptr as *mut *const IUnknownVtbl;
            let code = ((**unknown).QueryInterface)(self.this(), iid, &mut raw);
            (code >= 0 && !raw.is_null()).then(|| Com::from_raw(raw))
        }
    }
}

impl<V> Drop for Com<V> {
    fn drop(&mut self) {
        // SAFETY: toute vtable COM commence par IUnknown.
        unsafe {
            let unknown = self.ptr as *mut *const IUnknownVtbl;
            ((**unknown).Release)(self.this());
        }
    }
}

struct Apartment;

impl Apartment {
    fn enter() -> Result<Self, AudioError> {
        // SAFETY: appel sur un thread neuf, appairé avec CoUninitialize dans Drop.
        hr(unsafe { CoInitializeEx(null_mut(), COINIT_MULTITHREADED) }, "CoInitializeEx")?;
        Ok(Apartment)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: CoInitializeEx a réussi sur ce thread.
        unsafe { CoUninitialize() };
    }
}

struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: handle créé par CreateEventW.
        unsafe { CloseHandle(self.0) };
    }
}

/// Priorité MMCSS « Pro Audio » du thread courant, rendue à la destruction.
struct Mmcss(HANDLE);

impl Mmcss {
    fn enter() -> Self {
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut task_index = 0;
        // SAFETY: chaîne UTF-16 terminée par zéro ; échec sans conséquence (priorité normale).
        Mmcss(unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_index) })
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle renvoyé par AvSetMmThreadCharacteristicsW.
            unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }
}

/// Chaîne UTF-16 terminée par zéro → `String`.
///
/// SAFETY: `p` est nul ou pointe vers une chaîne terminée par zéro.
unsafe fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: invariant de la fonction.
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Lit le format de mixage du système.
///
/// SAFETY: `fmt` pointe vers un WAVEFORMATEX alloué par GetMixFormat.
unsafe fn parse_format(fmt: *const WaveFormatEx) -> Result<(OutputConfig, SampleKind), AudioError> {
    let base = unsafe { fmt.read_unaligned() };
    let bits = base.bits_per_sample;
    let tag = base.tag;
    let kind = match tag {
        WAVE_FORMAT_IEEE_FLOAT if bits == 32 => Some(SampleKind::F32),
        WAVE_FORMAT_PCM if bits == 16 => Some(SampleKind::I16),
        WAVE_FORMAT_EXTENSIBLE if base.cb_size >= 22 => {
            let ext = unsafe { fmt.cast::<WaveFormatExtensible>().read_unaligned() };
            let sub = ext.sub_format;
            match (sub == SUBTYPE_IEEE_FLOAT, sub == SUBTYPE_PCM, bits) {
                (true, _, 32) => Some(SampleKind::F32),
                (_, true, 16) => Some(SampleKind::I16),
                (_, true, 32) => Some(SampleKind::I32),
                _ => None,
            }
        }
        _ => None,
    };
    let channels = base.channels;
    let rate = base.samples_per_sec;
    let kind = kind.ok_or_else(|| AudioError::UnsupportedFormat(format!("tag 0x{tag:04X}, {bits} bits")))?;
    if base.block_align as usize != channels as usize * kind.bytes() {
        return Err(AudioError::UnsupportedFormat(format!("alignement de bloc {}", { base.block_align })));
    }
    Ok((OutputConfig { sample_rate: rate, channels }, kind))
}

/// Période du moteur partagé à demander : la moitié de la latence visée
/// (deux périodes en vol), arrondie au pas du moteur et bornée par ses limites.
fn shared_period(rate: u32, fundamental: u32, min: u32, max: u32) -> u32 {
    let fundamental = fundamental.max(1);
    let want = (rate as u64 * crate::target_latency_ms() as u64 / 2_000) as u32;
    (want.div_ceil(fundamental) * fundamental).clamp(min.max(1), max.max(min.max(1)))
}

/// Client audio partagé piloté par événement, commun au rendu et à la capture.
struct Client {
    // Ordre des champs = ordre de destruction : le client COM avant l'événement.
    client: Com<IAudioClientVtbl>,
    event: Event,
    buffer_frames: u32,
    channels: usize,
    kind: SampleKind,
    config: OutputConfig,
    /// Latence propre au flux (`GetStreamLatency`), en trames.
    stream_latency: u32,
    /// Période obtenue en faible latence partagée (`IAudioClient3`), en trames.
    low_latency_period: Option<u32>,
}

impl Client {
    /// Ouvre un périphérique dans le sens `flow` (`E_RENDER` ou `E_CAPTURE`) :
    /// celui d'identifiant `id` (UTF-16 terminé par zéro), sinon celui par défaut.
    fn open(flow: i32, id: Option<&[u16]>) -> Result<Self, AudioError> {
        let missing = || if flow == E_CAPTURE { AudioError::NoInputDevice } else { AudioError::NoOutputDevice };
        // SAFETY: séquence d'initialisation WASAPI standard ; chaque pointeur
        // renvoyé est enveloppé dans `Com` dès qu'il est valide.
        unsafe {
            let enumerator = enumerator()?;
            let mut raw = null_mut();
            let code = match id {
                Some(id) => (enumerator.vtbl().GetDevice)(enumerator.this(), id.as_ptr(), &mut raw),
                None => (enumerator.vtbl().GetDefaultAudioEndpoint)(enumerator.this(), flow, E_CONSOLE, &mut raw),
            };
            if code == E_NOTFOUND {
                return Err(missing());
            }
            hr(code, "GetDevice")?;
            let device = Com::<IMMDeviceVtbl>::from_raw(raw);

            let mut raw = null_mut();
            hr((device.vtbl().Activate)(device.this(), &IID_IAUDIO_CLIENT, CLSCTX_ALL, null_mut(), &mut raw), "Activate")?;
            let client = Com::<IAudioClientVtbl>::from_raw(raw);

            let mut fmt = null_mut();
            hr((client.vtbl().GetMixFormat)(client.this(), &mut fmt), "GetMixFormat")?;
            let parsed = parse_format(fmt);
            let low_latency = parsed.as_ref().ok().and_then(|(config, _)| Self::init_low_latency(&client, fmt, config.sample_rate));
            let init = match low_latency {
                Some(_) => 0,
                None => (client.vtbl().Initialize)(
                    client.this(),
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    buffer_hns(),
                    0,
                    fmt,
                    null(),
                ),
            };
            CoTaskMemFree(fmt.cast());
            let (config, kind) = parsed?;
            hr(init, "IAudioClient::Initialize")?;

            let mut buffer_frames = 0;
            hr((client.vtbl().GetBufferSize)(client.this(), &mut buffer_frames), "GetBufferSize")?;
            let mut latency_hns = 0i64;
            let _ = (client.vtbl().GetStreamLatency)(client.this(), &mut latency_hns);
            let stream_latency = (latency_hns.max(0) as u64 * config.sample_rate as u64 / 10_000_000) as u32;

            let handle = CreateEventW(null_mut(), 0, 0, null());
            if handle.is_null() {
                return Err(AudioError::System("CreateEventW a échoué".into()));
            }
            let event = Event(handle);
            hr((client.vtbl().SetEventHandle)(client.this(), event.0), "SetEventHandle")?;

            Ok(Client {
                client,
                event,
                buffer_frames,
                channels: config.channels as usize,
                kind,
                config,
                stream_latency,
                low_latency_period: low_latency,
            })
        }
    }

    /// Faible latence partagée : renvoie la période obtenue, ou `None` si le
    /// système ne la propose pas (le client reste alors à initialiser).
    ///
    /// SAFETY: `fmt` est le format de mixage du client, pas encore initialisé.
    unsafe fn init_low_latency(client: &Com<IAudioClientVtbl>, fmt: *const WaveFormatEx, rate: u32) -> Option<u32> {
        // SAFETY: IID_IAUDIO_CLIENT3 correspond à IAudioClient3Vtbl.
        unsafe {
            let c3 = client.query::<IAudioClient3Vtbl>(&IID_IAUDIO_CLIENT3)?;
            let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
            let code = (c3.vtbl().GetSharedModeEnginePeriod)(c3.this(), fmt, &mut default, &mut fundamental, &mut min, &mut max);
            if code < 0 || min == 0 {
                return None;
            }
            let period = shared_period(rate, fundamental, min, max);
            if period >= default {
                // Rien à gagner sur la période par défaut.
                return None;
            }
            let code = (c3.vtbl().InitializeSharedAudioStream)(c3.this(), AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, fmt, null());
            (code >= 0).then_some(period)
        }
    }

    /// SAFETY: la vtable de l'interface `iid` doit commencer par `V`.
    unsafe fn service<V>(&self, iid: &GUID) -> Result<Com<V>, AudioError> {
        let mut raw = null_mut();
        // SAFETY: client initialisé ; `raw` est enveloppé dès qu'il est valide.
        unsafe {
            hr((self.client.vtbl().GetService)(self.client.this(), iid, &mut raw), "GetService")?;
            Ok(Com::from_raw(raw))
        }
    }

    fn describe(&self, what: &str) -> String {
        let rate = self.config.sample_rate;
        let mode = match self.low_latency_period {
            Some(p) => format!("faible latence, période {p} trames ({:.1} ms)", p as f32 * 1000.0 / rate.max(1) as f32),
            None => format!("tampon {} trames", self.buffer_frames),
        };
        format!("WASAPI · {what} · {rate} Hz · {} canaux · {mode}", self.channels)
    }
}

/// SAFETY: à appeler sur un thread où COM est initialisé.
unsafe fn enumerator() -> Result<Com<IMMDeviceEnumeratorVtbl>, AudioError> {
    let mut raw = null_mut();
    // SAFETY: invariant de la fonction.
    unsafe {
        hr(
            CoCreateInstance(&CLSID_MM_DEVICE_ENUMERATOR, null_mut(), CLSCTX_ALL, &IID_IMM_DEVICE_ENUMERATOR, &mut raw),
            "CoCreateInstance(MMDeviceEnumerator)",
        )?;
        Ok(Com::from_raw(raw))
    }
}

/// Un sens de flux WASAPI : ouverture, démarrage et traitement d'un réveil.
trait Stream: Sized {
    type Callback: Send + 'static;
    const THREAD: &'static str;
    fn open(id: Option<&[u16]>) -> Result<Self, AudioError>;
    fn client(&self) -> &Client;
    fn name(&self) -> String;
    /// Prépare et lance le flux.
    fn begin(&self) -> Result<(), AudioError>;
    /// Traite ce que le système a rendu disponible depuis le dernier réveil.
    fn service(&self, callback: &mut Self::Callback, scratch: &mut [f32]) -> Result<(), AudioError>;
}

struct Renderer {
    // Détruit avant le client.
    render_client: Com<IAudioRenderClientVtbl>,
    client: Client,
}

impl Stream for Renderer {
    type Callback = Duplex;
    const THREAD: &'static str = "ponzik-audio";

    fn open(id: Option<&[u16]>) -> Result<Self, AudioError> {
        let client = Client::open(E_RENDER, id)?;
        // SAFETY: IID_IAUDIO_RENDER_CLIENT correspond à IAudioRenderClientVtbl.
        let render_client = unsafe { client.service(&IID_IAUDIO_RENDER_CLIENT)? };
        Ok(Renderer { render_client, client })
    }

    fn client(&self) -> &Client {
        &self.client
    }

    fn name(&self) -> String {
        self.client.describe("sortie par défaut")
    }

    fn begin(&self) -> Result<(), AudioError> {
        let (rc, c) = (self.render_client.vtbl(), &self.client);
        // SAFETY: protocole WASAPI GetBuffer/ReleaseBuffer ; préremplissage en
        // silence pour éviter un glitch au démarrage.
        unsafe {
            let mut data = null_mut();
            hr((rc.GetBuffer)(self.render_client.this(), c.buffer_frames, &mut data), "GetBuffer")?;
            hr(
                (rc.ReleaseBuffer)(self.render_client.this(), c.buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT),
                "ReleaseBuffer",
            )?;
            hr((c.client.vtbl().Start)(c.client.this()), "IAudioClient::Start")
        }
    }

    fn service(&self, duplex: &mut Duplex, scratch: &mut [f32]) -> Result<(), AudioError> {
        let (rc, c) = (self.render_client.vtbl(), &self.client);
        // SAFETY: le tampon système fait exactement `frames * bytes_per_frame`
        // octets entre GetBuffer et ReleaseBuffer.
        unsafe {
            let mut padding = 0;
            hr((c.client.vtbl().GetCurrentPadding)(c.client.this(), &mut padding), "GetCurrentPadding")?;
            let frames = c.buffer_frames.saturating_sub(padding);
            if frames == 0 {
                return Ok(());
            }
            let mut data = null_mut();
            hr((rc.GetBuffer)(self.render_client.this(), frames, &mut data), "GetBuffer")?;
            let samples = &mut scratch[..frames as usize * c.channels];
            // Ce qui attend d'être joué, plus la latence propre du flux.
            duplex.render(samples, c.channels, padding + c.stream_latency);
            let out = std::slice::from_raw_parts_mut(data, frames as usize * c.channels * c.kind.bytes());
            c.kind.encode(samples, out);
            hr((rc.ReleaseBuffer)(self.render_client.this(), frames, 0), "ReleaseBuffer")
        }
    }
}

struct Capturer {
    // Détruit avant le client.
    capture_client: Com<IAudioCaptureClientVtbl>,
    client: Client,
}

impl Stream for Capturer {
    type Callback = CaptureFn;
    const THREAD: &'static str = "ponzik-capture";

    fn open(id: Option<&[u16]>) -> Result<Self, AudioError> {
        let client = Client::open(E_CAPTURE, id)?;
        // SAFETY: IID_IAUDIO_CAPTURE_CLIENT correspond à IAudioCaptureClientVtbl.
        let capture_client = unsafe { client.service(&IID_IAUDIO_CAPTURE_CLIENT)? };
        Ok(Capturer { capture_client, client })
    }

    fn client(&self) -> &Client {
        &self.client
    }

    fn name(&self) -> String {
        self.client.describe("entrée")
    }

    fn begin(&self) -> Result<(), AudioError> {
        let c = &self.client;
        // SAFETY: client initialisé.
        hr(unsafe { (c.client.vtbl().Start)(c.client.this()) }, "IAudioClient::Start")
    }

    fn service(&self, capture: &mut CaptureFn, scratch: &mut [f32]) -> Result<(), AudioError> {
        let (cc, c) = (self.capture_client.vtbl(), &self.client);
        let this = self.capture_client.this();
        let bytes_per_frame = c.channels * c.kind.bytes();
        // SAFETY: protocole WASAPI GetBuffer/ReleaseBuffer ; le paquet système fait
        // `frames * bytes_per_frame` octets entre les deux appels.
        unsafe {
            loop {
                let mut packet = 0;
                hr((cc.GetNextPacketSize)(this, &mut packet), "GetNextPacketSize")?;
                if packet == 0 {
                    return Ok(());
                }
                let (mut data, mut frames, mut flags) = (null_mut(), 0u32, 0u32);
                let code = (cc.GetBuffer)(this, &mut data, &mut frames, &mut flags, null_mut(), null_mut());
                if code == AUDCLNT_S_BUFFER_EMPTY {
                    return Ok(());
                }
                hr(code, "IAudioCaptureClient::GetBuffer")?;
                // Un paquet ne dépasse jamais le tampon ; on borne par prudence.
                let n = (frames as usize).min(scratch.len() / c.channels.max(1));
                let samples = &mut scratch[..n * c.channels];
                if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                    samples.fill(0.0);
                } else {
                    c.kind.decode(std::slice::from_raw_parts(data, n * bytes_per_frame), samples);
                }
                hr((cc.ReleaseBuffer)(this, frames), "IAudioCaptureClient::ReleaseBuffer")?;
                if n > 0 {
                    capture(samples, c.channels);
                }
            }
        }
    }
}

type Started = mpsc::Sender<Result<(), AudioError>>;

/// Lance le flux, confirme via `started`, puis le sert jusqu'à `stop`.
fn run<S: Stream>(dev: &S, mut callback: S::Callback, stop: &AtomicBool, started: Started) -> Result<(), AudioError> {
    let _mmcss = Mmcss::enter();
    let c = dev.client();
    let mut scratch = vec![0.0f32; c.buffer_frames as usize * c.channels];
    let begun = dev.begin();
    let ok = begun.is_ok();
    let _ = started.send(begun);
    if !ok {
        return Ok(());
    }
    let looped = (|| {
        while !stop.load(Ordering::Acquire) {
            // SAFETY: événement valide tant que `c` vit.
            match unsafe { WaitForSingleObject(c.event.0, 100) } {
                WAIT_OBJECT_0 | WAIT_TIMEOUT => {}
                other => return Err(AudioError::System(format!("WaitForSingleObject : {other}"))),
            }
            dev.service(&mut callback, &mut scratch)?;
        }
        Ok(())
    })();
    // SAFETY: le flux a démarré.
    unsafe { (c.client.vtbl().Stop)(c.client.this()) };
    looped
}

pub(crate) struct Pending<C> {
    start_tx: mpsc::Sender<(C, Started)>,
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
    channels: usize,
    latency: u32,
}

pub(crate) type PendingOutput = Pending<Duplex>;
pub(crate) type PendingInput = Pending<CaptureFn>;

/// Ouvert : configuration, latence propre du flux et nom.
type Ready = (OutputConfig, u32, String);

/// Ouvre le flux sur son propre thread (qui garde tout l'état COM) et attend sa configuration.
fn spawn<S: Stream>(id: Option<Vec<u16>>) -> Result<(OutputConfig, String, Pending<S::Callback>), AudioError> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Ready, AudioError>>();
    let (start_tx, start_rx) = mpsc::channel::<(S::Callback, Started)>();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();

    let handle = thread::Builder::new()
        .name(S::THREAD.into())
        .spawn(move || {
            let opened = Apartment::enter().and_then(|apt| S::open(id.as_deref()).map(|dev| (apt, dev)));
            let (_apartment, device) = match opened {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let c = device.client();
            let _ = ready_tx.send(Ok((c.config, c.stream_latency + c.buffer_frames / 2, device.name())));
            // Si le flux est détruit sans être lancé, le canal se ferme et on libère tout.
            let Ok((callback, started)) = start_rx.recv() else { return };
            if let Err(e) = run(&device, callback, &flag, started) {
                eprintln!("ponzik-audio : {e}");
            }
        })
        .map_err(|e| AudioError::System(format!("thread audio : {e}")))?;

    let (config, latency, name) = match ready_rx.recv() {
        Ok(Ok(ready)) => ready,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(AudioError::System("le thread audio s'est arrêté à l'ouverture".into())),
    };
    let pending = Pending { start_tx, stop, handle, channels: config.channels as usize, latency };
    Ok((config, name, pending))
}

pub(crate) fn open_default(_preferred: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    // En mode partagé, on suit le format de mixage du système (rééchantillonnage
    // natif à venir côté moteur) : la préférence est ignorée.
    spawn::<Renderer>(None)
}

/// Capture au format de mixage de l'entrée, convertie en `f32` ; canaux natifs.
pub(crate) fn open_input(_preferred: OutputConfig, id: Option<&str>) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    let id = id.map(|s| s.encode_utf16().chain(std::iter::once(0)).collect());
    spawn::<Capturer>(id)
}

impl<C: Send + 'static> Pending<C> {
    pub(crate) fn start(self, callback: C) -> Result<AudioThread, AudioError> {
        let (started_tx, started_rx) = mpsc::channel();
        let thread = AudioThread { stop: self.stop, handle: Some(self.handle) };
        // En cas d'échec, `thread` est détruit : stop + join.
        self.start_tx
            .send((callback, started_tx))
            .map_err(|_| AudioError::System("le thread audio s'est arrêté".into()))?;
        match started_rx.recv() {
            Ok(Ok(())) => Ok(thread),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError::System("le thread audio s'est arrêté au démarrage".into())),
        }
    }
}

/// Capture de son thread vers le thread de sortie, par une file sans verrou.
struct RingCapture {
    rx: RingConsumer,
    channels: usize,
    latency: u32,
    scratch: Vec<f32>,
}

impl CaptureSource for RingCapture {
    fn pump(&mut self, fifo: &mut InputFifo) -> u32 {
        loop {
            let n = self.rx.pop(&mut self.scratch);
            // Trames entières seulement : le producteur pousse des paquets complets.
            fifo.push(&self.scratch[..n - n % self.channels]);
            if n < self.scratch.len() {
                break;
            }
        }
        self.latency
    }
}

/// Branche la capture : son thread pousse dans une file relue par la sortie.
pub(crate) fn attach(pending: PendingInput) -> Result<Attachment, AudioError> {
    let channels = pending.channels.max(1);
    let latency = pending.latency;
    let (mut tx, rx) = ring(16_384 * channels);
    let thread = pending.start(Box::new(move |data: &[f32], ch: usize| {
        tx.push(data, ch);
    }))?;
    let source = RingCapture { rx, channels, latency, scratch: vec![0.0; 1_024 * channels] };
    Ok((Box::new(source), channels, Some(thread)))
}

/// Entrées actives, celle par défaut en premier (énumérées sur un thread COM à part).
pub(crate) fn input_devices() -> Vec<DeviceInfo> {
    let lister = thread::Builder::new().name("ponzik-devices".into()).spawn(|| {
        let Ok(_apartment) = Apartment::enter() else { return Vec::new() };
        // SAFETY: COM initialisé sur ce thread ; chaque interface est enveloppée dans `Com`.
        unsafe { list_capture_endpoints().unwrap_or_default() }
    });
    lister.ok().and_then(|h| h.join().ok()).unwrap_or_default()
}

/// SAFETY: COM initialisé sur le thread courant.
unsafe fn list_capture_endpoints() -> Result<Vec<DeviceInfo>, AudioError> {
    // SAFETY: invariant de la fonction ; chaînes et PROPVARIANT libérées après lecture.
    unsafe {
        let enumerator = enumerator()?;
        let mut default_id = String::new();
        let mut raw = null_mut();
        if (enumerator.vtbl().GetDefaultAudioEndpoint)(enumerator.this(), E_CAPTURE, E_CONSOLE, &mut raw) >= 0 {
            let device = Com::<IMMDeviceVtbl>::from_raw(raw);
            default_id = device_id(&device);
        }
        let mut raw = null_mut();
        hr(
            (enumerator.vtbl().EnumAudioEndpoints)(enumerator.this(), E_CAPTURE, DEVICE_STATE_ACTIVE, &mut raw),
            "EnumAudioEndpoints",
        )?;
        let collection = Com::<IMMDeviceCollectionVtbl>::from_raw(raw);
        let mut count = 0;
        hr((collection.vtbl().GetCount)(collection.this(), &mut count), "GetCount")?;
        let mut out = Vec::new();
        for i in 0..count {
            let mut raw = null_mut();
            if (collection.vtbl().Item)(collection.this(), i, &mut raw) < 0 {
                continue;
            }
            let device = Com::<IMMDeviceVtbl>::from_raw(raw);
            let id = device_id(&device);
            if id.is_empty() {
                continue;
            }
            let name = friendly_name(&device).unwrap_or_else(|| id.clone());
            out.push(DeviceInfo { id, name });
        }
        if let Some(i) = out.iter().position(|d| d.id == default_id) {
            let d = out.remove(i);
            out.insert(0, d);
        }
        Ok(out)
    }
}

/// SAFETY: `device` est une interface valide, COM initialisé.
unsafe fn device_id(device: &Com<IMMDeviceVtbl>) -> String {
    let mut p = null_mut();
    // SAFETY: chaîne allouée par COM, libérée après copie.
    unsafe {
        if (device.vtbl().GetId)(device.this(), &mut p) < 0 || p.is_null() {
            return String::new();
        }
        let s = wide(p);
        CoTaskMemFree(p.cast());
        s
    }
}

/// SAFETY: `device` est une interface valide, COM initialisé.
unsafe fn friendly_name(device: &Com<IMMDeviceVtbl>) -> Option<String> {
    // SAFETY: invariant de la fonction ; le PROPVARIANT est vidé par PropVariantClear.
    unsafe {
        let mut raw = null_mut();
        if (device.vtbl().OpenPropertyStore)(device.this(), STGM_READ, &mut raw) < 0 {
            return None;
        }
        let store = Com::<IPropertyStoreVtbl>::from_raw(raw);
        let mut value = PropVariant { vt: 0, reserved: [0; 3], data: [0; 2] };
        if (store.vtbl().GetValue)(store.this(), &PKEY_DEVICE_FRIENDLY_NAME, &mut value) < 0 {
            return None;
        }
        let name = (value.vt == VT_LPWSTR).then(|| wide(value.data[0] as usize as *const u16));
        PropVariantClear(&mut value);
        name.filter(|n| !n.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_period_rounds_to_the_engine_step() {
        // 10 ms visés à 48 kHz : 240 trames, arrondies au pas de 48.
        assert_eq!(shared_period(48_000, 48, 128, 480), 240);
        assert_eq!(shared_period(48_000, 100, 128, 480), 300);
        // Bornée par la période minimale du moteur.
        assert_eq!(shared_period(48_000, 1, 480, 480), 480);
    }
}
