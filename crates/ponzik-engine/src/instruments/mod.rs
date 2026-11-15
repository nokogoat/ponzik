//! Instruments natifs de Ponzik.

mod bass303;
mod common;
mod drums;
mod epiano;
mod fm;
mod granular;
mod kick;
mod pluck;
mod pulse;
mod sampler;
mod slicer;
mod wavetable;

pub use bass303::Bass303;
pub use drums::{DrumKind, DrumSynth};
pub use epiano::EPiano;
pub use fm::FmSynth;
pub use granular::Granular;
pub use kick::Kick;
pub use pluck::Pluck;
pub use pulse::{Pulse, PulseParams};
pub use sampler::Sampler;
pub use slicer::Slicer;
pub use wavetable::WaveSynth;
