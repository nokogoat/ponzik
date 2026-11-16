use std::any::Any;
use std::sync::Arc;

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

    /// Retard introduit par l'effet, en échantillons (anticipation, suréchantillonnage,
    /// plugin externe). Le mixer le compense sur les autres chemins pour que tout
    /// arrive aligné au master. Lu au début de chaque bloc depuis le thread audio :
    /// ni allocation ni verrou.
    fn latency(&self) -> usize {
        0
    }

    /// État partagé avec l'interface (analyse, indicateurs), écrit par le thread
    /// audio sans verrou (atomiques). L'interface le récupère avant d'envoyer
    /// l'effet au moteur, puis le lit en le convertissant au type concret.
    fn shared(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }

    /// The effect rack behind this effect, for the commands that edit its
    /// chains and macros.
    fn as_rack(&mut self) -> Option<&mut crate::effects::EffectRack> {
        None
    }

    /// The plugin's own interface (CLAP or VST 3 plugin), fetched by the UI
    /// before the effect is sent to the engine.
    fn editor(&self) -> Option<crate::Editor> {
        None
    }

    /// Traitement sur place d'un bloc stéréo (`left.len() == right.len()`).
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}
