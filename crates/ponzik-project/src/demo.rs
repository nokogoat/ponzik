//! Projet de démo : un groove house en La mineur, 100 % instruments natifs.

use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR, TICKS_PER_STEP};

use crate::{ClipDef, InstrumentDef, Project};

/// Paramètres de DrumSynth (voir `instruments/drums.rs`).
const DRUM_TYPE: u32 = 0;
const KICK: f32 = 0.0;
const CLAP: f32 = 2.0;
const CLOSED_HAT: f32 = 3.0;
const OPEN_HAT: f32 = 4.0;

pub fn demo_project() -> Project {
    let mut p = Project::empty();
    p.name = "Groove house en La mineur".into();
    p.bpm = 124.0;

    let drum = |t: f32| InstrumentDef::new("drumsynth").with(DRUM_TYPE, t);
    let kick = p.add_channel("Kick", drum(KICK), 60);
    let clap = p.add_channel("Clap", drum(CLAP), 60);
    let hat = p.add_channel("Hat", drum(CLOSED_HAT), 60);
    let open = p.add_channel("Open Hat", drum(OPEN_HAT), 60);
    let bass = p.add_channel("Bass", InstrumentDef::new("pulse"), 45);
    p.channels[clap].gain = 0.65;
    p.channels[hat].pan = -0.3;
    p.channels[hat].gain = 0.7;
    p.channels[open].pan = 0.3;
    p.channels[open].gain = 0.55;

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
                Note { start: step * TICKS_PER_STEP, len: len * TICKS_PER_STEP * 4 / 5, key, vel: 0.9 },
            );
        }
    };

    // Pattern 1 : le groove complet.
    p.patterns[0].name = "Groove".into();
    hits(&mut p, 0, kick, &[0, 4, 8, 12], 1.0);
    hits(&mut p, 0, clap, &[4, 12], 0.9);
    hits(&mut p, 0, hat, &[1, 3, 5, 7, 9, 11, 13, 15], 0.5);
    hits(&mut p, 0, open, &[2, 6, 10, 14], 0.7);
    bassline(&mut p, 0, &[(0, 33, 1), (3, 45, 1), (6, 33, 1), (8, 36, 1), (10, 43, 1), (11, 45, 1), (14, 40, 2)]);

    // Pattern 2 : intro sans basse.
    let intro = p.add_pattern();
    p.patterns[intro].name = "Intro".into();
    hits(&mut p, intro, kick, &[0, 4, 8, 12], 1.0);
    hits(&mut p, intro, hat, &[2, 6, 10, 14], 0.45);

    // Pattern 3 : break, basse seule.
    let brk = p.add_pattern();
    p.patterns[brk].name = "Break".into();
    bassline(&mut p, brk, &[(0, 33, 3), (6, 36, 2), (10, 40, 2), (14, 43, 2)]);
    hits(&mut p, brk, clap, &[12], 0.6);

    let bar = TICKS_PER_BAR;
    let clip = |pattern, lane, start_bar: u32, bars: u32| ClipDef {
        pattern,
        lane,
        start: start_bar * bar,
        len: bars * bar,
        offset: 0,
    };
    p.clips = vec![clip(intro, 0, 0, 4), clip(0, 0, 4, 8), clip(brk, 1, 12, 4), clip(0, 0, 16, 8)];
    p.mode = PlayMode::Pattern(0);
    p
}
