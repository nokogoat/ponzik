//! Audio clips on the audio thread: unwarped, looped, reversed and warped
//! clips (rendered or still resampled along a tempo ramp), sequence swaps and
//! jumps never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

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

use ponzik_engine::{
    AudioClip, ClipWarp, Command, Engine, PlayMode, Sample, Sequence, TempoMap, TempoPoint,
    WarpMap, WarpMarker,
};

const SR: f32 = 48_000.0;

fn sample(len: usize, rate: f32) -> Arc<Sample> {
    let left: Vec<f32> = (0..len).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
    Arc::new(Sample {
        name: "loop".into(),
        sample_rate: rate,
        right: left.clone(),
        left,
        markers: Vec::new(),
    })
}

fn sequence(gain: f32) -> Box<Sequence> {
    let src = sample(96_000, 44_100.0);
    let rendered = sample(200_000, SR);
    let clip = |start: u32| AudioClip {
        sample: src.clone(),
        track: 1,
        start,
        len: 384 * 4,
        gain,
        fade_in: 0.01,
        fade_out: 0.05,
        ..Default::default()
    };
    let warp = |rendered: Option<Arc<Sample>>| {
        let markers = vec![
            WarpMarker {
                secs: 0.0,
                beat: 0.0,
            },
            WarpMarker {
                secs: 1.2,
                beat: 2.0,
            },
        ];
        Some(ClipWarp {
            map: WarpMap::new(97.0, markers),
            rendered,
        })
    };
    let clips = vec![
        clip(0),
        AudioClip {
            looped: true,
            reverse: true,
            fade_in_curve: 0.6,
            fade_out_curve: -0.4,
            ..clip(96)
        },
        AudioClip {
            warp: warp(None),
            looped: true,
            ..clip(192)
        },
        AudioClip {
            warp: warp(Some(rendered)),
            ..clip(384)
        },
    ];
    let tempo = TempoMap::new(
        vec![
            TempoPoint {
                tick: 0,
                bpm: 120.0,
                ramp: false,
            },
            TempoPoint {
                tick: 384 * 4,
                bpm: 150.0,
                ramp: true,
            },
        ],
        Vec::new(),
    );
    Box::new(Sequence {
        mode: PlayMode::Song,
        song_len: 384 * 8,
        audio_clips: clips,
        tempo,
        ..Default::default()
    })
}

#[test]
fn audio_clips_never_allocate() {
    let (mut engine, mut handle) = Engine::new(SR, Vec::new(), Sequence::default());
    handle.send(Command::SetSequence(sequence(1.0))).unwrap();
    handle.send(Command::Play).unwrap();
    let mut out = vec![0.0f32; 256 * 2];
    engine.process(&mut out, 2);
    let mut spare = Some(sequence(0.5));

    // The counter does see allocations.
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let mut energy = 0.0f32;
    let before = allocs();
    for block in 0..1_200u32 {
        if block == 300 {
            handle
                .send(Command::SetSequence(spare.take().unwrap()))
                .unwrap();
        }
        if block == 700 {
            handle.send(Command::Locate(500.0)).unwrap();
        }
        engine.process(&mut out, 2);
        energy += out.iter().map(|s| s * s).sum::<f32>();
    }
    assert_eq!(allocs() - before, 0, "the audio path allocated");
    assert!(energy > 1.0, "the clips play");
}
