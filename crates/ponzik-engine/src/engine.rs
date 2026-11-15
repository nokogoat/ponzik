use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use ponzik_dsp::util::balance_gains;

use crate::rack::{Channel, Step, MAX_STEPS};
use crate::ring::{self, Consumer, Producer};

/// Taille max d'un sous-bloc de rendu : les événements tombent à l'échantillon près.
const MAX_BLOCK: usize = 256;
const COMMAND_QUEUE: usize = 1_024;
/// Échantillons du master gardés pour l'oscilloscope (≈ 0,7 s à 24 kHz).
const SCOPE_QUEUE: usize = 16_384;
/// On ne garde qu'un échantillon sur deux pour l'oscilloscope.
const SCOPE_DECIMATION: u32 = 2;
/// Pas par temps : le séquenceur avance en double-croches.
const STEPS_PER_BEAT: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Stop,
    SetBpm(f32),
    SetPatternLength(usize),
    SetMasterGain(f32),
    ToggleStep { channel: usize, step: usize },
    SetStep { channel: usize, step: usize, value: Option<Step> },
    SetChannelGain { channel: usize, gain: f32 },
    SetChannelPan { channel: usize, pan: f32 },
    SetChannelMute { channel: usize, muted: bool },
    NoteOn { channel: usize, note: u8, velocity: f32 },
    NoteOff { channel: usize, note: u8 },
}

/// Crête stéréo publiée par le thread audio. Les bits d'un `f32` positif sont
/// ordonnés comme des entiers : `fetch_max` suffit, sans verrou.
#[derive(Default)]
struct PeakMeter([AtomicU32; 2]);

impl PeakMeter {
    fn record(&self, l: f32, r: f32) {
        self.0[0].fetch_max(l.abs().to_bits(), Ordering::Relaxed);
        self.0[1].fetch_max(r.abs().to_bits(), Ordering::Relaxed);
    }

    fn take(&self) -> [f32; 2] {
        [0, 1].map(|i| f32::from_bits(self.0[i].swap(0, Ordering::Relaxed)))
    }
}

/// Ce que le moteur publie pour l'interface.
struct SharedState {
    playing: AtomicBool,
    /// Pas en cours de lecture, `usize::MAX` à l'arrêt.
    playhead: AtomicUsize,
    channel_peaks: Vec<PeakMeter>,
    master_peak: PeakMeter,
    /// Pire charge DSP depuis la dernière lecture : temps de calcul / durée du bloc.
    load: AtomicU32,
}

/// Côté interface : envoie des commandes, lit l'état publié.
pub struct EngineHandle {
    commands: Producer<Command>,
    state: Arc<SharedState>,
    scope: Consumer<f32>,
}

impl EngineHandle {
    /// Rend la commande si la file est pleine.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands.push(command)
    }

    pub fn is_playing(&self) -> bool {
        self.state.playing.load(Ordering::Relaxed)
    }

    pub fn playhead(&self) -> Option<usize> {
        match self.state.playhead.load(Ordering::Relaxed) {
            usize::MAX => None,
            step => Some(step),
        }
    }

    /// Crête [gauche, droite] du canal depuis le dernier appel (après gain et pan).
    pub fn take_channel_peak(&self, channel: usize) -> [f32; 2] {
        self.state.channel_peaks.get(channel).map_or([0.0; 2], PeakMeter::take)
    }

    /// Crête [gauche, droite] du master depuis le dernier appel.
    pub fn take_master_peak(&self) -> [f32; 2] {
        self.state.master_peak.take()
    }

    /// Pire charge DSP (1.0 = le bloc a pris tout son temps réel) depuis le dernier appel.
    pub fn take_load(&self) -> f32 {
        f32::from_bits(self.state.load.swap(0, Ordering::Relaxed))
    }

    /// Échantillons mono du master (24 kHz) arrivés depuis le dernier appel.
    pub fn drain_scope(&mut self, mut f: impl FnMut(f32)) {
        while let Some(s) = self.scope.pop() {
            f(s);
        }
    }
}

pub struct Engine {
    sample_rate: f32,
    channels: Vec<Channel>,
    commands: Consumer<Command>,
    state: Arc<SharedState>,
    scope: Producer<f32>,
    scope_phase: u32,

    /// Horloge monotone en échantillons, avance même à l'arrêt (queues de release).
    clock: u64,
    playing: bool,
    bpm: f64,
    pattern_len: usize,
    master_gain: f32,
    /// Prochain pas à déclencher (absolu, non bouclé).
    next_step: u64,
    /// Point d'ancrage du tempo : au `clock` `anchor_sample`, on était au pas `anchor_step`.
    anchor_sample: u64,
    anchor_step: f64,

    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    mix_l: Vec<f32>,
    mix_r: Vec<f32>,
}

impl Engine {
    pub fn new(sample_rate: f32, mut channels: Vec<Channel>) -> (Engine, EngineHandle) {
        for ch in &mut channels {
            ch.instrument.set_sample_rate(sample_rate);
        }
        let (tx, rx) = ring::channel(COMMAND_QUEUE);
        let (scope_tx, scope_rx) = ring::channel(SCOPE_QUEUE);
        let state = Arc::new(SharedState {
            playing: AtomicBool::new(false),
            playhead: AtomicUsize::new(usize::MAX),
            channel_peaks: channels.iter().map(|_| PeakMeter::default()).collect(),
            master_peak: PeakMeter::default(),
            load: AtomicU32::new(0),
        });
        let engine = Engine {
            sample_rate,
            channels,
            commands: rx,
            state: state.clone(),
            scope: scope_tx,
            scope_phase: 0,
            clock: 0,
            playing: false,
            bpm: 120.0,
            pattern_len: 16,
            master_gain: 0.5,
            next_step: 0,
            anchor_sample: 0,
            anchor_step: 0.0,
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            mix_l: vec![0.0; MAX_BLOCK],
            mix_r: vec![0.0; MAX_BLOCK],
        };
        (engine, EngineHandle { commands: tx, state, scope: scope_rx })
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Durée d'un pas en échantillons.
    pub fn samples_per_step(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / (self.bpm * STEPS_PER_BEAT)
    }

    /// Remplit `out` (entrelacé, `channels` canaux) — appelé par le thread audio.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let started = Instant::now();
        self.drain_commands();
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            self.fire_due_events();
            let mut n = (frames - done).min(MAX_BLOCK);
            if let Some(at) = self.next_event() {
                n = n.min(at.saturating_sub(self.clock).max(1) as usize);
            }
            self.render_block(n);
            write_interleaved(
                &mut out[done * channels..(done + n) * channels],
                channels,
                &self.mix_l[..n],
                &self.mix_r[..n],
            );
            self.clock += n as u64;
            done += n;
        }
        if frames > 0 {
            let budget = frames as f32 / self.sample_rate;
            let load = started.elapsed().as_secs_f32() / budget;
            self.state.load.fetch_max(load.to_bits(), Ordering::Relaxed);
        }
    }

    fn drain_commands(&mut self) {
        while let Some(cmd) = self.commands.pop() {
            self.apply(cmd);
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                if !self.playing {
                    self.playing = true;
                    self.next_step = 0;
                    self.anchor_sample = self.clock;
                    self.anchor_step = 0.0;
                }
            }
            Command::Stop => {
                self.playing = false;
                for ch in &mut self.channels {
                    ch.silence();
                }
                self.state.playhead.store(usize::MAX, Ordering::Relaxed);
            }
            Command::SetBpm(bpm) => {
                let bpm = (bpm as f64).clamp(20.0, 999.0);
                // On ré-ancre pour que le changement de tempo ne fasse pas sauter la lecture.
                self.anchor_step = self.step_position();
                self.anchor_sample = self.clock;
                self.bpm = bpm;
            }
            Command::SetPatternLength(len) => self.pattern_len = len.clamp(1, MAX_STEPS),
            Command::SetMasterGain(gain) => self.master_gain = gain.max(0.0),
            Command::ToggleStep { channel, step } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.toggle_step(step);
                }
            }
            Command::SetStep { channel, step, value } => {
                if let Some(slot) = self.channels.get_mut(channel).and_then(|c| c.steps.get_mut(step)) {
                    *slot = value;
                }
            }
            Command::SetChannelGain { channel, gain } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.gain = gain.max(0.0);
                }
            }
            Command::SetChannelPan { channel, pan } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetChannelMute { channel, muted } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.muted = muted;
                }
            }
            Command::NoteOn { channel, note, velocity } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.instrument.note_on(note, velocity);
                }
            }
            Command::NoteOff { channel, note } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.instrument.note_off(note);
                }
            }
        }
        self.state.playing.store(self.playing, Ordering::Relaxed);
    }

    /// Position fractionnaire courante, en pas.
    fn step_position(&self) -> f64 {
        self.anchor_step + (self.clock - self.anchor_sample) as f64 / self.samples_per_step()
    }

    fn step_sample(&self, step: u64) -> u64 {
        let offset = ((step as f64 - self.anchor_step) * self.samples_per_step()).ceil();
        self.anchor_sample + offset.max(0.0) as u64
    }

    fn next_event(&self) -> Option<u64> {
        let step = self.playing.then(|| self.step_sample(self.next_step));
        let off = self.channels.iter().filter_map(Channel::next_off).min();
        match (step, off) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn fire_due_events(&mut self) {
        let now = self.clock;
        for ch in &mut self.channels {
            ch.release_due(now);
        }
        if !self.playing {
            return;
        }
        let sps = self.samples_per_step();
        while self.step_sample(self.next_step) <= now {
            let index = (self.next_step % self.pattern_len as u64) as usize;
            for ch in self.channels.iter_mut().filter(|c| !c.muted) {
                if let Some(step) = ch.steps[index] {
                    let off_at = now + (step.length.max(0.01) as f64 * sps).round().max(1.0) as u64;
                    ch.trigger(step.note, step.velocity, off_at);
                }
            }
            self.state.playhead.store(index, Ordering::Relaxed);
            self.next_step += 1;
        }
    }

    fn render_block(&mut self, n: usize) {
        let (mix_l, mix_r) = (&mut self.mix_l[..n], &mut self.mix_r[..n]);
        mix_l.fill(0.0);
        mix_r.fill(0.0);
        for (idx, ch) in self.channels.iter_mut().enumerate() {
            let (l, r) = (&mut self.scratch_l[..n], &mut self.scratch_r[..n]);
            l.fill(0.0);
            r.fill(0.0);
            ch.instrument.render(l, r);
            if ch.muted {
                continue;
            }
            let (gl, gr) = balance_gains(ch.pan);
            let (gl, gr) = (gl * ch.gain, gr * ch.gain);
            let (mut pl, mut pr) = (0.0f32, 0.0f32);
            for i in 0..n {
                let (sl, sr) = (l[i] * gl, r[i] * gr);
                mix_l[i] += sl;
                mix_r[i] += sr;
                pl = pl.max(sl.abs());
                pr = pr.max(sr.abs());
            }
            self.state.channel_peaks[idx].record(pl, pr);
        }
        let g = self.master_gain;
        for s in mix_l.iter_mut().chain(mix_r.iter_mut()) {
            // Garde-fou pour les oreilles et les enceintes.
            *s = if s.is_finite() { (*s * g).clamp(-1.0, 1.0) } else { 0.0 };
        }
        let (mut pl, mut pr) = (0.0f32, 0.0f32);
        for (&l, &r) in mix_l.iter().zip(mix_r.iter()) {
            pl = pl.max(l.abs());
            pr = pr.max(r.abs());
            self.scope_phase += 1;
            if self.scope_phase >= SCOPE_DECIMATION {
                self.scope_phase = 0;
                // File pleine (interface fermée ou lente) : on jette, sans bloquer.
                let _ = self.scope.push(0.5 * (l + r));
            }
        }
        self.state.master_peak.record(pl, pr);
    }
}

fn write_interleaved(out: &mut [f32], channels: usize, left: &[f32], right: &[f32]) {
    for (frame, (&l, &r)) in out.chunks_exact_mut(channels).zip(left.iter().zip(right)) {
        match frame {
            [mono] => *mono = 0.5 * (l + r),
            [fl, fr, rest @ ..] => {
                *fl = l;
                *fr = r;
                rest.fill(0.0);
            }
            [] => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Instrument;
    use std::sync::Mutex;

    type LogEntry = (&'static str, u8, u64);

    /// Instrument espion : journalise (événement, note) au `clock` courant.
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<LogEntry>>>,
        rendered: Arc<AtomicUsize>,
    }

    impl Instrument for Probe {
        fn set_sample_rate(&mut self, _: f32) {}
        fn note_on(&mut self, note: u8, _: f32) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.log.lock().unwrap().push(("on", note, t));
        }
        fn note_off(&mut self, note: u8) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.log.lock().unwrap().push(("off", note, t));
        }
        fn all_notes_off(&mut self) {}
        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            self.rendered.fetch_add(left.len(), Ordering::Relaxed);
            left.fill(0.25);
            right.fill(0.25);
        }
    }

    const SR: f32 = 48_000.0;

    fn setup(hits: &[usize]) -> (Engine, EngineHandle, Probe) {
        let probe = Probe::default();
        let ch = Channel::new("probe", Box::new(probe.clone())).with_hits(hits, 1.0);
        let (engine, handle) = Engine::new(SR, vec![ch]);
        (engine, handle, probe)
    }

    fn run(engine: &mut Engine, frames: usize, block: usize) {
        let mut buf = vec![0.0; block * 2];
        let mut left = frames;
        while left > 0 {
            let n = left.min(block);
            engine.process(&mut buf[..n * 2], 2);
            left -= n;
        }
    }

    #[test]
    fn steps_fire_sample_accurately_regardless_of_block_size() {
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, probe) = setup(&[0, 4, 8, 12]);
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // 120 BPM, 48 kHz -> 6000 échantillons par pas.
            run(&mut engine, 6_000 * 17, block);
            let ons: Vec<u64> =
                probe.log.lock().unwrap().iter().filter(|e| e.0 == "on").map(|e| e.2).collect();
            assert_eq!(ons, [0, 24_000, 48_000, 72_000, 96_000], "bloc {block}");
        }
    }

    #[test]
    fn notes_are_released_after_their_length() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 7_000, 512);
        let log = probe.log.lock().unwrap().clone();
        assert_eq!(log, [("on", 60, 0), ("off", 60, 6_000)]);
    }

    #[test]
    fn stopped_engine_triggers_nothing_and_reports_playhead() {
        let (mut engine, mut handle, probe) = setup(&[0, 1, 2]);
        run(&mut engine, 48_000, 512);
        assert!(probe.log.lock().unwrap().is_empty());
        assert_eq!(handle.playhead(), None);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 6_001, 512);
        assert!(handle.is_playing());
        assert_eq!(handle.playhead(), Some(1));
    }

    #[test]
    fn mix_applies_gain_pan_mute_and_clamp() {
        let (mut engine, mut handle, _) = setup(&[]);
        let mut buf = vec![0.0; 8];
        handle.send(Command::SetMasterGain(1.0)).unwrap();
        handle.send(Command::SetChannelPan { channel: 0, pan: 1.0 }).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(&buf[..2], &[0.0, 0.25]);

        handle.send(Command::SetChannelGain { channel: 0, gain: 100.0 }).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(&buf[..2], &[0.0, 1.0]);

        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        engine.process(&mut buf, 2);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn publishes_meters_and_scope() {
        let (mut engine, mut handle, _) = setup(&[]);
        let mut buf = vec![0.0; 512];
        handle.send(Command::SetMasterGain(1.0)).unwrap();
        handle.send(Command::SetChannelGain { channel: 0, gain: 0.5 }).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_channel_peak(0), [0.125, 0.125]);
        assert_eq!(handle.take_master_peak(), [0.125, 0.125]);
        assert_eq!(handle.take_channel_peak(0), [0.0, 0.0], "la crête se remet à zéro à la lecture");
        assert!(handle.take_load() > 0.0);
        let mut n = 0;
        handle.drain_scope(|s| {
            assert_eq!(s, 0.125);
            n += 1;
        });
        assert_eq!(n, 128);
    }

    #[test]
    fn tempo_change_keeps_position() {
        let (mut engine, mut handle, probe) = setup(&[0, 1, 2]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 9_000, 100); // pas 0 à 0, pas 1 à 6000, on est à 1.5 pas
        handle.send(Command::SetBpm(240.0)).unwrap(); // 3000 échantillons / pas
        run(&mut engine, 3_000, 100);
        let ons: Vec<u64> =
            probe.log.lock().unwrap().iter().filter(|e| e.0 == "on").map(|e| e.2).collect();
        // Le pas 2 tombe un demi-pas (1500 échantillons) après le changement.
        assert_eq!(ons, [0, 6_000, 10_500]);
    }
}
