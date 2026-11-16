//! Briques DSP de Ponzik : oscillateurs, enveloppes, filtres, retards,
//! dynamique et suréchantillonnage.
//!
//! Aucune allocation, aucune dépendance : tout ici est utilisable
//! directement dans le thread audio.

#![forbid(unsafe_code)]

pub mod biquad;
pub mod delay;
pub mod dynamics;
pub mod env;
pub mod fft;
pub mod filter;
pub mod lfo;
pub mod noise;
pub mod osc;
pub mod oversample;
pub mod smooth;
pub mod spectrum;
pub mod util;
