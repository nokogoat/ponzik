//! Registre des instruments et effets natifs : création par identifiant
//! (chargement de projet, menus du navigateur).

use crate::effect::Effect;
use crate::instrument::Instrument;
use crate::instruments::{DrumKind, DrumSynth, Pulse};

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
    ]
}

pub fn effects() -> &'static [EffectKind] {
    &[]
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
