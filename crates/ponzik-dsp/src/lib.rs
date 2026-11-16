//! Briques DSP de Ponzik : oscillateurs, enveloppes, filtres, retards,
//! dynamique et suréchantillonnage, plus l'analyse (FFT, spectre, sonie,
//! accordeur).
//!
//! Aucune allocation après construction, aucune dépendance : tout ici est utilisable
//! directement dans le thread audio.

#![forbid(unsafe_code)]

pub mod biquad;
pub mod delay;
pub mod dynamics;
pub mod env;
pub mod fft;
pub mod filter;
pub mod lfo;
pub mod loudness;
pub mod noise;
pub mod osc;
pub mod oversample;
pub mod smooth;
pub mod spectrum;
pub mod tuner;
pub mod util;
