//! Probability, conditions and ratchets on the audio thread: playing a
//! pattern full of them never allocates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::instruments::Pulse;
use ponzik_engine::{Channel, Command, Engine, Note, NoteCond, Pattern, Sequence, TICKS_PER_STEP};

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
fn chances_and_ratchets_never_allocate() {
    let mut p = Pattern::new(16 * TICKS_PER_STEP, 1);
    let conds = [NoteCond::Always, NoteCond::First, NoteCond::NotFirst, NoteCond::Every(1, 2), NoteCond::Every(3, 4)];
    p.notes[0] = (0..16u32)
        .map(|s| Note {
            prob: (s * 7 % 101) as u8,
            cond: conds[s as usize % conds.len()],
            ratchet: (s % 8 + 1) as u8,
            ..Note::new(s * TICKS_PER_STEP, TICKS_PER_STEP, 48 + s as u8, 0.8)
        })
        .collect();
    p.sort();
    let ch = Channel::new("pulse", Box::new(Pulse::new(Default::default()))).with_track(1);
    let (mut engine, mut handle) = Engine::new(48_000.0, vec![ch], Sequence { patterns: vec![p], ..Default::default() });
    handle.send(Command::SetBpm(174.0)).unwrap();
    handle.send(Command::Play).unwrap();
    let mut buf = vec![0.0f32; 2 * 256];
    // Warm up: the first blocks may settle lazily built state.
    for _ in 0..8 {
        engine.process(&mut buf, 2);
    }

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let before = allocs();
    for _ in 0..2_000 {
        engine.process(&mut buf, 2);
    }
    assert_eq!(allocs() - before, 0, "the audio path allocated");
}
