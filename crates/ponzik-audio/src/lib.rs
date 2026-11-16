//! Pilotes audio maison de Ponzik, sans dépendance : on parle directement
//! aux API du système.
//!
//! - Linux : ALSA (`libasound`), qui passe par PipeWire/PulseAudio s'ils tournent.
//! - Windows : WASAPI en mode partagé, piloté par événement, thread MMCSS « Pro Audio ».
//! - macOS (expérimental) : CoreAudio, unités AUHAL et rappels temps réel.
//!
//! Le pilote ne connaît pas le moteur : il appelle une fonction de rendu
//! `FnMut(&mut [f32], canaux)` depuis son propre thread audio, et, pour la
//! capture, une fonction `FnMut(&[f32], canaux)` depuis le thread de capture.
//!
//! Duplex intégral ([`Output::start_duplex`], [`OutputStream::set_input`]) :
//! l'entrée est remise à la fonction de rendu, dans le même appel que la
//! sortie, pour une écoute de contrôle sans retard ajouté. Sous ALSA, la
//! capture est lue directement par le thread de sortie ; sous WASAPI, son
//! thread dépose les trames dans une file sans verrou relue par la sortie.
//!
//! Un pilote (module `backend`, voir `unsupported.rs` pour le squelette) fournit :
//! `open_default` (sortie), `open_input` (entrée par identifiant),
//! `input_devices`, `PendingOutput::start(Duplex)` qui appelle
//! `Duplex::render` à chaque cycle avec le retard de sortie mesuré, et
//! `attach`, qui transforme une entrée ouverte en `CaptureSource` relue par
//! le thread de sortie. Le reste (file d'entrée, latences publiées, échange
//! d'entrée à chaud) est commun : un pilote CoreAudio n'aurait qu'à lire son
//! unité d'entrée dans son rappel de sortie, comme ALSA.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

mod fifo;

pub(crate) use fifo::InputFifo;
#[cfg_attr(not(windows), allow(unused_imports))]
pub(crate) use fifo::{RingConsumer, ring};

#[cfg(target_os = "linux")]
#[path = "alsa.rs"]
mod backend;

#[cfg(windows)]
#[path = "wasapi.rs"]
mod backend;

#[cfg(target_os = "macos")]
#[path = "coreaudio.rs"]
mod backend;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[path = "unsupported.rs"]
mod backend;

/// Latence de sortie visée, en millisecondes : 10 par défaut, ou
/// `PONZIK_LATENCY_MS` (2 à 100) pour une machine qui décroche.
pub fn target_latency_ms() -> u32 {
    std::env::var("PONZIK_LATENCY_MS").ok().and_then(|v| v.trim().parse::<u32>().ok()).map_or(10, |ms| ms.clamp(2, 100))
}

/// Ce qu'on demande au système quand il nous laisse le choix.
const PREFERRED: OutputConfig = OutputConfig { sample_rate: 48_000, channels: 2 };

pub(crate) type CaptureFn = Box<dyn FnMut(&[f32], usize) + Send + 'static>;
/// Rendu duplex : (entrée entrelacée, canaux d'entrée, sortie, canaux de sortie).
pub(crate) type DuplexFn = Box<dyn FnMut(&[f32], usize, &mut [f32], usize) + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Configuration d'une entrée : même forme que celle d'une sortie.
pub type InputConfig = OutputConfig;

#[derive(Debug)]
pub enum AudioError {
    NoOutputDevice,
    NoInputDevice,
    UnsupportedFormat(String),
    UnsupportedPlatform,
    System(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoOutputDevice => write!(f, "no audio output found"),
            AudioError::NoInputDevice => write!(f, "no audio input found"),
            AudioError::UnsupportedFormat(fmt) => write!(f, "unsupported audio format: {fmt}"),
            AudioError::UnsupportedPlatform => write!(f, "unsupported platform (Linux, Windows and macOS only)"),
            AudioError::System(msg) => write!(f, "system audio error: {msg}"),
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
    pub fn start<F>(self, mut render: F) -> Result<OutputStream, AudioError>
    where
        F: FnMut(&mut [f32], usize) + Send + 'static,
    {
        self.start_duplex(move |_, _, out, channels| render(out, channels))
    }

    /// Lance le thread audio en duplex : `render(entrée, canaux d'entrée, sortie,
    /// canaux de sortie)`. L'entrée est vide (0 canal) tant qu'aucune capture
    /// n'est branchée par [`OutputStream::set_input`] ; sinon elle a autant de
    /// trames que la sortie, captées au plus près.
    pub fn start_duplex<F>(self, render: F) -> Result<OutputStream, AudioError>
    where
        F: FnMut(&[f32], usize, &mut [f32], usize) + Send + 'static,
    {
        let (control_tx, control_rx) = mpsc::channel();
        let (back_tx, back_rx) = mpsc::channel();
        let info = Arc::new(StreamInfo::default());
        let duplex = Duplex { render: Box::new(render), control: control_rx, back: back_tx, attached: None, info: info.clone() };
        let thread = self.pending.start(duplex)?;
        Ok(OutputStream {
            config: self.config,
            device_name: self.device_name,
            thread,
            info,
            control: control_tx,
            returned: back_rx,
            capture: None,
            input: None,
        })
    }
}

/// Latences mesurées par le thread de sortie, en trames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Latency {
    /// D'un échantillon rendu à sa sortie du convertisseur (tampons du système compris).
    pub output: u32,
    /// D'un son capté à sa remise au rendu (tampons du système et file d'entrée).
    pub input: u32,
}

impl Latency {
    /// Aller-retour : ce qu'on joue revient enregistré `round_trip` trames plus tard.
    pub fn round_trip(&self) -> u32 {
        self.output + self.input
    }

    /// Trames → millisecondes à la fréquence `sample_rate`.
    pub fn ms(frames: u32, sample_rate: u32) -> f32 {
        frames as f32 * 1000.0 / sample_rate.max(1) as f32
    }
}

/// Ce que le thread de sortie publie.
#[derive(Default)]
pub(crate) struct StreamInfo {
    output: AtomicU32,
    input: AtomicU32,
    underruns: AtomicU32,
}

/// Une capture lue par le thread de sortie.
pub(crate) trait CaptureSource: Send {
    /// Verse dans `fifo` tout ce qui est arrivé, sans bloquer ; renvoie le
    /// retard (en trames) de la capture pas encore lue côté système.
    fn pump(&mut self, fifo: &mut InputFifo) -> u32;
}

/// Capture prête à brancher : source, canaux, thread de capture éventuel.
pub(crate) type Attachment = (Box<dyn CaptureSource>, usize, Option<AudioThread>);

/// Capture branchée sur la sortie, avec ses tampons alloués hors du thread audio.
pub(crate) struct Attached {
    source: Box<dyn CaptureSource>,
    fifo: InputFifo,
    buf: Vec<f32>,
}

/// Trames gardées par la file d'entrée du duplex.
const DUPLEX_FIFO: usize = 16_384;

/// État du rendu duplex, propre au thread de sortie.
pub(crate) struct Duplex {
    render: DuplexFn,
    control: mpsc::Receiver<Option<Box<Attached>>>,
    back: mpsc::Sender<Box<Attached>>,
    attached: Option<Box<Attached>>,
    info: Arc<StreamInfo>,
}

impl Duplex {
    /// Un cycle : relève la capture, puis rend `out`. `out_delay` : trames déjà
    /// en attente côté système avant celles-ci.
    pub(crate) fn render(&mut self, out: &mut [f32], channels: usize, out_delay: u32) {
        while let Ok(msg) = self.control.try_recv() {
            if let Some(old) = std::mem::replace(&mut self.attached, msg) {
                // Libérée par l'interface, pas ici.
                let _ = self.back.send(old);
            }
        }
        self.info.output.store(out_delay, Ordering::Relaxed);
        let frames = out.len() / channels.max(1);
        match self.attached.as_deref_mut() {
            Some(Attached { source, fifo, buf }) => {
                let pending = source.pump(fifo);
                let ch = fifo.channels();
                let n = frames.min(buf.len() / ch);
                let waited = fifo.pull(&mut buf[..n * ch]);
                self.info.input.store(pending.saturating_add(waited as u32), Ordering::Relaxed);
                self.info.underruns.store(fifo.underruns(), Ordering::Relaxed);
                (self.render)(&buf[..n * ch], ch, out, channels);
            }
            None => (self.render)(&[], 0, out, channels),
        }
    }
}

/// Flux actif ; le son s'arrête et le thread audio se termine quand il est détruit.
pub struct OutputStream {
    config: OutputConfig,
    device_name: String,
    thread: AudioThread,
    info: Arc<StreamInfo>,
    control: mpsc::Sender<Option<Box<Attached>>>,
    returned: mpsc::Receiver<Box<Attached>>,
    /// Thread de capture séparé (WASAPI), arrêté quand l'entrée change.
    capture: Option<AudioThread>,
    input: Option<(InputConfig, String)>,
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

    /// Branche une entrée sur le rendu duplex (`None` : la débranche). L'entrée
    /// doit tourner à la fréquence de la sortie.
    pub fn set_input(&mut self, input: Option<Input>) -> Result<(), AudioError> {
        self.collect();
        let old_thread = self.capture.take();
        let attached = match input {
            Some(input) => {
                let (source, channels, thread) = backend::attach(input.pending)?;
                self.capture = thread;
                self.input = Some((input.config, input.device_name));
                let fifo = InputFifo::new(DUPLEX_FIFO, channels);
                Some(Box::new(Attached { source, fifo, buf: vec![0.0; DUPLEX_FIFO * channels.max(1)] }))
            }
            None => {
                self.input = None;
                None
            }
        };
        self.control.send(attached).map_err(|_| AudioError::System("the audio thread stopped".into()))?;
        drop(old_thread);
        Ok(())
    }

    /// Configuration et nom de l'entrée branchée.
    pub fn input(&self) -> Option<(InputConfig, &str)> {
        self.input.as_ref().map(|(c, n)| (*c, n.as_str()))
    }

    /// Latences mesurées au dernier cycle.
    pub fn latency(&self) -> Latency {
        self.collect();
        Latency { output: self.info.output.load(Ordering::Relaxed), input: self.info.input.load(Ordering::Relaxed) }
    }

    /// Famines de l'entrée (trames arrivées trop tard) depuis son branchement.
    pub fn input_underruns(&self) -> u32 {
        self.info.underruns.load(Ordering::Relaxed)
    }

    /// Libère les captures remplacées.
    fn collect(&self) {
        while self.returned.try_recv().is_ok() {}
    }
}

/// Entrée (capture) ouverte mais pas encore lancée.
pub struct Input {
    config: InputConfig,
    device_name: String,
    pending: backend::PendingInput,
}

/// Ouvre l'entrée audio par défaut du système (micro, entrée ligne…).
pub fn open_default_input() -> Result<Input, AudioError> {
    open_input(None, PREFERRED.sample_rate)
}

/// Ouvre une entrée par son identifiant ([`input_devices`]), ou celle par
/// défaut, à la fréquence voulue quand le système la laisse choisir.
pub fn open_input(id: Option<&str>, sample_rate: u32) -> Result<Input, AudioError> {
    let preferred = OutputConfig { sample_rate, ..PREFERRED };
    let (config, device_name, pending) = backend::open_input(preferred, id)?;
    Ok(Input { config, device_name, pending })
}

/// Périphérique audio proposé par le système.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Identifiant à passer à [`open_input`].
    pub id: String,
    /// Nom lisible.
    pub name: String,
}

/// Entrées disponibles, celle par défaut en premier.
pub fn input_devices() -> Vec<DeviceInfo> {
    backend::input_devices()
}

impl Input {
    pub fn config(&self) -> InputConfig {
        self.config
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Lance le thread de capture ; `capture` reçoit des trames `f32` entrelacées et le nombre de canaux.
    pub fn start<F>(self, capture: F) -> Result<InputStream, AudioError>
    where
        F: FnMut(&[f32], usize) + Send + 'static,
    {
        let thread = self.pending.start(Box::new(capture))?;
        Ok(InputStream { config: self.config, device_name: self.device_name, thread })
    }
}

/// Capture active ; elle s'arrête et son thread se termine quand le flux est détruit.
pub struct InputStream {
    config: InputConfig,
    device_name: String,
    thread: AudioThread,
}

impl InputStream {
    pub fn config(&self) -> InputConfig {
        self.config
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Faux si le thread de capture s'est arrêté tout seul (périphérique débranché…).
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

    /// Lit `src` (octets, sans contrainte d'alignement) vers `dst`.
    pub(crate) fn decode(self, src: &[u8], dst: &mut [f32]) {
        debug_assert_eq!(src.len(), dst.len() * self.bytes());
        match self {
            SampleKind::F32 => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = f32::from_ne_bytes([s[0], s[1], s[2], s[3]]);
                }
            }
            SampleKind::I16 => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *d = i16::from_ne_bytes([s[0], s[1]]) as f32 / 32_768.0;
                }
            }
            SampleKind::I32 => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = (i32::from_ne_bytes([s[0], s[1], s[2], s[3]]) as f64 / 2_147_483_648.0) as f32;
                }
            }
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

    #[test]
    fn decodes_every_format() {
        let mut out = [0.0f32; 2];
        let b: Vec<u8> = [0.5f32, -1.0].iter().flat_map(|s| s.to_ne_bytes()).collect();
        SampleKind::F32.decode(&b, &mut out);
        assert_eq!(out, [0.5, -1.0]);

        let b: Vec<u8> = [16_384i16, i16::MIN].iter().flat_map(|s| s.to_ne_bytes()).collect();
        SampleKind::I16.decode(&b, &mut out);
        assert_eq!(out, [0.5, -1.0]);

        let b: Vec<u8> = [1i32 << 30, i32::MIN].iter().flat_map(|s| s.to_ne_bytes()).collect();
        SampleKind::I32.decode(&b, &mut out);
        assert_eq!(out, [0.5, -1.0]);
    }

    /// À lancer à la main : `cargo test -p ponzik-audio -- --ignored --nocapture`.
    #[test]
    #[ignore = "nécessite une entrée et une sortie audio"]
    fn duplex_delivers_input_with_the_output() {
        let output = open_default().expect("sortie");
        let rate = output.config().sample_rate;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut frames = 0usize;
        let mut sent = false;
        let mut stream = output
            .start_duplex(move |input, in_ch, out, ch| {
                out.fill(0.0);
                if let Some(n) = input.len().checked_div(in_ch) {
                    assert_eq!(n, out.len() / ch, "autant de trames en entrée qu'en sortie");
                    frames += n;
                }
                if frames >= rate as usize && !sent {
                    sent = true;
                    let _ = tx.send(frames);
                }
            })
            .expect("lancement");
        let device = std::env::var("PONZIK_INPUT").ok();
        let input = match open_input(device.as_deref(), rate) {
            Ok(i) => i,
            Err(e) => {
                println!("pas d'entrée : {e}");
                return;
            }
        };
        println!("entrée : {}", input.device_name());
        stream.set_input(Some(input)).expect("branchement");
        let got = rx.recv_timeout(std::time::Duration::from_secs(3));
        let lat = stream.latency();
        println!(
            "duplex : {got:?} · sortie {:.1} ms · entrée {:.1} ms · famines {}",
            Latency::ms(lat.output, rate),
            Latency::ms(lat.input, rate),
            stream.input_underruns()
        );
        assert!(got.is_ok(), "aucune trame d'entrée en 3 s");
        stream.set_input(None).expect("débranchement");
    }

    /// À lancer à la main : `cargo test -p ponzik-audio -- --ignored --nocapture`.
    #[test]
    #[ignore = "nécessite une entrée audio"]
    fn opens_default_input() {
        let input = match open_default_input() {
            Ok(input) => input,
            Err(e) => {
                println!("pas d'entrée : {e}");
                return;
            }
        };
        println!("entrée : {} · {:?}", input.device_name(), input.config());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut sent = false;
        let mut frames = 0;
        let mut peak = 0.0f32;
        let stream = input
            .start(move |buf, channels| {
                frames += buf.len() / channels;
                peak = buf.iter().fold(peak, |m, s| m.max(s.abs()));
                if frames >= 24_000 && !sent {
                    sent = true;
                    let _ = tx.send((frames, channels, peak));
                }
            })
            .expect("lancement de la capture");
        let got = rx.recv_timeout(std::time::Duration::from_secs(3));
        println!("capture : {got:?}, toujours active : {}", stream.is_running());
        assert!(got.is_ok(), "aucune trame capturée en 3 s");
    }
}
