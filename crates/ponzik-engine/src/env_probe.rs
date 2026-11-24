//! Lock-free view of an instrument's envelope for the interface.

use std::sync::atomic::{AtomicU32, Ordering};

pub use ponzik_dsp::env::EnvStage;

/// Where an instrument's amp envelope is, published by the audio thread once
/// per rendered block (the most recently triggered voice still sounding,
/// `Idle` when none) and read by the interface to animate its envelope
/// editor. Atomics only: publishing never allocates, locks or blocks.
#[derive(Debug, Default)]
pub struct EnvProbe {
    /// [`EnvStage::index`]: 0 Idle, 1 Delay, 2 Attack, 3 Hold, 4 Decay, 5 Sustain, 6 Release.
    stage: AtomicU32,
    /// Progress through the stage in [0, 1], as `f32` bits.
    progress: AtomicU32,
    /// Envelope level in [0, 1], as `f32` bits.
    level: AtomicU32,
    /// Note-ons received, wrapping.
    notes: AtomicU32,
}

impl EnvProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Audio thread: the envelope state at the end of a block.
    #[inline]
    pub fn publish(&self, stage: EnvStage, progress: f32, level: f32) {
        self.progress.store(progress.to_bits(), Ordering::Relaxed);
        self.level.store(level.to_bits(), Ordering::Relaxed);
        self.stage.store(stage.index(), Ordering::Relaxed);
    }

    /// Audio thread: a note started.
    #[inline]
    pub fn note_on(&self) {
        self.notes.fetch_add(1, Ordering::Relaxed);
    }

    /// The last published stage, progress and level.
    pub fn read(&self) -> (EnvStage, f32, f32) {
        let stage = EnvStage::from_index(self.stage.load(Ordering::Relaxed));
        let progress = f32::from_bits(self.progress.load(Ordering::Relaxed));
        let level = f32::from_bits(self.level.load(Ordering::Relaxed));
        (stage, progress, level)
    }

    /// Note-ons counted so far (wrapping): a change means a new note, even
    /// on a block where the stage looks the same.
    pub fn notes(&self) -> u32 {
        self.notes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_and_reads_back() {
        let p = EnvProbe::new();
        assert_eq!(p.read(), (EnvStage::Idle, 0.0, 0.0));
        p.publish(EnvStage::Decay, 0.25, 0.8);
        assert_eq!(p.read(), (EnvStage::Decay, 0.25, 0.8));
        p.note_on();
        p.note_on();
        assert_eq!(p.notes(), 2);
    }
}
