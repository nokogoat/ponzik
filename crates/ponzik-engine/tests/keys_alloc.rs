//! The modeled keyboards (tine and reed pianos, tonewheel organ, mallets)
//! and the rotary speaker on the audio thread: notes, pedals, controllers,
//! parameter changes and rendering never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::registry::{create_effect, create_instrument};
use ponzik_engine::{Controller, NoteExpression};

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
    let mut l = vec![0.0f32; 2_048];
    let mut r = vec![0.0f32; 2_048];
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    for id in ["rhodes", "wurli", "organ", "mallets"] {
        let mut inst = create_instrument(id).expect(id);
        inst.set_sample_rate(48_000.0);
        let params = inst.params();
        let before = allocs();
        for block in 0..300u32 {
            let note = 36 + (block * 7 % 48) as u8;
            inst.note_on(note, 0.2 + (block % 8) as f32 * 0.1);
            if block % 3 == 0 {
                let p = &params[block as usize % params.len()];
                inst.set_param(p.id, p.min + (p.max - p.min) * ((block % 5) as f32 / 4.0));
            }
            inst.controller(Controller::Cc(64, if block % 20 < 10 { 1.0 } else { 0.0 }));
            inst.controller(Controller::Cc(1, (block % 9) as f32 / 8.0));
            inst.controller(Controller::PitchBend(((block % 7) as f32 - 3.0) / 3.0));
            inst.note_expression(note, NoteExpression::Tuning { semitones: 1.0, secs: 0.05 });
            // Engine-sized blocks and one longer than the internal buffers.
            let n = if block % 50 == 0 { 2_048 } else { 128 + (block as usize % 4) * 64 };
            inst.render(&mut l[..n], &mut r[..n]);
            if block % 2 == 0 {
                inst.note_off(note);
            }
        }
        inst.all_notes_off();
        assert_eq!(allocs() - before, 0, "{id}: the audio path allocated");
    }

    let mut fx = create_effect("rotary").expect("rotary");
    fx.set_sample_rate(48_000.0);
    let before = allocs();
    for block in 0..200u32 {
        fx.set_param(0, (block / 40 % 3) as f32);
        fx.set_param(3, (block % 10) as f32 / 10.0);
        for (i, s) in l[..256].iter_mut().enumerate() {
            *s = ((i as f32) * 0.05).sin() * 0.5;
        }
        r[..256].copy_from_slice(&l[..256]);
        fx.process(&mut l[..256], &mut r[..256]);
    }
    assert_eq!(allocs() - before, 0, "rotary: the audio path allocated");
}
