//! Pilote ALSA : liaisons FFI maison vers `libasound`.
//!
//! On ouvre le périphérique `default` : sur un bureau moderne il est routé vers
//! PipeWire ou PulseAudio, sinon il attaque directement la carte son.

use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::{AudioError, AudioThread, CaptureFn, OutputConfig, RenderFn};

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
    fn snd_pcm_close(pcm: *mut SndPcm) -> c_int;
    fn snd_strerror(errnum: c_int) -> *const c_char;
}

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_STREAM_CAPTURE: c_int = 1;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;
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

/// Ouvre `default` dans le sens `stream` ; renvoie le handle, la période et la taille du tampon.
fn open_pcm(stream: c_int, channels: u16, rate: u32) -> Result<(Pcm, usize, c_ulong), AudioError> {
    let mut raw = null_mut();
    // SAFETY: appels FFI avec des pointeurs valides ; `pcm` possède le handle dès l'ouverture.
    unsafe {
        let code = snd_pcm_open(&mut raw, c"default".as_ptr(), stream, 0);
        if code == -2 {
            let capture = stream == SND_PCM_STREAM_CAPTURE;
            return Err(if capture { AudioError::NoInputDevice } else { AudioError::NoOutputDevice });
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
    let (pcm, period, buffer) = open_pcm(SND_PCM_STREAM_PLAYBACK, preferred.channels, preferred.sample_rate)?;
    let pending = PendingOutput { pcm, channels: preferred.channels as usize, period };
    let name = format!("ALSA default · période {period} / tampon {buffer} trames");
    Ok((preferred, name, pending))
}

impl PendingOutput {
    pub(crate) fn start(self, mut render: RenderFn) -> Result<AudioThread, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::Builder::new()
            .name("ponzik-audio".into())
            .spawn(move || {
                let Self { pcm, channels, period } = self;
                let mut buf = vec![0.0f32; period * channels];
                while !flag.load(Ordering::Acquire) {
                    render(&mut buf, channels);
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

/// Capture : stéréo de préférence, sinon mono dupliqué sur les deux canaux.
pub(crate) fn open_default_input(preferred: OutputConfig) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    let rate = preferred.sample_rate;
    let (pcm, period, buffer, device_channels) = match open_pcm(SND_PCM_STREAM_CAPTURE, preferred.channels, rate) {
        Ok((pcm, period, buffer)) => (pcm, period, buffer, preferred.channels as usize),
        Err(AudioError::NoInputDevice) => return Err(AudioError::NoInputDevice),
        Err(stereo) => match open_pcm(SND_PCM_STREAM_CAPTURE, 1, rate) {
            Ok((pcm, period, buffer)) => (pcm, period, buffer, 1),
            Err(_) => return Err(stereo),
        },
    };
    let mono = if device_channels == 1 { " · mono dupliqué" } else { "" };
    let name = format!("ALSA default (capture) · période {period} / tampon {buffer} trames{mono}");
    let config = OutputConfig { sample_rate: rate, channels: device_channels.max(2) as u16 };
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
