//! Registre des instruments et effets natifs : création par identifiant
//! (chargement de projet, menus du navigateur).

use crate::effect::Effect;
use crate::effects::{
    AutoFilter, Bitcrusher, Chorus, Compressor, Delay, Eq, Gate, Imager, Limiter, Ott, Phaser, PitchShifter, Pompe,
    Reverb, Saturator, Shimmer, Tranche, Transient, Utility,
};
use crate::instrument::Instrument;
use crate::instruments::{
    Bass303, DrumKind, DrumSynth, EPiano, FmSynth, Granular, Kick, Pluck, Pulse, Sampler, Slicer, WaveSynth,
};

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
        InstrumentKind {
            id: "kick",
            name: "Kick",
            category: "Batterie",
            description: "Grosse caisse sculptée : chute de hauteur, clic, corps, saturation",
            create: || Box::new(Kick::new()),
        },
        InstrumentKind {
            id: "slicer",
            name: "Découpeur",
            category: "Échantillonneurs",
            description: "Découpe un échantillon en tranches égales ou sur les attaques, une note par tranche",
            create: || Box::new(Slicer::new()),
        },
        InstrumentKind {
            id: "granular",
            name: "Granulaire",
            category: "Échantillonneurs",
            description: "Nuage de grains : position, dispersion, densité, hauteur et largeur aléatoires",
            create: || Box::new(Granular::new()),
        },
        InstrumentKind {
            id: "pluck",
            name: "Cordes pincées",
            category: "Synthés",
            description: "Corde physique Karplus-Strong avec médiator, caisse et largeur stéréo",
            create: || Box::new(Pluck::new()),
        },
        InstrumentKind {
            id: "epiano",
            name: "Piano électrique",
            category: "Claviers",
            description: "Piano électrique FM, lame et cloche, trémolo stéréo",
            create: || Box::new(EPiano::new()),
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
            id: "ott",
            name: "Multibande",
            category: "Dynamique",
            description: "Compression trois bandes vers le haut et vers le bas, dosée par la profondeur",
            create: || Box::new(Ott::default()),
        },
        EffectKind {
            id: "pompe",
            name: "Pompe",
            category: "Dynamique",
            description: "Volume qui plonge à chaque temps, façon compression latérale, synchronisé au tempo",
            create: || Box::new(Pompe::default()),
        },
        EffectKind {
            id: "transient",
            name: "Transitoires",
            category: "Dynamique",
            description: "Renforce ou adoucit l'attaque et le maintien des sons",
            create: || Box::new(Transient::default()),
        },
        EffectKind {
            id: "tranche",
            name: "Tranche",
            category: "Rythme",
            description: "Hachoir, répétition de tranches et arrêt de bande synchronisés au tempo",
            create: || Box::new(Tranche::default()),
        },
        EffectKind {
            id: "pitch",
            name: "Transposeur",
            category: "Hauteur",
            description: "Transposition granulaire de ±24 demi-tons",
            create: || Box::new(PitchShifter::default()),
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
            id: "shimmer",
            name: "Shimmer",
            category: "Espace",
            description: "Réverbération ample dont la queue monte d'une octave à chaque passage",
            create: || Box::new(Shimmer::default()),
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
        EffectKind {
            id: "imager",
            name: "Image stéréo",
            category: "Utilitaires",
            description: "Largeur stéréo sur trois bandes et graves en mono",
            create: || Box::new(Imager::default()),
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
