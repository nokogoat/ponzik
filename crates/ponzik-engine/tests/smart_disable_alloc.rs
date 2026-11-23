//! Smart disable on the audio thread: inserts falling asleep in silence,
//! waking on sound, the switch turned on and off, never allocate; the
//! interface sees which inserts sleep.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::{Channel, Command, Effect, Engine, Sequence, registry};

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

fn effect(kind: &str) -> Box<dyn Effect> {
    let mut fx = registry::create_effect(kind).expect(kind);
    fx.set_sample_rate(SR);
    fx
}

#[test]
fn smart_disable_never_allocates_and_is_seen() {
    let pulse = registry::create_instrument("pulse").expect("pulse");
    let channel = Channel::new("lead", pulse).with_track(1);
    let (mut engine, mut handle) = Engine::new(SR, vec![channel], Sequence::default());
    engine.set_smart_disable(true);
    handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(effect("eq")) }).unwrap();
    handle.send(Command::SetInsert { track: 1, slot: 1, effect: Some(effect("reverb")) }).unwrap();
    handle.send(Command::SetInsert { track: 1, slot: 2, effect: Some(effect("delay")) }).unwrap();

    let mut out = vec![0.0f32; 256 * 2];
    engine.process(&mut out, 2);
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    let before = allocs();
    // A note, then silence until every tail has died and the inserts sleep.
    handle.send(Command::NoteOn { channel: 0, note: 60, velocity: 0.9 }).unwrap();
    for _ in 0..40 {
        engine.process(&mut out, 2);
    }
    handle.send(Command::NoteOff { channel: 0, note: 60 }).unwrap();
    let mut slept = None;
    for block in 0..3_000 {
        engine.process(&mut out, 2);
        if slept.is_none() && (0..3).all(|s| handle.insert_asleep(1, s)) {
            slept = Some(block);
        }
    }
    assert!(slept.is_some(), "every insert of the silent track sleeps");
    // Sound again: they wake on the block.
    handle.send(Command::NoteOn { channel: 0, note: 64, velocity: 0.9 }).unwrap();
    engine.process(&mut out, 2);
    assert!(!handle.insert_asleep(1, 0));
    assert!(out.iter().any(|s| s.abs() > 1e-3), "the note is heard through the woken inserts");
    // The switch, back and forth.
    handle.send(Command::SetSmartDisable(false)).unwrap();
    engine.process(&mut out, 2);
    handle.send(Command::SetSmartDisable(true)).unwrap();
    engine.process(&mut out, 2);
    assert_eq!(allocs() - before, 0, "the audio path allocated");
    assert!(out.iter().all(|s| s.is_finite()));
}
