//! Briques DSP de Ponzik : oscillateurs, enveloppes, filtres.
//!
//! Aucune allocation, aucune dépendance : tout ici est utilisable
//! directement dans le thread audio.

#![forbid(unsafe_code)]

pub mod env;
pub mod filter;
pub mod noise;
pub mod osc;
pub mod util;
