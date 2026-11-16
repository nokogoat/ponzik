//! Moteur audio temps réel de Ponzik.
//!
//! Règle d'or : rien dans [`Engine::process`] n'alloue, ne verrouille ni ne bloque.
//! L'interface pilote le moteur uniquement via des [`Command`] envoyées dans une
//! file sans verrou ([`ring`]) ; les objets remplacés lui reviennent par la file
//! [`Garbage`] pour être libérés hors du thread audio.

pub mod audition;
pub mod effect;
pub mod effects;
pub mod input;
pub mod engine;
pub mod instrument;
pub mod instruments;
mod meter;
pub mod mixer;
pub mod note_fx;
pub mod param;
pub mod rack;
pub mod registry;
pub mod ring;
pub mod sample;
pub mod sequence;
pub mod wav;

pub use effect::Effect;
pub use engine::{Command, Engine, EngineHandle, Garbage, MAX_CHANNELS, MAX_RECORDINGS};
pub use input::{InputConsumer, InputProducer, InputSource, InputStrip, RecordBuffer, TakeSlice, input_channel};
pub use instrument::Instrument;
pub use note_fx::{NoteBuffer, NoteEvent, NoteFx};
pub use param::{ParamInfo, Params, Unit};
pub use rack::{Channel, NOTE_FX_SLOTS};
pub use sample::Sample;
pub use sequence::{
    AudioClip, AutoPoint, AutoScope, AutoTarget, AutomationLane, Clip, Note, PPQ, Pattern, PlayMode, Sequence, SessionSlot,
    TICKS_PER_BAR, TICKS_PER_STEP,
};
