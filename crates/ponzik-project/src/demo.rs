//! Projet de démo : un groove house en La mineur, 100 % instruments natifs.

use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR, TICKS_PER_STEP};

use ponzik_i18n::tr;

use crate::{ClipDef, EffectDef, InstrumentDef, Project};

/// Paramètres de DrumSynth (voir `instruments/drums.rs`).
const DRUM_TYPE: u32 = 0;
const KICK: f32 = 0.0;
const CLAP: f32 = 2.0;
const CLOSED_HAT: f32 = 3.0;
const OPEN_HAT: f32 = 4.0;

pub fn demo_project() -> Project {
    let mut p = Project::empty();
    p.name = tr("House groove in A minor").into();
    p.bpm = 124.0;

    let drum = |t: f32| InstrumentDef::new("drumsynth").with(DRUM_TYPE, t);
    let kick = p.add_channel(tr("Kick"), drum(KICK), 60);
    let clap = p.add_channel(tr("Clap"), drum(CLAP), 60);
    let hat = p.add_channel(tr("Hat"), drum(CLOSED_HAT), 60);
    let open = p.add_channel(tr("Open Hat"), drum(OPEN_HAT), 60);
    let bass = p.add_channel(tr("Bass"), InstrumentDef::new("pulse"), 45);
    p.channels[clap].gain = 0.65;
    p.channels[hat].pan = -0.3;
    p.channels[hat].gain = 0.7;
    p.channels[open].pan = 0.3;
    p.channels[open].gain = 0.55;
    // Marge au master : la démo crête autour de −3 dBFS.
    p.tracks[0].gain = 0.6;

    // Effets : saturation sur la basse, bus de réverbe, bus master compressé et limité.
    let fx = |kind: &str, params: &[(u32, f32)]| {
        let mut e = EffectDef::new(kind);
        for &(id, v) in params {
            e.set(id, v);
        }
        Some(e)
    };
    let bass_track = p.channels[bass].track;
    p.tracks[bass_track].inserts[0] = fx("saturator", &[(0, 2.0), (1, 9.0), (4, 0.7)]);
    let reverb_bus = 10;
    p.tracks[reverb_bus].name = tr("Reverb").into();
    p.tracks[reverb_bus].kind = crate::TrackKind::Bus;
    p.tracks[reverb_bus].color = crate::BUS_COLORS[1];
    p.tracks[reverb_bus].inserts[0] = fx("reverb", &[(2, 2.4), (5, 1.0)]);
    p.tracks[reverb_bus].gain = 0.7;
    for ch in [clap, hat, open] {
        let t = p.channels[ch].track;
        p.tracks[t].sends[0] = (reverb_bus, if ch == clap { 0.35 } else { 0.2 });
    }
    p.tracks[0].inserts[0] = fx("eq", &[(0, 1.0), (1, 30.0), (5, 120.0), (6, 2.0), (7, 0.8), (11, 3_500.0), (12, -1.5), (13, 1.2), (15, 2.0)]);
    p.tracks[0].inserts[1] = fx("compressor", &[(0, -14.0), (1, 2.5), (4, 0.02), (6, 2.0)]);
    p.tracks[0].inserts[2] = fx("limiter", &[(1, -0.5)]);

    let hits = |p: &mut Project, pat: usize, ch: usize, steps: &[u32], vel: f32| {
        for &s in steps {
            p.set_step(pat, ch, s, true);
            if let Some(n) = p.patterns[pat].notes[ch].iter_mut().find(|n| n.start == s * TICKS_PER_STEP) {
                n.vel = vel;
            }
        }
    };
    let bassline = |p: &mut Project, pat: usize, notes: &[(u32, u8, u32)]| {
        for &(step, key, len) in notes {
            p.add_note(
                pat,
                bass,
                Note { start: step * TICKS_PER_STEP, len: len * TICKS_PER_STEP * 4 / 5, key, vel: 0.9, ..Note::DEFAULT },
            );
        }
    };

    // Pattern 1 : le groove complet.
    p.patterns[0].name = tr("Groove").into();
    hits(&mut p, 0, kick, &[0, 4, 8, 12], 1.0);
    hits(&mut p, 0, clap, &[4, 12], 0.9);
    hits(&mut p, 0, hat, &[1, 3, 5, 7, 9, 11, 13, 15], 0.5);
    hits(&mut p, 0, open, &[2, 6, 10, 14], 0.7);
    bassline(&mut p, 0, &[(0, 33, 1), (3, 45, 1), (6, 33, 1), (8, 36, 1), (10, 43, 1), (11, 45, 1), (14, 40, 2)]);

    // Pattern 2 : intro sans basse.
    let intro = p.add_pattern();
    p.patterns[intro].name = tr("Intro").into();
    hits(&mut p, intro, kick, &[0, 4, 8, 12], 1.0);
    hits(&mut p, intro, hat, &[2, 6, 10, 14], 0.45);

    // Pattern 3 : break, basse seule.
    let brk = p.add_pattern();
    p.patterns[brk].name = tr("Break").into();
    bassline(&mut p, brk, &[(0, 33, 3), (6, 36, 2), (10, 40, 2), (14, 43, 2)]);
    hits(&mut p, brk, clap, &[12], 0.6);

    let bar = TICKS_PER_BAR;
    let clip = |pattern, lane, start_bar: u32, bars: u32| ClipDef {
        pattern,
        lane,
        start: start_bar * bar,
        len: bars * bar,
        offset: 0,
        muted: false,
    };
    p.clips = vec![clip(intro, 0, 0, 4), clip(0, 0, 4, 8), clip(brk, 1, 12, 4), clip(0, 0, 16, 8)];
    p.mode = PlayMode::Pattern(0);
    p
}
