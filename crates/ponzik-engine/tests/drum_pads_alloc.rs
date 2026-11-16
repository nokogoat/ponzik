//! Drum Pads on the audio thread: hits, chokes, parameter changes and
//! rendering never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use ponzik_engine::instruments::DrumPads;
use ponzik_engine::instruments::drum_pads::{PADS, S_CUTOFF, S_FILTER, S_TUNE, pad_param};
use ponzik_engine::{Instrument, Params, Sample};

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
    let mut pads = DrumPads::new();
    pads.set_sample_rate(48_000.0);
    for p in 0..PADS {
        let left: Vec<f32> = (0..9_600).map(|i| ((i * (p + 3)) as f32 * 0.01).sin() * 0.5).collect();
        let s = Sample { name: format!("pad {p}"), sample_rate: 44_100.0, right: left.clone(), left, markers: Vec::new() };
        pads.set_pad_sample(p, 0, Some(Arc::new(s)));
    }
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];

    // The counter does see allocations.
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let before = allocs();
    for block in 0..400u32 {
        let note = 36 + (block % 16) as u8;
        pads.note_on(note, 0.3 + (block % 7) as f32 * 0.1);
        if block % 5 == 0 {
            pads.set_param(pad_param((block % 16) as usize, S_TUNE), (block % 24) as f32 - 12.0);
            pads.set_param(pad_param((block % 16) as usize, S_FILTER), (block % 3) as f32);
            pads.set_param(pad_param((block % 16) as usize, S_CUTOFF), 200.0 + block as f32 * 30.0);
        }
        pads.render(&mut l, &mut r);
        pads.note_off(note);
    }
    pads.all_notes_off();
    assert_eq!(allocs() - before, 0, "the audio path allocated");
}
