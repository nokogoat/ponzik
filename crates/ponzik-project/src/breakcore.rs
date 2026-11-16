//! Démo breakcore : break façon amen haché, roulements qui montent, ligne acide,
//! sub, stabs FM, nappe. 180 BPM, uniquement des instruments natifs.

use ponzik_engine::{AutoScope, AutoTarget, Note, PlayMode, TICKS_PER_BAR};

use ponzik_i18n::tr;

use crate::{ClipDef, EffectDef, InstrumentDef, Project};

const T32: u32 = 12;
const T16: u32 = 24;

fn fx(kind: &str, params: &[(u32, f32)]) -> Option<EffectDef> {
    let mut e = EffectDef::new(kind);
    for &(id, v) in params {
        e.set(id, v);
    }
    Some(e)
}

fn inst(kind: &str, params: &[(u32, f32)]) -> InstrumentDef {
    let mut d = InstrumentDef::new(kind);
    for &(id, v) in params {
        d.set(id, v);
    }
    d
}

/// Pose des frappes : (pas de double-croche, vélocité) ; la note règle l'accord de la percussion.
fn hits(p: &mut Project, pat: usize, ch: usize, bar: u32, steps: &[(u32, f32)], key: u8) {
    for &(s, vel) in steps {
        p.add_note(pat, ch, Note { start: bar * TICKS_PER_BAR + s * T16, len: T16, key, vel });
    }
}

/// Roulement en triples croches de `from` à `to` (en pas), la hauteur montant de `rise` demi-tons.
#[allow(clippy::too_many_arguments)]
fn roll(p: &mut Project, pat: usize, ch: usize, bar: u32, from: u32, to: u32, base_key: u8, rise: u8) {
    let n = (to - from) * 2;
    for i in 0..n {
        let key = base_key + (rise as u32 * i / n.max(1)) as u8;
        let vel = 0.45 + 0.5 * i as f32 / n as f32;
        p.add_note(pat, ch, Note { start: bar * TICKS_PER_BAR + from * T16 + i * T32, len: T32, key, vel });
    }
}

pub fn demo_breakcore() -> Project {
    let mut p = Project::empty();
    p.name = "Amen rave".into();
    p.bpm = 180.0;
    p.swing = 0.0;

    // Percussions DrumSynth : 0 Type, 1 Accord, 2 Longueur, 3 Brillance, 4 Volume.
    // Kick : 1 Hauteur de départ, 2 Chute, 4 Déclin, 5 Clic, 8 Saturation, 10 Volume.
    let kick = p.add_channel(tr("Kick"), inst("kick", &[(1, 36.0), (2, 0.04), (4, 0.45), (5, 0.6), (8, 0.35), (10, -2.0)]), 60);
    let snare = p.add_channel(tr("Snare"), inst("drumsynth", &[(0, 1.0), (1, 2.0), (2, 0.8), (3, 0.75)]), 60);
    let ghost = p.add_channel(tr("Ghost"), inst("drumsynth", &[(0, 1.0), (1, 6.0), (2, 0.4), (3, 0.9), (4, -5.0)]), 60);
    let hat = p.add_channel(tr("Hat"), inst("drumsynth", &[(0, 3.0), (2, 0.6), (3, 0.8), (4, -3.0)]), 60);
    let ride = p.add_channel(tr("Ride"), inst("drumsynth", &[(0, 4.0), (1, 5.0), (2, 0.5), (4, -9.0)]), 60);
    // Acid 303 : 1 Coupure, 2 Résonance, 3 Modulation, 4 Déclin, 5 Accent, 6 Glissé, 7 Distorsion, 9 Volume.
    let acid = p.add_channel(
        tr("Acid"),
        inst("bass303", &[(1, 260.0), (2, 0.86), (3, 0.8), (4, 0.22), (5, 0.9), (6, 0.05), (7, 0.55), (9, -9.0)]),
        41,
    );
    // Pulse en sinus : sub.
    let sub = p.add_channel(tr("Sub"), inst("pulse", &[(0, 0.0), (2, 180.0), (6, 0.003), (9, 0.08), (10, -7.0)]), 29);
    // FM : stab court, opérateur 2 plus riche.
    let stab = p.add_channel(
        tr("Stab"),
        inst("fm", &[(4, 0.25), (5, 0.0), (12, 0.55), (10, 3.0), (14, 0.18), (46, -15.0)]),
        53,
    );
    // Wave : nappe large, attaque lente.
    let pad = p.add_channel(tr("Pad"), inst("wavetable", &[(1, 0.55), (2, 5.0), (3, 0.35), (20, 0.6), (23, 1.5), (41, -20.0)]), 53);

    // Mixer : les percussions passent par un bus batterie.
    let drum_bus = 12;
    p.tracks[drum_bus].name = tr("Drums").into();
    for ch in [kick, snare, ghost, hat, ride] {
        let t = p.channels[ch].track;
        p.tracks[t].output = drum_bus;
    }
    p.tracks[drum_bus].inserts[0] = fx("saturator", &[(0, 1.0), (1, 10.0), (4, 0.6)]);
    p.tracks[drum_bus].inserts[1] = fx("bitcrusher", &[(0, 10.0), (1, 2.0), (2, 0.3)]);
    p.tracks[drum_bus].inserts[2] = fx("compressor", &[(0, -20.0), (1, 5.0), (4, 0.002), (5, 0.08), (6, 5.0)]);
    // Multibande : 0 Profondeur, 8 Sortie.
    p.tracks[drum_bus].inserts[3] = fx("ott", &[(0, 0.35), (8, -2.0)]);
    let pad_t = p.channels[pad].track;
    // Pompe : 0 Cadence (1/4), 2 Profondeur.
    p.tracks[pad_t].inserts[0] = fx("pompe", &[(0, 0.0), (2, 0.8)]);
    let acid_t = p.channels[acid].track;
    p.tracks[acid_t].inserts[0] = fx("saturator", &[(0, 2.0), (1, 12.0), (4, 0.8)]);
    p.tracks[acid_t].inserts[1] = fx("delay", &[(2, 0.45), (3, 1.0), (6, 0.22)]);
    let stab_t = p.channels[stab].track;
    p.tracks[stab_t].inserts[0] = fx("delay", &[(3, 1.0), (2, 0.5), (6, 0.35)]);
    let reverb_bus = 10;
    p.tracks[reverb_bus].name = tr("Reverb").into();
    // Shimmer : 0 Taille, 1 Déclin, 2 Shimmer, 5 Mix.
    p.tracks[reverb_bus].inserts[0] = fx("shimmer", &[(0, 0.8), (1, 3.5), (2, 0.35), (5, 1.0)]);
    p.tracks[reverb_bus].gain = 0.6;
    for (ch, amount) in [(snare, 0.18), (ghost, 0.12), (stab, 0.45), (pad, 0.35)] {
        let t = p.channels[ch].track;
        p.tracks[t].sends[0] = (reverb_bus, amount);
    }
    p.tracks[0].inserts[0] = fx("compressor", &[(0, -12.0), (1, 2.0), (4, 0.01), (6, 1.5)]);
    p.tracks[0].inserts[1] = fx("limiter", &[(0, 3.0), (1, -0.3)]);
    p.tracks[0].gain = 0.55;

    // Pattern 1 : le break façon amen, deux mesures.
    let amen = 0;
    p.patterns[amen].name = "Amen".into();
    p.patterns[amen].len = 2 * TICKS_PER_BAR;
    let hat8: Vec<(u32, f32)> = (0..8).map(|i| (i * 2, if i % 2 == 0 { 0.7 } else { 0.5 })).collect();
    for bar in 0..2 {
        hits(&mut p, amen, kick, bar, &[(0, 1.0), (2, 0.85), (10, 0.95), (11, 0.8)], 60);
        hits(&mut p, amen, snare, bar, &[(4, 1.0), (12, 1.0)], 60);
        hits(&mut p, amen, ghost, bar, &[(7, 0.5), (9, 0.6), (15, 0.55)], 60);
        hits(&mut p, amen, hat, bar, &hat8, 60);
    }
    hits(&mut p, amen, ride, 1, &[(0, 0.6), (8, 0.5)], 60);
    roll(&mut p, amen, ghost, 1, 13, 16, 60, 5);

    // Pattern 2 : haché, kicks en contretemps, roulement qui monte d'une octave.
    let chop = p.add_pattern();
    p.patterns[chop].name = tr("Chop").into();
    p.patterns[chop].len = 2 * TICKS_PER_BAR;
    hits(&mut p, chop, kick, 0, &[(0, 1.0), (3, 0.9), (6, 0.9), (8, 1.0), (11, 0.85), (14, 0.9)], 60);
    hits(&mut p, chop, snare, 0, &[(4, 1.0), (12, 1.0), (13, 0.7)], 60);
    roll(&mut p, chop, ghost, 0, 8, 12, 58, 7);
    hits(&mut p, chop, kick, 1, &[(0, 1.0), (2, 0.8), (5, 0.9), (10, 0.95)], 60);
    hits(&mut p, chop, snare, 1, &[(4, 1.0), (7, 0.8)], 60);
    roll(&mut p, chop, snare, 1, 8, 16, 58, 12);
    for i in 0..32 {
        let vel = 0.3 + 0.4 * ((i % 4) as f32 / 3.0);
        p.add_note(chop, hat, Note { start: i * T32 * 2, len: T32, key: 60, vel });
    }

    // Pattern 3 : ligne acide en Fa mineur, glissés et accents, sub et stabs.
    let acid_p = p.add_pattern();
    p.patterns[acid_p].name = tr("Acid").into();
    p.patterns[acid_p].len = 2 * TICKS_PER_BAR;
    let line: [(i32, f32, bool); 16] = [
        (0, 0.95, false),
        (0, 0.6, false),
        (12, 0.9, true),
        (0, 0.6, false),
        (3, 0.7, false),
        (0, 0.6, true),
        (15, 0.95, false),
        (0, 0.6, false),
        (7, 0.9, false),
        (0, 0.6, true),
        (10, 0.7, false),
        (12, 0.95, true),
        (0, 0.6, false),
        (3, 0.7, false),
        (5, 0.9, true),
        (7, 0.6, false),
    ];
    for bar in 0..2u32 {
        for (i, &(st, vel, slide)) in line.iter().enumerate() {
            let shift = if bar == 1 && i >= 8 { -2 } else { 0 };
            let len = if slide { T16 + T32 } else { T16 * 3 / 4 };
            let key = (41 + st + shift) as u8;
            p.add_note(acid_p, acid, Note { start: bar * TICKS_PER_BAR + i as u32 * T16, len, key, vel });
        }
        p.add_note(acid_p, sub, Note { start: bar * TICKS_PER_BAR, len: TICKS_PER_BAR / 2, key: 29, vel: 0.9 });
        p.add_note(acid_p, sub, Note { start: bar * TICKS_PER_BAR + TICKS_PER_BAR / 2, len: TICKS_PER_BAR / 2, key: if bar == 0 { 32 } else { 27 }, vel: 0.9 });
        for step in [2u32, 7, 10, 14] {
            for key in [53u8, 56, 60, 63] {
                p.add_note(acid_p, stab, Note { start: bar * TICKS_PER_BAR + step * T16, len: T16, key, vel: 0.75 });
            }
        }
    }

    // Pattern 4 : respiration, nappe et stabs.
    let breath = p.add_pattern();
    p.patterns[breath].name = tr("Breath").into();
    p.patterns[breath].len = 2 * TICKS_PER_BAR;
    for (bar, chord) in [(0u32, [53u8, 56, 60]), (1, [51, 55, 58])] {
        for key in chord {
            p.add_note(breath, pad, Note { start: bar * TICKS_PER_BAR, len: TICKS_PER_BAR, key, vel: 0.7 });
        }
        p.add_note(breath, sub, Note { start: bar * TICKS_PER_BAR, len: TICKS_PER_BAR, key: chord[0] - 24, vel: 0.7 });
    }
    hits(&mut p, breath, kick, 0, &[(0, 0.9)], 60);
    hits(&mut p, breath, stab, 1, &[(14, 0.6)], 60);

    // Pattern 5 : drill, une mesure de triples croches qui accélère vers la reprise.
    let drill = p.add_pattern();
    p.patterns[drill].name = tr("Drill").into();
    roll(&mut p, drill, snare, 0, 0, 16, 55, 17);
    roll(&mut p, drill, kick, 0, 8, 16, 58, 6);

    let bar = TICKS_PER_BAR;
    let clip = |pattern, lane, start: u32, bars: u32| ClipDef { pattern, lane, start: start * bar, len: bars * bar, offset: 0 };
    p.clips = vec![
        clip(breath, 2, 0, 4),
        clip(acid_p, 1, 2, 2),
        clip(amen, 0, 4, 8),
        clip(acid_p, 1, 4, 8),
        clip(chop, 0, 12, 2),
        clip(drill, 0, 14, 1),
        clip(drill, 0, 15, 1),
        clip(amen, 0, 16, 4),
        clip(chop, 0, 20, 2),
        clip(amen, 0, 22, 2),
        clip(acid_p, 1, 16, 8),
        clip(breath, 2, 24, 4),
        clip(chop, 0, 28, 4),
        clip(acid_p, 1, 28, 8),
        clip(amen, 0, 32, 4),
        clip(drill, 0, 36, 1),
    ];
    // Automation : la coupure de l'acide s'ouvre et se referme sur le morceau.
    let cutoff = AutoTarget::InstrumentParam { channel: acid, id: 1 };
    for (bar_at, hz) in [(2u32, 180.0f32), (12, 1_400.0), (16, 320.0), (24, 900.0), (28, 2_600.0), (36, 260.0)] {
        p.add_automation_point(cutoff, AutoScope::Song, bar_at * bar, hz);
    }

    // Vue Session : batterie, acide et respiration à lancer en direct.
    for (row, pat) in [amen, chop, drill, amen].into_iter().enumerate() {
        p.session_grid[0][row] = Some(pat);
    }
    p.session_grid[1][0] = Some(acid_p);
    p.session_grid[1][1] = Some(acid_p);
    p.session_grid[2][2] = Some(breath);
    p.session_grid[2][3] = Some(breath);

    p.mode = PlayMode::Song;
    p.current_pattern = amen;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RenderMode, render_song};

    #[test]
    fn breakcore_renders_loud_but_clean() {
        let p = demo_breakcore();
        assert!(p.routing_order().is_some());
        let none = |_: &str| None;
        let audio = render_song(&p, 48_000, &none, RenderMode::Pattern(0, 2));
        let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.2 && peak <= 1.0, "{peak}");
    }
}
