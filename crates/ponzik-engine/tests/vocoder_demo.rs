//! Listening demo and CPU cost of the vocoder. Ignored by default:
//!
//! cargo test --release -p ponzik-engine --test vocoder_demo -- --ignored --nocapture
//!
//! writes /tmp/vocoder_demo.wav (a synthetic singer, with hiss for the
//! consonants, through every factory preset; Talk Box and Track audio presets
//! vocode a saw pad played on the track) and prints the level of each preset
//! and the share of one core the vocoder takes at 20 and 40 bands.

use std::path::Path;
use std::time::Instant;

use ponzik_dsp::noise::WhiteNoise;
use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
use ponzik_engine::effects::vocoder as voc;
use ponzik_engine::registry::create_effect;
use ponzik_engine::{ChannelNotes, Effect};

const SR: f32 = 48_000.0;
const PRESETS: [&str; 6] = ["Robot voice", "Daft", "Choir pad", "Whisper", "Talk Box", "Deep Formant"];

fn preset(name: &str) -> Vec<(u32, f32)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../assets/presets/vocoder/{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let body = &text[text.find("\"params\"").expect("params")..];
    let nums: Vec<f32> = body
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))
        .filter_map(|s| s.parse().ok())
        .collect();
    nums.chunks(2).map(|p| (p[0] as u32, p[1])).collect()
}

/// Three seconds of singing, with a burst of hiss (an "s") between phrases.
fn voice() -> Vec<f32> {
    let notes: Vec<Sung> = (0..5).map(|i| Sung { start: 0.1 + 0.55 * i as f32, dur: 0.45, midi: 52.0 + [0.0, 3.0, 5.0, 7.0, 3.0][i], vowel: i % 5 }).collect();
    let mut v = sing(SR, &notes, &Style::default(), 3.0);
    let mut n = WhiteNoise::new(3);
    for (k, s) in v.iter_mut().enumerate() {
        let t = k as f32 / SR;
        if (1.15..1.3).contains(&t) || (2.35..2.5).contains(&t) {
            *s += 0.12 * n.next_sample();
        }
    }
    v
}

/// A saw chord (C minor) for the presets whose carrier is the track.
fn pad(frames: usize) -> Vec<f32> {
    let hz = [130.81f32, 155.56, 196.0];
    (0..frames).map(|k| hz.iter().map(|f| ((k as f32 * f / SR).fract() * 2.0 - 1.0) * 0.12).sum()).collect()
}

fn run(fx: &mut dyn Effect, track: &[f32], key: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (track.to_vec(), track.to_vec());
    let notes = [ChannelNotes::EMPTY; 2];
    for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
        let kb = &key[k * 256..k * 256 + a.len()];
        fx.sidechain(kb, kb);
        fx.channel_notes(&notes);
        fx.process(a, b);
    }
    (l, r)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
#[ignore]
fn vocoder_demo() {
    let key = voice();
    let frames = key.len();
    let mut out = Vec::new();
    for name in PRESETS {
        let mut fx = create_effect("vocoder").unwrap();
        fx.set_sample_rate(SR);
        for (id, v) in preset(name) {
            fx.set_param(id, v);
        }
        let track = if fx.get_param(voc::P_CARRIER) == 0.0 { pad(frames) } else { vec![0.0; frames] };
        let (l, r) = run(fx.as_mut(), &track, &key);
        let peak = l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()));
        println!("{name:14} rms {:6.1} dBFS, peak {:6.1} dBFS", 20.0 * rms(&l).log10(), 20.0 * peak.log10());
        out.extend(l.iter().zip(&r).flat_map(|(&a, &b)| [a, b]));
        out.extend(std::iter::repeat_n(0.0, (SR * 0.4) as usize * 2));
    }
    ponzik_engine::wav::write_wav_i16("/tmp/vocoder_demo.wav", SR as u32, 2, &out).expect("write");

    for bands in [5.0, 10.0] {
        let mut fx = create_effect("vocoder").unwrap();
        fx.set_sample_rate(SR);
        fx.set_param(voc::P_BANDS, bands);
        let track = pad(frames);
        let started = Instant::now();
        let _ = run(fx.as_mut(), &track, &key);
        let share = started.elapsed().as_secs_f32() / (frames as f32 / SR);
        println!("{} bands, track carrier: {:.2}% of one core", voc::BAND_COUNTS[bands as usize], share * 100.0);
    }
}
