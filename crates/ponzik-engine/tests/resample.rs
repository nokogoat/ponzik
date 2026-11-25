//! Resampling: a take of the master's output (or of a track's, post-fader)
//! recorded while the song plays. It equals what was heard sample for
//! sample, its latency lines it up with the timeline, the armed track's own
//! sound never comes back into it, and nothing allocates on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::instruments::Pulse;
use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Channel, Command, Effect, Engine, EngineHandle, InputSource, Note, Params, ParamInfo, Pattern, RecordBuffer, Sequence, TICKS_PER_STEP};

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
const BLOCK: usize = 256;

/// A pure delay of `n` samples that says so: the mixer compensates it.
struct Late {
    line: Vec<[f32; 2]>,
    at: usize,
}

impl Late {
    fn new(n: usize) -> Self {
        Self { line: vec![[0.0; 2]; n], at: 0 }
    }
}

impl Params for Late {
    fn params(&self) -> &'static [ParamInfo] {
        &[]
    }
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
    fn set_param(&mut self, _: u32, _: f32) {}
}

impl Effect for Late {
    fn kind(&self) -> &'static str {
        "late"
    }
    fn set_sample_rate(&mut self, _: f32) {}
    fn latency(&self) -> usize {
        self.line.len()
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let [x, y] = std::mem::replace(&mut self.line[self.at], [*a, *b]);
            (*a, *b) = (x, y);
            self.at = (self.at + 1) % self.line.len();
        }
    }
}

fn pattern(key: u8) -> Pattern {
    let mut p = Pattern::new(16 * TICKS_PER_STEP, 2);
    p.notes[0] = (0..16u32).step_by(2).map(|s| Note::new(s * TICKS_PER_STEP, TICKS_PER_STEP, key + s as u8, 0.8)).collect();
    p.notes[1] = (0..16u32).step_by(4).map(|s| Note::new(s * TICKS_PER_STEP, 2 * TICKS_PER_STEP, key - 12, 0.9)).collect();
    p.sort();
    p
}

/// Two Pulse channels, on tracks 1 and 2; the second is muted unless `both`.
fn song(both: bool) -> (Engine, EngineHandle) {
    let a = Channel::new("a", Box::new(Pulse::new(Default::default()))).with_track(1);
    let b = Channel::new("b", Box::new(Pulse::new(Default::default()))).with_track(2);
    let (engine, mut handle) = Engine::new(SR, vec![a, b], Sequence { patterns: vec![pattern(60)], ..Default::default() });
    if !both {
        handle.send(Command::SetChannelMute { channel: 1, muted: true }).unwrap();
    }
    (engine, handle)
}

/// Plays `blocks` blocks; returns the output (left, right).
fn play(engine: &mut Engine, blocks: usize) -> (Vec<f32>, Vec<f32>) {
    let mut buf = vec![0.0f32; 2 * BLOCK];
    let (mut l, mut r) = (Vec::new(), Vec::new());
    for _ in 0..blocks {
        engine.process(&mut buf, 2);
        l.extend(buf.iter().step_by(2));
        r.extend(buf.iter().skip(1).step_by(2));
    }
    (l, r)
}

fn arm(handle: &mut EngineHandle, track: usize, source: InputSource, seconds: f32) {
    let mut buf = RecordBuffer::for_source(source, seconds, SR);
    buf.source = source;
    handle.send(Command::Record { track, armed: true, buffer: Some(Box::new(buf)) }).unwrap();
}

fn take(engine: &mut Engine, handle: &mut EngineHandle, track: usize) -> Box<RecordBuffer> {
    handle.send(Command::Record { track, armed: false, buffer: None }).unwrap();
    engine.process(&mut [], 2);
    handle.collect_garbage();
    let mut recs = handle.take_recordings();
    assert_eq!(recs.len(), 1);
    recs.remove(0)
}

#[test]
fn the_master_take_equals_the_output_sample_for_sample() {
    let (mut engine, mut handle) = song(true);
    handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(Box::new(Late::new(300))) }).unwrap();
    handle.send(Command::SetTrackGain { track: 2, gain: 0.5 }).unwrap();
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 5, InputSource::Resample(MASTER), 2.0);
    let (l, r) = play(&mut engine, 200);
    let rec = take(&mut engine, &mut handle, 5);
    assert_eq!(rec.len, 200 * BLOCK);
    assert!(!rec.is_mono() && rec.source == InputSource::Resample(MASTER));
    assert_eq!(rec.left[..rec.len], l[..], "left");
    assert_eq!(rec.right[..rec.len], r[..], "right");
    assert!(l.iter().any(|s| s.abs() > 0.05), "something played");
    assert_eq!((rec.start_tick, rec.mix_latency), (0.0, 300));
}

#[test]
fn the_take_is_aligned_with_the_timeline_whatever_the_latency() {
    // The same song without latency, then with a delay the mixer compensates
    // on the other paths: shifted by its latency, the take is the same.
    let (mut engine, mut handle) = song(true);
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 6, InputSource::Resample(MASTER), 2.0);
    play(&mut engine, 150);
    let dry = take(&mut engine, &mut handle, 6);
    assert_eq!(dry.mix_latency, 0);

    let (mut engine, mut handle) = song(true);
    handle.send(Command::SetInsert { track: 2, slot: 0, effect: Some(Box::new(Late::new(1_000))) }).unwrap();
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 6, InputSource::Resample(MASTER), 2.0);
    play(&mut engine, 150);
    let late = take(&mut engine, &mut handle, 6);
    assert_eq!(late.mix_latency, 1_000);
    let n = dry.len - 1_000;
    let diff = (0..n).map(|k| (late.left[k + 1_000] - dry.left[k]).abs()).fold(0.0f32, f32::max);
    assert!(diff < 1e-6, "{diff}");
}

#[test]
fn a_track_take_is_its_post_fader_output() {
    // Track 1 alone at half gain: its output, and nothing of track 2.
    let (mut engine, mut handle) = song(true);
    handle.send(Command::SetTrackGain { track: 1, gain: 0.5 }).unwrap();
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 7, InputSource::Resample(1), 2.0);
    play(&mut engine, 120);
    let track = take(&mut engine, &mut handle, 7);

    let (mut engine, mut handle) = song(false);
    handle.send(Command::SetTrackGain { track: 1, gain: 0.5 }).unwrap();
    handle.send(Command::Play).unwrap();
    let (alone, _) = play(&mut engine, 120);
    assert_eq!(track.left[..track.len], alone[..]);
}

#[test]
fn the_armed_track_is_cut_so_nothing_loops_back() {
    // Channel b plays on track 2, which records the master: it is not heard
    // (nor recorded) while it does; the master is channel a alone.
    let (mut engine, mut handle) = song(true);
    handle.send(Command::SetMonitor { track: 2, source: Some(InputSource::Resample(MASTER)) }).unwrap();
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 2, InputSource::Resample(MASTER), 2.0);
    let (heard, _) = play(&mut engine, 120);
    let rec = take(&mut engine, &mut handle, 2);

    let (mut engine, mut handle) = song(false);
    handle.send(Command::Play).unwrap();
    let (alone, _) = play(&mut engine, 120);
    assert_eq!(heard, alone);
    assert_eq!(rec.left[..rec.len], alone[..]);
    assert_eq!(handle.take_track_peak(2), [0.0, 0.0]);
}

#[test]
fn the_metronome_is_heard_but_not_resampled() {
    let (mut engine, mut handle) = song(false);
    handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
    handle.send(Command::SetMetronome(true)).unwrap();
    handle.send(Command::Play).unwrap();
    arm(&mut handle, 3, InputSource::Resample(MASTER), 1.0);
    let (heard, _) = play(&mut engine, 100);
    let rec = take(&mut engine, &mut handle, 3);
    assert!(heard.iter().any(|s| s.abs() > 0.05), "the click plays");
    assert!(rec.left[..rec.len].iter().all(|&s| s == 0.0), "but stays out of the take");
}

#[test]
fn resampling_never_allocates() {
    let (mut engine, mut handle) = song(true);
    handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(Box::new(Late::new(500))) }).unwrap();
    handle.send(Command::SetMonitor { track: 4, source: Some(InputSource::Resample(MASTER)) }).unwrap();
    handle.send(Command::SetMetronome(true)).unwrap();
    handle.send(Command::Play).unwrap();
    let mut buf = vec![0.0f32; 2 * BLOCK];
    engine.process(&mut buf, 2);
    // Buffers made off the audio thread, handed over by commands.
    arm(&mut handle, 4, InputSource::Resample(MASTER), 3.0);
    arm(&mut handle, 5, InputSource::Resample(1), 3.0);

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let before = allocs();
    for k in 0..400 {
        if k == 200 {
            handle.send(Command::Locate(0.0)).unwrap();
        }
        engine.process(&mut buf, 2);
    }
    handle.send(Command::Record { track: 4, armed: false, buffer: None }).unwrap();
    engine.process(&mut buf, 2);
    assert_eq!(allocs() - before, 0, "the audio thread allocated");
    handle.collect_garbage();
    let rec = handle.take_recordings();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].jumps.len(), 1, "the jump back is noted");
}
