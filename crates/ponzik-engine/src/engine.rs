use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use ponzik_dsp::util::balance_gains;

use crate::effect::Effect;
use crate::instrument::Instrument;
use crate::meter::{PeakMeter, peak};
use crate::mixer::{INSERT_SLOTS, MASTER, Mixer, SEND_SLOTS, TRACKS};
use crate::rack::Channel;
use crate::ring::{self, Consumer, Producer};
use crate::sequence::{PPQ, SeqEvent, Sequence};

/// Taille max d'un bloc de traitement.
const MAX_BLOCK: usize = 256;
const COMMAND_QUEUE: usize = 4_096;
const GARBAGE_QUEUE: usize = 1_024;
const MAX_EVENTS: usize = 8_192;
pub const MAX_CHANNELS: usize = 128;
/// Échantillons du master gardés pour l'oscilloscope (≈ 0,7 s à 24 kHz).
const SCOPE_QUEUE: usize = 16_384;
const SCOPE_DECIMATION: u32 = 2;

/// Ordres de l'interface au moteur. Les objets lourds (instruments, effets,
/// séquences) sont préparés par l'interface ; ceux qu'ils remplacent repartent
/// par la file [`Garbage`] pour être libérés hors du thread audio.
pub enum Command {
    Play,
    /// Premier appui : pause ; second appui : retour au début.
    Stop,
    /// Saute à une position, en ticks.
    Locate(f64),
    SetBpm(f32),
    SetSequence(Box<Sequence>),
    AddChannel(Box<Channel>),
    RemoveChannel(usize),
    ReplaceInstrument { channel: usize, instrument: Box<dyn Instrument> },
    SetChannelGain { channel: usize, gain: f32 },
    SetChannelPan { channel: usize, pan: f32 },
    SetChannelMute { channel: usize, muted: bool },
    SetChannelTrack { channel: usize, track: usize },
    SetInstrumentParam { channel: usize, id: u32, value: f32 },
    NoteOn { channel: usize, note: u8, velocity: f32 },
    NoteOff { channel: usize, note: u8 },
    SetTrackGain { track: usize, gain: f32 },
    SetTrackPan { track: usize, pan: f32 },
    SetTrackMute { track: usize, muted: bool },
    SetTrackOutput { track: usize, output: usize },
    SetTrackSend { track: usize, slot: usize, target: usize, amount: f32 },
    SetRoutingOrder(Box<[usize]>),
    SetInsert { track: usize, slot: usize, effect: Option<Box<dyn Effect>> },
    SetInsertBypass { track: usize, slot: usize, bypass: bool },
    SetEffectParam { track: usize, slot: usize, id: u32, value: f32 },
    SetMetronome(bool),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Command::Play => "Play",
            Command::Stop => "Stop",
            Command::Locate(_) => "Locate",
            Command::SetBpm(_) => "SetBpm",
            Command::SetSequence(_) => "SetSequence",
            Command::AddChannel(_) => "AddChannel",
            Command::RemoveChannel(_) => "RemoveChannel",
            Command::ReplaceInstrument { .. } => "ReplaceInstrument",
            Command::SetChannelGain { .. } => "SetChannelGain",
            Command::SetChannelPan { .. } => "SetChannelPan",
            Command::SetChannelMute { .. } => "SetChannelMute",
            Command::SetChannelTrack { .. } => "SetChannelTrack",
            Command::SetInstrumentParam { .. } => "SetInstrumentParam",
            Command::NoteOn { .. } => "NoteOn",
            Command::NoteOff { .. } => "NoteOff",
            Command::SetTrackGain { .. } => "SetTrackGain",
            Command::SetTrackPan { .. } => "SetTrackPan",
            Command::SetTrackMute { .. } => "SetTrackMute",
            Command::SetTrackOutput { .. } => "SetTrackOutput",
            Command::SetTrackSend { .. } => "SetTrackSend",
            Command::SetRoutingOrder(_) => "SetRoutingOrder",
            Command::SetInsert { .. } => "SetInsert",
            Command::SetInsertBypass { .. } => "SetInsertBypass",
            Command::SetEffectParam { .. } => "SetEffectParam",
            Command::SetMetronome(_) => "SetMetronome",
        };
        f.write_str(name)
    }
}

/// Objets remplacés, rendus à l'interface pour être libérés.
pub enum Garbage {
    Sequence(Box<Sequence>),
    Channel(Box<Channel>),
    Instrument(Box<dyn Instrument>),
    Effect(Box<dyn Effect>),
    Routing(Box<[usize]>),
}

/// Ce que le moteur publie pour l'interface.
struct SharedState {
    playing: AtomicBool,
    /// Position en ticks (bits d'un `f64`).
    position: AtomicU64,
    channel_peaks: Vec<PeakMeter>,
    track_peaks: Vec<PeakMeter>,
    /// Pire charge DSP depuis la dernière lecture : temps de calcul / durée du bloc.
    load: AtomicU32,
}

/// Côté interface : envoie des commandes, lit l'état publié.
pub struct EngineHandle {
    commands: Producer<Command>,
    state: Arc<SharedState>,
    scope: Consumer<f32>,
    garbage: Consumer<Garbage>,
    sample_rate: f32,
}

impl EngineHandle {
    /// Rend la commande si la file est pleine.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.collect_garbage();
        self.commands.push(command)
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn is_playing(&self) -> bool {
        self.state.playing.load(Ordering::Relaxed)
    }

    /// Position de lecture en ticks.
    pub fn position(&self) -> f64 {
        f64::from_bits(self.state.position.load(Ordering::Relaxed))
    }

    /// Crête [gauche, droite] du canal depuis le dernier appel (après gain et pan).
    pub fn take_channel_peak(&self, channel: usize) -> [f32; 2] {
        self.state.channel_peaks.get(channel).map_or([0.0; 2], PeakMeter::take)
    }

    /// Crête [gauche, droite] d'une piste de mixer depuis le dernier appel.
    pub fn take_track_peak(&self, track: usize) -> [f32; 2] {
        self.state.track_peaks.get(track).map_or([0.0; 2], PeakMeter::take)
    }

    pub fn take_master_peak(&self) -> [f32; 2] {
        self.take_track_peak(MASTER)
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

    /// Libère les objets que le moteur a remplacés.
    pub fn collect_garbage(&mut self) {
        while self.garbage.pop().is_some() {}
    }
}

pub struct Engine {
    sample_rate: f32,
    channels: Vec<Channel>,
    mixer: Mixer,
    sequence: Box<Sequence>,
    commands: Consumer<Command>,
    state: Arc<SharedState>,
    scope: Producer<f32>,
    scope_phase: u32,
    garbage: Producer<Garbage>,

    /// Horloge monotone en échantillons, avance même à l'arrêt (queues de release).
    clock: u64,
    playing: bool,
    bpm: f64,
    /// Position de lecture en ticks.
    pos: f64,
    /// Ancre de la position : au `clock` `anchor_clock`, on était au tick `anchor_tick`.
    /// Recalculer depuis l'ancre évite toute dérive d'arrondi.
    anchor_tick: f64,
    anchor_clock: u64,
    metronome: bool,
    click_phase: f32,
    click_env: f32,
    click_hz: f32,

    events: Vec<SeqEvent>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl Engine {
    pub fn new(sample_rate: f32, mut channels: Vec<Channel>, sequence: Sequence) -> (Engine, EngineHandle) {
        for ch in &mut channels {
            ch.instrument.set_sample_rate(sample_rate);
        }
        channels.reserve(MAX_CHANNELS.saturating_sub(channels.len()));
        let (tx, rx) = ring::channel(COMMAND_QUEUE);
        let (scope_tx, scope_rx) = ring::channel(SCOPE_QUEUE);
        let (trash_tx, trash_rx) = ring::channel(GARBAGE_QUEUE);
        let state = Arc::new(SharedState {
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0f64.to_bits()),
            channel_peaks: (0..MAX_CHANNELS).map(|_| PeakMeter::default()).collect(),
            track_peaks: (0..TRACKS).map(|_| PeakMeter::default()).collect(),
            load: AtomicU32::new(0),
        });
        let engine = Engine {
            sample_rate,
            channels,
            mixer: Mixer::new(MAX_BLOCK),
            sequence: Box::new(sequence),
            commands: rx,
            state: state.clone(),
            scope: scope_tx,
            scope_phase: 0,
            garbage: trash_tx,
            clock: 0,
            playing: false,
            bpm: 120.0,
            pos: 0.0,
            anchor_tick: 0.0,
            anchor_clock: 0,
            metronome: false,
            click_phase: 0.0,
            click_env: 0.0,
            click_hz: 0.0,
            events: Vec::with_capacity(MAX_EVENTS),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
        };
        let handle = EngineHandle { commands: tx, state, scope: scope_rx, garbage: trash_rx, sample_rate };
        (engine, handle)
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Accès direct au mixer, pour préparer l'état avant le premier bloc.
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Échantillons par tick au tempo courant.
    fn spt(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / (self.bpm * PPQ as f64)
    }

    fn anchor(&mut self) {
        self.anchor_tick = self.pos;
        self.anchor_clock = self.clock;
    }

    fn trash(&mut self, g: Garbage) {
        // File pleine : l'objet est libéré ici, en dernier recours.
        let _ = self.garbage.push(g);
    }

    /// Remplit `out` (entrelacé, `channels` canaux) — appelé par le thread audio.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let started = Instant::now();
        self.drain_commands();
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            let mut n = (frames - done).min(MAX_BLOCK);
            if self.playing
                && let Some(end) = self.sequence.loop_end()
            {
                // On coupe le bloc à la fin de la boucle du morceau.
                if self.pos >= end - 1e-9 {
                    self.pos = 0.0;
                    self.anchor();
                }
                let left = ((end - self.pos) * self.spt()).ceil().max(1.0) as usize;
                n = n.min(left);
            }
            self.process_chunk(n);
            let (l, r) = self.mixer.master(n);
            write_interleaved(&mut out[done * channels..(done + n) * channels], channels, l, r);
            done += n;
        }
        self.state.position.store(self.pos.to_bits(), Ordering::Relaxed);
        if frames > 0 {
            let budget = frames as f32 / self.sample_rate;
            let load = started.elapsed().as_secs_f32() / budget;
            self.state.load.fetch_max(load.to_bits(), Ordering::Relaxed);
        }
    }

    fn process_chunk(&mut self, n: usize) {
        let spt = self.spt();
        let pos_start = self.pos;
        if self.playing {
            let to = pos_start + n as f64 / spt;
            self.sequence.collect(pos_start, to, &mut self.events);
        } else {
            self.events.clear();
        }
        let offset_of = |tick: f64| ((((tick - pos_start) * spt) - 1e-6).ceil().max(0.0) as usize).min(n - 1);

        self.mixer.clear(n);
        let mut i = 0;
        let mut next_event = 0;
        while i < n {
            let now = self.clock + i as u64;
            for ch in &mut self.channels {
                ch.release_due(now);
            }
            while let Some(ev) = self.events.get(next_event) {
                if offset_of(ev.tick) > i {
                    break;
                }
                let ev = *ev;
                next_event += 1;
                if let Some(ch) = self.channels.get_mut(ev.channel)
                    && !ch.muted
                {
                    let off_at = now + (ev.len.max(1) as f64 * spt).round().max(1.0) as u64;
                    ch.trigger(ev.key, ev.vel, off_at);
                }
            }
            // Jusqu'au prochain événement (début ou fin de note).
            let mut end = n;
            if let Some(ev) = self.events.get(next_event) {
                end = end.min(offset_of(ev.tick).clamp(i + 1, n));
            }
            if let Some(off) = self.channels.iter().filter_map(Channel::next_off).min() {
                end = end.min((off.saturating_sub(self.clock) as usize).clamp(i + 1, n));
            }
            self.render_channels(i, end - i);
            i = end;
        }

        self.metronome_click(pos_start, n, spt);
        self.mixer.process(n, &self.state.track_peaks);
        self.feed_scope(n);
        self.clock += n as u64;
        if self.playing {
            self.pos = self.anchor_tick + (self.clock - self.anchor_clock) as f64 / spt;
        }
    }

    fn render_channels(&mut self, offset: usize, len: usize) {
        for (idx, ch) in self.channels.iter_mut().enumerate() {
            let (l, r) = (&mut self.scratch_l[..len], &mut self.scratch_r[..len]);
            l.fill(0.0);
            r.fill(0.0);
            ch.instrument.render(l, r);
            if ch.muted {
                continue;
            }
            let (gl, gr) = balance_gains(ch.pan);
            let (gl, gr) = (gl * ch.gain, gr * ch.gain);
            for s in l.iter_mut() {
                *s *= gl;
            }
            for s in r.iter_mut() {
                *s *= gr;
            }
            if let Some(m) = self.state.channel_peaks.get(idx) {
                m.record(peak(l), peak(r));
            }
            let (tl, tr) = self.mixer.input(ch.track, offset + len);
            for k in 0..len {
                tl[offset + k] += l[k];
                tr[offset + k] += r[k];
            }
        }
    }

    /// Clic de métronome sur chaque temps, ajouté directement au master.
    fn metronome_click(&mut self, pos_start: f64, n: usize, spt: f64) {
        if !self.playing || !self.metronome {
            self.click_env = 0.0;
            return;
        }
        let sr = self.sample_rate;
        let decay = (-1.0 / (0.012 * sr)).exp();
        let beat_ticks = PPQ as f64;
        let (l, r) = self.mixer.input(MASTER, n);
        for k in 0..n {
            let tick = pos_start + k as f64 / spt;
            let beat = (tick / beat_ticks).floor();
            if tick - beat * beat_ticks < 1.0 / spt {
                let bar_start = (beat as i64).rem_euclid(4) == 0;
                self.click_hz = if bar_start { 1_760.0 } else { 1_320.0 };
                self.click_env = 0.35;
                self.click_phase = 0.0;
            }
            if self.click_env > 1e-4 {
                self.click_phase = (self.click_phase + self.click_hz / sr).fract();
                let s = (self.click_phase * std::f32::consts::TAU).sin() * self.click_env;
                self.click_env *= decay;
                l[k] += s;
                r[k] += s;
            }
        }
    }

    fn feed_scope(&mut self, n: usize) {
        let (l, r) = self.mixer.master(n);
        for k in 0..n {
            self.scope_phase += 1;
            if self.scope_phase >= SCOPE_DECIMATION {
                self.scope_phase = 0;
                // File pleine (interface fermée ou lente) : on jette, sans bloquer.
                let _ = self.scope.push(0.5 * (l[k] + r[k]));
            }
        }
    }

    fn drain_commands(&mut self) {
        while let Some(cmd) = self.commands.pop() {
            self.apply(cmd);
        }
    }

    fn silence_all(&mut self) {
        for ch in &mut self.channels {
            ch.silence();
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                self.playing = true;
                self.anchor();
            }
            Command::Stop => {
                if !self.playing {
                    self.pos = 0.0;
                }
                self.playing = false;
                self.silence_all();
                self.mixer.reset_effects();
            }
            Command::Locate(tick) => {
                self.pos = tick.max(0.0);
                self.anchor();
                self.silence_all();
            }
            Command::SetBpm(bpm) => {
                self.anchor();
                self.bpm = (bpm as f64).clamp(20.0, 999.0);
                self.mixer.set_tempo(self.bpm as f32);
            }
            Command::SetSequence(seq) => {
                let old = std::mem::replace(&mut self.sequence, seq);
                self.trash(Garbage::Sequence(old));
            }
            Command::AddChannel(mut ch) => {
                if self.channels.len() < MAX_CHANNELS {
                    // Le canal sort de sa boîte ; la boîte et son bouche-trou repartent.
                    let channel = std::mem::replace(&mut *ch, Channel::new(String::new(), Box::new(Silence)));
                    self.channels.push(channel);
                }
                self.trash(Garbage::Channel(ch));
            }
            Command::RemoveChannel(idx) => {
                if idx < self.channels.len() {
                    let ch = self.channels.remove(idx);
                    self.trash(Garbage::Channel(Box::new(ch)));
                }
            }
            Command::ReplaceInstrument { channel, instrument } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.silence();
                    let old = std::mem::replace(&mut ch.instrument, instrument);
                    self.trash(Garbage::Instrument(old));
                } else {
                    self.trash(Garbage::Instrument(instrument));
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
                    if muted {
                        ch.silence();
                    }
                }
            }
            Command::SetChannelTrack { channel, track } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.track = track.min(TRACKS - 1);
                }
            }
            Command::SetInstrumentParam { channel, id, value } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.instrument.set_param(id, value);
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
            Command::SetTrackGain { track, gain } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.gain = gain.max(0.0);
                }
            }
            Command::SetTrackPan { track, pan } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetTrackMute { track, muted } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.muted = muted;
                }
            }
            Command::SetTrackOutput { track, output } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.output = output.min(TRACKS - 1);
                }
            }
            Command::SetTrackSend { track, slot, target, amount } => {
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < SEND_SLOTS
                {
                    t.sends[slot] = (target.min(TRACKS - 1), amount.max(0.0));
                }
            }
            Command::SetRoutingOrder(order) => {
                let old = std::mem::replace(&mut self.mixer.order, order);
                self.trash(Garbage::Routing(old));
            }
            Command::SetInsert { track, slot, mut effect } => {
                if let Some(fx) = effect.as_mut() {
                    fx.set_tempo(self.bpm as f32);
                }
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    if let Some(old) = std::mem::replace(&mut t.inserts[slot], effect) {
                        self.trash(Garbage::Effect(old));
                    }
                } else if let Some(fx) = effect {
                    self.trash(Garbage::Effect(fx));
                }
            }
            Command::SetInsertBypass { track, slot, bypass } => {
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    t.bypass[slot] = bypass;
                }
            }
            Command::SetEffectParam { track, slot, id, value } => {
                if let Some(Some(fx)) = self.mixer.tracks.get_mut(track).and_then(|t| t.inserts.get_mut(slot)) {
                    fx.set_param(id, value);
                }
            }
            Command::SetMetronome(on) => self.metronome = on,
        }
        self.state.playing.store(self.playing, Ordering::Relaxed);
    }
}

/// Instrument muet, utilisé comme bouche-trou.
struct Silence;

impl crate::param::Params for Silence {}

impl Instrument for Silence {
    fn kind(&self) -> &'static str {
        "silence"
    }
    fn set_sample_rate(&mut self, _: f32) {}
    fn note_on(&mut self, _: u8, _: f32) {}
    fn note_off(&mut self, _: u8) {}
    fn all_notes_off(&mut self) {}
    fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
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
    use crate::sequence::{Clip, Note, Pattern, PlayMode, TICKS_PER_STEP};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    type LogEntry = (&'static str, u8, u64);

    /// Instrument espion : journalise (événement, note, échantillon).
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<LogEntry>>>,
        rendered: Arc<AtomicUsize>,
    }

    impl crate::param::Params for Probe {}

    impl Instrument for Probe {
        fn kind(&self) -> &'static str {
            "probe"
        }
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

    fn pattern(steps: &[usize]) -> Pattern {
        let mut p = Pattern::new(16 * TICKS_PER_STEP, 1);
        p.notes[0] = steps
            .iter()
            .map(|&s| Note { start: s as u32 * TICKS_PER_STEP, len: TICKS_PER_STEP, key: 60, vel: 1.0 })
            .collect();
        p
    }

    fn setup(steps: &[usize]) -> (Engine, EngineHandle, Probe) {
        let probe = Probe::default();
        let ch = Channel::new("probe", Box::new(probe.clone())).with_track(1);
        let seq = Sequence { patterns: vec![pattern(steps)], ..Default::default() };
        let (engine, handle) = Engine::new(SR, vec![ch], seq);
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

    fn ons(probe: &Probe) -> Vec<u64> {
        probe.log.lock().unwrap().iter().filter(|e| e.0 == "on").map(|e| e.2).collect()
    }

    #[test]
    fn notes_fire_sample_accurately_regardless_of_block_size() {
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, probe) = setup(&[0, 4, 8, 12]);
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // 120 BPM, 48 kHz : 250 échantillons par tick, 6000 par pas.
            run(&mut engine, 6_000 * 17, block);
            assert_eq!(ons(&probe), [0, 24_000, 48_000, 72_000, 96_000], "bloc {block}");
        }
    }

    #[test]
    fn notes_are_released_after_their_length() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 7_000, 512);
        assert_eq!(probe.log.lock().unwrap().clone(), [("on", 60, 0), ("off", 60, 6_000)]);
    }

    #[test]
    fn tempo_change_keeps_position() {
        let (mut engine, mut handle, probe) = setup(&[0, 1, 2]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 9_000, 100);
        handle.send(Command::SetBpm(240.0)).unwrap();
        run(&mut engine, 3_000, 100);
        // À 1,5 pas, le pas 2 tombe un demi-pas plus tard au nouveau tempo (1500 échantillons).
        assert_eq!(ons(&probe), [0, 6_000, 10_500]);
        assert!((handle.position() - (36.0 + 3_000.0 / 125.0)).abs() < 1e-6);
    }

    #[test]
    fn song_mode_loops_at_song_end() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        let seq = Sequence {
            patterns: vec![pattern(&[0])],
            clips: vec![Clip { pattern: 0, start: 0, len: 96, offset: 0 }],
            mode: PlayMode::Song,
            song_len: 384,
            swing: 0.0,
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        // Une mesure = 384 ticks = 96 000 échantillons ; on en joue deux.
        run(&mut engine, 192_000, 480);
        assert_eq!(ons(&probe), [0, 96_000]);
    }

    #[test]
    fn mixer_routing_meters_and_garbage() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelGain { channel: 0, gain: 0.5 }).unwrap();
        handle.send(Command::SetTrackGain { track: 1, gain: 0.5 }).unwrap();
        let mut buf = vec![0.0; 512];
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_channel_peak(0), [0.125, 0.125]);
        assert_eq!(handle.take_track_peak(1), [0.0625, 0.0625]);
        assert_eq!(handle.take_master_peak(), [0.0625, 0.0625]);
        assert_eq!(buf[0], 0.0625);
        assert!(handle.take_load() > 0.0);
        let mut n = 0;
        handle.drain_scope(|_| n += 1);
        assert_eq!(n, 128);

        handle.send(Command::SetSequence(Box::default())).unwrap();
        engine.process(&mut buf, 2);
        assert!(handle.garbage.pop().is_some(), "l'ancienne séquence revient à l'interface");
    }

    #[test]
    fn channels_can_be_added_and_removed_live() {
        let (mut engine, mut handle, _) = setup(&[]);
        let extra = Probe::default();
        handle.send(Command::AddChannel(Box::new(Channel::new("b", Box::new(extra)).with_track(2)))).unwrap();
        let mut buf = vec![0.0; 64];
        engine.process(&mut buf, 2);
        assert_eq!(engine.channels.len(), 2);
        assert_eq!(handle.take_track_peak(2), [0.25, 0.25]);
        handle.send(Command::RemoveChannel(0)).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(engine.channels.len(), 1);
        assert_eq!(engine.channels[0].name, "b");
    }

    #[test]
    fn stop_twice_returns_to_start() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 4_800, 480);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 480, 480);
        assert!(handle.position() > 0.0);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 480, 480);
        assert_eq!(handle.position(), 0.0);
    }

    #[test]
    fn metronome_clicks_on_beats() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetMetronome(true)).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 2_000];
        engine.process(&mut buf, 2);
        assert!(buf.iter().any(|s| s.abs() > 0.05), "clic au premier temps");
    }
}
