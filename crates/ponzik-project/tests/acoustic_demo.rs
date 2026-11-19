//! Listening demo of the modeled acoustic instruments, rendered with their
//! factory presets to `/tmp/acoustic_demo.wav`, with the CPU cost of each part.
//!
//! cargo test --release -p ponzik-project --test acoustic_demo -- --ignored --nocapture

use std::time::Instant;

use ponzik_engine::{Controller, NoteExpression, registry};
use ponzik_project::presets::factory_preset;

const SR: f32 = 48_000.0;
const BLOCK: usize = 128;

#[derive(Clone, Copy)]
enum Ev {
    On(u8, f32),
    Off(u8),
    Slide(u8, f32, f32),
    Cc(u8, f32),
}

/// A part: one instrument with a preset, its timed events, a length.
struct Part {
    kind: &'static str,
    preset: &'static str,
    events: Vec<(f32, Ev)>,
    secs: f32,
    reverb: f32,
}

impl Part {
    fn new(kind: &'static str, preset: &'static str, secs: f32, reverb: f32) -> Self {
        Self { kind, preset, events: Vec::new(), secs, reverb }
    }

    fn note(&mut self, t: f32, note: u8, vel: f32, dur: f32) -> &mut Self {
        self.events.push((t, Ev::On(note, vel)));
        self.events.push((t + dur, Ev::Off(note)));
        self
    }

    fn chord(&mut self, t: f32, notes: &[u8], vel: f32, dur: f32) -> &mut Self {
        for &n in notes {
            self.note(t, n, vel, dur);
        }
        self
    }
}

/// Renders a part; returns the stereo interleaved audio and the render time.
fn render(part: &Part) -> (Vec<f32>, f64) {
    let mut inst = registry::create_instrument(part.kind).expect(part.kind);
    inst.set_sample_rate(SR);
    for (id, v) in factory_preset(part.kind, part.preset).unwrap_or_else(|| panic!("{}/{}", part.kind, part.preset)) {
        inst.set_param(id, v);
    }
    let mut events = part.events.clone();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let frames = (part.secs * SR) as usize;
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut next = 0;
    let start = Instant::now();
    let mut pos = 0;
    while pos < frames {
        let n = BLOCK.min(frames - pos);
        let t = pos as f32 / SR;
        while next < events.len() && events[next].0 <= t {
            match events[next].1 {
                Ev::On(k, v) => inst.note_on(k, v),
                Ev::Off(k) => inst.note_off(k),
                Ev::Slide(k, st, secs) => inst.note_expression(k, NoteExpression::Tuning { semitones: st, secs }),
                Ev::Cc(cc, v) => inst.controller(Controller::Cc(cc, v)),
            }
            next += 1;
        }
        inst.render(&mut l[pos..pos + n], &mut r[pos..pos + n]);
        pos += n;
    }
    let cpu = start.elapsed().as_secs_f64();
    if part.reverb > 0.0 {
        let mut fx = registry::create_effect("reverb").expect("reverb");
        fx.set_sample_rate(SR);
        let mix = fx.params().iter().find(|p| p.name == "Mix").map(|p| p.id);
        if let Some(id) = mix {
            fx.set_param(id, part.reverb);
        }
        for (a, b) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
            fx.process(a, b);
        }
    }
    (l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect(), cpu)
}

fn guitar_parts() -> Vec<Part> {
    let beat = 60.0 / 92.0;
    let mut parts = Vec::new();

    // Acoustic strumming: G C D Em, down-up eighths.
    let mut p = Part::new("guitar", "Acoustic Strum", 8.0 * 4.0 * beat * 0.5 + 2.0, 0.25);
    let chords: [&[u8]; 4] = [&[43, 47, 50, 55, 59, 67], &[48, 52, 55, 60, 64], &[50, 57, 62, 66], &[40, 47, 52, 55, 59, 64]];
    let pattern = [(0.0, 0.9), (1.0, 0.6), (1.5, 0.5), (2.5, 0.7), (3.0, 0.55), (3.5, 0.5)];
    for (bar, c) in chords.iter().enumerate() {
        for &(b, v) in &pattern {
            let t = (bar as f32 * 4.0 + b) * beat * 0.5;
            p.chord(t, c, v, 0.45 * beat);
        }
    }
    parts.push(p);

    // Nylon fingerstyle arpeggios: Am F C G.
    let mut p = Part::new("guitar", "Nylon Fingerstyle", 16.0 * beat * 0.5 * 2.0 + 2.5, 0.3);
    let arps: [[u8; 4]; 4] = [[45, 57, 60, 64], [41, 57, 60, 65], [48, 55, 60, 64], [43, 55, 59, 62]];
    for (bar, a) in arps.iter().enumerate() {
        for (k, &i) in [0usize, 1, 2, 3, 2, 1, 3, 2].iter().enumerate() {
            let t = (bar as f32 * 8.0 + k as f32) * beat * 0.5;
            p.note(t, a[i], if k == 0 { 0.8 } else { 0.55 }, beat * 1.5);
        }
    }
    parts.push(p);

    // Lofi / trap plucks.
    let mut p = Part::new("guitar", "Trap Guitar Pluck", 6.0, 0.35);
    for (k, &n) in [64u8, 67, 71, 69, 67, 64, 62, 64, 60, 62, 64, 67].iter().enumerate() {
        p.note(k as f32 * beat * 0.5, n, 0.7, beat * 0.4);
    }
    parts.push(p);

    // Indie clean arpeggio with the amp.
    let mut p = Part::new("guitar", "Indie Clean", 7.0, 0.3);
    for (k, &n) in [52u8, 59, 64, 66, 71, 66, 64, 59, 50, 57, 62, 66, 69, 66, 62, 57].iter().enumerate() {
        p.note(k as f32 * beat * 0.5 * 0.75, n, 0.7, beat);
    }
    parts.push(p);

    // Crunch rhythm: power chords and palm-muted chugs come from the presets.
    let mut p = Part::new("guitar", "Crunch Rhythm", 6.0, 0.15);
    for (k, root) in [40u8, 43, 45, 38].iter().enumerate() {
        let t = k as f32 * 2.0 * beat;
        p.chord(t, &[*root, root + 7, root + 12], 0.9, 1.6 * beat);
    }
    parts.push(p);
    let mut p = Part::new("guitar", "Palm Mute Chug", 4.0, 0.1);
    for k in 0..12 {
        let t = k as f32 * beat * 0.25;
        p.chord(t, &[40, 47], if k % 4 == 0 { 0.95 } else { 0.7 }, beat * 0.2);
    }
    p.chord(12.0 * beat * 0.25, &[43, 50, 55], 0.95, beat * 2.0);
    parts.push(p);

    // Lead: legato hammer-ons and pull-offs, then a slide up and vibrato-free hold.
    let mut p = Part::new("guitar", "Lead Drive", 7.0, 0.3);
    let line = [(64u8, 0.0, 0.5), (67, 0.4, 0.3), (69, 0.6, 0.6), (67, 1.1, 0.3), (64, 1.3, 0.8), (62, 2.0, 0.5)];
    for &(n, t, d) in &line {
        p.note(t * beat * 2.0, n, 0.85, d * beat * 2.0 + 0.05);
    }
    p.note(5.0 * beat, 69, 0.9, 3.0 * beat);
    p.events.push((5.6 * beat, Ev::Slide(69, 2.0, 0.25)));
    parts.push(p);

    // Bass: finger line, then slap.
    let mut p = Part::new("guitar", "Finger Bass", 6.0, 0.05);
    p.events.push((0.0, Ev::Cc(11, 1.0)));
    for (k, &n) in [33u8, 33, 40, 33, 31, 31, 38, 31, 29, 29, 36, 29, 31, 33, 35, 36].iter().enumerate() {
        p.note(k as f32 * beat * 0.5, n, 0.8, beat * 0.45);
    }
    parts.push(p);
    let mut p = Part::new("guitar", "Slap Bass", 5.0, 0.05);
    for (k, &(n, v)) in [(28u8, 1.0), (40, 0.8), (28, 0.6), (38, 0.8), (28, 1.0), (40, 0.9), (31, 0.7), (33, 0.9)].iter().enumerate() {
        p.note(k as f32 * beat * 0.5, n, v, beat * 0.3);
    }
    parts.push(p);
    parts
}

fn string_parts() -> Vec<Part> {
    let mut parts = Vec::new();
    // Ensemble chords with a mod wheel swell on each: Cmaj7 Am7 Fmaj7 G.
    let mut p = Part::new("strings", "Warm Strings Ensemble", 17.0, 0.35);
    let chords: [&[u8]; 4] = [&[36, 48, 55, 64, 71], &[33, 45, 55, 60, 67], &[29, 41, 57, 64, 69], &[31, 43, 55, 62, 71]];
    for (k, c) in chords.iter().enumerate() {
        let t = k as f32 * 3.8;
        p.chord(t, c, 0.7, 3.7);
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            let w = 0.35 + 0.55 * (std::f32::consts::PI * x).sin();
            p.events.push((t + x * 3.7, Ev::Cc(1, w)));
        }
    }
    parts.push(p);
    // Solo cello, legato (overlapping notes) with expression.
    let mut p = Part::new("strings", "Solo Cello Legato", 12.0, 0.3);
    p.events.push((0.0, Ev::Cc(1, 0.55)));
    let line = [(48u8, 0.0, 1.6), (55, 1.5, 0.9), (53, 2.3, 0.7), (52, 2.9, 0.7), (50, 3.5, 1.2), (48, 4.6, 0.7), (47, 5.2, 0.7), (48, 5.8, 2.6), (43, 8.3, 2.8)];
    for &(n, t, d) in &line {
        p.note(t, n, 0.7, d + 0.12);
    }
    for i in 0..=40 {
        let x = i as f32 / 40.0;
        p.events.push((x * 11.0, Ev::Cc(1, 0.4 + 0.45 * (std::f32::consts::PI * 2.0 * x).sin().abs())));
    }
    parts.push(p);
    // Pizzicato section: a walking figure.
    let mut p = Part::new("strings", "Pizzicato Section", 5.5, 0.3);
    for (k, &n) in [48u8, 55, 60, 64, 62, 59, 55, 50, 53, 57, 60, 65, 64, 60, 55, 48].iter().enumerate() {
        p.note(k as f32 * 0.3, n, 0.6 + 0.3 * ((k % 4 == 0) as u8 as f32), 0.2);
    }
    parts.push(p);
    // Tremolo, cinematic.
    let mut p = Part::new("strings", "Tremolo Strings", 5.0, 0.35);
    p.events.push((0.0, Ev::Cc(1, 0.4)));
    p.chord(0.0, &[40, 52, 59, 64], 0.8, 4.0);
    for i in 0..=20 {
        p.events.push((i as f32 * 0.2, Ev::Cc(1, 0.3 + 0.035 * i as f32)));
    }
    parts.push(p);
    parts
}

#[test]
#[ignore]
fn render_acoustic_demo() {
    let mut parts = guitar_parts();
    parts.extend(string_parts());
    let mut out: Vec<f32> = Vec::new();
    println!("{:<28} {:>8} {:>10} {:>8}", "part", "audio s", "cpu ms", "x RT");
    for p in &parts {
        let (audio, cpu) = render(p);
        let rt = p.secs as f64 / cpu.max(1e-9);
        println!("{:<28} {:>8.1} {:>10.1} {:>8.0}", format!("{}/{}", p.kind, p.preset), p.secs, cpu * 1e3, rt);
        out.extend(audio);
        out.extend(std::iter::repeat_n(0.0, (0.4 * SR) as usize * 2));
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(out.iter().all(|s| s.is_finite()) && peak > 0.05, "peak {peak}");
    let g = if peak > 0.9 { 0.9 / peak } else { 1.0 };
    out.iter_mut().for_each(|s| *s *= g);
    ponzik_engine::wav::write_wav_i16("/tmp/acoustic_demo.wav", SR as u32, 2, &out).expect("write");
    println!("wrote /tmp/acoustic_demo.wav ({:.1} s, peak {peak:.2})", out.len() as f32 / 2.0 / SR);
}
