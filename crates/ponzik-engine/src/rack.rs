//! Channel Rack : chaque canal = un instrument, son volume, son pan et sa piste
//! de mixer. Les notes vivent dans les patterns ([`crate::sequence`]).

use crate::Instrument;

const MAX_PENDING_OFFS: usize = 64;

pub struct Channel {
    pub name: String,
    pub instrument: Box<dyn Instrument>,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// Piste de mixer de destination.
    pub track: usize,
    /// (note, échantillon absolu de fin).
    pending_offs: Vec<(u8, u64)>,
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
            pending_offs: Vec::with_capacity(MAX_PENDING_OFFS),
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
