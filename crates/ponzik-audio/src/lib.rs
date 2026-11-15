//! Pilotes audio maison de Ponzik, sans dépendance : on parle directement
//! aux API du système.
//!
//! - Linux : ALSA (`libasound`), qui passe par PipeWire/PulseAudio s'ils tournent.
//! - Windows : WASAPI en mode partagé, piloté par événement, thread MMCSS « Pro Audio ».
//!
//! Le pilote ne connaît pas le moteur : il appelle une fonction de rendu
//! `FnMut(&mut [f32], canaux)` depuis son propre thread audio.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

#[cfg(target_os = "linux")]
#[path = "alsa.rs"]
mod backend;

#[cfg(windows)]
#[path = "wasapi.rs"]
mod backend;

#[cfg(not(any(target_os = "linux", windows)))]
#[path = "unsupported.rs"]
mod backend;

/// Ce qu'on demande au système quand il nous laisse le choix.
const PREFERRED: OutputConfig = OutputConfig { sample_rate: 48_000, channels: 2 };

pub(crate) type RenderFn = Box<dyn FnMut(&mut [f32], usize) + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug)]
pub enum AudioError {
    NoOutputDevice,
    UnsupportedFormat(String),
    UnsupportedPlatform,
    System(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoOutputDevice => write!(f, "aucune sortie audio trouvée"),
            AudioError::UnsupportedFormat(fmt) => write!(f, "format audio non géré : {fmt}"),
            AudioError::UnsupportedPlatform => write!(f, "plateforme non supportée (Linux et Windows uniquement)"),
            AudioError::System(msg) => write!(f, "erreur audio système : {msg}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Sortie ouverte mais pas encore lancée : on connaît sa configuration,
/// ce qui permet de créer le moteur au bon taux d'échantillonnage.
pub struct Output {
    config: OutputConfig,
    device_name: String,
    pending: backend::PendingOutput,
}

/// Ouvre la sortie audio par défaut du système.
pub fn open_default() -> Result<Output, AudioError> {
    let (config, device_name, pending) = backend::open_default(PREFERRED)?;
    Ok(Output { config, device_name, pending })
}

impl Output {
    pub fn config(&self) -> OutputConfig {
        self.config
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Lance le thread audio ; `render` reçoit un tampon `f32` entrelacé et le nombre de canaux.
    pub fn start<F>(self, render: F) -> Result<OutputStream, AudioError>
    where
        F: FnMut(&mut [f32], usize) + Send + 'static,
    {
        let thread = self.pending.start(Box::new(render))?;
        Ok(OutputStream { config: self.config, device_name: self.device_name, thread })
    }
}

/// Flux actif ; le son s'arrête et le thread audio se termine quand il est détruit.
pub struct OutputStream {
    config: OutputConfig,
    device_name: String,
    thread: AudioThread,
}

impl OutputStream {
    pub fn config(&self) -> OutputConfig {
        self.config
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Faux si le thread audio s'est arrêté tout seul (périphérique débranché…).
    pub fn is_running(&self) -> bool {
        self.thread.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

pub(crate) struct AudioThread {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) handle: Option<JoinHandle<()>>,
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Copie un tampon f32 vers un tampon système d'un autre format.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleKind {
    F32,
    I16,
    I32,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl SampleKind {
    pub(crate) fn bytes(self) -> usize {
        match self {
            SampleKind::I16 => 2,
            SampleKind::F32 | SampleKind::I32 => 4,
        }
    }

    /// Écrit `src` dans `dst` (octets, sans contrainte d'alignement).
    pub(crate) fn encode(self, src: &[f32], dst: &mut [u8]) {
        debug_assert_eq!(dst.len(), src.len() * self.bytes());
        match self {
            SampleKind::F32 => {
                for (d, s) in dst.chunks_exact_mut(4).zip(src) {
                    d.copy_from_slice(&s.to_ne_bytes());
                }
            }
            SampleKind::I16 => {
                for (d, s) in dst.chunks_exact_mut(2).zip(src) {
                    let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                    d.copy_from_slice(&v.to_ne_bytes());
                }
            }
            SampleKind::I32 => {
                for (d, s) in dst.chunks_exact_mut(4).zip(src) {
                    let v = (s.clamp(-1.0, 1.0) as f64 * i32::MAX as f64).round() as i32;
                    d.copy_from_slice(&v.to_ne_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_every_format() {
        let src = [0.0, 1.0, -1.0, 2.0];
        let mut b = [0u8; 16];
        SampleKind::F32.encode(&src, &mut b);
        assert_eq!(f32::from_ne_bytes(b[4..8].try_into().unwrap()), 1.0);

        let mut b = [0u8; 8];
        SampleKind::I16.encode(&src, &mut b);
        let v: Vec<i16> = b.chunks(2).map(|c| i16::from_ne_bytes([c[0], c[1]])).collect();
        assert_eq!(v, [0, i16::MAX, -i16::MAX, i16::MAX]);

        let mut b = [0u8; 16];
        SampleKind::I32.encode(&src, &mut b);
        assert_eq!(i32::from_ne_bytes(b[12..16].try_into().unwrap()), i32::MAX);
    }
}
