//! Pilote ALSA : liaisons FFI maison vers `libasound`.
//!
//! On ouvre le périphérique `default` : sur un bureau moderne il est routé vers
//! PipeWire ou PulseAudio, sinon il attaque directement la carte son.

use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::{AudioError, AudioThread, OutputConfig, RenderFn};

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
    fn snd_pcm_recover(pcm: *mut SndPcm, err: c_int, silent: c_int) -> c_int;
    fn snd_pcm_close(pcm: *mut SndPcm) -> c_int;
    fn snd_strerror(errnum: c_int) -> *const c_char;
}

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;
#[cfg(target_endian = "little")]
const SND_PCM_FORMAT_FLOAT: c_int = 14;
#[cfg(target_endian = "big")]
const SND_PCM_FORMAT_FLOAT: c_int = 15;
/// Latence visée ; ALSA découpe ce tampon en périodes.
const LATENCY_US: c_uint = 20_000;

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

pub(crate) fn open_default(preferred: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    let mut raw = null_mut();
    // SAFETY: appels FFI avec des pointeurs valides ; `pcm` possède le handle dès l'ouverture.
    unsafe {
        let code = snd_pcm_open(&mut raw, c"default".as_ptr(), SND_PCM_STREAM_PLAYBACK, 0);
        if code == -2 {
            return Err(AudioError::NoOutputDevice);
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
                preferred.channels as c_uint,
                preferred.sample_rate,
                1, // ALSA rééchantillonne si la carte ne suit pas
                LATENCY_US,
            ),
            "snd_pcm_set_params",
        )?;
        check(snd_pcm_get_params(pcm.0, &mut buffer, &mut period), "snd_pcm_get_params")?;
    }
    let pending = PendingOutput { pcm, channels: preferred.channels as usize, period: (period as usize).max(64) };
    let name = format!("ALSA default · période {} / tampon {} trames", pending.period, buffer);
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
