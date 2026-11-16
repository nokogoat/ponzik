//! Pilote CoreAudio : liaisons FFI maison vers `AudioToolbox` et `CoreAudio`.
//!
//! Sortie et capture passent chacune par une unité AUHAL (`kAudioUnitSubType_HALOutput`)
//! accrochée au périphérique par défaut. Le système appelle nos rappels sur son
//! thread temps réel ; un thread de veille garde l'unité en vie, surveille le
//! périphérique et la libère quand le flux est détruit, ce qui donne la même
//! forme (`AudioThread`) que les pilotes ALSA et WASAPI.

#![allow(non_snake_case, non_upper_case_globals)]

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::{AudioError, AudioThread, CaptureFn, OutputConfig, RenderFn};

type OSStatus = i32;
type AudioObjectID = u32;
type AudioComponent = *mut c_void;
type AudioUnit = *mut c_void;
type CFStringRef = *const c_void;

#[repr(C)]
struct AudioComponentDescription {
    componentType: u32,
    componentSubType: u32,
    componentManufacturer: u32,
    componentFlags: u32,
    componentFlagsMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioStreamBasicDescription {
    mSampleRate: f64,
    mFormatID: u32,
    mFormatFlags: u32,
    mBytesPerPacket: u32,
    mFramesPerPacket: u32,
    mBytesPerFrame: u32,
    mChannelsPerFrame: u32,
    mBitsPerChannel: u32,
    mReserved: u32,
}

#[repr(C)]
struct AudioObjectPropertyAddress {
    mSelector: u32,
    mScope: u32,
    mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    mNumberChannels: u32,
    mDataByteSize: u32,
    mData: *mut c_void,
}

/// `AudioBufferList` à un seul tampon : c'est tout ce qu'on fournit nous-mêmes.
#[repr(C)]
struct AudioBufferList {
    mNumberBuffers: u32,
    mBuffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Default)]
struct AudioValueRange {
    mMinimum: f64,
    mMaximum: f64,
}

/// Horodatage CoreAudio : on ne fait que le transmettre.
#[repr(C)]
struct AudioTimeStamp {
    _opaque: [u8; 0],
}

type AURenderCallback = unsafe extern "C" fn(
    ref_con: *mut c_void,
    flags: *mut u32,
    time: *const AudioTimeStamp,
    bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> OSStatus;

#[repr(C)]
struct AURenderCallbackStruct {
    inputProc: AURenderCallback,
    inputProcRefCon: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(component: AudioComponent, desc: *const AudioComponentDescription) -> AudioComponent;
    fn AudioComponentInstanceNew(component: AudioComponent, out: *mut AudioUnit) -> OSStatus;
    fn AudioComponentInstanceDispose(unit: AudioUnit) -> OSStatus;
    fn AudioUnitInitialize(unit: AudioUnit) -> OSStatus;
    fn AudioUnitUninitialize(unit: AudioUnit) -> OSStatus;
    fn AudioUnitSetProperty(unit: AudioUnit, id: u32, scope: u32, element: u32, data: *const c_void, size: u32) -> OSStatus;
    fn AudioUnitGetProperty(unit: AudioUnit, id: u32, scope: u32, element: u32, data: *mut c_void, size: *mut u32) -> OSStatus;
    fn AudioUnitRender(
        unit: AudioUnit,
        flags: *mut u32,
        time: *const AudioTimeStamp,
        bus: u32,
        frames: u32,
        data: *mut AudioBufferList,
    ) -> OSStatus;
    fn AudioOutputUnitStart(unit: AudioUnit) -> OSStatus;
    fn AudioOutputUnitStop(unit: AudioUnit) -> OSStatus;
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(string: CFStringRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(object: *const c_void);
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const kAudioUnitType_Output: u32 = fourcc(b"auou");
const kAudioUnitSubType_HALOutput: u32 = fourcc(b"ahal");
const kAudioUnitManufacturer_Apple: u32 = fourcc(b"appl");

const kAudioUnitScope_Global: u32 = 0;
const kAudioUnitScope_Input: u32 = 1;
const kAudioUnitScope_Output: u32 = 2;
/// Élément 0 : côté haut-parleur ; élément 1 : côté micro.
const OUTPUT_BUS: u32 = 0;
const INPUT_BUS: u32 = 1;

const kAudioUnitProperty_StreamFormat: u32 = 8;
const kAudioUnitProperty_MaximumFramesPerSlice: u32 = 14;
const kAudioUnitProperty_SetRenderCallback: u32 = 23;
const kAudioOutputUnitProperty_CurrentDevice: u32 = 2000;
const kAudioOutputUnitProperty_EnableIO: u32 = 2003;
const kAudioOutputUnitProperty_SetInputCallback: u32 = 2005;

const kAudioObjectSystemObject: AudioObjectID = 1;
const kAudioObjectUnknown: AudioObjectID = 0;
const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
const kAudioObjectPropertyScopeInput: u32 = fourcc(b"inpt");
const kAudioObjectPropertyScopeOutput: u32 = fourcc(b"outp");
const kAudioObjectPropertyElementMain: u32 = 0;
const kAudioHardwarePropertyDefaultOutputDevice: u32 = fourcc(b"dOut");
const kAudioHardwarePropertyDefaultInputDevice: u32 = fourcc(b"dIn ");
const kAudioObjectPropertyName: u32 = fourcc(b"lnam");
const kAudioDevicePropertyNominalSampleRate: u32 = fourcc(b"nsrt");
const kAudioDevicePropertyBufferFrameSize: u32 = fourcc(b"fsiz");
const kAudioDevicePropertyBufferFrameSizeRange: u32 = fourcc(b"fsz#");
const kAudioDevicePropertyDeviceIsAlive: u32 = fourcc(b"livn");

const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
const kAudioFormatFlagIsFloat: u32 = 1 << 0;
#[cfg(target_endian = "big")]
const kAudioFormatFlagIsBigEndian: u32 = 1 << 1;
const kAudioFormatFlagIsPacked: u32 = 1 << 3;
const kAudioFormatFlagIsNonInterleaved: u32 = 1 << 5;
#[cfg(target_endian = "little")]
const kAudioFormatFlagsNativeFloatPacked: u32 = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
#[cfg(target_endian = "big")]
const kAudioFormatFlagsNativeFloatPacked: u32 =
    kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | kAudioFormatFlagIsBigEndian;

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

/// Plus grand bloc qu'on accepte de rendre d'un coup (le système en demande bien moins).
const MAX_FRAMES: usize = 4096;

fn check(code: OSStatus, call: &str) -> Result<(), AudioError> {
    if code == 0 {
        return Ok(());
    }
    // Les erreurs CoreAudio sont souvent des codes à quatre lettres.
    let bytes = code.to_be_bytes();
    let text = if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        code.to_string()
    };
    Err(AudioError::System(format!("{call} : OSStatus {text}")))
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

/// Lit une propriété de taille fixe d'un objet audio.
fn get<T>(object: AudioObjectID, selector: u32, scope: u32, value: &mut T) -> Result<(), AudioError> {
    let addr = address(selector, scope);
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` peut recevoir `size` octets.
    let code =
        unsafe { AudioObjectGetPropertyData(object, &addr, 0, null(), &mut size, (value as *mut T).cast()) };
    check(code, "AudioObjectGetPropertyData")
}

fn default_device(input: bool) -> Result<AudioObjectID, AudioError> {
    let selector = if input { kAudioHardwarePropertyDefaultInputDevice } else { kAudioHardwarePropertyDefaultOutputDevice };
    let mut id: AudioObjectID = kAudioObjectUnknown;
    get(kAudioObjectSystemObject, selector, kAudioObjectPropertyScopeGlobal, &mut id)?;
    if id == kAudioObjectUnknown {
        return Err(if input { AudioError::NoInputDevice } else { AudioError::NoOutputDevice });
    }
    Ok(id)
}

fn device_name(device: AudioObjectID) -> String {
    let mut name: CFStringRef = null();
    if get(device, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, &mut name).is_err() || name.is_null() {
        return format!("périphérique {device}");
    }
    let mut buf = [0 as c_char; 256];
    // SAFETY: `name` est une CFString possédée (règle « Copy ») ; libérée une seule fois.
    let ok = unsafe {
        let ok = CFStringGetCString(name, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8);
        CFRelease(name);
        ok != 0
    };
    if !ok {
        return format!("périphérique {device}");
    }
    // SAFETY: CFStringGetCString a écrit une chaîne terminée par zéro.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn alive(device: AudioObjectID) -> bool {
    let mut alive: u32 = 1;
    // Si la question échoue, le périphérique a disparu.
    get(device, kAudioDevicePropertyDeviceIsAlive, kAudioObjectPropertyScopeGlobal, &mut alive).is_ok() && alive != 0
}

/// Demande au périphérique un petit tampon (≈ `target_latency_ms`) ; renvoie la taille obtenue.
fn set_buffer_frames(device: AudioObjectID, rate: f64, scope: u32) -> u32 {
    let wanted = (rate * f64::from(crate::target_latency_ms()) / 1000.0).round().max(16.0);
    let mut range = AudioValueRange::default();
    let wanted = match get(device, kAudioDevicePropertyBufferFrameSizeRange, scope, &mut range) {
        Ok(()) if range.mMaximum >= range.mMinimum && range.mMaximum > 0.0 => wanted.clamp(range.mMinimum, range.mMaximum),
        _ => wanted,
    } as u32;
    let addr = address(kAudioDevicePropertyBufferFrameSize, scope);
    // SAFETY: valeur u32 passée avec sa taille. Un refus n'est pas fatal : on garde le tampon actuel.
    unsafe { AudioObjectSetPropertyData(device, &addr, 0, null(), 4, (&wanted as *const u32).cast()) };
    let mut got = wanted;
    let _ = get(device, kAudioDevicePropertyBufferFrameSize, scope, &mut got);
    got
}

fn float_format(rate: f64, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 4 * channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * channels,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

/// Unité AUHAL possédée : arrêtée, désinitialisée et libérée à la destruction.
struct Unit {
    raw: AudioUnit,
    initialized: bool,
}

// SAFETY: une unité AudioUnit peut être pilotée depuis n'importe quel thread ;
// on ne la touche que depuis un thread à la fois (ouverture, puis thread de veille).
unsafe impl Send for Unit {}

impl Unit {
    fn new_hal() -> Result<Self, AudioError> {
        let desc = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: description valide ; l'instance appartient à `Unit` dès sa création.
        unsafe {
            let component = AudioComponentFindNext(null_mut(), &desc);
            if component.is_null() {
                return Err(AudioError::System("composant AUHAL introuvable".into()));
            }
            let mut raw = null_mut();
            check(AudioComponentInstanceNew(component, &mut raw), "AudioComponentInstanceNew")?;
            Ok(Unit { raw, initialized: false })
        }
    }

    fn set<T>(&self, id: u32, scope: u32, element: u32, value: &T, call: &str) -> Result<(), AudioError> {
        // SAFETY: `value` fait exactement `size_of::<T>()` octets.
        let code = unsafe {
            AudioUnitSetProperty(self.raw, id, scope, element, (value as *const T).cast(), size_of::<T>() as u32)
        };
        check(code, call)
    }

    fn get<T>(&self, id: u32, scope: u32, element: u32, value: &mut T, call: &str) -> Result<(), AudioError> {
        let mut size = size_of::<T>() as u32;
        // SAFETY: `value` peut recevoir `size` octets.
        let code = unsafe { AudioUnitGetProperty(self.raw, id, scope, element, (value as *mut T).cast(), &mut size) };
        check(code, call)
    }

    fn start(&mut self) -> Result<(), AudioError> {
        // SAFETY: unité configurée, rappel en place.
        unsafe {
            check(AudioUnitInitialize(self.raw), "AudioUnitInitialize")?;
            self.initialized = true;
            check(AudioOutputUnitStart(self.raw), "AudioOutputUnitStart")
        }
    }
}

impl Drop for Unit {
    fn drop(&mut self) {
        // SAFETY: instance créée par AudioComponentInstanceNew, libérée une seule fois.
        // Après AudioOutputUnitStop, le système n'appelle plus nos rappels.
        unsafe {
            if self.initialized {
                AudioOutputUnitStop(self.raw);
                AudioUnitUninitialize(self.raw);
            }
            AudioComponentInstanceDispose(self.raw);
        }
    }
}

/// Garde l'unité vivante jusqu'à l'arrêt demandé ou la disparition du périphérique.
/// `state` (lu par le rappel) n'est libéré qu'après l'unité.
fn watch<S: Send + 'static>(
    name: &str,
    mut unit: Unit,
    device: AudioObjectID,
    state: Box<S>,
    failed: Arc<AtomicBool>,
) -> Result<AudioThread, AudioError> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    unit.start()?;
    let handle = thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            while !flag.load(Ordering::Acquire) {
                if failed.load(Ordering::Acquire) {
                    eprintln!("ponzik-audio : le rappel audio a paniqué");
                    break;
                }
                if !alive(device) {
                    eprintln!("ponzik-audio : périphérique {device} débranché");
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            drop(unit);
            drop(state);
        })
        .map_err(|e| AudioError::System(format!("thread audio : {e}")))?;
    Ok(AudioThread { stop, handle: Some(handle) })
}

// ---------------------------------------------------------------- sortie

pub(crate) struct PendingOutput {
    unit: Unit,
    device: AudioObjectID,
    channels: usize,
}

struct OutputState {
    render: RenderFn,
    channels: usize,
    scratch: Vec<f32>,
    failed: Arc<AtomicBool>,
}

/// Rappel de rendu : thread temps réel de CoreAudio, ni allocation ni verrou.
unsafe extern "C" fn render_proc(
    ref_con: *mut c_void,
    _flags: *mut u32,
    _time: *const AudioTimeStamp,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> OSStatus {
    // SAFETY: `ref_con` est l'`OutputState` du thread de veille, libéré après l'unité.
    let state = unsafe { &mut *ref_con.cast::<OutputState>() };
    // SAFETY: liste fournie par le système, `mNumberBuffers` tampons à la suite.
    let buffers = unsafe {
        let list = &mut *data;
        std::slice::from_raw_parts_mut(list.mBuffers.as_mut_ptr(), list.mNumberBuffers as usize)
    };
    let ch = state.channels;
    let frames = frames as usize;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(MAX_FRAMES);
        let block = &mut state.scratch[..n * ch];
        if state.failed.load(Ordering::Relaxed)
            || catch_unwind(AssertUnwindSafe(|| (state.render)(block, ch))).is_err()
        {
            state.failed.store(true, Ordering::Release);
            block.fill(0.0);
        }
        write_block(buffers, block, ch, done, n);
        done += n;
    }
    0
}

/// Copie `n` trames entrelacées vers les tampons du système, entrelacés (un
/// tampon) ou séparés (un tampon par canal), à partir de la trame `offset`.
fn write_block(buffers: &mut [AudioBuffer], block: &[f32], ch: usize, offset: usize, n: usize) {
    if let [buf] = buffers {
        let dst_ch = (buf.mNumberChannels as usize).max(1);
        let len = buf.mDataByteSize as usize / 4;
        // SAFETY: le système fournit `mDataByteSize` octets de f32 alignés.
        let dst = unsafe { std::slice::from_raw_parts_mut(buf.mData.cast::<f32>(), len) };
        for f in 0..n {
            let at = (offset + f) * dst_ch;
            if at + dst_ch > len {
                break;
            }
            for c in 0..dst_ch {
                dst[at + c] = block[f * ch + c.min(ch - 1)];
            }
        }
        return;
    }
    for (c, buf) in buffers.iter_mut().enumerate() {
        let len = buf.mDataByteSize as usize / 4;
        // SAFETY: idem, un canal par tampon.
        let dst = unsafe { std::slice::from_raw_parts_mut(buf.mData.cast::<f32>(), len) };
        let src = c.min(ch - 1);
        for f in 0..n.min(len.saturating_sub(offset)) {
            dst[offset + f] = block[f * ch + src];
        }
    }
}

pub(crate) fn open_default(preferred: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    let device = default_device(false)?;
    // On suit la fréquence du périphérique : pas de rééchantillonnage caché.
    let mut rate = 0.0f64;
    let _ = get(device, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeOutput, &mut rate);
    if !(8_000.0..=768_000.0).contains(&rate) {
        rate = f64::from(preferred.sample_rate);
    }
    let frames = set_buffer_frames(device, rate, kAudioObjectPropertyScopeOutput);

    let unit = Unit::new_hal()?;
    unit.set(kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &device, "CurrentDevice")?;
    let channels = u32::from(preferred.channels.max(1));
    // Ce qu'on fournit à l'unité : f32 entrelacé ; AUHAL convertit vers le matériel.
    let format = float_format(rate, channels);
    unit.set(kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, OUTPUT_BUS, &format, "StreamFormat (sortie)")?;
    let max = MAX_FRAMES as u32;
    let _ = unit.set(kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitScope_Global, 0, &max, "MaximumFramesPerSlice");

    let config = OutputConfig { sample_rate: rate.round() as u32, channels: channels as u16 };
    let name = format!("CoreAudio · {} · tampon {frames} trames", device_name(device));
    Ok((config, name, PendingOutput { unit, device, channels: channels as usize }))
}

impl PendingOutput {
    pub(crate) fn start(self, render: RenderFn) -> Result<AudioThread, AudioError> {
        let failed = Arc::new(AtomicBool::new(false));
        let mut state = Box::new(OutputState {
            render,
            channels: self.channels,
            scratch: vec![0.0; MAX_FRAMES * self.channels],
            failed: failed.clone(),
        });
        let callback =
            AURenderCallbackStruct { inputProc: render_proc, inputProcRefCon: (&mut *state as *mut OutputState).cast() };
        self.unit.set(
            kAudioUnitProperty_SetRenderCallback,
            kAudioUnitScope_Input,
            OUTPUT_BUS,
            &callback,
            "SetRenderCallback",
        )?;
        watch("ponzik-audio", self.unit, self.device, state, failed)
    }
}

// ---------------------------------------------------------------- capture

pub(crate) struct PendingInput {
    unit: Unit,
    device: AudioObjectID,
    /// Canaux demandés à l'unité (1 si l'entrée est mono).
    device_channels: usize,
}

struct InputState {
    capture: CaptureFn,
    unit: AudioUnit,
    device_channels: usize,
    raw: Vec<f32>,
    stereo: Vec<f32>,
    failed: Arc<AtomicBool>,
}

/// Rappel de capture : des trames sont prêtes, on les tire avec AudioUnitRender.
unsafe extern "C" fn input_proc(
    ref_con: *mut c_void,
    flags: *mut u32,
    time: *const AudioTimeStamp,
    bus: u32,
    frames: u32,
    _data: *mut AudioBufferList,
) -> OSStatus {
    // SAFETY: `ref_con` est l'`InputState` du thread de veille, libéré après l'unité.
    let state = unsafe { &mut *ref_con.cast::<InputState>() };
    let ch = state.device_channels;
    let frames = (frames as usize).min(MAX_FRAMES);
    if frames == 0 || state.failed.load(Ordering::Relaxed) {
        return 0;
    }
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: ch as u32,
            mDataByteSize: (frames * ch * 4) as u32,
            mData: state.raw.as_mut_ptr().cast(),
        }],
    };
    // SAFETY: `raw` peut recevoir `frames * ch` échantillons ; unité vivante pendant le rappel.
    let code = unsafe { AudioUnitRender(state.unit, flags, time, bus, frames as u32, &mut list) };
    if code != 0 {
        return code;
    }
    let raw = &state.raw[..frames * ch];
    let capture = &mut state.capture;
    let ok = if ch == 1 {
        let stereo = &mut state.stereo[..frames * 2];
        for (d, &s) in stereo.chunks_exact_mut(2).zip(raw) {
            d.fill(s);
        }
        catch_unwind(AssertUnwindSafe(|| capture(stereo, 2))).is_ok()
    } else {
        catch_unwind(AssertUnwindSafe(|| capture(raw, ch))).is_ok()
    };
    if !ok {
        state.failed.store(true, Ordering::Release);
    }
    0
}

/// Capture : stéréo de préférence, sinon mono dupliqué sur les deux canaux.
pub(crate) fn open_default_input(preferred: OutputConfig) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    let device = default_device(true)?;
    let unit = Unit::new_hal()?;
    let on: u32 = 1;
    let off: u32 = 0;
    // L'ordre compte : activer l'entrée et couper la sortie avant de choisir le périphérique.
    unit.set(kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, INPUT_BUS, &on, "EnableIO (entrée)")?;
    unit.set(kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Output, OUTPUT_BUS, &off, "EnableIO (sortie)")?;
    unit.set(kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &device, "CurrentDevice")?;

    // Format côté matériel : AUHAL ne rééchantillonne pas la capture, on garde sa fréquence.
    let mut hw = AudioStreamBasicDescription::default();
    unit.get(kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, INPUT_BUS, &mut hw, "StreamFormat (matériel)")?;
    if hw.mChannelsPerFrame == 0 {
        return Err(AudioError::NoInputDevice);
    }
    let rate = if hw.mSampleRate > 0.0 { hw.mSampleRate } else { f64::from(preferred.sample_rate) };
    let device_channels = hw.mChannelsPerFrame.min(u32::from(preferred.channels.max(1)));
    let format = float_format(rate, device_channels);
    debug_assert_eq!(format.mFormatFlags & kAudioFormatFlagIsNonInterleaved, 0);
    unit.set(kAudioUnitProperty_StreamFormat, kAudioUnitScope_Output, INPUT_BUS, &format, "StreamFormat (capture)")?;
    let max = MAX_FRAMES as u32;
    let _ = unit.set(kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitScope_Global, 0, &max, "MaximumFramesPerSlice");
    let frames = set_buffer_frames(device, rate, kAudioObjectPropertyScopeInput);

    let mono = if device_channels == 1 { " · mono dupliqué" } else { "" };
    let name = format!("CoreAudio · {} (capture) · tampon {frames} trames{mono}", device_name(device));
    let config = OutputConfig { sample_rate: rate.round() as u32, channels: device_channels.max(2) as u16 };
    Ok((config, name, PendingInput { unit, device, device_channels: device_channels as usize }))
}

impl PendingInput {
    pub(crate) fn start(self, capture: CaptureFn) -> Result<AudioThread, AudioError> {
        let failed = Arc::new(AtomicBool::new(false));
        let ch = self.device_channels;
        let mut state = Box::new(InputState {
            capture,
            unit: self.unit.raw,
            device_channels: ch,
            raw: vec![0.0; MAX_FRAMES * ch],
            stereo: vec![0.0; if ch == 1 { MAX_FRAMES * 2 } else { 0 }],
            failed: failed.clone(),
        });
        let callback =
            AURenderCallbackStruct { inputProc: input_proc, inputProcRefCon: (&mut *state as *mut InputState).cast() };
        self.unit.set(
            kAudioOutputUnitProperty_SetInputCallback,
            kAudioUnitScope_Global,
            0,
            &callback,
            "SetInputCallback",
        )?;
        watch("ponzik-capture", self.unit, self.device, state, failed)
    }
}

// SAFETY: `InputState::unit` n'est qu'une copie du pointeur possédé par `Unit`,
// utilisée par le rappel tant que l'unité vit.
unsafe impl Send for InputState {}
