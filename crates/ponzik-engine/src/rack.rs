//! Channel Rack : chaque canal = un instrument + une rangée de pas (à la FL Studio).

use crate::Instrument;

pub const MAX_STEPS: usize = 64;
const MAX_PENDING_OFFS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub note: u8,
    /// Dans [0, 1].
    pub velocity: f32,
    /// Durée en pas (1.0 = une double-croche).
    pub length: f32,
}

pub struct Channel {
    pub name: String,
    pub instrument: Box<dyn Instrument>,
    pub steps: Vec<Option<Step>>,
    /// Note utilisée quand on active un pas d'un simple clic.
    pub root_note: u8,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// (note, échantillon absolu de fin).
    pending_offs: Vec<(u8, u64)>,
}

impl Channel {
    pub fn new(name: impl Into<String>, instrument: Box<dyn Instrument>) -> Self {
        Self {
            name: name.into(),
            instrument,
            steps: vec![None; MAX_STEPS],
            root_note: 60,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            pending_offs: Vec::with_capacity(MAX_PENDING_OFFS),
        }
    }

    pub fn with_root_note(mut self, note: u8) -> Self {
        self.root_note = note;
        self
    }

    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = gain;
        self
    }

    pub fn with_pan(mut self, pan: f32) -> Self {
        self.pan = pan;
        self
    }

    /// Active les pas donnés avec la note racine.
    pub fn with_hits(mut self, steps: &[usize], velocity: f32) -> Self {
        for &i in steps {
            self.steps[i] = Some(Step { note: self.root_note, velocity, length: 1.0 });
        }
        self
    }

    /// Place des notes : `(pas, note, durée en pas)`.
    pub fn with_notes(mut self, notes: &[(usize, u8, f32)], velocity: f32) -> Self {
        for &(i, note, length) in notes {
            self.steps[i] = Some(Step { note, velocity, length });
        }
        self
    }

    pub fn toggle_step(&mut self, index: usize) {
        if let Some(slot) = self.steps.get_mut(index) {
            *slot = match slot {
                Some(_) => None,
                None => Some(Step { note: self.root_note, velocity: 0.8, length: 1.0 }),
            };
        }
    }

    pub(crate) fn trigger(&mut self, note: u8, velocity: f32, off_at: u64) {
        // Une note relancée remplace sa fin programmée.
        if let Some(i) = self.pending_offs.iter().position(|&(n, _)| n == note) {
            self.pending_offs.swap_remove(i);
            self.instrument.note_off(note);
        }
        if self.pending_offs.len() == MAX_PENDING_OFFS {
            let (old, _) = self.pending_offs.swap_remove(0);
            self.instrument.note_off(old);
        }
        self.instrument.note_on(note, velocity);
        self.pending_offs.push((note, off_at));
    }

    pub(crate) fn release_due(&mut self, now: u64) {
        let Self { instrument, pending_offs, .. } = self;
        pending_offs.retain(|&(note, at)| {
            let due = at <= now;
            if due {
                instrument.note_off(note);
            }
            !due
        });
    }

    pub(crate) fn next_off(&self) -> Option<u64> {
        self.pending_offs.iter().map(|&(_, at)| at).min()
    }

    pub(crate) fn silence(&mut self) {
        self.pending_offs.clear();
        self.instrument.all_notes_off();
    }
}
