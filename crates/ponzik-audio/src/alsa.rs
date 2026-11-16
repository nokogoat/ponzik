//! Pilote ALSA : liaisons FFI maison vers `libasound`.
//!
//! La sortie ouvre le périphérique `default` : sur un bureau moderne il est
//! routé vers PipeWire ou PulseAudio, sinon il attaque directement la carte
//! son. Une entrée peut être choisie parmi les périphériques listés par ALSA.
//!
//! En duplex, la capture est ouverte en mode non bloquant et lue par le thread
//! de sortie juste avant chaque rendu : l'entrée et la sortie partagent le même
//! cycle, sans thread ni file intermédiaire.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::{Attachment, AudioError, AudioThread, CaptureFn, CaptureSource, DeviceInfo, Duplex, InputFifo, OutputConfig};

#[repr(C)]
struct SndPcm {
    _opaque: [u8; 0],
}

#[link(name = "asound")]
unsafe extern "C" {
    fn snd_pcm_open(pcm: *mut *mut SndPcm, name: *const c_char, stream: c_int, mode: c_int) -> c_int;
    fn snd_pcm_set_params(
        pcm: *mut SndPcm,
        format: c_int,
        access: c_int,
        channels: c_uint,
        rate: c_uint,
        soft_resample: c_int,
        latency_us: c_uint,
    ) -> c_int;
    fn snd_pcm_get_params(pcm: *mut SndPcm, buffer_size: *mut c_ulong, period_size: *mut c_ulong) -> c_int;
    fn snd_pcm_writei(pcm: *mut SndPcm, buffer: *const c_void, frames: c_ulong) -> c_long;
    fn snd_pcm_readi(pcm: *mut SndPcm, buffer: *mut c_void, frames: c_ulong) -> c_long;
    fn snd_pcm_recover(pcm: *mut SndPcm, err: c_int, silent: c_int) -> c_int;
    fn snd_pcm_delay(pcm: *mut SndPcm, delay: *mut c_long) -> c_int;
    fn snd_pcm_state(pcm: *mut SndPcm) -> c_int;
    fn snd_pcm_start(pcm: *mut SndPcm) -> c_int;
    fn snd_pcm_nonblock(pcm: *mut SndPcm, nonblock: c_int) -> c_int;
    fn snd_pcm_close(pcm: *mut SndPcm) -> c_int;
    fn snd_strerror(errnum: c_int) -> *const c_char;
    fn snd_device_name_hint(card: c_int, iface: *const c_char, hints: *mut *mut *mut c_void) -> c_int;
    fn snd_device_name_get_hint(hint: *const c_void, id: *const c_char) -> *mut c_char;
    fn snd_device_name_free_hint(hints: *mut *mut c_void) -> c_int;
}

unsafe extern "C" {
    fn free(p: *mut c_void);
}

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_STREAM_CAPTURE: c_int = 1;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;
const SND_PCM_STATE_PREPARED: c_int = 2;
const EAGAIN: c_long = 11;
#[cfg(target_endian = "little")]
const SND_PCM_FORMAT_FLOAT: c_int = 14;
#[cfg(target_endian = "big")]
const SND_PCM_FORMAT_FLOAT: c_int = 15;

struct Pcm(*mut SndPcm);

// SAFETY: le handle n'est utilisé que par un thread à la fois (ouverture, puis thread audio).
unsafe impl Send for Pcm {}

impl Drop for Pcm {
    fn drop(&mut self) {
        // SAFETY: handle ouvert par snd_pcm_open, fermé une seule fois.
        unsafe { snd_pcm_close(self.0) };
    }
}

impl Pcm {
    /// Trames en attente côté système (à jouer, ou captées et pas encore lues).
    fn delay(&self) -> u32 {
        let mut d: c_long = 0;
        // SAFETY: handle ouvert, pointeur valide.
        let ok = unsafe { snd_pcm_delay(self.0, &mut d) } >= 0;
        if ok { d.clamp(0, u32::MAX as c_long) as u32 } else { 0 }
    }
}

pub(crate) struct PendingOutput {
    pcm: Pcm,
    channels: usize,
    period: usize,
}

fn check(code: c_int, call: &str) -> Result<(), AudioError> {
    if code >= 0 {
        return Ok(());
    }
    // SAFETY: snd_strerror renvoie une chaîne statique.
    let msg = unsafe { CStr::from_ptr(snd_strerror(code)) }.to_string_lossy();
    Err(AudioError::System(format!("{call} : {msg}")))
}

/// Ouvre `name` dans le sens `stream` ; renvoie le handle, la période et la taille du tampon.
fn open_pcm(name: &str, stream: c_int, channels: u16, rate: u32) -> Result<(Pcm, usize, c_ulong), AudioError> {
    let capture = stream == SND_PCM_STREAM_CAPTURE;
    let missing = || if capture { AudioError::NoInputDevice } else { AudioError::NoOutputDevice };
    let cname = CString::new(name).map_err(|_| missing())?;
    let mut raw = null_mut();
    // SAFETY: appels FFI avec des pointeurs valides ; `pcm` possède le handle dès l'ouverture.
    unsafe {
        let code = snd_pcm_open(&mut raw, cname.as_ptr(), stream, 0);
        if code == -2 {
            return Err(missing());
        }
        check(code, "snd_pcm_open")?;
    }
    let pcm = Pcm(raw);
    let (mut buffer, mut period) = (0, 0);
    // SAFETY: `pcm.0` est un handle ouvert.
    unsafe {
        check(
            snd_pcm_set_params(
                pcm.0,
                SND_PCM_FORMAT_FLOAT,
                SND_PCM_ACCESS_RW_INTERLEAVED,
                channels as c_uint,
                rate,
                1, // ALSA rééchantillonne si la carte ne suit pas
                // Latence visée ; ALSA découpe ce tampon en périodes.
                crate::target_latency_ms() * 1000,
            ),
            "snd_pcm_set_params",
        )?;
        check(snd_pcm_get_params(pcm.0, &mut buffer, &mut period), "snd_pcm_get_params")?;
    }
    Ok((pcm, (period as usize).max(64), buffer))
}

pub(crate) fn open_default(preferred: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    let (pcm, period, buffer) = open_pcm("default", SND_PCM_STREAM_PLAYBACK, preferred.channels, preferred.sample_rate)?;
    let pending = PendingOutput { pcm, channels: preferred.channels as usize, period };
    let name = format!("ALSA default · période {period} / tampon {buffer} trames");
    Ok((preferred, name, pending))
}

impl PendingOutput {
    pub(crate) fn start(self, mut duplex: Duplex) -> Result<AudioThread, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::Builder::new()
            .name("ponzik-audio".into())
            .spawn(move || {
                let Self { pcm, channels, period } = self;
                let mut buf = vec![0.0f32; period * channels];
                while !flag.load(Ordering::Acquire) {
                    // Ce qui attend déjà d'être joué : le retard du premier échantillon rendu.
                    duplex.render(&mut buf, channels, pcm.delay());
                    let mut written = 0;
                    while written < period {
                        // SAFETY: `buf` contient `period * channels` échantillons entrelacés.
                        let n = unsafe {
                            snd_pcm_writei(
                                pcm.0,
                                buf[written * channels..].as_ptr().cast(),
                                (period - written) as c_ulong,
                            )
                        };
                        if n >= 0 {
                            written += n as usize;
                            continue;
                        }
                        // Sous-alimentation (xrun) ou suspension : on relance le flux.
                        // SAFETY: handle ouvert.
                        let recovered = unsafe { snd_pcm_recover(pcm.0, n as c_int, 1) };
                        if let Err(e) = check(recovered, "snd_pcm_writei") {
                            eprintln!("ponzik-audio : {e}");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| AudioError::System(format!("thread audio : {e}")))?;
        Ok(AudioThread { stop, handle: Some(handle) })
    }
}

pub(crate) struct PendingInput {
    pcm: Pcm,
    /// Canaux ouverts côté ALSA (1 si l'entrée n'accepte que le mono).
    device_channels: usize,
    period: usize,
}

/// Capture : stéréo de préférence, sinon mono. Le flux autonome
/// ([`PendingInput::start`]) duplique le mono sur deux canaux ; le duplex le
/// remet tel quel.
pub(crate) fn open_input(preferred: OutputConfig, id: Option<&str>) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    let rate = preferred.sample_rate;
    let device = id.unwrap_or("default");
    let (pcm, period, buffer, device_channels) = match open_pcm(device, SND_PCM_STREAM_CAPTURE, preferred.channels, rate) {
        Ok((pcm, period, buffer)) => (pcm, period, buffer, preferred.channels as usize),
        Err(AudioError::NoInputDevice) => return Err(AudioError::NoInputDevice),
        Err(stereo) => match open_pcm(device, SND_PCM_STREAM_CAPTURE, 1, rate) {
            Ok((pcm, period, buffer)) => (pcm, period, buffer, 1),
            Err(_) => return Err(stereo),
        },
    };
    let mono = if device_channels == 1 { " · mono" } else { "" };
    let name = format!("ALSA {device} (capture) · période {period} / tampon {buffer} trames{mono}");
    let config = OutputConfig { sample_rate: rate, channels: device_channels as u16 };
    Ok((config, name, PendingInput { pcm, device_channels, period }))
}

impl PendingInput {
    pub(crate) fn start(self, mut capture: CaptureFn) -> Result<AudioThread, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::Builder::new()
            .name("ponzik-capture".into())
            .spawn(move || {
                let Self { pcm, device_channels, period } = self;
                let mut raw = vec![0.0f32; period * device_channels];
                let mut stereo = vec![0.0f32; if device_channels == 1 { period * 2 } else { 0 }];
                while !flag.load(Ordering::Acquire) {
                    // SAFETY: `raw` peut recevoir `period` trames de `device_channels` échantillons.
                    let n = unsafe { snd_pcm_readi(pcm.0, raw.as_mut_ptr().cast(), period as c_ulong) };
                    if n < 0 {
                        // Débordement (xrun) ou suspension : on relance le flux.
                        // SAFETY: handle ouvert.
                        let recovered = unsafe { snd_pcm_recover(pcm.0, n as c_int, 1) };
                        if let Err(e) = check(recovered, "snd_pcm_readi") {
                            eprintln!("ponzik-audio : {e}");
                            return;
                        }
                        continue;
                    }
                    let frames = (n as usize).min(period);
                    if frames == 0 {
                        continue;
                    }
                    if device_channels == 1 {
                        for (d, &s) in stereo.chunks_exact_mut(2).zip(&raw[..frames]) {
                            d.fill(s);
                        }
                        capture(&stereo[..frames * 2], 2);
                    } else {
                        capture(&raw[..frames * device_channels], device_channels);
                    }
                }
            })
            .map_err(|e| AudioError::System(format!("thread de capture : {e}")))?;
        Ok(AudioThread { stop, handle: Some(handle) })
    }
}

/// Capture lue par le thread de sortie, en mode non bloquant.
struct AlsaCapture {
    pcm: Pcm,
    channels: usize,
    scratch: Vec<f32>,
}

impl CaptureSource for AlsaCapture {
    fn pump(&mut self, fifo: &mut InputFifo) -> u32 {
        let frames = self.scratch.len() / self.channels;
        // SAFETY: handle ouvert ; `scratch` peut recevoir `frames` trames.
        unsafe {
            if snd_pcm_state(self.pcm.0) == SND_PCM_STATE_PREPARED {
                snd_pcm_start(self.pcm.0);
            }
            loop {
                let n = snd_pcm_readi(self.pcm.0, self.scratch.as_mut_ptr().cast(), frames as c_ulong);
                if n == -EAGAIN || n == 0 {
                    break;
                }
                if n < 0 {
                    // Débordement : on relance et on reprendra au prochain cycle.
                    if snd_pcm_recover(self.pcm.0, n as c_int, 1) >= 0 {
                        snd_pcm_start(self.pcm.0);
                    }
                    break;
                }
                let n = (n as usize).min(frames);
                fifo.push(&self.scratch[..n * self.channels]);
                if n < frames {
                    break;
                }
            }
        }
        self.pcm.delay()
    }
}

/// Branche la capture sur le thread de sortie : lue sur place, sans thread.
pub(crate) fn attach(pending: PendingInput) -> Result<Attachment, AudioError> {
    let PendingInput { pcm, device_channels, period } = pending;
    // SAFETY: handle ouvert.
    check(unsafe { snd_pcm_nonblock(pcm.0, 1) }, "snd_pcm_nonblock")?;
    let scratch = vec![0.0; period.max(256) * 4 * device_channels];
    Ok((Box::new(AlsaCapture { pcm, channels: device_channels, scratch }), device_channels, None))
}

/// Lit un champ d'une indication de périphérique (chaîne libérée ici).
///
/// SAFETY: `hint` vient de `snd_device_name_hint`.
unsafe fn hint_field(hint: *const c_void, id: &CStr) -> Option<String> {
    // SAFETY: invariant de la fonction ; la chaîne rendue est à libérer par `free`.
    unsafe {
        let p = snd_device_name_get_hint(hint, id.as_ptr());
        if p.is_null() {
            return None;
        }
        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
        free(p.cast());
        Some(s)
    }
}

/// Périphériques de capture utiles : le défaut, les serveurs de son, et une
/// entrée convertie (`plughw`, `sysdefault`, `dsnoop`) par carte.
pub(crate) fn input_devices() -> Vec<DeviceInfo> {
    let mut out = vec![DeviceInfo { id: "default".into(), name: "Entrée par défaut du système".into() }];
    let mut hints = null_mut();
    // SAFETY: liste rendue par ALSA, terminée par un pointeur nul, libérée une fois.
    unsafe {
        if snd_device_name_hint(-1, c"pcm".as_ptr(), &mut hints) < 0 || hints.is_null() {
            return out;
        }
        let mut at = hints;
        while !(*at).is_null() {
            let hint = *at as *const c_void;
            at = at.add(1);
            let Some(name) = hint_field(hint, c"NAME") else { continue };
            if hint_field(hint, c"IOID").is_some_and(|io| io == "Output") || !useful_capture(&name) {
                continue;
            }
            let desc = hint_field(hint, c"DESC").unwrap_or_default();
            let label = desc.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" · ");
            if !out.iter().any(|d| d.id == name) {
                out.push(DeviceInfo { name: if label.is_empty() { name.clone() } else { label }, id: name });
            }
        }
        snd_device_name_free_hint(hints);
    }
    out
}

fn useful_capture(name: &str) -> bool {
    matches!(name, "pipewire" | "pulse" | "jack")
        || ["plughw:", "sysdefault:", "dsnoop:"].iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_filter_keeps_converting_devices() {
        assert!(useful_capture("pipewire"));
        assert!(useful_capture("plughw:CARD=PCH,DEV=0"));
        assert!(!useful_capture("hw:CARD=PCH,DEV=0"));
        assert!(!useful_capture("front:CARD=PCH,DEV=0"));
        assert!(!useful_capture("null"));
    }

    /// À lancer à la main : `cargo test -p ponzik-audio -- --ignored --nocapture`.
    #[test]
    #[ignore = "dépend des cartes son présentes"]
    fn lists_input_devices() {
        for d in input_devices() {
            println!("{} — {}", d.id, d.name);
        }
    }
}
