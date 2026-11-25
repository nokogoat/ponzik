//! What the vocoder shows the interface, written on the audio thread without
//! locks: the level of each band of the modulator and of the output, and how
//! unvoiced (a consonant) the modulator sounds.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::MAX_BANDS;

pub struct VocoderShared {
    modulator: [AtomicU32; MAX_BANDS],
    output: [AtomicU32; MAX_BANDS],
    bands: AtomicU32,
    unvoiced: AtomicU32,
    keyed: AtomicBool,
}

impl Default for VocoderShared {
    fn default() -> Self {
        Self {
            modulator: std::array::from_fn(|_| AtomicU32::new(0)),
            output: std::array::from_fn(|_| AtomicU32::new(0)),
            bands: AtomicU32::new(0),
            unvoiced: AtomicU32::new(0),
            keyed: AtomicBool::new(false),
        }
    }
}

impl VocoderShared {
    pub(super) fn publish(&self, band: usize, modulator: f32, output: f32) {
        if band < MAX_BANDS {
            self.modulator[band].store(modulator.to_bits(), Ordering::Relaxed);
            self.output[band].store(output.to_bits(), Ordering::Relaxed);
        }
    }

    pub(super) fn publish_state(&self, bands: usize, unvoiced: f32, keyed: bool) {
        self.bands.store(bands as u32, Ordering::Relaxed);
        self.unvoiced.store(unvoiced.to_bits(), Ordering::Relaxed);
        self.keyed.store(keyed, Ordering::Relaxed);
    }

    /// Bands processed at the last block.
    pub fn bands(&self) -> usize {
        self.bands.load(Ordering::Relaxed) as usize
    }

    /// Level (linear, peak) of the modulator in band `i`.
    pub fn modulator(&self, i: usize) -> f32 {
        self.modulator.get(i).map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
    }

    /// Level (linear, peak) of the output in band `i`.
    pub fn output(&self, i: usize) -> f32 {
        self.output.get(i).map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
    }

    /// 0 (voiced) to 1 (a consonant, noise passes).
    pub fn unvoiced(&self) -> f32 {
        f32::from_bits(self.unvoiced.load(Ordering::Relaxed))
    }

    /// The modulator comes from a key track (otherwise the track itself).
    pub fn keyed(&self) -> bool {
        self.keyed.load(Ordering::Relaxed)
    }
}
