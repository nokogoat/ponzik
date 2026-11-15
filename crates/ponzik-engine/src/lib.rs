//! Moteur audio temps réel de Ponzik.
//!
//! Règle d'or : rien dans [`Engine::process`] n'alloue, ne verrouille ni ne bloque.
//! L'interface pilote le moteur uniquement via des [`Command`] envoyées
//! dans une file sans verrou ([`ring`]).

pub mod engine;
pub mod instrument;
pub mod instruments;
pub mod rack;
pub mod ring;
pub mod wav;

pub use engine::{Command, Engine, EngineHandle};
pub use instrument::Instrument;
pub use rack::{Channel, Step, MAX_STEPS};
