//! Channel Rack : chaque canal = un instrument, son volume, son pan et sa piste
//! de mixer, précédé d'une chaîne d'effets de notes. Les notes vivent dans les
//! patterns ([`crate::sequence`]).

use crate::Instrument;
use crate::note_fx::{NoteBuffer, NoteEvent, NoteFx};

const MAX_PENDING_OFFS: usize = 64;
/// Emplacements d'effets de notes par canal.
pub const NOTE_FX_SLOTS: usize = 4;
/// Notes entrantes gardées pour un même instant avant de passer la chaîne :
/// avec un accord de cinq notes, la sortie tient dans un [`NoteBuffer`].
const MAX_INPUTS: usize = 24;

pub struct Channel {
    pub name: String,
    pub instrument: Box<dyn Instrument>,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// Piste de mixer de destination.
    pub track: usize,
    /// Effets de notes, dans l'ordre de la chaîne.
    pub note_fx: [Option<Box<dyn NoteFx>>; NOTE_FX_SLOTS],
    /// (note, échantillon absolu de fin).
    pending_offs: Vec<(u8, u64)>,
    /// Échantillon où la chaîne a été avancée pour la dernière fois.
    fx_clock: u64,
    /// Notes entrantes de l'instant `fx_clock`, pas encore passées dans la chaîne.
    inputs: NoteBuffer,
    /// Tampons de travail entre deux maillons.
    stages: [NoteBuffer; 2],
    /// Temps de rendu de l'instrument accumulé (ns), relevé par le moteur.
    pub(crate) cpu_ns: u64,
}

impl Channel {
    pub fn new(name: impl Into<String>, instrument: Box<dyn Instrument>) -> Self {
        Self {
            name: name.into(),
            instrument,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            track: 0,
            note_fx: Default::default(),
            pending_offs: Vec::with_capacity(MAX_PENDING_OFFS),
            fx_clock: 0,
            inputs: NoteBuffer::default(),
            stages: Default::default(),
            cpu_ns: 0,
        }
    }

    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = gain;
        self
    }

    pub fn with_pan(mut self, pan: f32) -> Self {
        self.pan = pan;
        self
    }

    pub fn with_track(mut self, track: usize) -> Self {
        self.track = track;
        self
    }

    /// Place un effet de notes (déjà réglé à la bonne fréquence d'échantillonnage).
    pub fn with_note_fx(mut self, slot: usize, fx: Box<dyn NoteFx>) -> Self {
        if let Some(s) = self.note_fx.get_mut(slot) {
            *s = Some(fx);
        }
        self
    }

    fn has_note_fx(&self) -> bool {
        self.note_fx.iter().any(Option::is_some)
    }

    /// Une note entre dans la chaîne à l'instant `now` ; sans effet de notes,
    /// elle va directement à l'instrument.
    pub(crate) fn note_in(&mut self, now: u64, ev: NoteEvent) {
        if !self.has_note_fx() {
            if ev.on {
                self.instrument.note_on(ev.key, ev.vel);
            } else {
                self.instrument.note_off(ev.key);
            }
            return;
        }
        if self.inputs.len() >= MAX_INPUTS {
            self.flush(now);
        }
        self.inputs.push(ev);
    }

    /// Fait passer la chaîne à l'instant `now` : chaque effet avance jusqu'à
    /// `now`, reçoit les notes du maillon précédent, puis émet ; la sortie du
    /// dernier joue l'instrument. Appelé à chaque début de sous-bloc.
    pub(crate) fn flush(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.fx_clock).min(u32::MAX as u64) as u32;
        self.fx_clock = now;
        let Self { instrument, note_fx, inputs, stages, .. } = self;
        let [a, b] = stages;
        let (mut src, mut dst) = (a, b);
        src.clear();
        for &ev in inputs.as_slice() {
            src.push(ev);
        }
        inputs.clear();
        for fx in note_fx.iter_mut().flatten() {
            dst.clear();
            fx.advance(elapsed);
            for ev in src.as_slice() {
                if ev.on {
                    fx.note_on(ev.key, ev.vel, dst);
                } else {
                    fx.note_off(ev.key, dst);
                }
            }
            fx.tick(dst);
            std::mem::swap(&mut src, &mut dst);
        }
        for ev in src.as_slice() {
            if ev.on {
                instrument.note_on(ev.key, ev.vel);
            } else {
                instrument.note_off(ev.key);
            }
        }
        src.clear();
    }

    /// Échantillon absolu de la prochaine émission spontanée de la chaîne.
    pub(crate) fn next_fx_event(&self) -> Option<u64> {
        let wait = self.note_fx.iter().flatten().filter_map(|fx| fx.next_event()).min()?;
        Some(self.fx_clock + wait.max(1) as u64)
    }

    pub(crate) fn set_tempo(&mut self, bpm: f32) {
        for fx in self.note_fx.iter_mut().flatten() {
            fx.set_tempo(bpm);
        }
    }

    pub(crate) fn trigger(&mut self, now: u64, note: u8, velocity: f32, off_at: u64) {
        // Une note relancée remplace sa fin programmée.
        if let Some(i) = self.pending_offs.iter().position(|&(n, _)| n == note) {
            self.pending_offs.swap_remove(i);
            self.note_in(now, NoteEvent::off(note));
        }
        if self.pending_offs.len() == MAX_PENDING_OFFS {
            let (old, _) = self.pending_offs.swap_remove(0);
            self.note_in(now, NoteEvent::off(old));
        }
        self.note_in(now, NoteEvent::on(note, velocity));
        self.pending_offs.push((note, off_at));
    }

    pub(crate) fn release_due(&mut self, now: u64) {
        let mut i = 0;
        while i < self.pending_offs.len() {
            let (note, at) = self.pending_offs[i];
            if at <= now {
                self.pending_offs.remove(i);
                self.note_in(now, NoteEvent::off(note));
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn next_off(&self) -> Option<u64> {
        self.pending_offs.iter().map(|&(_, at)| at).min()
    }

    pub(crate) fn silence(&mut self) {
        self.pending_offs.clear();
        self.inputs.clear();
        for fx in self.note_fx.iter_mut().flatten() {
            fx.reset();
        }
        self.instrument.all_notes_off();
    }
}
