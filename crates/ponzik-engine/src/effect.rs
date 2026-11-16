use crate::param::Params;

/// Un effet audio inséré sur une piste du mixer : EQ, compresseur, réverb…
///
/// `process` et `set_param` sont appelés depuis le thread audio : ni allocation,
/// ni verrou. Tout ce qui alloue (lignes de retard…) se fait dans
/// `set_sample_rate`, appelé par l'interface avant l'envoi au moteur.
pub trait Effect: Params + Send {
    /// Identifiant stable dans le registre (sauvegarde des projets).
    fn kind(&self) -> &'static str;

    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Vide les états internes (queues de réverb, lignes de retard).
    fn reset(&mut self) {}

    /// Tempo courant, pour les effets synchronisés.
    fn set_tempo(&mut self, _bpm: f32) {}

    /// Signal clé (entrée latérale) du bloc à venir, appelé juste avant `process`
    /// quand une piste clé est branchée sur l'insert : même longueur que le bloc,
    /// post-fader de la piste clé. Sans clé, n'est pas appelé.
    fn sidechain(&mut self, _left: &[f32], _right: &[f32]) {}

    /// Traitement sur place d'un bloc stéréo (`left.len() == right.len()`).
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}
