//! Registre des instruments et effets natifs : création par identifiant
//! (chargement de projet, menus du navigateur).

use crate::effect::Effect;
use crate::effects::{
    AutoFilter, Bitcrusher, Chorus, Compressor, Delay, Eq, Gate, Limiter, Phaser, Reverb, Saturator, Utility,
};
use crate::instrument::Instrument;
use crate::instruments::{Bass303, DrumKind, DrumSynth, FmSynth, Pulse, Sampler, WaveSynth};

pub struct InstrumentKind {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub create: fn() -> Box<dyn Instrument>,
}

pub struct EffectKind {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub create: fn() -> Box<dyn Effect>,
}

pub fn instruments() -> &'static [InstrumentKind] {
    &[
        InstrumentKind {
            id: "pulse",
            name: "Pulse",
            category: "Synthés",
            description: "Synthé soustractif polyphonique",
            create: || Box::new(Pulse::default()),
        },
        InstrumentKind {
            id: "drumsynth",
            name: "DrumSynth",
            category: "Batterie",
            description: "Percussions synthétisées façon 808 et 909",
            create: || Box::new(DrumSynth::new(DrumKind::Kick)),
        },
        InstrumentKind {
            id: "fm",
            name: "Ponzik FM",
            category: "Synthés",
            description: "Synthé FM à quatre opérateurs et huit algorithmes",
            create: || Box::new(FmSynth::new()),
        },
        InstrumentKind {
            id: "wavetable",
            name: "Ponzik Wave",
            category: "Synthés",
            description: "Synthé à tables d'ondes avec unisson et sous-oscillateur",
            create: || Box::new(WaveSynth::new()),
        },
        InstrumentKind {
            id: "sampler",
            name: "Sampler",
            category: "Échantillonneurs",
            description: "Lecteur d'échantillons, mélodique ou one-shot, avec boucle",
            create: || Box::new(Sampler::new()),
        },
        InstrumentKind {
            id: "bass303",
            name: "Acid 303",
            category: "Basses",
            description: "Basse acide monophonique avec accent et glissé",
            create: || Box::new(Bass303::new()),
        },
    ]
}

pub fn effects() -> &'static [EffectKind] {
    &[
        EffectKind {
            id: "eq",
            name: "EQ",
            category: "EQ et filtres",
            description: "Égaliseur paramétrique : coupe-bas, plateaux, trois cloches, coupe-haut",
            create: || Box::new(Eq::default()),
        },
        EffectKind {
            id: "autofilter",
            name: "AutoFilter",
            category: "EQ et filtres",
            description: "Filtre modulé par LFO synchronisable et suivi d'enveloppe",
            create: || Box::new(AutoFilter::default()),
        },
        EffectKind {
            id: "compressor",
            name: "Compresseur",
            category: "Dynamique",
            description: "Compresseur stéréo lié, genou doux, mix parallèle",
            create: || Box::new(Compressor::default()),
        },
        EffectKind {
            id: "limiter",
            name: "Limiteur",
            category: "Dynamique",
            description: "Limiteur à anticipation : aucun dépassement du plafond",
            create: || Box::new(Limiter::default()),
        },
        EffectKind {
            id: "gate",
            name: "Porte",
            category: "Dynamique",
            description: "Porte de bruit avec maintien et plage",
            create: || Box::new(Gate::default()),
        },
        EffectKind {
            id: "reverb",
            name: "Réverb",
            category: "Espace",
            description: "Réverbération algorithmique à huit lignes",
            create: || Box::new(Reverb::default()),
        },
        EffectKind {
            id: "delay",
            name: "Delay",
            category: "Espace",
            description: "Écho stéréo synchronisé au tempo, ping-pong",
            create: || Box::new(Delay::default()),
        },
        EffectKind {
            id: "chorus",
            name: "Chorus",
            category: "Modulation",
            description: "Chorus et flanger stéréo",
            create: || Box::new(Chorus::default()),
        },
        EffectKind {
            id: "phaser",
            name: "Phaser",
            category: "Modulation",
            description: "Phaser de 4 à 12 étages",
            create: || Box::new(Phaser::default()),
        },
        EffectKind {
            id: "saturator",
            name: "Saturateur",
            category: "Saturation",
            description: "Saturation suréchantillonnée : douce, dure, lampe, repli, redresseur",
            create: || Box::new(Saturator::default()),
        },
        EffectKind {
            id: "bitcrusher",
            name: "Bitcrusher",
            category: "Saturation",
            description: "Réduction de bits et de fréquence d'échantillonnage",
            create: || Box::new(Bitcrusher::default()),
        },
        EffectKind {
            id: "utility",
            name: "Utilitaire",
            category: "Utilitaires",
            description: "Gain, balance, largeur, graves mono, phase",
            create: || Box::new(Utility::default()),
        },
    ]
}

pub fn instrument_kind(id: &str) -> Option<&'static InstrumentKind> {
    instruments().iter().find(|k| k.id == id)
}

pub fn effect_kind(id: &str) -> Option<&'static EffectKind> {
    effects().iter().find(|k| k.id == id)
}

pub fn create_instrument(id: &str) -> Option<Box<dyn Instrument>> {
    instrument_kind(id).map(|k| (k.create)())
}

pub fn create_effect(id: &str) -> Option<Box<dyn Effect>> {
    effect_kind(id).map(|k| (k.create)())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_reports_its_own_id() {
        for k in instruments() {
            assert_eq!((k.create)().kind(), k.id);
        }
        for k in effects() {
            assert_eq!((k.create)().kind(), k.id);
        }
    }

    #[test]
    fn params_have_unique_ids_and_valid_defaults() {
        let check = |params: &[crate::param::ParamInfo]| {
            for (i, p) in params.iter().enumerate() {
                assert!(p.min <= p.default && p.default <= p.max, "{} : défaut hors bornes", p.name);
                assert!(params[i + 1..].iter().all(|q| q.id != p.id), "{} : id en double", p.name);
            }
        };
        for k in instruments() {
            check((k.create)().params());
        }
        for k in effects() {
            check((k.create)().params());
        }
    }
}
