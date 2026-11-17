//! Adding, deleting and moving mixer tracks: the new mixer is built off the
//! audio thread, and taking it over never allocates on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::instruments::Pulse;
use ponzik_engine::mixer::{MASTER, default_order};
use ponzik_engine::{Channel, Command, Engine, Garbage, Note, Pattern, Sequence, TICKS_PER_STEP, registry, track_remap};

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

#[test]
fn track_remaps_never_allocate_on_the_audio_thread() {
    const SR: f32 = 48_000.0;
    let mut p = Pattern::new(16 * TICKS_PER_STEP, 4);
    for ch in 0..4 {
        p.notes[ch] = (0..4u32).map(|s| Note::new(s * 4 * TICKS_PER_STEP, TICKS_PER_STEP, 48 + ch as u8, 0.8)).collect();
    }
    p.sort();
    let channels = (0..4).map(|i| Channel::new("pulse", Box::new(Pulse::new(Default::default()))).with_track(1 + i * 40)).collect();
    let (mut engine, mut handle) = Engine::new(SR, channels, Sequence { patterns: vec![p], ..Default::default() });
    engine.set_track_count(200);
    engine.set_worker_threads(2);
    engine.force_parallel(true);
    for t in (1..200).step_by(7) {
        let mut fx = registry::create_effect(if t % 2 == 0 { "eq" } else { "limiter" }).unwrap();
        fx.set_sample_rate(SR);
        engine.mixer_mut().tracks[t].inserts[0] = Some(fx);
    }
    handle.send(Command::Play).unwrap();
    let mut buf = vec![0.0f32; 2 * 256];
    for _ in 0..16 {
        engine.process(&mut buf, 2);
    }

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let mut count = 200;
    let mut audio_thread = 0;
    for round in 0..30 {
        // Interface side: a layout with a track inserted, one deleted, two swapped
        // or one appended, in turn.
        let mut from: Vec<Option<usize>> = (0..count).map(Some).collect();
        match round % 4 {
            0 => from.insert(1 + round % 50, None),
            1 => {
                from.remove(1 + round % 50);
            }
            2 => from.swap(3, 150),
            _ => from.push(None),
        }
        count = from.len();
        let mut order = default_order(count).into_vec();
        order.retain(|&t| t != MASTER);
        handle.send(track_remap(SR, &from, order.into())).unwrap();
        // Audio side.
        let before = allocs();
        for _ in 0..8 {
            engine.process(&mut buf, 2);
        }
        audio_thread += allocs() - before;
        let returned = std::iter::from_fn(|| handle.take_garbage()).any(|g| matches!(g, Garbage::Mixer(_)));
        assert!(returned, "round {round}: the old mixer comes back");
    }
    assert_eq!(audio_thread, 0, "the audio path allocated");
    assert_eq!(engine.mixer_mut().len(), count);
}
