//! The demos on the audio thread: playing, the adaptive worker threads
//! switching on and off, and the latency compensation growing its delay
//! lines never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ponzik_engine::{Command, registry};
use ponzik_project::{demo_breakcore, demo_project};

struct Counting;

static ARMED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations (on any thread) while `f` runs.
fn count(f: impl FnOnce()) -> usize {
    ALLOCS.store(0, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    f();
    ARMED.store(false, Ordering::SeqCst);
    ALLOCS.load(Ordering::SeqCst)
}

#[test]
fn playing_the_demos_never_allocates() {
    // The counter does see allocations.
    assert!(count(|| drop(std::hint::black_box(vec![1u8; 16]))) > 0);

    let none = |_: &str| None;
    for project in [demo_project(), demo_breakcore()] {
        let (mut engine, mut handle) = project.build_engine(48_000.0, &none);
        engine.set_worker_threads(2);
        engine.set_adaptive_threads(true);
        handle.send(Command::Play).unwrap();
        let mut out = vec![0.0f32; 2 * 256];
        // Warm-up: first blocks, first notes.
        for _ in 0..200 {
            engine.process(&mut out, 2);
        }
        // An insert with latency, made here: the delay lines grow on the
        // audio thread, inside their reserved capacity.
        let mut limiter = registry::create_effect("limiter").unwrap();
        limiter.set_sample_rate(48_000.0);
        let insert = Command::SetInsert { track: 1, slot: 5, effect: Some(limiter) };
        handle.send(insert).unwrap();
        let n = count(|| {
            for block in 0..2_000usize {
                // The adaptive choice follows the load; force both paths too.
                if block % 500 == 250 {
                    engine.force_parallel(true);
                } else if block % 500 == 400 {
                    engine.set_adaptive_threads(true);
                }
                engine.process(&mut out, 2);
            }
        });
        assert!(engine.mixer_mut().latency() > 0, "{}: the limiter adds latency", project.name);
        assert_eq!(n, 0, "{}: {n} allocations while playing", project.name);
        handle.collect_garbage();
    }
}
