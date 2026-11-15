/// Un générateur de son piloté par des notes : synthé, boîte à rythmes, sampler…
///
/// Toutes les méthodes sont appelées depuis le thread audio :
/// elles ne doivent ni allouer, ni bloquer.
pub trait Instrument: Send {
    /// Nom affiché dans l'interface.
    fn name(&self) -> &'static str {
        "Instrument"
    }

    /// Vrai si l'instrument joue des hauteurs (synthé), faux pour une percussion.
    fn is_melodic(&self) -> bool {
        false
    }

    fn set_sample_rate(&mut self, sample_rate: f32);

    /// `velocity` dans [0, 1].
    fn note_on(&mut self, note: u8, velocity: f32);

    fn note_off(&mut self, note: u8);

    /// Coupe tout immédiatement (stop du transport, panique).
    fn all_notes_off(&mut self);

    /// Ajoute (`+=`) le rendu dans `left` et `right`, de même longueur.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
}
