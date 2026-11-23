use std::any::Any;
use std::sync::Arc;

use crate::param::Params;

/// Notes a channel of the rack holds down at the end of the block (after its
/// note effects), as an effect can follow them: a pitch corrector taking its
/// target melody from a channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelNotes {
    /// Bit `n % 64` of word `n / 64`: MIDI note `n` is held.
    pub held: [u64; 2],
    /// Most recent note started and still held.
    pub last: Option<u8>,
}

impl ChannelNotes {
    pub const EMPTY: Self = Self { held: [0; 2], last: None };

    pub fn is_held(&self, note: u8) -> bool {
        let n = usize::from(note.min(127));
        self.held[n / 64] & (1 << (n % 64)) != 0
    }

    pub fn any(&self) -> bool {
        self.held != [0; 2]
    }

    /// Pitch classes held (bit 0: C … bit 11: B).
    pub fn pitch_classes(&self) -> u16 {
        (0..128u8).filter(|&n| self.is_held(n)).fold(0, |m, n| m | 1 << (n % 12))
    }

    pub fn note_on(&mut self, note: u8) {
        let n = usize::from(note.min(127));
        self.held[n / 64] |= 1 << (n % 64);
        self.last = Some(n as u8);
    }

    pub fn note_off(&mut self, note: u8) {
        let n = usize::from(note.min(127));
        self.held[n / 64] &= !(1 << (n % 64));
        if self.last == Some(n as u8) {
            // The highest note still held takes over.
            self.last = (0..128u8).rev().find(|&k| self.is_held(k));
        }
    }
}

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

    /// Notes held on each channel of the rack (indexed by channel), called
    /// before every `process`. Most effects ignore it.
    fn channel_notes(&mut self, _notes: &[ChannelNotes]) {}

    /// Retard introduit par l'effet, en échantillons (anticipation, suréchantillonnage,
    /// plugin externe). Le mixer le compense sur les autres chemins pour que tout
    /// arrive aligné au master. Lu au début de chaque bloc depuis le thread audio :
    /// ni allocation ni verrou.
    fn latency(&self) -> usize {
        0
    }

    /// Longest time, in seconds, the effect may give silence back while
    /// sound is still on its way out (the gap between two echoes of a delay,
    /// a pre-delay): the smart disable waits that much longer before putting
    /// it to sleep. Read on the audio thread: no allocation, no lock.
    fn tail(&self) -> f32 {
        0.0
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

    /// Restores a plugin's opaque state ([`PluginEditor::save_state`](crate::PluginEditor::save_state))
    /// on an instance not sent to the engine yet, before `set_sample_rate`
    /// activates it; the parameter values are read back from the plugin.
    fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
        Err(ponzik_i18n::tr("this device has no state").into())
    }

    /// Traitement sur place d'un bloc stéréo (`left.len() == right.len()`).
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}
