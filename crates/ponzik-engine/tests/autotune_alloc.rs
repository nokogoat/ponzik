//! The pitch correction never allocates on the audio thread: processing,
//! changing any setting (mode, latency, range included) and the MIDI notes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
use ponzik_engine::effects::autotune::{self as at};
use ponzik_engine::{ChannelNotes, registry};

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
fn pitch_correction_never_allocates_while_processing() {
    const SR: f32 = 48_000.0;
    let notes: Vec<Sung> = (0..6).map(|i| Sung { start: 0.05 + 0.3 * i as f32, dur: 0.3, midi: 50.0 + 2.3 * i as f32, vowel: i % 5 }).collect();
    let voice = sing(SR, &notes, &Style::default(), 2.0);
    let mut fx = registry::create_effect("autotune").unwrap();
    fx.set_sample_rate(SR);
    let mut held = vec![ChannelNotes::EMPTY; 4];
    held[0].note_on(62);
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let before = allocs();
    let settings: [(u32, f32); 12] = [
        (at::P_MODE, 1.0),
        (at::P_LATENCY, 0.0),
        (at::P_RANGE, 4.0),
        (at::P_MODE, 0.0),
        (at::P_RETUNE, 0.0),
        (at::P_WIDTH, 20.0),
        (at::P_FORMANT_SHIFT, -5.0),
        (at::P_MIDI_MODE, 2.0),
        (at::P_MIDI_CHANNEL, 1.0),
        (at::P_VIB_DEPTH, 40.0),
        (at::P_LATENCY, 1.0),
        (at::P_SCALE, 3.0),
    ];
    for (k, block) in voice.chunks(256).enumerate() {
        if k % 30 == 0 {
            let (id, v) = settings[(k / 30) % settings.len()];
            fx.set_param(id, v);
        }
        let n = block.len();
        l[..n].copy_from_slice(block);
        r[..n].copy_from_slice(block);
        fx.channel_notes(&held);
        fx.process(&mut l[..n], &mut r[..n]);
    }
    assert_eq!(allocs() - before, 0, "allocations on the audio thread");
}
