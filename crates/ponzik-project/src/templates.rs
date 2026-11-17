//! Modèles de projet intégrés : de quoi démarrer une session en deux secondes.
//!
//! Chaque modèle pose ses canaux (instruments natifs), son routage de mixer
//! (bus de réverbe et de délai, master compressé et limité) et un pattern de
//! départ, vide ou non.

use ponzik_engine::{Note, TICKS_PER_BAR, TICKS_PER_STEP};

use ponzik_i18n::tr;

use crate::{EffectDef, InstrumentDef, Project, ScaleKind};

/// Un modèle : (nom affiché, constructeur).
pub type Template = (&'static str, fn() -> Project);

/// Modèles intégrés, dans l'ordre d'affichage (noms traduits).
pub fn templates() -> Vec<Template> {
    vec![
        (tr("Empty"), empty as fn() -> Project),
        (tr("Beat (drums + bass)"), beat),
        (tr("House"), house),
        (tr("Hip-hop / trap 140"), trap),
        (tr("Drum & bass 174"), drum_and_bass),
        (tr("Ambient"), ambient),
    ]
}

// Types de DrumSynth (paramètre 0, voir `instruments/drums.rs`).
const SNARE: f32 = 1.0;
const CLAP: f32 = 2.0;
const CLOSED_HAT: f32 = 3.0;
const OPEN_HAT: f32 = 4.0;

/// Bus d'effets, loin des pistes attribuées aux canaux.
const REVERB_BUS: usize = 30;
const DELAY_BUS: usize = 31;

fn drum(kind: f32) -> InstrumentDef {
    InstrumentDef::new("drumsynth").with(0, kind)
}

fn fx(kind: &str, params: &[(u32, f32)]) -> Option<EffectDef> {
    let mut e = EffectDef::new(kind);
    for &(id, v) in params {
        e.set(id, v);
    }
    Some(e)
}

/// Nouveau projet nommé, avec sa gamme et un master compressé puis limité.
fn base(name: &str, bpm: f32, key: u8, scale: ScaleKind) -> Project {
    let mut p = Project::empty();
    p.name = name.into();
    p.bpm = bpm;
    p.key = key;
    p.scale = scale;
    p.tracks[0].gain = 0.7;
    p.tracks[0].inserts[0] = fx("compressor", &[(0, -12.0), (1, 2.0), (4, 0.02), (6, 1.5)]);
    p.tracks[0].inserts[1] = fx("limiter", &[(1, -0.5)]);
    p
}

fn reverb_bus(p: &mut Project, decay: f32) {
    let t = &mut p.tracks[REVERB_BUS];
    t.name = tr("Reverb").into();
    t.gain = 0.7;
    t.inserts[0] = fx("reverb", &[(2, decay), (5, 1.0)]);
}

fn delay_bus(p: &mut Project, feedback: f32) {
    let t = &mut p.tracks[DELAY_BUS];
    t.name = tr("Delay").into();
    t.gain = 0.6;
    t.inserts[0] = fx("delay", &[(2, feedback), (3, 1.0), (6, 1.0)]);
}

/// Envoi du canal `ch` vers un bus, dans le premier emplacement libre.
fn send(p: &mut Project, ch: usize, bus: usize, amount: f32) {
    let t = p.channels[ch].track;
    if let Some(slot) = p.tracks[t].sends.iter_mut().find(|s| s.1 == 0.0) {
        *slot = (bus, amount);
    }
}

fn insert(p: &mut Project, ch: usize, slot: usize, effect: Option<EffectDef>) {
    let t = p.channels[ch].track;
    p.tracks[t].inserts[slot] = effect;
}

/// Coups de batterie d'un pas (double croche) dans le premier pattern.
fn hits(p: &mut Project, ch: usize, steps: &[u32], vel: f32) {
    for &s in steps {
        let key = p.channels[ch].root_note;
        p.add_note(0, ch, Note { start: s * TICKS_PER_STEP, len: TICKS_PER_STEP, key, vel, ..Note::DEFAULT });
    }
}

/// Notes mélodiques du premier pattern : (pas, note, durée en pas).
fn line(p: &mut Project, ch: usize, notes: &[(u32, u8, u32)], vel: f32) {
    for &(step, key, len) in notes {
        p.add_note(0, ch, Note { start: step * TICKS_PER_STEP, len: len * TICKS_PER_STEP, key, vel, ..Note::DEFAULT });
    }
}

fn empty() -> Project {
    Project::empty()
}

/// Kick, caisse claire, charleston et basse acide, en do mineur à 120.
fn beat() -> Project {
    let mut p = base(tr("Beat"), 120.0, 0, ScaleKind::Minor);
    let kick = p.add_channel(tr("Kick"), InstrumentDef::new("kick"), 36);
    let snare = p.add_channel(tr("Snare"), drum(SNARE), 60);
    let hat = p.add_channel(tr("Hi-hat"), drum(CLOSED_HAT), 60);
    let bass = p.add_channel(tr("Bass"), InstrumentDef::new("bass303").with(1, 600.0), 36);
    p.channels[hat].gain = 0.6;
    p.channels[hat].pan = -0.2;
    p.channels[bass].gain = 0.7;
    reverb_bus(&mut p, 1.6);
    send(&mut p, snare, REVERB_BUS, 0.25);
    p.patterns[0].name = tr("Beat").into();
    hits(&mut p, kick, &[0, 8, 10], 1.0);
    hits(&mut p, snare, &[4, 12], 0.9);
    hits(&mut p, hat, &[0, 2, 4, 6, 8, 10, 12, 14], 0.5);
    line(&mut p, bass, &[(0, 36, 2), (3, 36, 1), (6, 39, 2), (10, 43, 2), (14, 34, 2)], 0.85);
    p
}

/// Quatre temps à 124 en la mineur : kick, clap, charlestons, basse pompée, accords.
fn house() -> Project {
    let mut p = base(tr("House"), 124.0, 9, ScaleKind::Minor);
    let kick = p.add_channel(tr("Kick"), InstrumentDef::new("kick").with(4, 0.45), 36);
    let clap = p.add_channel(tr("Clap"), drum(CLAP), 60);
    let hat = p.add_channel(tr("Hi-hat"), drum(CLOSED_HAT), 60);
    let open = p.add_channel(tr("Open hi-hat"), drum(OPEN_HAT), 60);
    let bass = p.add_channel(tr("Bass"), InstrumentDef::new("pulse").with(2, 300.0), 45);
    let keys = p.add_channel(tr("Chords"), InstrumentDef::new("epiano"), 57);
    p.channels[clap].gain = 0.65;
    p.channels[hat].gain = 0.55;
    p.channels[hat].pan = -0.3;
    p.channels[open].gain = 0.5;
    p.channels[open].pan = 0.3;
    p.channels[keys].gain = 0.6;
    reverb_bus(&mut p, 2.2);
    delay_bus(&mut p, 0.35);
    // La basse et les accords respirent avec le kick.
    insert(&mut p, bass, 0, fx("pompe", &[(2, 0.7)]));
    insert(&mut p, keys, 0, fx("pompe", &[(2, 0.5)]));
    insert(&mut p, keys, 1, fx("chorus", &[(5, 0.3)]));
    send(&mut p, clap, REVERB_BUS, 0.3);
    send(&mut p, open, REVERB_BUS, 0.15);
    send(&mut p, keys, REVERB_BUS, 0.35);
    send(&mut p, keys, DELAY_BUS, 0.2);
    p.patterns[0].name = tr("Groove").into();
    hits(&mut p, kick, &[0, 4, 8, 12], 1.0);
    hits(&mut p, clap, &[4, 12], 0.9);
    hits(&mut p, hat, &[1, 3, 5, 7, 9, 11, 13, 15], 0.45);
    hits(&mut p, open, &[2, 6, 10, 14], 0.6);
    line(&mut p, bass, &[(2, 45, 1), (6, 45, 1), (10, 48, 1), (14, 43, 1)], 0.9);
    // Accord de la mineur 7 en contretemps (degré I de la gamme).
    let chord = p.diatonic_chord(0, 4, 3);
    for (i, &key) in chord.iter().enumerate() {
        let vel = 0.7 - 0.05 * i as f32;
        line(&mut p, keys, &[(2, key, 2), (10, key, 2)], vel);
    }
    p
}

/// Trap à 140 en fa mineur : 808 longue, caisse en demi-tempo, charlestons en roulements.
fn trap() -> Project {
    let mut p = base(tr("Trap"), 140.0, 5, ScaleKind::Minor);
    let k808 = InstrumentDef::new("kick").with(1, 24.0).with(4, 1.6).with(7, 0.8).with(8, 0.5);
    let bass = p.add_channel("808", k808, 29);
    let snare = p.add_channel(tr("Snare"), drum(SNARE), 60);
    let clap = p.add_channel(tr("Clap"), drum(CLAP), 60);
    let hat = p.add_channel(tr("Hi-hat"), drum(CLOSED_HAT), 60);
    let lead = p.add_channel(tr("Melody"), InstrumentDef::new("pluck"), 65);
    p.channels[clap].gain = 0.5;
    p.channels[hat].gain = 0.5;
    p.channels[hat].pan = 0.2;
    p.channels[lead].gain = 0.55;
    reverb_bus(&mut p, 2.8);
    delay_bus(&mut p, 0.45);
    insert(&mut p, bass, 0, fx("saturator", &[(1, 6.0), (4, 0.5)]));
    send(&mut p, snare, REVERB_BUS, 0.2);
    send(&mut p, lead, REVERB_BUS, 0.35);
    send(&mut p, lead, DELAY_BUS, 0.3);
    p.patterns[0].name = tr("Trap").into();
    line(&mut p, bass, &[(0, 29, 6), (7, 29, 2), (10, 32, 4)], 1.0);
    hits(&mut p, snare, &[8], 0.95);
    hits(&mut p, clap, &[8], 0.7);
    hits(&mut p, hat, &[0, 2, 4, 6, 8, 10, 12, 13, 14, 15], 0.45);
    line(&mut p, lead, &[(0, 72, 2), (4, 68, 2), (8, 70, 2), (12, 65, 4)], 0.7);
    p
}

/// Drum & bass à 174 en ré mineur : deux temps cassés, basse Reese, nappe.
fn drum_and_bass() -> Project {
    let mut p = base(tr("Drum & bass"), 174.0, 2, ScaleKind::Minor);
    let kick = p.add_channel(tr("Kick"), InstrumentDef::new("kick").with(4, 0.35), 36);
    let snare = p.add_channel(tr("Snare"), drum(SNARE), 60);
    let hat = p.add_channel(tr("Hi-hat"), drum(CLOSED_HAT), 60);
    // Reese : unisson très désaccordé, filtre fermé.
    let reese = InstrumentDef::new("wavetable").with(3, 35.0).with(10, 500.0).with(12, 6.0).with(22, 1.0);
    let bass = p.add_channel(tr("Reese bass"), reese, 38);
    let pad = InstrumentDef::new("wavetable").with(20, 0.8).with(23, 2.0).with(10, 1_500.0);
    let pad = p.add_channel(tr("Pad"), pad, 62);
    p.channels[hat].gain = 0.5;
    p.channels[bass].gain = 0.6;
    p.channels[pad].gain = 0.45;
    reverb_bus(&mut p, 3.0);
    insert(&mut p, bass, 0, fx("saturator", &[(1, 9.0), (4, 0.6)]));
    send(&mut p, snare, REVERB_BUS, 0.2);
    send(&mut p, pad, REVERB_BUS, 0.5);
    p.patterns[0].name = tr("Two-step").into();
    hits(&mut p, kick, &[0, 10], 1.0);
    hits(&mut p, snare, &[4, 12], 0.95);
    hits(&mut p, hat, &[2, 6, 7, 10, 14], 0.45);
    line(&mut p, bass, &[(0, 38, 6), (10, 41, 6)], 0.9);
    let chord = p.diatonic_chord(0, 3, 4);
    for &key in &chord {
        line(&mut p, pad, &[(0, key, 16)], 0.6);
    }
    p
}

/// Ambient à 80 en ré lydien : nappe, piano, cordes pincées, cloche FM, grands espaces.
fn ambient() -> Project {
    let mut p = base(tr("Ambient"), 80.0, 2, ScaleKind::Lydian);
    let pad = InstrumentDef::new("wavetable").with(20, 1.5).with(23, 3.0).with(10, 1_200.0).with(12, 0.0);
    let pad = p.add_channel(tr("Pad"), pad, 50);
    let keys = p.add_channel(tr("Piano"), InstrumentDef::new("epiano").with(0, 0.3), 62);
    let pluck = p.add_channel(tr("Plucked strings"), InstrumentDef::new("pluck").with(1, 6.0), 74);
    let bell = p.add_channel(tr("Bell"), InstrumentDef::new("fm"), 86);
    p.channels[pad].gain = 0.5;
    p.channels[keys].gain = 0.55;
    p.channels[pluck].gain = 0.45;
    p.channels[pluck].pan = 0.3;
    p.channels[bell].gain = 0.35;
    p.channels[bell].pan = -0.3;
    reverb_bus(&mut p, 8.0);
    delay_bus(&mut p, 0.6);
    p.tracks[REVERB_BUS].gain = 0.8;
    insert(&mut p, pad, 0, fx("chorus", &[(5, 0.4)]));
    for (ch, rev, del) in [(pad, 0.6, 0.0), (keys, 0.5, 0.25), (pluck, 0.5, 0.4), (bell, 0.7, 0.4)] {
        send(&mut p, ch, REVERB_BUS, rev);
        if del > 0.0 {
            send(&mut p, ch, DELAY_BUS, del);
        }
    }
    // Un accord tenu de quatre mesures pour démarrer ; le reste est à écrire.
    p.patterns[0].name = tr("Pad").into();
    p.patterns[0].len = 4 * TICKS_PER_BAR;
    let chord = p.diatonic_chord(0, 4, 3);
    for &key in &chord {
        line(&mut p, pad, &[(0, key, 64)], 0.6);
    }
    p
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ponzik_engine::{Command, Sample, registry};

    use super::*;

    fn no_samples(_: &str) -> Option<Arc<Sample>> {
        None
    }

    #[test]
    fn templates_use_known_devices_and_stay_in_scale() {
        let list = templates();
        assert_eq!(list.len(), 6);
        assert_eq!(list[0].0, "Empty");
        assert_eq!(list[0].1(), Project::empty());
        for (name, make) in list {
            let p = make();
            for c in &p.channels {
                assert!(registry::create_instrument(&c.instrument.kind).is_some(), "{name} : {}", c.instrument.kind);
            }
            for fx in p.tracks.iter().flat_map(|t| t.inserts.iter().flatten()) {
                assert!(registry::create_effect(&fx.kind).is_some(), "{name} : {}", fx.kind);
            }
            assert!(p.routing_order().is_some(), "{name}");
            // Les parties mélodiques respectent la gamme du modèle.
            for (ch, notes) in p.patterns[0].notes.iter().enumerate() {
                if !["drumsynth", "kick"].contains(&p.channels[ch].instrument.kind.as_str()) {
                    assert!(notes.iter().all(|n| p.in_scale(n.key)), "{name} : {}", p.channels[ch].name);
                }
            }
            assert_eq!(crate::load(&crate::save(&p)).unwrap(), p, "{name}");
        }
    }

    #[test]
    fn templates_render_a_bar_without_nan() {
        let sr = 48_000.0;
        for (name, make) in templates() {
            let p = make();
            let (mut engine, mut handle) = p.build_engine(sr, &no_samples);
            handle.send(Command::Play).unwrap();
            let frames = (TICKS_PER_BAR as f32 / TICKS_PER_STEP as f32 / 4.0 * 60.0 / p.bpm * sr) as usize;
            let mut buf = vec![0.0f32; 2 * frames];
            for chunk in buf.chunks_mut(1_024) {
                engine.process(chunk, 2);
            }
            assert!(buf.iter().all(|s| s.is_finite()), "{name} : NaN ou infini");
            let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak <= 1.0, "{name} : crête {peak}");
            if name != "Empty" {
                assert!(peak > 0.01, "{name} : silence ({peak})");
            }
        }
    }
}
