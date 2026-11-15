use std::sync::atomic::{AtomicU32, Ordering};

/// Crête stéréo publiée par le thread audio. Les bits d'un `f32` positif sont
/// ordonnés comme des entiers : `fetch_max` suffit, sans verrou.
#[derive(Default)]
pub(crate) struct PeakMeter([AtomicU32; 2]);

impl PeakMeter {
    pub(crate) fn record(&self, l: f32, r: f32) {
        self.0[0].fetch_max(l.abs().to_bits(), Ordering::Relaxed);
        self.0[1].fetch_max(r.abs().to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn take(&self) -> [f32; 2] {
        [0, 1].map(|i| f32::from_bits(self.0[i].swap(0, Ordering::Relaxed)))
    }
}

pub(crate) fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}
