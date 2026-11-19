//! Voice to notes in the engine: the input sung into a full-duplex call
//! plays the channel's instrument (through its note effects), reports its
//! notes with their song position, never allocates on the audio path, and
//! the end-to-end latency from the voice to the instrument's sound is
//! measured.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use ponzik_engine::instruments::Pulse;
use ponzik_engine::param::Params;
use ponzik_engine::registry::create_note_fx;
use ponzik_engine::{Channel, Command, Engine, Instrument, InputSource, Pattern, Sequence, VoiceConfig, VoiceReport, VoiceSettings};

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

const SR: f32 = 48_000.0;
const BLOCK: usize = 128;

/// A hummed melody (harmonics, vibrato, soft attacks) with a gap before each
/// note; returns the signal and the note starts.
fn hum(keys: &[f32], secs: f32, gap: f32) -> (Vec<f32>, Vec<usize>) {
    let sr = SR as f64;
    let mut out = vec![0.0f32; (gap * SR) as usize];
    let mut starts = Vec::new();
    for &k in keys {
        starts.push(out.len());
        let n = (secs * SR) as usize;
        let mut phase = 0.0;
        for i in 0..n {
            let t = i as f64 / sr;
            let midi = k as f64 + 0.3 * (TAU * 5.5 * t).sin() * (t / 0.15).min(1.0);
            phase += 440.0 * 2f64.powf((midi - 69.0) / 12.0) / sr;
            let x: f64 = (1..=12).map(|h| (TAU * phase * h as f64).sin() / (h as f64).powf(1.2)).sum();
            let env = (t / 0.02).min(1.0) * ((secs as f64 - t) / 0.03).clamp(0.0, 1.0);
            out.push((x * 0.08 * env) as f32);
        }
        out.extend(std::iter::repeat_n(0.0, (gap * SR) as usize));
    }
    (out, starts)
}

/// Note-ons the instrument received (keys), counted without allocating.
static RECEIVED: [AtomicU8; 64] = [const { AtomicU8::new(0) }; 64];
static RECEIVED_LEN: AtomicUsize = AtomicUsize::new(0);

struct Recorder;

impl Params for Recorder {}

impl Instrument for Recorder {
    fn kind(&self) -> &'static str {
        "recorder"
    }
    fn is_melodic(&self) -> bool {
        true
    }
    fn set_sample_rate(&mut self, _: f32) {}
    fn note_on(&mut self, note: u8, _: f32) {
        let i = RECEIVED_LEN.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = RECEIVED.get(i) {
            slot.store(note, Ordering::Relaxed);
        }
    }
    fn note_off(&mut self, _: u8) {}
    fn all_notes_off(&mut self) {}
    fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
}

/// Plays `input` (mono, on input 1) through the engine in duplex blocks;
/// returns the left output, the reports, and the allocations of the audio calls.
fn run(engine: &mut Engine, handle: &mut ponzik_engine::EngineHandle, input: &[f32]) -> (Vec<f32>, Vec<VoiceReport>, usize) {
    let mut out = Vec::with_capacity(input.len());
    let mut reports = Vec::new();
    let mut inter = vec![0.0f32; BLOCK * 2];
    let mut buf = vec![0.0f32; BLOCK * 2];
    let mut allocated = 0;
    for block in input.chunks(BLOCK) {
        let n = block.len();
        for (k, &x) in block.iter().enumerate() {
            inter[2 * k] = x;
            inter[2 * k + 1] = 0.0;
        }
        let before = allocs();
        engine.process_duplex(&inter[..2 * n], 2, &mut buf[..2 * n], 2);
        allocated += allocs() - before;
        out.extend(buf[..2 * n].iter().step_by(2));
        handle.drain_voice(|r| reports.push(r));
    }
    (out, reports, allocated)
}

fn note_ons(reports: &[VoiceReport]) -> Vec<(u8, f64)> {
    reports
        .iter()
        .filter_map(|r| match *r {
            VoiceReport::NoteOn { key, tick, .. } => Some((key, tick)),
            _ => None,
        })
        .collect()
}

#[test]
fn humming_plays_the_instrument_without_allocating() {
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    let ch = Channel::new("lead", Box::new(Pulse::new(Default::default())));
    let (mut engine, mut handle) = Engine::new(SR, vec![ch], Sequence { patterns: vec![Pattern::new(3_072, 1)], ..Default::default() });
    let config = VoiceConfig { retune: 0.5, ..VoiceConfig::default() };
    handle.send(Command::SetVoice(VoiceSettings { enabled: true, channel: 0, source: InputSource::Left, config })).unwrap();
    // Warm up on silence: lazily built state settles.
    let mut buf = vec![0.0f32; BLOCK * 2];
    for _ in 0..16 {
        engine.process_duplex(&vec![0.0; BLOCK * 2], 2, &mut buf, 2);
    }
    let keys = [57.0, 60.0, 64.0, 67.0, 52.0];
    let (voice, starts) = hum(&keys, 0.4, 0.25);
    let (out, reports, allocated) = run(&mut engine, &mut handle, &voice);
    assert_eq!(allocated, 0, "the audio path allocated");
    let ons = note_ons(&reports);
    assert_eq!(ons.iter().map(|o| o.0).collect::<Vec<_>>(), keys.map(|k| k as u8));
    assert!(ons.iter().all(|o| o.1.is_nan()), "transport stopped: no song position");
    let offs = reports.iter().filter(|r| matches!(r, VoiceReport::NoteOff { .. })).count();
    assert_eq!(offs, keys.len());
    assert!(reports.iter().filter(|r| matches!(r, VoiceReport::Frame(_))).count() > 500, "pitch frames for the trail");

    // End to end: from the voice's first sample to the instrument's sound,
    // through the engine (the driver's buffers come on top).
    let mut lat = Vec::new();
    for &s in &starts {
        let heard = out[s..].iter().position(|x| x.abs() > 0.01).expect("the instrument sounds");
        lat.push(heard as f32 / SR * 1000.0);
    }
    let mean = lat.iter().sum::<f32>() / lat.len() as f32;
    eprintln!("voice → instrument sound through the engine (ms): {lat:.1?}, mean {mean:.1}");
    assert!(lat.iter().all(|&l| l < 30.0), "{lat:?}");
}

#[test]
fn hummed_notes_go_through_the_note_effects_and_carry_the_song_position() {
    let mut chord = create_note_fx("chord").expect("chord");
    chord.set_param(0, 1.0);
    let ch = Channel::new("keys", Box::new(Recorder)).with_note_fx(0, chord);
    let (mut engine, mut handle) = Engine::new(SR, vec![ch], Sequence { patterns: vec![Pattern::new(3_072, 1)], ..Default::default() });
    handle.send(Command::SetVoice(VoiceSettings { enabled: true, channel: 0, source: InputSource::Left, config: VoiceConfig::default() })).unwrap();
    handle.send(Command::Play).unwrap();
    let (voice, _) = hum(&[60.0, 62.0], 0.35, 0.2);
    let (_, reports, allocated) = run(&mut engine, &mut handle, &voice);
    assert_eq!(allocated, 0, "the audio path allocated");
    let ons = note_ons(&reports);
    assert_eq!(ons.iter().map(|o| o.0).collect::<Vec<_>>(), [60, 62]);
    // Playing: each note knows where it falls in the song (ticks, increasing).
    assert!(ons[0].1 > 0.0 && ons[1].1 > ons[0].1, "{ons:?}");
    let n = RECEIVED_LEN.load(Ordering::Relaxed).min(64);
    let got: Vec<u8> = RECEIVED[..n].iter().map(|k| k.load(Ordering::Relaxed)).collect();
    assert!(got.len() >= 4 && got.len() > ons.len(), "one hummed note, a chord: {got:?}");
    assert!(got.contains(&60) && got.contains(&64), "{got:?}");

    // Switched off mid-note: the note ends at once.
    let (voice, _) = hum(&[65.0], 0.6, 0.1);
    let half = voice.len() / 2;
    let (_, reports, _) = run(&mut engine, &mut handle, &voice[..half]);
    assert_eq!(note_ons(&reports).len(), 1);
    handle.send(Command::SetVoice(VoiceSettings { enabled: false, channel: 0, source: InputSource::Left, config: VoiceConfig::default() })).unwrap();
    let (_, reports, _) = run(&mut engine, &mut handle, &voice[half..]);
    assert!(matches!(reports.first(), Some(VoiceReport::NoteOff { key: 65, .. })), "{reports:?}");
    assert_eq!(note_ons(&reports).len(), 0);
}
