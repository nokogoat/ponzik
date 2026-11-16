use std::sync::Arc;

use crate::midi_out::MidiOutEvent;
use crate::param::Params;
use crate::sample::Sample;

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

    /// Échantillon à jouer, pour les instruments qui en utilisent un.
    fn set_sample(&mut self, _sample: Arc<Sample>) {}

    /// Sample of velocity layer `layer` of pad `pad` (`None`: empty), for
    /// instruments made of pads (Drum Pads). Called from the UI thread, like
    /// `set_sample`.
    fn set_pad_sample(&mut self, _pad: usize, _layer: usize, _sample: Option<Arc<Sample>>) {}

    /// `velocity` dans [0, 1].
    fn note_on(&mut self, note: u8, velocity: f32);

    fn note_off(&mut self, note: u8);

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

    /// The plugin's own interface (CLAP or VST 3 plugin), fetched by the UI
    /// before the instrument is sent to the engine.
    fn editor(&self) -> Option<crate::Editor> {
        None
    }
}
