//! Registre des instruments, effets et effets de notes natifs : création par
//! identifiant (chargement de projet, menus du navigateur).

use crate::effect::Effect;
use crate::effects::{
    AutoFilter, Bitcrusher, Chorus, Compressor, Delay, Ducker, Eq, Gate, Imager, Limiter, Ott, Phaser, PitchShifter,
    Pompe, Reverb, Saturator, Shimmer, Tranche, Transient, Utility,
};
use crate::effects::BusComp;
use crate::effects::CharDelay;
use crate::effects::MasterLimiter;
use crate::effects::MixAssistant;
use crate::effects::CharSaturation;
use crate::effects::Lofi;
use crate::effects::VintageVerb;
use crate::instruments::DrumMachine;
use crate::instrument::Instrument;
use crate::instruments::{
    Bass303, DrumKind, DrumSynth, EPiano, ExternalMidi, FmSynth, Granular, Kick, Pluck, Pulse, Sampler, Slicer, WaveSynth,
};
use crate::note_fx::{Arpeggiator, Chord, Humanizer, NoteFx, Repeat, ScaleSnap};

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

pub struct NoteFxKind {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub create: fn() -> Box<dyn NoteFx>,
}

pub fn instruments() -> &'static [InstrumentKind] {
    &[
        InstrumentKind {
            id: "pulse",
            name: "Pulse",
            category: "Synths",
            description: "Polyphonic subtractive synth",
            create: || Box::new(Pulse::default()),
        },
        InstrumentKind {
            id: "drumsynth",
            name: "DrumSynth",
            category: "Drums",
            description: "Synthesized 808 and 909 style percussion",
            create: || Box::new(DrumSynth::new(DrumKind::Kick)),
        },
        InstrumentKind {
            id: "fm",
            name: "Ponzik FM",
            category: "Synths",
            description: "Four-operator FM synth with eight algorithms",
            create: || Box::new(FmSynth::new()),
        },
        InstrumentKind {
            id: "wavetable",
            name: "Ponzik Wave",
            category: "Synths",
            description: "Wavetable synth with unison and sub oscillator",
            create: || Box::new(WaveSynth::new()),
        },
        InstrumentKind {
            id: "sampler",
            name: "Sampler",
            category: "Samplers",
            description: "Sample player, melodic or one-shot, with looping",
            create: || Box::new(Sampler::new()),
        },
        InstrumentKind {
            id: "bass303",
            name: "Acid 303",
            category: "Bass",
            description: "Monophonic acid bass with accent and slide",
            create: || Box::new(Bass303::new()),
        },
        InstrumentKind {
            id: "kick",
            name: "Kick",
            category: "Drums",
            description: "Sculpted kick drum: pitch drop, click, body, saturation",
            create: || Box::new(Kick::new()),
        },
        InstrumentKind {
            id: "slicer",
            name: "Slicer",
            category: "Samplers",
            description: "Slices a sample into equal parts or at transients, one note per slice",
            create: || Box::new(Slicer::new()),
        },
        InstrumentKind {
            id: "granular",
            name: "Granular",
            category: "Samplers",
            description: "Grain cloud: position, spray, density, random pitch and width",
            create: || Box::new(Granular::new()),
        },
        InstrumentKind {
            id: "pluck",
            name: "Pluck",
            category: "Synths",
            description: "Physical Karplus-Strong string with pick, body and stereo width",
            create: || Box::new(Pluck::new()),
        },
        InstrumentKind {
            id: "epiano",
            name: "Electric Piano",
            category: "Keys",
            description: "FM electric piano, tine and bell, stereo tremolo",
            create: || Box::new(EPiano::new()),
        },
        InstrumentKind {
            id: "drum_machine",
            name: "Drum Machine",
            category: "Drums",
            description: "Eight layered synthesized pads (transient, body, noise), tune, decay and level per pad",
            create: || Box::new(DrumMachine::new()),
        },
        InstrumentKind {
            id: "midi_out",
            name: "External MIDI",
            category: "External",
            description: "Plays a hardware synth or another app: sends the channel's notes to a MIDI output port",
            create: || Box::new(ExternalMidi::new()),
        },
    ]
}

pub fn effects() -> &'static [EffectKind] {
    &[
        EffectKind {
            id: "eq",
            name: "EQ",
            category: "EQ & Filters",
            description: "Parametric EQ: low cut, shelves, three bells, high cut",
            create: || Box::new(Eq::default()),
        },
        EffectKind {
            id: "autofilter",
            name: "AutoFilter",
            category: "EQ & Filters",
            description: "Filter modulated by a syncable LFO and an envelope follower",
            create: || Box::new(AutoFilter::default()),
        },
        EffectKind {
            id: "compressor",
            name: "Compressor",
            category: "Dynamics",
            description: "Linked stereo compressor, soft knee, parallel mix",
            create: || Box::new(Compressor::default()),
        },
        EffectKind {
            id: "limiter",
            name: "Limiter",
            category: "Dynamics",
            description: "Lookahead limiter: never goes over the ceiling",
            create: || Box::new(Limiter::default()),
        },
        EffectKind {
            id: "gate",
            name: "Gate",
            category: "Dynamics",
            description: "Noise gate with hold and range",
            create: || Box::new(Gate::default()),
        },
        EffectKind {
            id: "ott",
            name: "Multiband",
            category: "Dynamics",
            description: "Three-band upward and downward compression, dialed in with depth",
            create: || Box::new(Ott::default()),
        },
        EffectKind {
            id: "pompe",
            name: "Pump",
            category: "Dynamics",
            description: "Volume that dips on every beat, sidechain style, synced to the tempo",
            create: || Box::new(Pompe::default()),
        },
        EffectKind {
            id: "ducker",
            name: "Ducking",
            category: "Dynamics",
            description: "Lowers the volume when the key track plays (sidechain input), with hold and release",
            create: || Box::new(Ducker::default()),
        },
        EffectKind {
            id: "transient",
            name: "Transient Shaper",
            category: "Dynamics",
            description: "Boosts or softens the attack and sustain of sounds",
            create: || Box::new(Transient::default()),
        },
        EffectKind {
            id: "tranche",
            name: "Stutter",
            category: "Rhythm",
            description: "Tempo-synced chopper, slice repeat and tape stop",
            create: || Box::new(Tranche::default()),
        },
        EffectKind {
            id: "pitch",
            name: "Pitch Shifter",
            category: "Pitch",
            description: "Granular pitch shifting over ±24 semitones",
            create: || Box::new(PitchShifter::default()),
        },
        EffectKind {
            id: "reverb",
            name: "Reverb",
            category: "Space",
            description: "Eight-line algorithmic reverb",
            create: || Box::new(Reverb::default()),
        },
        EffectKind {
            id: "delay",
            name: "Delay",
            category: "Space",
            description: "Tempo-synced stereo echo, ping-pong",
            create: || Box::new(Delay::default()),
        },
        EffectKind {
            id: "shimmer",
            name: "Shimmer",
            category: "Space",
            description: "Lush reverb whose tail rises an octave on every pass",
            create: || Box::new(Shimmer::default()),
        },
        EffectKind {
            id: "chorus",
            name: "Chorus",
            category: "Modulation",
            description: "Stereo chorus and flanger",
            create: || Box::new(Chorus::default()),
        },
        EffectKind {
            id: "phaser",
            name: "Phaser",
            category: "Modulation",
            description: "Phaser with 4 to 12 stages",
            create: || Box::new(Phaser::default()),
        },
        EffectKind {
            id: "saturator",
            name: "Saturator",
            category: "Saturation",
            description: "Oversampled saturation: soft, hard, tube, fold, rectifier",
            create: || Box::new(Saturator::default()),
        },
        EffectKind {
            id: "bitcrusher",
            name: "Bitcrusher",
            category: "Saturation",
            description: "Bit depth and sample rate reduction",
            create: || Box::new(Bitcrusher::default()),
        },
        EffectKind {
            id: "utility",
            name: "Utility",
            category: "Utilities",
            description: "Gain, balance, width, mono bass, phase",
            create: || Box::new(Utility::default()),
        },
        EffectKind {
            id: "imager",
            name: "Stereo Imager",
            category: "Utilities",
            description: "Three-band stereo width and mono bass",
            create: || Box::new(Imager::default()),
        },
        EffectKind {
            id: "bus_comp",
            name: "Bus Compressor",
            category: "Dynamics",
            description: "Bus glue: clean, opto and VCA modes, auto release, gain reduction meter",
            create: || Box::new(BusComp::default()),
        },
        EffectKind {
            id: "master_limiter",
            name: "Mastering Limiter",
            category: "Dynamics",
            description: "True peak lookahead limiter: transparent, punchy or aggressive",
            create: || Box::new(MasterLimiter::default()),
        },
        EffectKind {
            id: "char_delay",
            name: "Character Delay",
            category: "Space",
            description: "Tape, analog or digital echo: saturation and filters in the loop, wow, ping-pong",
            create: || Box::new(CharDelay::default()),
        },
        EffectKind {
            id: "char_saturation",
            name: "Character Saturation",
            category: "Saturation",
            description: "Tube, tape, transistor or rectifier, full range or three bands, with tone and mix",
            create: || Box::new(CharSaturation::default()),
        },
        EffectKind {
            id: "lofi",
            name: "Noise & Texture",
            category: "Saturation",
            description: "Vinyl crackle, tape hiss, wow, block bit loss and spectral wear",
            create: || Box::new(Lofi::default()),
        },
        EffectKind {
            id: "vintage_verb",
            name: "Algorithmic Reverb",
            category: "Space",
            description: "Hall, plate or diffusion cloud, rich modulation, tails up to a minute and freeze",
            create: || Box::new(VintageVerb::default()),
        },
        EffectKind {
            id: "mix_assistant",
            name: "Mix Assistant",
            category: "Mixing",
            description: "Listens to the track and suggests a high-pass, EQ, de-esser, compressor, saturation, width and level per source type; nothing changes until you apply",
            create: || Box::new(MixAssistant::default()),
        },
    ]
}

/// Effets de notes, placés avant l'instrument d'un canal.
pub fn note_fx() -> &'static [NoteFxKind] {
    &[
        NoteFxKind {
            id: "arpeggiator",
            name: "Arpeggiator",
            description: "Plays held notes one by one, synced to the tempo, over one to four octaves",
            create: || Box::new(Arpeggiator::default()),
        },
        NoteFxKind {
            id: "chord",
            name: "Chord",
            description: "Adds up to four notes to every note played: presets or custom intervals",
            create: || Box::new(Chord::default()),
        },
        NoteFxKind {
            id: "scale",
            name: "Scale",
            description: "Snaps notes to a scale, or drops the ones outside it",
            create: || Box::new(ScaleSnap::default()),
        },
        NoteFxKind {
            id: "random",
            name: "Random",
            description: "Random play probability, velocity and delay to humanize",
            create: || Box::new(Humanizer::default()),
        },
        NoteFxKind {
            id: "repeat",
            name: "Repeat",
            description: "Retriggers each note at the chosen rate, while held or a fixed number of hits",
            create: || Box::new(Repeat::default()),
        },
    ]
}

pub fn instrument_kind(id: &str) -> Option<&'static InstrumentKind> {
    instruments().iter().find(|k| k.id == id)
}

pub fn effect_kind(id: &str) -> Option<&'static EffectKind> {
    effects().iter().find(|k| k.id == id)
}

/// Effets qui écoutent une entrée latérale ([`Effect::sidechain`]).
const SIDECHAIN_EFFECTS: [&str; 4] = ["compressor", "bus_comp", "gate", "ducker"];

/// Vrai si l'effet `id` accepte une piste clé.
pub fn takes_sidechain(id: &str) -> bool {
    SIDECHAIN_EFFECTS.contains(&id)
}

pub fn note_fx_kind(id: &str) -> Option<&'static NoteFxKind> {
    note_fx().iter().find(|k| k.id == id)
}

pub fn create_note_fx(id: &str) -> Option<Box<dyn NoteFx>> {
    note_fx_kind(id).map(|k| (k.create)())
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
    fn sidechain_effects_exist() {
        for id in SIDECHAIN_EFFECTS {
            assert!(effect_kind(id).is_some(), "{id}");
        }
        assert!(!takes_sidechain("reverb"));
    }

    #[test]
    fn every_kind_reports_its_own_id() {
        for k in instruments() {
            assert_eq!((k.create)().kind(), k.id);
        }
        for k in effects() {
            assert_eq!((k.create)().kind(), k.id);
        }
        for k in note_fx() {
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
        for k in note_fx() {
            check((k.create)().params());
        }
    }
}
