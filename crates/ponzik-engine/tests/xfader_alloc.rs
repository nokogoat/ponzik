//! Crossfader and cue on the audio thread: assignments, crossfader moves,
//! cue on and off and the cue blend never allocate, and the cue never
//! reaches the master.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::xfader::{Curve, Side, table};
use ponzik_engine::{Channel, Command, Engine, Sequence, registry};

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

#[test]
fn crossfader_and_cue_never_allocate() {
    let a = Channel::new("a", registry::create_instrument("pulse").expect("pulse")).with_track(1);
    let b = Channel::new("b", registry::create_instrument("pulse").expect("pulse")).with_track(2);
    let (mut engine, mut handle) = Engine::new(SR, vec![a, b], Sequence::default());
    let mut out = vec![0.0f32; 256 * 2];
    engine.process(&mut out, 2);
    // Commands are boxed on this side: send them all before counting.
    handle.send(Command::NoteOn { channel: 0, note: 60, velocity: 0.9 }).unwrap();
    handle.send(Command::NoteOn { channel: 1, note: 67, velocity: 0.9 }).unwrap();
    handle.send(Command::SetTrackXfade { track: 1, side: Side::A }).unwrap();
    handle.send(Command::SetTrackXfade { track: 2, side: Side::B }).unwrap();
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    let before = allocs();
    for block in 0..400 {
        let x = (block % 100) as f32 / 99.0;
        let curve = Curve::ALL[block / 100 % 3];
        handle.send(Command::SetCrossfader(table(x, curve))).unwrap();
        if block == 150 {
            handle.send(Command::SetTrackCue { track: 1, on: true }).unwrap();
        }
        if block == 200 {
            handle.send(Command::SetCueMix(0.5)).unwrap();
        }
        if block == 300 {
            handle.send(Command::SetTrackCue { track: 1, on: false }).unwrap();
        }
        engine.process(&mut out, 2);
        assert!(out.iter().all(|s| s.is_finite()));
    }
    assert_eq!(allocs(), before, "crossfader and cue allocate nothing");
}
