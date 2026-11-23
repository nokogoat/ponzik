//! Moteur audio temps réel de Ponzik.
//!
//! Règle d'or : rien dans [`Engine::process`] n'alloue, ne verrouille ni ne bloque.
//! L'interface pilote le moteur uniquement via des [`Command`] envoyées dans une
//! file sans verrou ([`ring`]) ; les objets remplacés lui reviennent par la file
//! [`Garbage`] pour être libérés hors du thread audio.

pub mod audition;
pub mod codecs;
pub mod denormal;
pub mod dylib;
pub mod effect;
pub mod editor;
pub mod effects;
pub mod flac;
pub mod freeze;
pub mod input;
pub mod engine;
pub mod instrument;
pub mod instruments;
pub mod md5;
mod meter;
pub mod midi_out;
pub mod mixer;
pub mod modulation;
pub mod note_fx;
pub mod param;
pub mod pool;
pub mod rack;
pub mod registry;
pub mod ring;
pub mod sample;
pub mod sequence;
pub mod tempo_map;
pub mod voice;
pub mod wav;

pub use editor::{Editor, EditorEvent, EditorSize, ParentWindow, PluginEditor, RunLoop, StateContext};
pub use denormal::FlushDenormals;
pub use effect::{ChannelNotes, Effect};
pub use freeze::{FreezeClock, Frozen, Tap};
pub use effects::fx_rack::{MacroMap, RackLayout};
pub use modulation::{ModClock, ModKind, ModLink, ModMeters, ModParam, ModSet, Modulator};
pub use engine::{Command, Engine, EngineHandle, Garbage, MAX_CHANNELS, MAX_RECORDINGS, render_audio_clips, silent_instrument, track_remap};
pub use input::{InputConsumer, InputProducer, InputSource, InputStrip, RecordBuffer, RecordChunk, TakeSlice, input_channel};
pub use instrument::{Controller, Instrument, NoteExpression, PITCH_BEND_RANGE, Ramp, TuningRamps};
pub use midi_out::{MidiOut, MidiOutEvent};
pub use note_fx::{NoteBuffer, NoteEvent, NoteFx};
pub use param::{ParamInfo, Params, Unit};
pub use pool::{Parallelism, WorkerPool, default_worker_threads, promote_current_thread};
pub use rack::{Channel, NOTE_FX_SLOTS};
pub use sample::Sample;
pub use sequence::{
    AudioClip, AutoPoint, AutoScope, AutoTarget, AutomationLane, Clip, ClipWarp, MAX_FINE, MAX_RATCHET, Note, NoteCond, PPQ, Pattern, PlayMode, Sequence, SessionSlot,
    TICKS_PER_BAR, TICKS_PER_STEP, WarpMap, WarpMarker, equal_power, fade_shape, splitmix,
};
pub use voice::{VoiceConfig, VoiceEvent, VoiceFrame, VoiceMode, VoiceReport, VoiceSettings};
pub use tempo_map::{BarBeat, TempoMap, TempoPoint, TempoView, TimeSig};
