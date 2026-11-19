//! The modeled acoustic instruments and the Amp & Cab never allocate on the
//! audio thread: notes, strums, legato, slides, controllers and rendering.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::{Controller, NoteExpression, registry};

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

/// Instruments covered; each is played with every value of its choice parameters.
const KINDS: &[&str] = &["guitar"];

#[test]
fn acoustic_instruments_never_allocate() {
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for &kind in KINDS {
        let mut inst = registry::create_instrument(kind).expect(kind);
        inst.set_sample_rate(48_000.0);
        let params: Vec<_> = inst.params().to_vec();
        let before = allocs();
        for p in &params {
            for v in [p.min, p.max, p.default] {
                inst.set_param(p.id, v);
                // A chord (strummed or not), a legato line, a slide, controllers.
                for n in [40u8, 47, 52, 56, 59, 64] {
                    inst.note_on(n, 0.8);
                }
                inst.render(&mut l, &mut r);
                inst.note_on(66, 0.9);
                inst.note_expression(66, NoteExpression::Tuning { semitones: 2.0, secs: 0.05 });
                inst.controller(Controller::Cc(1, 0.7));
                inst.controller(Controller::Cc(11, 0.5));
                inst.controller(Controller::Cc(64, 1.0));
                inst.controller(Controller::PitchBend(0.5));
                for _ in 0..4 {
                    inst.render(&mut l, &mut r);
                }
                for n in [40u8, 47, 52, 56, 59, 64, 66] {
                    inst.note_off(n);
                }
                inst.controller(Controller::Cc(64, 0.0));
                inst.render(&mut l, &mut r);
                inst.set_param(p.id, p.default);
            }
        }
        inst.all_notes_off();
        assert_eq!(allocs() - before, 0, "{kind} allocated on the audio thread");
    }
}

#[test]
fn amp_never_allocates() {
    let mut fx = registry::create_effect("amp").expect("amp");
    fx.set_sample_rate(48_000.0);
    let mut l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let mut r = l.clone();
    let params: Vec<_> = fx.params().to_vec();
    let before = allocs();
    for p in &params {
        for v in [p.min, p.max] {
            fx.set_param(p.id, v);
            fx.process(&mut l, &mut r);
        }
    }
    assert_eq!(allocs() - before, 0);
}
