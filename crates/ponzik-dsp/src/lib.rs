//! Briques DSP de Ponzik : oscillateurs, enveloppes, filtres, retards,
//! dynamique et suréchantillonnage, plus l'analyse (FFT, spectre, sonie,
//! accordeur).
//!
//! Aucune allocation après construction, aucune dépendance : tout ici est utilisable
//! directement dans le thread audio, sauf les outils d'édition hors ligne
//! ([`onset`], [`tempo`], [`stretch`], [`warp`]) qui allouent leurs résultats.

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
pub mod onset;
pub mod osc;
pub mod oversample;
pub mod smooth;
pub mod spectrum;
pub mod stretch;
pub mod tempo;
pub mod tuner;
pub mod util;
pub mod warp;
