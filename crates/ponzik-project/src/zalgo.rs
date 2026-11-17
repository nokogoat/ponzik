//! « Zalgo Rave » demo: experimental rap × electro in E minor at 160 BPM.
//! Halftime rage drops (distorted kick, gliding saturated 808, detuned
//! supersaw lead, square arps, FM bell), a raw 4/4 techno switch with an acid
//! 303, stutters and tape stops, risers and impacts from the sound library.
//! Native instruments and effects only; every note is original.

use ponzik_engine::{AutoScope, AutoTarget, Note, PlayMode, TICKS_PER_BAR};

use ponzik_i18n::tr;

use crate::{AudioClipDef, BUS_COLORS, ClipDef, EffectDef, InstrumentDef, Project, ScaleKind, TrackKind};

/// Ticks of a sixteenth, a thirty-second and a sixteenth triplet.
const T16: u32 = 24;
const T32: u32 = 12;
const T16T: u32 = 16;
const BAR: u32 = TICKS_PER_BAR;
const BPM: f32 = 160.0;

// Song map, in bars.
const DROP1: u32 = 8;
const SWITCH: u32 = 24;
const BUILD: u32 = 40;
const DROP2: u32 = 44;
const OUTRO: u32 = 60;
const END: u32 = 68;

// Library sounds (paths relative to the library root, rendered in memory if
// the files are not on disk yet).
const RISER_NOISE: &str = "FX/Risers/Riser - Noise 8s.wav";
const RISER_SAW: &str = "FX/Risers/Riser - Supersaw 8s.wav";
const IMPACT_BOOM: &str = "FX/Impacts/Impact - Boom.wav";
const IMPACT_HIT: &str = "FX/Impacts/Impact - Cinematic Hit.wav";
const SUB_DROP: &str = "FX/Impacts/Impact - Sub Drop.wav";
const REVERSE_CRASH: &str = "FX/Sweeps/Sweep - Reverse Crash.wav";

fn fx(kind: &str, params: &[(u32, f32)]) -> Option<EffectDef> {
    let mut e = EffectDef::new(kind);
    for &(id, v) in params {
        e.set(id, v);
    }
    Some(e)
}

fn keyed(kind: &str, params: &[(u32, f32)], key: usize) -> Option<EffectDef> {
    let mut e = fx(kind, params)?;
    e.sidechain = Some(key);
    Some(e)
}

fn inst(kind: &str, params: &[(u32, f32)]) -> InstrumentDef {
    let mut d = InstrumentDef::new(kind);
    for &(id, v) in params {
        d.set(id, v);
    }
    d
}

/// Seconds → ticks at the demo tempo.
fn secs(s: f32) -> u32 {
    (s * BPM / 60.0 * (BAR / 4) as f32).round() as u32
}

fn note(p: &mut Project, pat: usize, ch: usize, start: u32, len: u32, key: u8, vel: f32) {
    p.add_note(pat, ch, Note { start, len, key, vel });
}

/// Percussion hits on the sixteenth grid of `bar`: (step, velocity).
fn hits(p: &mut Project, pat: usize, ch: usize, bar: u32, steps: &[(u32, f32)]) {
    for &(s, vel) in steps {
        note(p, pat, ch, bar * BAR + s * T16, T16, 60, vel);
    }
}

/// A roll of `count` hits `every` ticks apart from `start`, swelling from `v0` to `v1`.
#[allow(clippy::too_many_arguments)]
fn roll(p: &mut Project, pat: usize, ch: usize, start: u32, every: u32, count: u32, v0: f32, v1: f32) {
    for i in 0..count {
        let x = i as f32 / (count.max(2) - 1) as f32;
        note(p, pat, ch, start + i * every, every, 60, v0 + (v1 - v0) * x);
    }
}

/// Four-bar progression i – VI – III – VII in E minor: Em, Cmaj7, G, D.
const CHORDS: [[u8; 4]; 4] = [[52, 55, 59, 62], [48, 52, 55, 59], [55, 59, 62, 66], [50, 54, 57, 62]];

pub fn demo_zalgo() -> Project {
    let mut p = Project::empty();
    p.name = "Zalgo Rave".into();
    p.bpm = BPM;
    p.key = 4;
    p.scale = ScaleKind::Minor;

    // --- Instruments ---
    // Kick: 0 Tune, 1 Start Pitch, 2 Drop, 4 Decay, 5 Click, 6 Click Tone, 7 Body, 8 Saturation, 9 Tone, 10 Volume.
    // Tuned to G1 (49 Hz), under the 808.
    let kick = p.add_channel(
        tr("Kick"),
        inst("kick", &[(0, -2.0), (1, 40.0), (2, 0.03), (4, 0.42), (5, 0.55), (6, 5_000.0), (7, 0.35), (8, 0.55), (9, 9_000.0), (10, -4.0)]),
        60,
    );
    // DrumSynth: 0 Type, 1 Tune, 2 Length, 3 Brightness, 4 Volume.
    let clap = p.add_channel(tr("Clap"), inst("drumsynth", &[(0, 2.0), (1, 1.0), (2, 0.9), (3, 0.7), (4, -3.0)]), 60);
    let snare = p.add_channel(tr("Snare"), inst("drumsynth", &[(0, 1.0), (1, 3.0), (2, 0.7), (3, 0.85), (4, -4.0)]), 60);
    let hat = p.add_channel(tr("Hat"), inst("drumsynth", &[(0, 3.0), (1, 2.0), (2, 0.45), (3, 0.9), (4, -5.5)]), 60);
    let open = p.add_channel(tr("Open Hat"), inst("drumsynth", &[(0, 4.0), (1, 1.0), (2, 0.6), (3, 0.8), (4, -10.0)]), 60);
    // Wave as an 808: sine, one voice, portamento for the glides.
    // 0 Table, 1 Position, 2 Voices, 10 Cutoff, 12 Envelope, 20–23 ADSR, 40 Portamento, 41 Volume.
    let bass808 = p.add_channel(
        tr("808"),
        inst(
            "wavetable",
            &[(0, 0.0), (1, 0.0), (2, 0.0), (10, 20_000.0), (12, 0.0), (20, 0.002), (21, 1.6), (22, 0.55), (23, 0.12), (40, 0.07), (41, -8.0)],
        ),
        40,
    );
    // Acid 303: 0 Wave, 1 Cutoff, 2 Resonance, 3 Modulation, 4 Decay, 5 Accent, 6 Slide, 7 Distortion, 9 Volume.
    let acid = p.add_channel(
        tr("Acid"),
        inst("bass303", &[(0, 0.0), (1, 300.0), (2, 0.82), (3, 0.75), (4, 0.2), (5, 0.9), (6, 0.06), (7, 0.6), (9, -12.0)]),
        40,
    );
    // Supersaw lead: saw frame, 7 voices wide and detuned, a short glide between notes.
    let lead = p.add_channel(
        tr("Lead"),
        inst(
            "wavetable",
            &[
                (0, 0.0),
                (1, 0.5),
                (2, 6.0),
                (3, 28.0),
                (4, 0.85),
                (10, 5_500.0),
                (11, 1.2),
                (12, 18.0),
                (20, 0.004),
                (21, 0.5),
                (22, 0.75),
                (23, 0.18),
                (31, 0.35),
                (32, 0.4),
                (40, 0.045),
                (41, -15.5),
            ],
        ),
        64,
    );
    // Square plucks for the arps: Pulse, 0 Wave, 2 Cutoff, 3 Resonance, 4 Envelope, 5 Filter Decay, 6–9 ADSR, 10 Volume.
    let arp = p.add_channel(
        tr("Arp"),
        inst("pulse", &[(0, 2.0), (1, 9.0), (2, 600.0), (3, 2.0), (4, 5_000.0), (5, 0.09), (6, 0.001), (7, 0.16), (8, 0.0), (9, 0.08), (10, -16.0)]),
        72,
    );
    // FM bell: operator 2 at ratio 3.5 (coarse 3, fine 0.5), long decay without sustain.
    let bell = p.add_channel(
        tr("Bell"),
        inst(
            "fm",
            &[(4, 2.2), (5, 0.0), (6, 1.2), (10, 3.0), (11, 0.5), (12, 0.5), (14, 1.0), (15, 0.0), (46, -18.0)],
        ),
        72,
    );
    // Dark pad: harmonic table, slow attack, low-passed.
    let pad = p.add_channel(
        tr("Pad"),
        inst("wavetable", &[(0, 1.0), (1, 0.6), (2, 3.0), (3, 14.0), (10, 1_800.0), (12, 6.0), (20, 0.8), (22, 0.8), (23, 1.8), (41, -22.0)]),
        52,
    );
    p.channels[hat].pan = 0.18;
    p.channels[open].pan = -0.22;
    p.channels[arp].pan = -0.15;
    p.channels[bell].pan = 0.2;
    let track = |p: &Project, ch: usize| p.channels[ch].track;
    let (kick_t, bass_t, acid_t, lead_t, arp_t, bell_t, pad_t) =
        (track(&p, kick), track(&p, bass808), track(&p, acid), track(&p, lead), track(&p, arp), track(&p, bell), track(&p, pad));

    // --- Mixer ---
    let bus = |p: &mut Project, t: usize, name: &str, color: usize| {
        p.tracks[t].name = name.into();
        p.tracks[t].kind = TrackKind::Bus;
        p.tracks[t].color = BUS_COLORS[color];
    };
    let (drum_bus, synth_bus, reverb_bus, delay_bus, fx_t) = (16, 17, 18, 19, 20);
    bus(&mut p, drum_bus, tr("Drums"), 0);
    bus(&mut p, synth_bus, tr("Synths"), 1);
    bus(&mut p, reverb_bus, tr("Reverb"), 2);
    bus(&mut p, delay_bus, tr("Delay"), 3);
    p.tracks[fx_t].name = tr("FX").into();
    for ch in [kick, clap, snare, hat, open] {
        let t = track(&p, ch);
        p.tracks[t].output = drum_bus;
    }
    for t in [lead_t, arp_t, bell_t, pad_t] {
        p.tracks[t].output = synth_bus;
    }

    // Kick: crunchy tape saturation, then the tail tamed.
    // Character saturation: 0 Character, 1 Drive, 8 Tone, 9 Output, 10 Mix.
    p.tracks[kick_t].inserts[0] = fx("char_saturation", &[(0, 1.0), (1, 15.0), (8, -2.0), (9, -7.0), (10, 1.0)]);
    // Transient: 0 Attack, 1 Sustain.
    p.tracks[kick_t].inserts[1] = fx("transient", &[(0, 0.4), (1, -0.25)]);
    // Drum bus: glue, grit, then the stutter (Tranche, off until automated).
    // Bus comp: 0 Character, 1 Threshold, 2 Ratio, 3 Attack, 4 Release, 7 Makeup.
    p.tracks[drum_bus].inserts[0] = fx("bus_comp", &[(0, 2.0), (1, -14.0), (2, 4.0), (3, 0.01), (4, 0.12), (7, 2.0)]);
    p.tracks[drum_bus].inserts[1] = fx("char_saturation", &[(0, 1.0), (1, 6.0), (9, -3.0), (10, 0.6)]);
    // Tranche: 0 Mode (Repeat), 1 Step (1/16), 3 Cycle (1/2), 5 Mix.
    p.tracks[drum_bus].inserts[2] = fx("tranche", &[(0, 1.0), (1, 1.0), (3, 1.0), (5, 0.0)]);

    // 808: tape multiband drive (lows kept cleaner, mids screaming), ducked under the kick, mono.
    p.tracks[bass_t].inserts[0] = fx(
        "char_saturation",
        &[(0, 1.0), (1, 18.0), (2, 1.0), (3, 120.0), (5, -6.0), (6, 4.0), (7, 0.0), (9, -9.0), (10, 1.0)],
    );
    // EQ: 0 Low Cut, 1 freq, 16 High Cut, 17 freq.
    p.tracks[bass_t].inserts[1] = fx("eq", &[(0, 1.0), (1, 28.0), (16, 1.0), (17, 6_000.0)]);
    // Ducker: 0 Threshold, 1 Amount, 2 Attack, 3 Hold, 4 Release.
    p.tracks[bass_t].inserts[2] = keyed("ducker", &[(0, -30.0), (1, 0.75), (2, 0.001), (3, 0.03), (4, 0.11)], kick_t);
    // Imager: 5 Mono Bass, 6 below.
    p.tracks[bass_t].inserts[3] = fx("imager", &[(5, 1.0), (6, 200.0)]);

    // Acid: saturated, filtered echoes on a send.
    p.tracks[acid_t].inserts[0] = fx("saturator", &[(0, 2.0), (1, 10.0), (2, -2.0), (3, -6.0), (4, 0.8)]);
    p.tracks[acid_t].inserts[1] = fx("eq", &[(0, 1.0), (1, 70.0)]);
    p.tracks[acid_t].inserts[2] = keyed("ducker", &[(0, -30.0), (1, 0.5), (2, 0.001), (3, 0.02), (4, 0.1)], kick_t);

    // Lead: driven, squashed by OTT, a chopper for the gated parts (off until automated).
    p.tracks[lead_t].inserts[0] = fx("char_saturation", &[(0, 2.0), (1, 9.0), (8, 1.5), (9, -5.0), (10, 0.7)]);
    // OTT: 0 Depth, 8 Output.
    p.tracks[lead_t].inserts[1] = fx("ott", &[(0, 0.45), (8, -3.0)]);
    p.tracks[lead_t].inserts[2] = fx("eq", &[(0, 1.0), (1, 220.0), (2, 1.0)]);
    // Tranche: Chopper, 1/16, Syncopated, one bar.
    p.tracks[lead_t].inserts[3] = fx("tranche", &[(0, 0.0), (1, 1.0), (2, 2.0), (3, 2.0), (5, 0.0)]);
    // Arps: bitcrushed digital grit.
    p.tracks[arp_t].inserts[0] = fx("bitcrusher", &[(0, 7.0), (1, 3.0), (2, 0.35)]);
    p.tracks[arp_t].inserts[1] = fx("eq", &[(0, 1.0), (1, 300.0), (2, 1.0)]);
    // Bell: low end out.
    p.tracks[bell_t].inserts[0] = fx("eq", &[(0, 1.0), (1, 400.0)]);
    // Pad: low end out, chorus, ducked hard under the kick.
    p.tracks[pad_t].inserts[0] = fx("eq", &[(0, 1.0), (1, 180.0), (2, 1.0)]);
    p.tracks[pad_t].inserts[1] = fx("chorus", &[(1, 0.4), (2, 0.6), (4, 1.0), (5, 0.5)]);
    p.tracks[pad_t].inserts[2] = keyed("ducker", &[(0, -30.0), (1, 0.7), (2, 0.002), (3, 0.04), (4, 0.2)], kick_t);

    // Synth bus: the filter that opens the intro and closes the outro, a bit of OTT.
    // Auto filter: 0 Type, 1 Cutoff, 2 Resonance.
    p.tracks[synth_bus].inserts[0] = fx("autofilter", &[(0, 0.0), (1, 20_000.0), (2, 1.1)]);
    p.tracks[synth_bus].inserts[1] = fx("ott", &[(0, 0.2), (8, -1.0)]);
    p.tracks[synth_bus].inserts[2] = keyed("ducker", &[(0, -30.0), (1, 0.25), (2, 0.002), (3, 0.02), (4, 0.15)], kick_t);

    // Returns: a dark plate and a dotted-eighth tape delay.
    // Vintage verb: 0 Character (Plate), 1 Pre-delay, 2 Size, 4 Decay, 5 Damping, 6 Low Cut, 11 Mix.
    p.tracks[reverb_bus].inserts[0] = fx("vintage_verb", &[(0, 1.0), (1, 0.02), (2, 0.7), (4, 2.6), (5, 7_000.0), (6, 300.0), (11, 1.0)]);
    // Char delay: 0 Tape, 1 Sync (1/8 D), 3 Feedback, 4 Ping-pong, 6 Low Cut, 7 High Cut, 11 Mix.
    p.tracks[delay_bus].inserts[0] =
        fx("char_delay", &[(0, 0.0), (1, 9.0), (3, 0.4), (4, 1.0), (6, 350.0), (7, 6_000.0), (11, 1.0)]);
    p.tracks[reverb_bus].gain = 0.55;
    p.tracks[delay_bus].gain = 0.5;
    let sends: [(usize, f32, f32); 7] = [
        (track(&p, clap), 0.18, 0.0),
        (track(&p, snare), 0.12, 0.0),
        (lead_t, 0.2, 0.22),
        (arp_t, 0.15, 0.3),
        (bell_t, 0.45, 0.35),
        (pad_t, 0.4, 0.0),
        (acid_t, 0.0, 0.18),
    ];
    for (t, verb, echo) in sends {
        p.tracks[t].sends[0] = (reverb_bus, verb);
        p.tracks[t].sends[1] = (delay_bus, echo);
    }
    p.tracks[fx_t].inserts[0] = fx("eq", &[(0, 1.0), (1, 40.0)]);
    p.tracks[fx_t].gain = 0.6;

    // Master: tape stop (off until automated), tone, glue, true-peak limiter at −1 dBTP.
    // Tranche: 0 Mode (Tape Stop), 3 Cycle (1 bar).
    p.tracks[0].inserts[0] = fx("tranche", &[(0, 2.0), (3, 2.0), (5, 0.0)]);
    // EQ: low cut 25 Hz, low shelf, high shelf for air.
    p.tracks[0].inserts[1] = fx("eq", &[(0, 1.0), (1, 25.0), (2, 1.0), (3, 90.0), (4, 0.5), (14, 9_000.0), (15, 1.0)]);
    p.tracks[0].inserts[2] = fx("bus_comp", &[(0, 0.0), (1, -10.0), (2, 2.0), (3, 0.03), (4, 0.2), (5, 1.0), (6, 90.0), (7, 1.0)]);
    // Master limiter: 0 Algorithm (Punchy), 1 Input, 2 Ceiling, 3 Release, 5 True Peak.
    p.tracks[0].inserts[3] = fx("master_limiter", &[(0, 1.0), (1, 6.0), (2, -1.0), (3, 0.06), (5, 1.0)]);
    p.tracks[0].gain = 1.0;

    // --- Patterns ---
    // The empty project's first pattern becomes the first one here.
    let mut fresh = true;
    let mut new_pattern = |p: &mut Project, name: &str, bars: u32| {
        let i = if std::mem::take(&mut fresh) { 0 } else { p.add_pattern() };
        p.patterns[i].name = name.into();
        p.patterns[i].len = bars * BAR;
        i
    };

    // Halftime rage drums: snare and clap on beat 3, bouncing kicks, hats with triplet rolls.
    let rage = new_pattern(&mut p, tr("Rage"), 4);
    let kicks: [&[(u32, f32)]; 4] = [
        &[(0, 1.0), (7, 0.85), (10, 0.95)],
        &[(0, 1.0), (3, 0.8), (6, 0.9), (14, 0.85)],
        &[(0, 1.0), (7, 0.85), (10, 0.95), (11, 0.7)],
        &[(0, 1.0), (5, 0.85), (12, 0.9), (14, 0.8)],
    ];
    for (bar, k) in kicks.iter().enumerate() {
        let bar = bar as u32;
        hits(&mut p, rage, kick, bar, k);
        hits(&mut p, rage, clap, bar, &[(8, 1.0)]);
        hits(&mut p, rage, snare, bar, &[(8, 0.8)]);
        // Eighth hats, with a sixteenth-triplet roll at the end of every bar
        // and a thirty-second burst on the last.
        let hat_steps: Vec<(u32, f32)> = (0..8).filter(|i| !(bar % 2 == 1 && *i >= 6)).map(|i| (i * 2, if i % 2 == 0 { 0.8 } else { 0.55 })).collect();
        hits(&mut p, rage, hat, bar, &hat_steps);
        if bar % 2 == 1 {
            roll(&mut p, rage, hat, bar * BAR + 12 * T16, T16T, 6, 0.45, 0.85);
        } else {
            hits(&mut p, rage, hat, bar, &[(13, 0.5), (15, 0.5)]);
        }
    }
    roll(&mut p, rage, hat, 3 * BAR + 14 * T16, T32, 4, 0.5, 0.9);
    hits(&mut p, rage, snare, 3, &[(14, 0.45), (15, 0.6)]);
    hits(&mut p, rage, open, 1, &[(4, 0.6)]);
    hits(&mut p, rage, open, 3, &[(4, 0.6)]);

    // 808: follows the kick, glides between roots, octave jumps at the turnarounds.
    let bass_p = new_pattern(&mut p, tr("808"), 4);
    let line: [&[(u32, u32, u8)]; 4] = [
        &[(0, 7, 28), (7, 3, 28), (10, 6, 40)],
        &[(0, 3, 36), (3, 3, 36), (6, 8, 36), (14, 2, 43)],
        &[(0, 7, 31), (7, 3, 31), (10, 4, 43), (14, 2, 38)],
        &[(0, 5, 38), (5, 7, 38), (12, 2, 35), (14, 2, 26)],
    ];
    for (bar, notes) in line.iter().enumerate() {
        for &(step, len, key) in notes.iter() {
            note(&mut p, bass_p, bass808, bar as u32 * BAR + step * T16, len * T16 - 4, key, 0.95);
        }
    }

    // Rage lead: a stabbing call-and-answer riff, the glide bending every note into the next.
    let lead_p = new_pattern(&mut p, tr("Lead"), 4);
    let riff: [(u32, u32, u8); 14] = [
        (0, 2, 71),
        (3, 2, 76),
        (6, 2, 74),
        (8, 2, 71),
        (10, 1, 67),
        (11, 3, 69),
        (14, 2, 71),
        (16, 2, 72),
        (19, 2, 71),
        (22, 2, 67),
        (24, 2, 64),
        (26, 1, 66),
        (27, 3, 67),
        (30, 2, 74),
    ];
    for half in 0..2u32 {
        for (i, &(step, len, key)) in riff.iter().enumerate() {
            // The second time round, the answer climbs instead of falling.
            let key = if half == 1 && i >= 9 { key + [0, 3, 5, 5, 4][i - 9] } else { key };
            note(&mut p, lead_p, lead, half * 2 * BAR + step * T16, len * T16 - 2, key, if step % 4 == 0 { 0.95 } else { 0.8 });
        }
    }

    // Arps: up-and-down sixteenths over each chord, two octaves.
    let arp_p = new_pattern(&mut p, tr("Arp"), 4);
    for (bar, chord) in CHORDS.iter().enumerate() {
        let tones: Vec<u8> = chord.iter().map(|k| k + 12).chain(chord.iter().map(|k| k + 24)).collect();
        let order = [0usize, 1, 2, 3, 4, 5, 6, 7, 6, 5, 4, 3, 2, 1, 2, 3];
        for (s, &i) in order.iter().enumerate() {
            let vel = if s % 4 == 0 { 0.9 } else { 0.6 };
            note(&mut p, arp_p, arp, bar as u32 * BAR + s as u32 * T16, T16, tones[i], vel);
        }
    }

    // Pad: the progression held, one chord per bar.
    let pad_p = new_pattern(&mut p, tr("Pad"), 4);
    for (bar, chord) in CHORDS.iter().enumerate() {
        for &k in chord {
            note(&mut p, pad_p, pad, bar as u32 * BAR, BAR, k, 0.75);
        }
    }

    // Bell motif: sparse, high, ringing.
    let bell_p = new_pattern(&mut p, tr("Bell"), 4);
    let motif: [(u32, u32, u8); 7] = [(0, 6, 83), (6, 4, 79), (10, 6, 76), (16, 8, 84), (24, 8, 79), (32, 6, 86), (38, 26, 81)];
    for &(step, len, key) in &motif {
        note(&mut p, bell_p, bell, step * T16, len * T16, key, 0.8);
    }

    // Raw techno: four on the floor, offbeat open hats, sixteenth hats, claps on 2 and 4.
    let techno = new_pattern(&mut p, tr("Techno"), 4);
    for bar in 0..4u32 {
        hits(&mut p, techno, kick, bar, &[(0, 1.0), (4, 0.95), (8, 1.0), (12, 0.95)]);
        hits(&mut p, techno, clap, bar, &[(4, 0.85), (12, 0.9)]);
        hits(&mut p, techno, open, bar, &[(2, 0.7), (6, 0.65), (10, 0.7), (14, 0.65)]);
        let sixteenths: Vec<(u32, f32)> = (0..16).map(|s| (s, [0.7, 0.35, 0.5, 0.35][s as usize % 4])).collect();
        hits(&mut p, techno, hat, bar, &sixteenths);
    }
    hits(&mut p, techno, snare, 3, &[(13, 0.5), (14, 0.65), (15, 0.8)]);

    // Acid: sixteenths on E with octave jumps, slides and accents.
    let acid_p = new_pattern(&mut p, tr("Acid"), 4);
    let acid_line: [(i32, f32, bool); 16] = [
        (0, 0.95, false),
        (0, 0.5, false),
        (12, 0.8, true),
        (0, 0.5, false),
        (3, 0.95, false),
        (0, 0.5, true),
        (10, 0.7, false),
        (12, 0.95, true),
        (0, 0.5, false),
        (7, 0.8, false),
        (0, 0.5, true),
        (15, 0.95, false),
        (12, 0.6, true),
        (5, 0.7, false),
        (0, 0.95, false),
        (-2, 0.6, true),
    ];
    for bar in 0..4u32 {
        let shift = [0, 0, -4, -2][bar as usize];
        for (i, &(st, vel, slide)) in acid_line.iter().enumerate() {
            let len = if slide { T16 + T32 } else { T16 * 2 / 3 };
            note(&mut p, acid_p, acid, bar * BAR + i as u32 * T16, len, (40 + st + shift) as u8, vel);
        }
    }

    // Build: snare from quarters to thirty-seconds, kick on quarters, pitched up.
    let build = new_pattern(&mut p, tr("Build"), 4);
    for (bar, every) in [(0u32, 4 * T16), (1, 2 * T16), (2, T16), (3, T32)] {
        let count = BAR / every;
        for i in 0..count {
            let x = (bar * BAR + i * every) as f32 / (4 * BAR) as f32;
            let key = 60 + (x * 10.0) as u8;
            note(&mut p, build, snare, bar * BAR + i * every, every, key, 0.15 + 0.3 * x);
        }
        if bar < 3 {
            hits(&mut p, build, kick, bar, &[(0, 0.9), (8, 0.8)]);
        }
    }

    // Fill: one bar of triplet hats and a snare drill, the kick dropping out.
    let fill = new_pattern(&mut p, tr("Fill"), 1);
    hits(&mut p, fill, kick, 0, &[(0, 1.0), (3, 0.8)]);
    hits(&mut p, fill, clap, 0, &[(8, 1.0)]);
    roll(&mut p, fill, hat, 0, T16T, 24, 0.4, 0.8);
    roll(&mut p, fill, snare, 12 * T16, T32, 8, 0.4, 0.95);

    // Intro hats: sparse, filtered by velocity, waiting for the drop.
    let intro = new_pattern(&mut p, tr("Intro"), 4);
    for bar in 0..4u32 {
        hits(&mut p, intro, hat, bar, &[(2, 0.4), (6, 0.45), (10, 0.4), (14, 0.5)]);
        if bar >= 2 {
            hits(&mut p, intro, hat, bar, &[(0, 0.3), (8, 0.35)]);
        }
    }
    roll(&mut p, intro, hat, 3 * BAR + 8 * T16, T16T, 12, 0.35, 0.8);
    hits(&mut p, intro, clap, 3, &[(12, 0.5)]);

    // --- Playlist ---
    let clip = |pattern: usize, lane: usize, start: u32, bars: u32| ClipDef { pattern, lane, start: start * BAR, len: bars * BAR, offset: 0 };
    let (l_drums, l_bass, l_acid, l_lead, l_arp, l_bell, l_pad) = (0, 1, 2, 3, 4, 5, 6);
    let mut clips = vec![
        // Intro.
        clip(pad_p, l_pad, 0, 4),
        clip(pad_p, l_pad, 4, 4),
        clip(bell_p, l_bell, 0, 4),
        clip(bell_p, l_bell, 4, 4),
        clip(intro, l_drums, 4, 4),
        // Drop 1: halftime rage.
        clip(rage, l_drums, DROP1, 4),
        clip(rage, l_drums, DROP1 + 4, 4),
        clip(rage, l_drums, DROP1 + 8, 4),
        clip(rage, l_drums, DROP1 + 12, 3),
        clip(fill, l_drums, SWITCH - 1, 1),
        clip(arp_p, l_arp, DROP1 + 8, 4),
        clip(arp_p, l_arp, DROP1 + 12, 4),
        clip(pad_p, l_pad, DROP1 + 8, 4),
        clip(pad_p, l_pad, DROP1 + 12, 4),
        // Switch: raw techno and acid.
        clip(techno, l_drums, SWITCH, 4),
        clip(techno, l_drums, SWITCH + 4, 4),
        clip(techno, l_drums, SWITCH + 8, 4),
        clip(techno, l_drums, SWITCH + 12, 4),
        clip(acid_p, l_acid, SWITCH, 4),
        clip(acid_p, l_acid, SWITCH + 4, 4),
        clip(acid_p, l_acid, SWITCH + 8, 4),
        clip(acid_p, l_acid, SWITCH + 12, 4),
        clip(pad_p, l_pad, SWITCH + 8, 4),
        clip(pad_p, l_pad, SWITCH + 12, 4),
        clip(arp_p, l_arp, SWITCH + 12, 4),
        // Build.
        clip(build, l_drums, BUILD, 4),
        clip(pad_p, l_pad, BUILD, 4),
        clip(bell_p, l_bell, BUILD, 4),
        // Drop 2: everything.
        clip(rage, l_drums, DROP2, 4),
        clip(rage, l_drums, DROP2 + 4, 4),
        clip(rage, l_drums, DROP2 + 8, 4),
        clip(rage, l_drums, DROP2 + 12, 3),
        clip(fill, l_drums, OUTRO - 1, 1),
        clip(acid_p, l_acid, DROP2 + 8, 4),
        clip(acid_p, l_acid, DROP2 + 12, 4),
        clip(arp_p, l_arp, DROP2, 4),
        clip(arp_p, l_arp, DROP2 + 4, 4),
        clip(arp_p, l_arp, DROP2 + 8, 4),
        clip(arp_p, l_arp, DROP2 + 12, 4),
        clip(bell_p, l_bell, DROP2 + 8, 4),
        clip(bell_p, l_bell, DROP2 + 12, 4),
        clip(pad_p, l_pad, DROP2, 4),
        clip(pad_p, l_pad, DROP2 + 4, 4),
        clip(pad_p, l_pad, DROP2 + 8, 4),
        clip(pad_p, l_pad, DROP2 + 12, 4),
        // Outro.
        clip(pad_p, l_pad, OUTRO, 4),
        clip(pad_p, l_pad, OUTRO + 4, 4),
        clip(bell_p, l_bell, OUTRO, 4),
        clip(bell_p, l_bell, OUTRO + 4, 4),
    ];
    for start in [DROP1, DROP1 + 4, DROP1 + 8, DROP1 + 12, DROP2, DROP2 + 4, DROP2 + 8, DROP2 + 12] {
        clips.push(clip(bass_p, l_bass, start, 4));
        clips.push(clip(lead_p, l_lead, start, 4));
    }
    // The 808 holds a last low E under the outro.
    let tail = new_pattern(&mut p, tr("808 tail"), 2);
    note(&mut p, tail, bass808, 0, 2 * BAR - T16, 28, 0.85);
    clips.push(clip(tail, l_bass, OUTRO, 2));
    p.clips = clips;

    // Risers and impacts from the library, on the FX track.
    let fx_lane = 7;
    let audio = |path: &str, start: u32, len: u32, gain: f32| AudioClipDef {
        path: path.into(),
        lane: fx_lane,
        track: fx_t,
        start,
        len,
        offset: 0.0,
        gain,
        fade_in: 0.0,
        fade_out: 0.02,
        take: 0,
        muted: false,
    };
    let eight = secs(8.0);
    p.audio_clips = vec![
        audio(RISER_NOISE, DROP1 * BAR - eight, eight, 0.8),
        audio(IMPACT_BOOM, DROP1 * BAR, 3 * BAR, 0.8),
        audio(REVERSE_CRASH, SWITCH * BAR - secs(3.0), secs(3.0), 0.6),
        audio(IMPACT_HIT, SWITCH * BAR, 3 * BAR, 0.7),
        audio(RISER_SAW, DROP2 * BAR - eight, eight, 0.45),
        audio(IMPACT_BOOM, DROP2 * BAR, 3 * BAR, 0.9),
        audio(SUB_DROP, OUTRO * BAR, 3 * BAR, 0.7),
    ];

    // --- Automation ---
    let song = AutoScope::Song;
    // Synth bus filter: opens over the intro and the build, closes in the outro.
    let cutoff = AutoTarget::EffectParam { track: synth_bus, slot: 0, id: 1 };
    for (bar, hz) in [(0, 350.0), (DROP1 - 1, 4_000.0), (DROP1, 20_000.0), (BUILD, 900.0), (DROP2, 20_000.0), (OUTRO, 20_000.0), (END, 300.0)] {
        p.add_automation_point(cutoff, song, bar * BAR, hz);
    }
    // Acid cutoff: slowly opens over the switch, screams in drop 2.
    let acid_cut = AutoTarget::InstrumentParam { channel: acid, id: 1 };
    for (bar, hz) in [(SWITCH, 220.0), (SWITCH + 8, 900.0), (SWITCH + 15, 2_400.0), (SWITCH + 16, 300.0), (DROP2 + 8, 500.0), (OUTRO, 3_000.0)] {
        p.add_automation_point(acid_cut, song, bar * BAR, hz);
    }
    // On/off gates: (target, [(from tick, to tick)]).
    let gate = |p: &mut Project, target: AutoTarget, spans: &[(u32, u32)]| {
        p.add_automation_point(target, song, 0, 0.0);
        for &(a, b) in spans {
            p.add_automation_point(target, song, a - 1, 0.0);
            p.add_automation_point(target, song, a, 1.0);
            p.add_automation_point(target, song, b - 1, 1.0);
            p.add_automation_point(target, song, b, 0.0);
        }
    };
    // Drum stutter on the second half of the bars before each switch.
    let stutter = AutoTarget::EffectParam { track: drum_bus, slot: 2, id: 5 };
    gate(&mut p, stutter, &[((DROP1 + 7) * BAR + BAR / 2, (DROP1 + 8) * BAR), ((SWITCH + 7) * BAR + BAR / 2, (SWITCH + 8) * BAR), ((DROP2 + 7) * BAR + BAR / 2, (DROP2 + 8) * BAR)]);
    // Lead chopper in the second half of drop 2.
    let chop = AutoTarget::EffectParam { track: lead_t, slot: 3, id: 5 };
    gate(&mut p, chop, &[((DROP2 + 8) * BAR, (DROP2 + 12) * BAR)]);
    // Tape stop on the master over the last bar of the switch.
    let tape = AutoTarget::EffectParam { track: 0, slot: 0, id: 5 };
    gate(&mut p, tape, &[((BUILD - 1) * BAR, BUILD * BAR)]);

    // --- Session ---
    for (scene, pat) in [intro, rage, techno, build, fill].into_iter().enumerate() {
        p.session_grid[0][scene] = Some(pat);
    }
    p.session_grid[1][1] = Some(bass_p);
    p.session_grid[1][4] = Some(tail);
    p.session_grid[2][2] = Some(acid_p);
    p.session_grid[3][1] = Some(lead_p);
    p.session_grid[4][1] = Some(arp_p);
    p.session_grid[4][2] = Some(arp_p);
    p.session_grid[5][0] = Some(bell_p);
    p.session_grid[5][3] = Some(bell_p);
    for scene in 0..4 {
        p.session_grid[6][scene] = Some(pad_p);
    }

    p.mode = PlayMode::Song;
    p.current_pattern = rage;
    p
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ponzik_dsp::filter::{FilterMode, Svf};
    use ponzik_dsp::loudness::LoudnessMeter;
    use ponzik_engine::Sample;

    use super::*;
    use crate::library::library_sample;
    use crate::{RenderMode, render_song};

    fn library(path: &str) -> Option<Arc<Sample>> {
        library_sample(path).map(Arc::new)
    }

    #[test]
    fn zalgo_builds_and_renders_clean() {
        let p = demo_zalgo();
        assert!(p.routing_order().is_some());
        assert_eq!(p.song_len(), END * BAR);
        for c in &p.audio_clips {
            assert!(library_sample(&c.path).is_some(), "{}", c.path);
        }
        // The first bars of the first drop: drums, 808 and lead together.
        let mut drop = p.clone();
        drop.clips.retain(|c| c.start == DROP1 * BAR);
        drop.audio_clips.clear();
        drop.automation.clear();
        let audio = render_song(&drop, 48_000, &|_| None, RenderMode::Song);
        assert!(audio.iter().all(|s| s.is_finite()));
        let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.3 && peak < 0.95, "{peak}");
    }

    /// Full mix report: `cargo test --release -p ponzik-project zalgo_mix_report -- --ignored --nocapture`
    /// (writes the song to `$ZALGO_WAV` if set; `ZALGO_MUTE=3,6` mutes mixer tracks).
    #[test]
    #[ignore]
    fn zalgo_mix_report() {
        const SR: u32 = 48_000;
        let mut p = demo_zalgo();
        for t in std::env::var("ZALGO_MUTE").unwrap_or_default().split(',').filter_map(|t| t.parse::<usize>().ok()) {
            p.tracks[t].muted = true;
        }
        let started = std::time::Instant::now();
        let audio = render_song(&p, SR, &library, RenderMode::Song);
        println!("rendered in {:.1} s", started.elapsed().as_secs_f32());
        if let Ok(path) = std::env::var("ZALGO_WAV") {
            crate::write_wav(&path, &audio, SR, crate::BitDepth::Pcm24).unwrap();
        }
        let (l, r): (Vec<f32>, Vec<f32>) = audio.chunks(2).map(|f| (f[0], f[1])).unzip();
        let mut meter = LoudnessMeter::new(SR as f32, 2);
        meter.process_stereo(&l, &r);
        let dc = audio.iter().map(|&s| s as f64).sum::<f64>() / audio.len() as f64;
        println!(
            "{:.1} s · {:.2} LUFS · TP {:.2} dBTP · peak {:.2} dBFS · LRA {:.1} · DC {:.2e}",
            l.len() as f32 / SR as f32,
            meter.integrated(),
            meter.true_peak(),
            meter.sample_peak(),
            meter.loudness_range(),
            dc
        );
        let bar_frames = 60.0 / BPM * 4.0 * SR as f32;
        let sections = [
            ("intro", 0, DROP1),
            ("drop 1", DROP1, SWITCH),
            ("switch", SWITCH, BUILD),
            ("build", BUILD, DROP2),
            ("drop 2", DROP2, OUTRO),
            ("outro", OUTRO, END),
        ];
        let bands: [(&str, f32, f32); 5] =
            [("sub", 20.0, 60.0), ("low", 60.0, 250.0), ("mid", 250.0, 2_000.0), ("high", 2_000.0, 8_000.0), ("air", 8_000.0, 20_000.0)];
        for (name, a, b) in sections {
            let (fa, fb) = ((a as f32 * bar_frames) as usize, ((b as f32 * bar_frames) as usize).min(l.len()));
            let mono: Vec<f32> = (fa..fb).map(|i| 0.5 * (l[i] + r[i])).collect();
            let side: f64 = (fa..fb).map(|i| (0.5 * (l[i] - r[i])) as f64).map(|x| x * x).sum();
            let mid: f64 = mono.iter().map(|&x| (x as f64) * (x as f64)).sum();
            let mut m = LoudnessMeter::new(SR as f32, 2);
            m.process_stereo(&l[fa..fb], &r[fa..fb]);
            let dc = mono.iter().map(|&x| x as f64).sum::<f64>() / mono.len() as f64;
            let mut line = format!("{name:>7}: {:6.2} LUFS, max ST {:6.2}, DC {dc:9.2e}, S/M {:5.1} dB |", m.integrated(), m.max_short_term(), 10.0 * (side / mid).log10());
            for (band, lo, hi) in bands {
                let mut f = [
                    Svf::new(FilterMode::HighPass, lo, 0.707, SR as f32),
                    Svf::new(FilterMode::HighPass, lo, 0.707, SR as f32),
                    Svf::new(FilterMode::LowPass, hi, 0.707, SR as f32),
                    Svf::new(FilterMode::LowPass, hi, 0.707, SR as f32),
                ];
                let e: f64 = mono
                    .iter()
                    .map(|&x| f.iter_mut().fold(x, |y, f| f.process(y)) as f64)
                    .map(|y| y * y)
                    .sum();
                line += &format!(" {band} {:6.1}", 10.0 * (e / mono.len() as f64).log10());
            }
            println!("{line}");
        }
    }
}
