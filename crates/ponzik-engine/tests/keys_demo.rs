//! Listening demo and CPU cost of the modeled keyboards. Ignored by default:
//!
//! cargo test --release -p ponzik-engine --test keys_demo -- --ignored --nocapture
//!
//! writes /tmp/keys_demo.wav (tine piano chords, reed piano riff, organ with
//! the rotary speeding up and slowing down, mallet runs) and prints the
//! share of one core each instrument takes.

use std::path::Path;
use std::time::Instant;

use ponzik_engine::registry::create_instrument;
use ponzik_engine::{Controller, Instrument};

const SR: f32 = 48_000.0;

/// Params of a factory preset file (`"params": [[id, value], …]`).
fn preset(kind: &str, name: &str) -> Vec<(u32, f32)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../assets/presets/{kind}/{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let body = &text[text.find("\"params\"").expect("params")..];
    let nums: Vec<f32> = body
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))
        .filter_map(|s| s.parse().ok())
        .collect();
    nums.chunks(2).map(|p| (p[0] as u32, p[1])).collect()
}

fn instrument(kind: &str, preset_name: &str) -> Box<dyn Instrument> {
    let mut inst = create_instrument(kind).expect(kind);
    inst.set_sample_rate(SR);
    for (id, v) in preset(kind, preset_name) {
        inst.set_param(id, v);
    }
    inst
}

#[derive(Clone, Copy)]
enum Ev {
    On(u8, f32),
    Off(u8),
    Cc(u8, f32),
}

/// Renders `events` (time in seconds) for `secs`, mixed into `out` at `at`.
/// Returns the section's peak and RMS.
fn play(inst: &mut dyn Instrument, events: &[(f32, Ev)], secs: f32, out: &mut Vec<[f32; 2]>, at: f32, gain: f32) -> (f32, f32) {
    let mut evs = events.to_vec();
    evs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let frames = (secs * SR) as usize;
    let start = (at * SR) as usize;
    if out.len() < start + frames {
        out.resize(start + frames, [0.0; 2]);
    }
    let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
    let mut next = 0;
    let mut pos = 0;
    while pos < frames {
        while next < evs.len() && (evs[next].0 * SR) as usize <= pos {
            match evs[next].1 {
                Ev::On(n, v) => inst.note_on(n, v),
                Ev::Off(n) => inst.note_off(n),
                Ev::Cc(c, v) => inst.controller(Controller::Cc(c, v)),
            }
            next += 1;
        }
        let n = 128.min(frames - pos);
        l[..n].fill(0.0);
        r[..n].fill(0.0);
        inst.render(&mut l[..n], &mut r[..n]);
        for i in 0..n {
            out[start + pos + i][0] += l[i] * gain;
            out[start + pos + i][1] += r[i] * gain;
        }
        pos += n;
    }
    let peak = out[start..start + frames].iter().flat_map(|f| f.iter()).fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (out[start..start + frames].iter().map(|f| f[0] * f[0]).sum::<f32>() / frames as f32).sqrt();
    println!("{:5.1} s  {:<8} peak {peak:.2}  rms {rms:.3}", at, inst.kind());
    (peak, rms)
}

fn chord(evs: &mut Vec<(f32, Ev)>, t: f32, len: f32, notes: &[u8], vel: f32, strum: f32) {
    for (i, &n) in notes.iter().enumerate() {
        let v = (vel + 0.06 * ((i * 7 % 5) as f32 - 2.0) / 2.0).clamp(0.05, 1.0);
        evs.push((t + i as f32 * strum, Ev::On(n, v)));
        evs.push((t + len, Ev::Off(n)));
    }
}

fn run(evs: &mut Vec<(f32, Ev)>, t: f32, step: f32, notes: &[u8], vel: f32, len: f32) {
    for (i, &n) in notes.iter().enumerate() {
        let accent = if i % 4 == 0 { 0.15 } else { 0.0 };
        evs.push((t + i as f32 * step, Ev::On(n, (vel + accent).min(1.0))));
        evs.push((t + i as f32 * step + len, Ev::Off(n)));
    }
}

#[test]
#[ignore]
fn render_keys_demo() {
    let mut out: Vec<[f32; 2]> = Vec::new();
    let mut t = 0.0;

    // Tine piano: a soul progression with the pedal, soft then hard (bark).
    let mut ev = Vec::new();
    let prog: [&[u8]; 4] = [&[41, 56, 60, 63, 67], &[46, 56, 61, 65, 68], &[39, 55, 58, 61, 65], &[44, 55, 60, 63, 67]];
    for (i, c) in prog.iter().enumerate() {
        let t0 = i as f32 * 2.0;
        ev.push((t0, Ev::Cc(64, 1.0)));
        chord(&mut ev, t0, 1.8, c, 0.45 + 0.15 * i as f32, 0.012);
        ev.push((t0 + 1.95, Ev::Cc(64, 0.0)));
    }
    play(instrument("rhodes", "Classic Suitcase").as_mut(), &ev, 9.5, &mut out, t, 1.0);
    t += 9.5;
    let mut ev = Vec::new();
    for (i, c) in prog.iter().enumerate() {
        chord(&mut ev, i as f32 * 1.0, 0.8, &c[1..], 0.95, 0.0);
    }
    run(&mut ev, 4.0, 0.15, &[60, 63, 65, 67, 70, 72, 75, 72, 70, 67], 0.85, 0.12);
    play(instrument("rhodes", "Bark Drive").as_mut(), &ev, 7.0, &mut out, t, 1.0);
    t += 7.0;

    // Reed piano: a gritty riff.
    let mut ev = Vec::new();
    for bar in 0..3 {
        let b = bar as f32 * 1.6;
        chord(&mut ev, b, 0.35, &[57, 60, 64], 0.95, 0.0);
        chord(&mut ev, b + 0.4, 0.2, &[57, 60, 64], 0.55, 0.0);
        chord(&mut ev, b + 0.8, 0.6, &[55, 59, 62], 1.0, 0.0);
        run(&mut ev, b + 1.2, 0.1, &[45, 48, 50, 52], 0.8, 0.09);
    }
    play(instrument("wurli", "Wurli Grit").as_mut(), &ev, 6.0, &mut out, t, 1.0);
    t += 6.0;
    let mut ev = Vec::new();
    chord(&mut ev, 0.0, 3.0, &[53, 57, 60, 64], 0.5, 0.02);
    chord(&mut ev, 3.0, 2.5, &[52, 55, 59, 62], 0.45, 0.02);
    play(instrument("wurli", "Tremolo Reed").as_mut(), &ev, 6.5, &mut out, t, 1.0);
    t += 6.5;

    // Organ: gospel chords, the rotary spins up on the mod wheel at 4 s and
    // slows down at 10 s (listen to the horn and drum ramps).
    let mut ev = Vec::new();
    let organ_prog: [&[u8]; 4] = [&[48, 60, 64, 67, 70], &[53, 60, 65, 69, 72], &[55, 62, 65, 67, 71], &[48, 60, 64, 67, 72]];
    for rep in 0..2 {
        for (i, c) in organ_prog.iter().enumerate() {
            chord(&mut ev, (rep * 4 + i) as f32 * 1.6, 1.5, c, 0.9, 0.0);
        }
    }
    ev.push((4.0, Ev::Cc(1, 1.0)));
    ev.push((10.0, Ev::Cc(1, 0.0)));
    play(instrument("organ", "Gospel B3").as_mut(), &ev, 15.0, &mut out, t, 1.0);
    t += 15.0;
    let mut ev = Vec::new();
    run(&mut ev, 0.0, 0.14, &[67, 70, 72, 74, 75, 74, 72, 70, 67, 70, 72, 79, 77, 75, 74, 72], 0.9, 0.11);
    chord(&mut ev, 2.4, 1.8, &[48, 55, 58, 62], 0.9, 0.0);
    play(instrument("organ", "Jazz Organ").as_mut(), &ev, 5.0, &mut out, t, 1.0);
    t += 5.0;

    // Mallets.
    let up_down = |root: u8| -> Vec<u8> {
        let scale = [0u8, 2, 4, 7, 9, 12, 14, 16, 19, 21, 24];
        let mut v: Vec<u8> = scale.iter().map(|s| root + s).collect();
        v.extend(scale.iter().rev().skip(1).map(|s| root + s));
        v
    };
    let mut ev = Vec::new();
    run(&mut ev, 0.0, 0.11, &up_down(48), 0.75, 0.1);
    // A marimba roll.
    for i in 0..24 {
        ev.push((2.6 + i as f32 * 0.06, Ev::On(if i % 2 == 0 { 60 } else { 64 }, 0.5 + 0.02 * i as f32)));
    }
    play(instrument("mallets", "Marimba").as_mut(), &ev, 5.0, &mut out, t, 1.0);
    t += 5.0;
    let mut ev = Vec::new();
    ev.push((0.0, Ev::Cc(64, 1.0)));
    chord(&mut ev, 0.0, 0.3, &[53, 57, 60, 64], 0.6, 0.03);
    chord(&mut ev, 1.5, 0.3, &[52, 55, 59, 62], 0.6, 0.03);
    ev.push((2.9, Ev::Cc(64, 0.0)));
    ev.push((3.0, Ev::Cc(64, 1.0)));
    run(&mut ev, 3.0, 0.16, &[72, 74, 76, 79, 81, 79, 76, 74], 0.6, 0.15);
    ev.push((5.5, Ev::Cc(64, 0.0)));
    play(instrument("mallets", "Soft Vibes").as_mut(), &ev, 6.5, &mut out, t, 1.0);
    t += 6.5;
    for (name, root, step, secs) in
        [("Xylophone", 72u8, 0.08, 2.4), ("Glockenspiel", 84, 0.12, 3.0), ("Kalimba Lofi", 60, 0.16, 3.6), ("Steel Drum", 60, 0.13, 3.2)]
    {
        let mut ev = Vec::new();
        run(&mut ev, 0.0, step, &up_down(root)[..11], 0.7, step * 0.9);
        play(instrument("mallets", name).as_mut(), &ev, secs, &mut out, t, 1.0);
        t += secs;
    }
    let mut ev = Vec::new();
    for (i, n) in [72u8, 67, 69, 60].iter().enumerate() {
        ev.push((i as f32 * 0.9, Ev::On(*n, 0.8)));
    }
    play(instrument("mallets", "Tubular Bells").as_mut(), &ev, 7.0, &mut out, t, 1.0);

    let peak = out.iter().flat_map(|f| f.iter()).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(out.iter().flat_map(|f| f.iter()).all(|s| s.is_finite()), "NaN in the demo");
    let norm = if peak > 0.89 { 0.89 / peak } else { 1.0 };
    let interleaved: Vec<f32> = out.iter().flat_map(|f| [f[0] * norm, f[1] * norm]).collect();
    ponzik_engine::wav::write_wav_i16("/tmp/keys_demo.wav", SR as u32, 2, &interleaved).expect("write");
    println!("/tmp/keys_demo.wav: {:.1} s, peak {:.2} (scaled ×{norm:.2})", out.len() as f32 / SR, peak);
}

#[test]
#[ignore]
fn cpu_cost() {
    // Ten seconds of eight-note chords every half second (organ: held
    // chords, the whole generator running), 128-sample blocks.
    for (kind, name) in [
        ("rhodes", "Classic Suitcase"),
        ("rhodes", "Chorus Dream"),
        ("wurli", "Wurli Grit"),
        ("organ", "Gospel B3"),
        ("organ", "Full Organ"),
        ("mallets", "Marimba"),
        ("mallets", "Soft Vibes"),
        ("mallets", "Tubular Bells"),
    ] {
        let mut inst = instrument(kind, name);
        inst.controller(Controller::Cc(64, 1.0));
        let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
        let secs = 10.0;
        let blocks = (secs * SR / 128.0) as usize;
        let started = Instant::now();
        for b in 0..blocks {
            if b % 188 == 0 {
                let root = 40 + (b / 188 % 12) as u8;
                for k in 0..8 {
                    inst.note_on(root + k * 5, 0.8);
                }
            }
            inst.render(&mut l, &mut r);
        }
        let used = started.elapsed().as_secs_f32();
        println!("{kind:8} {name:18} {:5.2} % of one core (sustain pedal down, up to 8 notes a chord)", used / secs * 100.0);
    }
}

#[test]
#[ignore]
fn preset_levels() {
    // Loudness of every factory preset on the same chord, to balance them.
    for kind in ["rhodes", "wurli", "organ", "mallets"] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../assets/presets/{kind}"));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("presets")
            .filter_map(|e| e.ok()?.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        for name in names {
            let mut inst = instrument(kind, &name);
            let mut out = Vec::new();
            let mut ev = Vec::new();
            chord(&mut ev, 0.0, 1.5, &[48, 55, 60, 64, 67], 0.8, 0.0);
            let (peak, rms) = play(inst.as_mut(), &ev, 2.0, &mut out, 0.0, 1.0);
            println!("PRESET {kind:8} {name:20} peak {peak:.2} rms {:.1} dB", 20.0 * rms.max(1e-9).log10());
        }
    }
}
