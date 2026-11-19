//! Listening material and CPU measurement for the Grand Piano.
//!
//! cargo test --release -p ponzik-engine --test piano_demo -- --ignored --nocapture
//!
//! `demo` writes /tmp/piano_demo.wav (scales, chords at every dynamic, a
//! pedaled passage, then variants of the instrument); `cpu` plays dense
//! pedaled chords and reports the share of one core.

use std::time::Instant;

use ponzik_engine::instruments::Piano;
use ponzik_engine::instruments::piano::{P_FELT, P_HARDNESS, P_LID, P_MATERIAL, P_SIZE, P_UNISON};
use ponzik_engine::{Controller, Instrument, Params, wav};

const SR: f32 = 48_000.0;

enum Ev {
    On(u8, f32),
    Off(u8),
    Pedal(f32),
    Soft(f32),
}

/// Events at times in beats.
struct Score {
    events: Vec<(f64, Ev)>,
    bpm: f64,
}

impl Score {
    fn new(bpm: f64) -> Self {
        Self { events: Vec::new(), bpm }
    }

    fn note(&mut self, at: f64, len: f64, key: u8, vel: f32) {
        self.events.push((at, Ev::On(key, vel)));
        self.events.push((at + len, Ev::Off(key)));
    }

    fn chord(&mut self, at: f64, len: f64, keys: &[u8], vel: f32) {
        for (i, &k) in keys.iter().enumerate() {
            // Slightly rolled, as played.
            self.note(at + i as f64 * 0.012, len, k, vel);
        }
    }

    /// Renders onto the end of `out` (interleaved stereo), plus `tail` beats.
    fn render(mut self, piano: &mut Piano, tail: f64, out: &mut Vec<f32>) {
        self.events.sort_by(|a, b| a.0.total_cmp(&b.0));
        let spb = 60.0 / self.bpm * SR as f64;
        let end = self.events.last().map_or(0.0, |e| e.0) + tail;
        let total = (end * spb) as usize;
        let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
        let mut pos = 0;
        for (at, ev) in self.events {
            let frame = ((at.max(0.0) * spb) as usize).min(total);
            if frame > pos {
                piano.render(&mut l[pos..frame], &mut r[pos..frame]);
                pos = frame;
            }
            match ev {
                Ev::On(k, v) => piano.note_on(k, v),
                Ev::Off(k) => piano.note_off(k),
                Ev::Pedal(v) => piano.controller(Controller::Cc(64, v)),
                Ev::Soft(v) => piano.controller(Controller::Cc(67, v)),
            }
        }
        piano.render(&mut l[pos..], &mut r[pos..]);
        for (a, b) in l.iter().zip(&r) {
            out.push(*a);
            out.push(*b);
        }
    }
}

const C_MAJOR: [u8; 8] = [0, 2, 4, 5, 7, 9, 11, 12];

fn scales(s: &mut Score, at: f64) -> f64 {
    let mut t = at;
    for (oct, vel) in [(36u8, 0.55f32), (60, 0.65), (84, 0.6)] {
        for (i, &d) in C_MAJOR.iter().chain(C_MAJOR.iter().rev().skip(1)).enumerate() {
            s.note(t + i as f64 * 0.5, 0.45, oct + d, vel);
        }
        t += 8.0;
    }
    t
}

fn dynamics(s: &mut Score, at: f64) -> f64 {
    let mut t = at;
    for vel in [0.2f32, 0.4, 0.6, 0.8, 1.0] {
        s.chord(t, 1.6, &[48, 55, 60, 64, 67], vel);
        t += 2.0;
    }
    t + 1.0
}

/// A pedaled progression, the pedal changed on each bar; the last bar under
/// the soft pedal, then a wide chord left to ring.
fn pedaled(s: &mut Score, at: f64) -> f64 {
    let bars: [(&[u8], &[u8]); 4] = [
        (&[36, 48], &[60, 64, 67, 72]),
        (&[33, 45], &[57, 60, 64, 69]),
        (&[29, 41], &[57, 60, 65, 69]),
        (&[31, 43], &[59, 62, 67, 71]),
    ];
    let melody: [u8; 16] = [76, 79, 84, 83, 81, 76, 72, 74, 77, 76, 72, 69, 71, 74, 79, 77];
    let mut t = at;
    for (i, (bass, chord)) in bars.iter().enumerate() {
        s.events.push((t - 0.05, Ev::Pedal(0.0)));
        s.events.push((t + 0.1, Ev::Pedal(1.0)));
        if i == 3 {
            s.events.push((t, Ev::Soft(1.0)));
        }
        s.chord(t, 0.5, bass, 0.75);
        for b in 0..4 {
            s.chord(t + b as f64, 0.4, chord, 0.45);
            s.note(t + b as f64, 0.9, melody[i * 4 + b], 0.7);
        }
        t += 4.0;
    }
    s.events.push((t - 0.05, Ev::Pedal(0.0)));
    s.events.push((t, Ev::Soft(0.0)));
    s.events.push((t + 0.1, Ev::Pedal(1.0)));
    s.chord(t, 4.0, &[24, 36, 48, 55, 60, 64, 67, 72, 76, 84], 0.7);
    s.events.push((t + 7.0, Ev::Pedal(0.0)));
    t + 8.0
}

#[test]
#[ignore]
fn demo() {
    let mut out = Vec::new();
    let mut piano = Piano::new();
    piano.set_sample_rate(SR);
    let mut s = Score::new(100.0);
    let t = scales(&mut s, 0.0);
    let t = dynamics(&mut s, t + 1.0);
    pedaled(&mut s, t + 1.0);
    s.render(&mut piano, 2.0, &mut out);

    // Variants, on the same little phrase.
    let variants: [(&str, &[(u32, f32)]); 5] = [
        ("Bright", &[(P_HARDNESS, 0.85), (P_LID, 1.0)]),
        ("Upright", &[(P_SIZE, 0.0), (P_LID, 0.4)]),
        ("Felt", &[(P_FELT, 0.9)]),
        ("Honky-tonk", &[(P_UNISON, 18.0), (P_HARDNESS, 0.7)]),
        ("Celesta", &[(P_MATERIAL, 2.0)]),
    ];
    for (name, params) in variants {
        let mut piano = Piano::new();
        piano.set_sample_rate(SR);
        for &(id, v) in params {
            piano.set_param(id, v);
        }
        let mut s = Score::new(110.0);
        for (i, &k) in [60u8, 64, 67, 72, 71, 67, 64, 62].iter().enumerate() {
            s.note(i as f64 * 0.5, 0.45, k + 12, 0.7);
        }
        s.chord(0.0, 3.5, &[48, 55, 64], 0.5);
        s.chord(4.0, 3.0, &[53, 57, 60, 65], 0.6);
        s.render(&mut piano, 2.0, &mut out);
        println!("variant {name} rendered");
    }
    let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(out.iter().all(|x| x.is_finite()) && peak < 1.0, "peak {peak}");
    wav::write_wav_i16("/tmp/piano_demo.wav", SR as u32, 2, &out).expect("writing the demo");
    println!("/tmp/piano_demo.wav: {:.1} s, peak {:.2} dBFS", out.len() as f32 / 2.0 / SR, 20.0 * peak.log10());
}

#[test]
#[ignore]
fn cpu() {
    let _ftz = ponzik_engine::FlushDenormals::new();
    let mut piano = Piano::new();
    piano.set_sample_rate(SR);
    piano.controller(Controller::Cc(64, 1.0));
    let mut s = Score::new(120.0);
    // Ten-note chords across the keyboard, one per beat, pedal down all
    // along: up to 64 voices plus the 88 sympathetic strings.
    for beat in 0..32 {
        let root = 24 + (beat * 5 % 36) as u8;
        let keys: Vec<u8> = [0u8, 7, 12, 16, 19, 24, 28, 31, 36, 43].iter().map(|k| root + k).collect();
        s.chord(beat as f64, 0.8, &keys, 0.5 + (beat % 4) as f32 * 0.12);
    }
    let start = Instant::now();
    let mut out = Vec::new();
    s.render(&mut piano, 4.0, &mut out);
    let elapsed = start.elapsed().as_secs_f64();
    let audio = out.len() as f64 / 2.0 / SR as f64;
    println!("dense pedaled chords: {audio:.1} s of audio in {elapsed:.2} s: {:.1} % of one core", 100.0 * elapsed / audio);
    assert!(elapsed < audio * 0.5, "too slow for real time: {elapsed} s for {audio} s");
}
