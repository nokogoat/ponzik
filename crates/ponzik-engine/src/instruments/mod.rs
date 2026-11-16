//! Instruments natifs de Ponzik.

mod bass303;
pub(crate) mod common;
mod drum_machine;
pub mod drum_pads;
mod drums;
mod epiano;
mod external_midi;
mod fm;
mod granular;
mod kick;
mod pluck;
mod pulse;
mod sampler;
mod slicer;
mod wavetable;

pub use bass303::Bass303;
pub use drum_machine::DrumMachine;
pub use drum_pads::DrumPads;
pub use drums::{DrumKind, DrumSynth};
pub use epiano::EPiano;
pub use external_midi::ExternalMidi;
pub use fm::FmSynth;
pub use granular::Granular;
pub use kick::Kick;
pub use pluck::Pluck;
pub use pulse::{Pulse, PulseParams};
pub use sampler::Sampler;
pub use slicer::Slicer;
pub use wavetable::WaveSynth;
