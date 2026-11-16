//! Sortie MIDI du moteur : notes des instruments externes et horloge MIDI.
//!
//! Le moteur horodate chaque message à l'échantillon près sur sa propre
//! horloge, puis le convertit en échéance murale ([`ponzik_midi::out::now_ns`]) :
//! heure du début du `process` en cours + écart en échantillons + latence du
//! mixer (compensation des inserts) + latence de sortie audio. Le message part
//! ainsi quand l'audio du même instant sort des haut-parleurs. Le thread
//! d'envoi ([`ponzik_midi::out::OutputScheduler`]) écrit ensuite au plus près
//! de cette échéance ; voir la documentation de ce module pour la gigue.

use ponzik_midi::message::{CLOCK, CONTINUE, MAX_SONG_POSITION, START, STOP};
use ponzik_midi::out::{MessageSender, TimedMessage, now_ns};

pub use ponzik_midi::ShortMessage;

use crate::sequence::PPQ;

/// Ticks par impulsion d'horloge MIDI (24 par noire).
const TICKS_PER_CLOCK: f64 = PPQ as f64 / 24.0;
/// Ticks par « temps MIDI » du Song Position Pointer (une double croche).
const TICKS_PER_SIXTEENTH: f64 = PPQ as f64 / 4.0;
const EPS: f64 = 1e-9;

/// Message produit par un instrument (voir [`crate::Instrument::drain_midi`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MidiOutEvent {
    /// Indice du port de sortie.
    pub port: u8,
    /// Décalage en échantillons par rapport à l'instant de l'événement
    /// (négatif : envoyé en avance, pour compenser un appareil lent).
    pub offset: i32,
    pub message: ShortMessage,
}

/// Lien vers le thread d'envoi, confié au moteur par [`crate::Command::SetMidiOut`].
pub struct MidiOut {
    pub sender: MessageSender,
    /// Latence de sortie audio en secondes (du rendu au convertisseur) :
    /// les messages sont retardés d'autant pour sortir avec le son.
    pub output_latency: f32,
}

impl MidiOut {
    pub fn new(sender: MessageSender, output_latency: f32) -> Self {
        Self { sender, output_latency }
    }
}

/// Lien et repère de temps du `process` en cours.
pub(crate) struct MidiLink {
    pub out: Option<Box<MidiOut>>,
    sample_rate: f64,
    /// Horloge du moteur et heure murale au début du `process` en cours.
    base_clock: u64,
    base_ns: u64,
    /// Retard ajouté à tout message (latence du mixer), en échantillons.
    delay: u64,
}

impl MidiLink {
    pub fn new(sample_rate: f32) -> Self {
        Self { out: None, sample_rate: sample_rate.max(1.0) as f64, base_clock: 0, base_ns: 0, delay: 0 }
    }

    pub fn is_connected(&self) -> bool {
        self.out.is_some()
    }

    /// Début d'un `process` : fixe le repère horloge du moteur ↔ heure murale.
    pub fn begin(&mut self, clock: u64, mixer_latency: usize) {
        if self.out.is_some() {
            self.base_clock = clock;
            self.base_ns = now_ns();
            self.delay = mixer_latency as u64;
        }
    }

    /// Programme `message` sur `port` à l'échantillon `frame` du moteur.
    pub fn send(&mut self, port: u8, frame: i64, message: ShortMessage) {
        let Some(out) = self.out.as_mut() else { return };
        let frame = frame.saturating_add(self.delay as i64).max(0);
        let secs = (frame - self.base_clock as i64) as f64 / self.sample_rate + out.output_latency.max(0.0) as f64;
        let at_ns = (self.base_ns as f64 + secs * 1e9).max(0.0) as u64;
        // File pleine : le message est perdu (compté par la file), sans bloquer.
        out.sender.push(TimedMessage { port, at_ns, frame: frame as u64, message });
    }

    /// Envoie `message` sur chaque port du masque.
    fn send_ports(&mut self, ports: u32, frame: i64, message: ShortMessage) {
        for port in (0..32u8).filter(|p| ports & (1 << p) != 0) {
            self.send(port, frame, message);
        }
    }
}

/// Horloge MIDI sortante : 24 impulsions par noire, Start, Stop, Continue et
/// Song Position Pointer, calés sur la position du moteur.
///
/// - Lecture depuis 0 : Start puis une impulsion sur chaque 1/24 de noire.
/// - Lecture ailleurs (reprise, saut, retour de boucle) : Stop si l'horloge
///   tournait, Song Position Pointer de la double croche suivante, puis
///   Continue et la première impulsion pile sur cette double croche — le
///   récepteur reprend ainsi exactement en phase.
/// - Arrêt : Stop ; un saut à l'arrêt envoie le Song Position Pointer.
#[derive(Debug)]
pub(crate) struct MidiClock {
    /// Ports qui reçoivent l'horloge (bit par port) ; 0 : horloge coupée.
    pub ports: u32,
    running: bool,
    /// Continue en attente : envoyé juste avant l'impulsion de ce tick.
    resume_at: Option<f64>,
    /// Position annoncée à l'arrêt (NaN : rien d'annoncé).
    stopped_at: f64,
}

impl Default for MidiClock {
    fn default() -> Self {
        Self { ports: 0, running: false, resume_at: None, stopped_at: f64::NAN }
    }
}

/// Song Position Pointer d'une position en ticks.
fn song_position(sixteenths: f64) -> ShortMessage {
    ShortMessage::song_position(sixteenths.clamp(0.0, MAX_SONG_POSITION as f64) as u32)
}

impl MidiClock {
    /// Change les ports ; si l'horloge tournait, ceux-ci reçoivent Stop et
    /// les nouveaux repartent depuis la position courante.
    pub fn set_ports(&mut self, link: &mut MidiLink, ports: u32, now: u64) {
        if ports == self.ports {
            return;
        }
        if self.running {
            link.send_ports(self.ports, now as i64, ShortMessage::realtime(STOP));
        }
        *self = Self { ports, ..Self::default() };
    }

    /// Bloc de `n` échantillons commençant à l'horloge `clock`, au tick
    /// `pos_start` et finissant au tick `to` ; `contiguous` : le bloc suit le
    /// précédent sans saut. `frame_of` gives the frame (in the block) of a tick,
    /// through the tempo map.
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        link: &mut MidiLink,
        playing: bool,
        pos_start: f64,
        contiguous: bool,
        n: usize,
        to: f64,
        frame_of: &dyn Fn(f64) -> usize,
        clock: u64,
    ) {
        if self.ports == 0 || !link.is_connected() || n == 0 {
            self.running = false;
            self.resume_at = None;
            return;
        }
        let ports = self.ports;
        let now = clock as i64;
        if !playing {
            if self.running {
                link.send_ports(ports, now, ShortMessage::realtime(STOP));
                self.running = false;
                self.resume_at = None;
                self.stopped_at = pos_start;
            } else if (pos_start - self.stopped_at).abs() > EPS || self.stopped_at.is_nan() {
                // Saut à l'arrêt (ou horloge tout juste activée).
                link.send_ports(ports, now, song_position((pos_start / TICKS_PER_SIXTEENTH + EPS).floor()));
                self.stopped_at = pos_start;
            }
            return;
        }
        if self.running && !contiguous {
            link.send_ports(ports, now, ShortMessage::realtime(STOP));
            self.running = false;
        }
        if !self.running {
            self.running = true;
            self.stopped_at = f64::NAN;
            if pos_start.abs() < EPS {
                link.send_ports(ports, now, ShortMessage::realtime(START));
                self.resume_at = None;
            } else {
                let next = (pos_start / TICKS_PER_SIXTEENTH - EPS).ceil();
                link.send_ports(ports, now, song_position(next));
                self.resume_at = Some(next * TICKS_PER_SIXTEENTH);
            }
        }
        let from = self.resume_at.map_or(pos_start, |r| r.max(pos_start));
        let mut k = ((from - EPS) / TICKS_PER_CLOCK).ceil();
        loop {
            let tick = k * TICKS_PER_CLOCK;
            if tick >= to - EPS {
                break;
            }
            let frame = now + frame_of(tick).min(n - 1) as i64;
            if self.resume_at.is_some_and(|r| (r - tick).abs() < EPS) {
                link.send_ports(ports, frame, ShortMessage::realtime(CONTINUE));
                self.resume_at = None;
            }
            link.send_ports(ports, frame, ShortMessage::realtime(CLOCK));
            k += 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_midi::out::{MessageReceiver, queue};

    const SR: f32 = 48_000.0;
    /// 120 BPM à 48 kHz : 250 échantillons par tick, 1 000 par impulsion.
    const SPT: f64 = 250.0;

    fn link() -> (MidiLink, MessageReceiver) {
        let (tx, rx) = queue(4_096);
        let mut link = MidiLink::new(SR);
        link.out = Some(Box::new(MidiOut::new(tx, 0.0)));
        (link, rx)
    }

    fn drain(rx: &mut MessageReceiver) -> Vec<(u8, u64, Vec<u8>)> {
        std::iter::from_fn(|| rx.pop()).map(|m| (m.port, m.frame, m.message.bytes().to_vec())).collect()
    }

    /// Un bloc à tempo constant.
    #[allow(clippy::too_many_arguments)]
    fn block(clock: &mut MidiClock, link: &mut MidiLink, playing: bool, pos: f64, contiguous: bool, n: usize, at: u64) {
        let frame_of = |tick: f64| ((tick - pos) * SPT - 1e-6).ceil().max(0.0) as usize;
        clock.process(link, playing, pos, contiguous, n, pos + n as f64 / SPT, &frame_of, at);
    }

    /// Joue `blocks` blocs de `n` échantillons depuis `pos`, sans saut.
    fn play(clock: &mut MidiClock, link: &mut MidiLink, pos: f64, clock0: u64, blocks: usize, n: usize) {
        for b in 0..blocks {
            let at = clock0 + (b * n) as u64;
            link.begin(at, 0);
            block(clock, link, true, pos + (b * n) as f64 / SPT, b > 0, n, at);
        }
    }

    #[test]
    fn start_then_24_pulses_per_quarter_whatever_the_block_size() {
        for n in [64, 256, 1_000, 7] {
            let (mut link, mut rx) = link();
            let mut clock = MidiClock { ports: 1, ..Default::default() };
            play(&mut clock, &mut link, 0.0, 0, 12_000 / n + 1, n);
            let sent = drain(&mut rx);
            assert_eq!(sent[0], (0, 0, vec![START]), "block {n}");
            let pulses: Vec<u64> = sent.iter().filter(|m| m.2 == [CLOCK]).map(|m| m.1).take(12).collect();
            assert_eq!(pulses, (0..12).map(|k| k * 1_000).collect::<Vec<_>>(), "block {n}");
        }
    }

    #[test]
    fn resume_mid_sixteenth_waits_for_the_next_one() {
        let (mut link, mut rx) = link();
        let mut clock = MidiClock { ports: 0b101, ..Default::default() };
        // Tick 30 : la double croche suivante est au tick 48 (SPP 2), 18 ticks plus loin.
        play(&mut clock, &mut link, 30.0, 10_000, 40, 256);
        let port0: Vec<(u64, Vec<u8>)> = drain(&mut rx).into_iter().filter(|m| m.0 == 0).map(|m| (m.1, m.2)).collect();
        assert_eq!(port0[0], (10_000, vec![0xF2, 2, 0]));
        let resume = 10_000 + 18 * 250;
        assert_eq!(port0[1], (resume, vec![CONTINUE]));
        assert_eq!(port0[2], (resume, vec![CLOCK]));
        assert_eq!(port0[3], (resume + 1_000, vec![CLOCK]));
    }

    #[test]
    fn stop_jump_and_locate_while_stopped() {
        let (mut link, mut rx) = link();
        let mut clock = MidiClock { ports: 1, ..Default::default() };
        play(&mut clock, &mut link, 0.0, 0, 4, 250);
        drain(&mut rx);
        // Saut (retour de boucle) au tick 96 : Stop, SPP 4, Continue, impulsion.
        link.begin(1_000, 0);
        block(&mut clock, &mut link, true, 96.0, false, 250, 1_000);
        let sent: Vec<Vec<u8>> = drain(&mut rx).into_iter().map(|m| m.2).collect();
        assert_eq!(sent, [vec![STOP], vec![0xF2, 4, 0], vec![CONTINUE], vec![CLOCK]]);
        // Arrêt, puis saut à l'arrêt.
        block(&mut clock, &mut link, false, 97.0, false, 250, 1_250);
        block(&mut clock, &mut link, false, 97.0, false, 250, 1_500);
        block(&mut clock, &mut link, false, 384.0, false, 250, 1_750);
        let sent: Vec<(u64, Vec<u8>)> = drain(&mut rx).into_iter().map(|m| (m.1, m.2)).collect();
        assert_eq!(sent, [(1_250, vec![STOP]), (1_750, vec![0xF2, 16, 0])]);
    }

    #[test]
    fn mixer_latency_and_output_latency_delay_the_deadline() {
        let (mut link, mut rx) = link();
        link.out.as_mut().unwrap().output_latency = 0.010;
        link.begin(1_000, 480);
        let before = now_ns();
        link.send(3, 1_000 + 4_800, ShortMessage::note_on(0, 60, 100));
        let m = rx.pop().unwrap();
        assert_eq!((m.port, m.frame), (3, 1_000 + 4_800 + 480));
        // 4 800 + 480 échantillons (110 ms) + 10 ms de sortie.
        let expected = before + 110_000_000 + 10_000_000;
        assert!(m.at_ns.abs_diff(expected) < 5_000_000, "{} vs {expected}", m.at_ns);
    }

    #[test]
    fn disabling_the_clock_stops_running_ports() {
        let (mut link, mut rx) = link();
        let mut clock = MidiClock { ports: 0b11, ..Default::default() };
        play(&mut clock, &mut link, 0.0, 0, 1, 100);
        drain(&mut rx);
        clock.set_ports(&mut link, 0, 100);
        let sent: Vec<(u8, Vec<u8>)> = drain(&mut rx).into_iter().map(|m| (m.0, m.2)).collect();
        assert_eq!(sent, [(0, vec![STOP]), (1, vec![STOP])]);
        block(&mut clock, &mut link, true, 1.0, true, 100, 100);
        assert!(rx.pop().is_none());
    }
}
