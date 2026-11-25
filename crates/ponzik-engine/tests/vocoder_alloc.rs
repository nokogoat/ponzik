//! The vocoder never allocates on the audio thread: processing with and
//! without a key, changing any setting (band count, carrier, channel) and
//! following the notes of a channel.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::effects::vocoder as voc;
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
fn vocoder_never_allocates_while_processing() {
    const SR: f32 = 48_000.0;
    let mut fx = registry::create_effect("vocoder").unwrap();
    fx.set_sample_rate(SR);
    let mut held = vec![ChannelNotes::EMPTY; 4];
    let key: Vec<f32> = (0..256).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let before = allocs();
    let settings: [(u32, f32); 12] = [
        (voc::P_BANDS, 10.0),
        (voc::P_CARRIER, 1.0),
        (voc::P_MIDI_CHANNEL, 2.0),
        (voc::P_FORMANT, -7.0),
        (voc::P_CARRIER, 2.0),
        (voc::P_BANDS, 0.0),
        (voc::P_UNVOICED, 1.0),
        (voc::P_CARRIER, 3.0),
        (voc::P_MIDI_CHANNEL, 0.0),
        (voc::P_WIDTH, 4.0),
        (voc::P_CARRIER, 0.0),
        (voc::P_SPREAD, 1.0),
    ];
    for k in 0..2_000 {
        if k % 40 == 0 {
            let (id, v) = settings[(k / 40) % settings.len()];
            fx.set_param(id, v);
        }
        if k % 25 == 0 {
            let note = 48 + (k / 25 % 12) as u8;
            if held[1].is_held(note) { held[1].note_off(note) } else { held[1].note_on(note) }
        }
        for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            *a = ((k * 256 + i) as f32 * 0.013).sin() * 0.3;
            *b = *a;
        }
        if k % 3 != 0 {
            fx.sidechain(&key, &key);
        }
        fx.channel_notes(&held);
        fx.process(&mut l, &mut r);
    }
    assert_eq!(allocs() - before, 0, "allocations on the audio thread");
}
