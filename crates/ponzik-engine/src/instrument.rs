use std::sync::Arc;

use crate::midi_out::MidiOutEvent;
use crate::param::Params;
use crate::sample::Sample;

/// Pitch bend range of the native instruments, in semitones (±).
pub const PITCH_BEND_RANGE: f32 = 2.0;

/// Expression of one playing note, addressed by its key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoteExpression {
    /// Pitch offset from the note's key in semitones, reached in a straight
    /// line (in semitones) over `secs` from the current offset; 0: at once.
    /// Slides, fine pitch and portamento.
    Tuning { semitones: f32, secs: f32 },
    /// Pan in [−1, 1] (0: center).
    Pan(f32),
}

/// A channel-wide controller, like MIDI's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Controller {
    /// In [−1, 1]: ±[`PITCH_BEND_RANGE`] semitones on the native instruments.
    PitchBend(f32),
    /// Control change `cc` (0 to 127, 1: the mod wheel), value in [0, 1].
    Cc(u8, f32),
}

/// Un générateur de son piloté par des notes : synthé, boîte à rythmes, sampler…
///
/// Les méthodes de jeu sont appelées depuis le thread audio : elles ne doivent
/// ni allouer, ni bloquer. `set_sample_rate` et `set_sample` sont appelés par
/// l'interface avant l'envoi au moteur, ils peuvent allouer.
pub trait Instrument: Params + Send {
    /// Identifiant stable dans le registre (sauvegarde des projets).
    fn kind(&self) -> &'static str;

    /// Vrai si l'instrument joue des hauteurs (synthé), faux pour une percussion.
    fn is_melodic(&self) -> bool {
        false
    }

    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Current tempo, for the tempo-synced devices an instrument holds (an
    /// Instrument Rack's note effects and effects). Audio thread.
    fn set_tempo(&mut self, _bpm: f32) {}

    /// Échantillon à jouer, pour les instruments qui en utilisent un.
    fn set_sample(&mut self, _sample: Arc<Sample>) {}

    /// Sample of velocity layer `layer` of pad `pad` (`None`: empty), for
    /// instruments made of pads (Drum Pads). Called from the UI thread, like
    /// `set_sample`.
    fn set_pad_sample(&mut self, _pad: usize, _layer: usize, _sample: Option<Arc<Sample>>) {}

    /// `velocity` dans [0, 1].
    fn note_on(&mut self, note: u8, velocity: f32);

    fn note_off(&mut self, note: u8);

    /// True when the instrument follows [`NoteExpression::Tuning`]: slide
    /// notes and portamento glide on it, otherwise slide notes play as
    /// ordinary notes.
    fn supports_expression(&self) -> bool {
        false
    }

    /// Expression of the playing note `key` (the last one started on that
    /// key). A new note on a key starts without expression. Audio thread.
    fn note_expression(&mut self, _key: u8, _expr: NoteExpression) {}

    /// A channel-wide controller (pitch bend, mod wheel, any CC). Audio thread.
    fn controller(&mut self, _ctl: Controller) {}

    /// Coupe tout immédiatement (stop du transport, panique).
    fn all_notes_off(&mut self);

    /// Ajoute (`+=`) le rendu dans `left` et `right`, de même longueur.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);

    /// Messages MIDI à envoyer vers le matériel (instrument externe), produits
    /// depuis le dernier appel. Le moteur l'appelle après chaque sous-bloc
    /// rendu : tout ce qui a été produit depuis l'est à l'instant du début de
    /// ce sous-bloc (plus le décalage propre de chaque message). Thread audio :
    /// ni allocation, ni blocage.
    fn drain_midi(&mut self, _out: &mut dyn FnMut(MidiOutEvent)) {}

    /// Live state of the instrument's amp envelope, for an envelope editor
    /// that shows where each note is. Instruments that have one create it at
    /// construction (interface thread); the interface fetches it before the
    /// instrument is sent to the engine and reads it each frame, the
    /// instrument publishes into it once per rendered block. `None`: no
    /// envelope to show.
    fn env_probe(&self) -> Option<Arc<crate::EnvProbe>> {
        None
    }

    /// The plugin's own interface (CLAP or VST 3 plugin), fetched by the UI
    /// before the instrument is sent to the engine.
    fn editor(&self) -> Option<crate::Editor> {
        None
    }

    /// Restores a plugin's opaque state ([`PluginEditor::save_state`](crate::PluginEditor::save_state))
    /// on an instance not sent to the engine yet, before `set_sample_rate`
    /// activates it; the parameter values are read back from the plugin.
    fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
        Err(ponzik_i18n::tr("this device has no state").into())
    }

    /// The Instrument Rack behind this instrument, for the commands that edit it.
    fn as_inst_rack(&mut self) -> Option<&mut crate::instruments::InstrumentRack> {
        None
    }
}

/// A value that moves in a straight line to a target over some samples.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Ramp {
    pub const fn new(value: f32) -> Self {
        Self { value, target: value, step: 0.0, left: 0 }
    }

    /// Heads for `target` over `secs` (at once under a sample).
    pub fn go(&mut self, target: f32, secs: f32, sample_rate: f32) {
        let n = (secs * sample_rate).round();
        self.target = target;
        if n.is_nan() || n < 1.0 {
            self.value = target;
            self.left = 0;
        } else {
            let n = n.min(u32::MAX as f32) as u32;
            self.step = (target - self.value) / n as f32;
            self.left = n;
        }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn is_moving(&self) -> bool {
        self.left > 0
    }

    /// Advances one sample and returns the new value.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 { self.target } else { self.value + self.step };
        }
        self.value
    }

    /// Advances `n` samples.
    pub fn skip(&mut self, n: u32) -> f32 {
        if n >= self.left {
            self.value = self.target;
            self.left = 0;
        } else {
            self.left -= n;
            self.value += self.step * n as f32;
        }
        self.value
    }
}

/// Tuning of every key of a plugin instrument, for plugins whose note
/// expressions are plain values: a ramp becomes a value every few samples.
#[derive(Clone, Debug)]
pub struct TuningRamps {
    ramps: [Ramp; 128],
    /// Keys whose ramp moves, or whose last value is not sent yet.
    busy: u128,
}

impl Default for TuningRamps {
    fn default() -> Self {
        Self { ramps: [Ramp::new(0.0); 128], busy: 0 }
    }
}

impl TuningRamps {
    /// A new note on `key`: no tuning.
    pub fn reset(&mut self, key: u8) {
        let k = usize::from(key & 127);
        self.ramps[k] = Ramp::new(0.0);
        self.busy &= !(1 << k);
    }

    /// Tuning expression for `key`: `Some(value)` to send at once, `None`
    /// when it ramps (see [`TuningRamps::advance`]).
    pub fn set(&mut self, key: u8, semitones: f32, secs: f32, sample_rate: f32) -> Option<f32> {
        let k = usize::from(key & 127);
        let r = &mut self.ramps[k];
        r.go(semitones, secs, sample_rate);
        if r.is_moving() {
            self.busy |= 1 << k;
            None
        } else {
            self.busy &= !(1 << k);
            Some(semitones)
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy != 0
    }

    /// Ramps over the next `frames` samples: `emit(offset, key, semitones)`
    /// every `every` samples while a ramp moves, then once at its end; in
    /// the order of the offsets.
    pub fn advance(&mut self, frames: usize, every: usize, mut emit: impl FnMut(usize, u8, f32)) {
        let every = every.max(1);
        let mut off = 0;
        while off < frames && self.busy != 0 {
            let n = every.min(frames - off);
            let mut busy = self.busy;
            while busy != 0 {
                let k = busy.trailing_zeros() as usize;
                busy &= busy - 1;
                let r = &mut self.ramps[k];
                emit(off, k as u8, r.value());
                if r.is_moving() {
                    r.skip(n as u32);
                } else {
                    self.busy &= !(1 << k);
                }
            }
            off += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_moves_in_a_straight_line() {
        let mut r = Ramp::new(0.0);
        r.go(12.0, 0.01, 1_000.0);
        let v: Vec<f32> = (0..12).map(|_| r.tick()).collect();
        assert!((v[4] - 6.0).abs() < 1e-4, "{v:?}");
        assert_eq!(v[9], 12.0);
        assert_eq!(v[11], 12.0);
        assert!(!r.is_moving());
        r.go(-1.0, 0.0, 1_000.0);
        assert_eq!(r.value(), -1.0);
    }

    #[test]
    fn tuning_ramps_send_values_then_the_target() {
        let mut t = TuningRamps::default();
        assert_eq!(t.set(60, 0.5, 0.0, 48_000.0), Some(0.5));
        assert!(!t.is_busy());
        assert_eq!(t.set(60, 2.5, 0.002, 48_000.0), None);
        let mut sent = Vec::new();
        t.advance(64, 32, |off, key, v| sent.push((off, key, v)));
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], (0, 60, 0.5));
        assert!((sent[1].2 - (0.5 + 2.0 * 32.0 / 96.0)).abs() < 1e-4);
        sent.clear();
        // 64 → 96: the end of the ramp, sent once.
        t.advance(64, 32, |off, key, v| sent.push((off, key, v)));
        assert_eq!(sent.last().map(|s| (s.0, s.2)), Some((32, 2.5)));
        sent.clear();
        t.advance(64, 32, |off, key, v| sent.push((off, key, v)));
        assert!(sent.is_empty() && !t.is_busy());
    }
}
