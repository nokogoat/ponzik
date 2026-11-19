//! Grand Piano on the audio thread: strikes, restrikes, pedals, parameter
//! changes, voice stealing and rendering never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::instruments::Piano;
use ponzik_engine::instruments::piano::{P_HARDNESS, P_LID, P_STRETCH, P_TUNING};
use ponzik_engine::{Controller, Instrument, Params};

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
fn playing_never_allocates() {
    let mut piano = Piano::new();
    piano.set_sample_rate(48_000.0);
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    let before = allocs();
    for block in 0..600u32 {
        // Chords over the whole keyboard, restrikes, more keys than voices.
        let root = 21 + (block * 7 % 80) as u8;
        for k in [0u8, 4, 7, 12] {
            piano.note_on(root + k, 0.3 + (block % 7) as f32 * 0.1);
        }
        if block % 50 == 0 {
            piano.controller(Controller::Cc(64, if block % 100 == 0 { 1.0 } else { 0.0 }));
            piano.controller(Controller::Cc(66, (block % 150 == 0) as u8 as f32));
            piano.controller(Controller::Cc(67, 0.5));
            piano.controller(Controller::Cc(64, 0.45));
        }
        if block % 40 == 0 {
            piano.set_param(P_HARDNESS, (block % 3) as f32 * 0.5);
            piano.set_param(P_LID, (block % 5) as f32 * 0.25);
            piano.set_param(P_TUNING, 430.0 + (block % 20) as f32);
            piano.set_param(P_STRETCH, (block % 3) as f32);
        }
        piano.render(&mut l, &mut r);
        for k in [0u8, 4, 7, 12] {
            piano.note_off(root + k);
        }
    }
    piano.all_notes_off();
    assert_eq!(allocs() - before, 0, "the audio path allocated");
}
