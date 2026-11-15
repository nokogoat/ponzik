//! Moteur audio temps réel de Ponzik.
//!
//! Règle d'or : rien dans [`Engine::process`] n'alloue, ne verrouille ni ne bloque.
//! L'interface pilote le moteur uniquement via des [`Command`] envoyées dans une
//! file sans verrou ([`ring`]) ; les objets remplacés lui reviennent par la file
//! [`Garbage`] pour être libérés hors du thread audio.

pub mod effect;
pub mod effects;
pub mod engine;
pub mod instrument;
pub mod instruments;
mod meter;
pub mod mixer;
pub mod param;
pub mod rack;
pub mod registry;
pub mod ring;
pub mod sample;
pub mod sequence;
pub mod wav;

pub use effect::Effect;
pub use engine::{Command, Engine, EngineHandle, Garbage, MAX_CHANNELS};
pub use instrument::Instrument;
pub use param::{ParamInfo, Params, Unit};
pub use rack::Channel;
pub use sample::Sample;
pub use sequence::{Clip, Note, PPQ, Pattern, PlayMode, Sequence, TICKS_PER_BAR, TICKS_PER_STEP};
