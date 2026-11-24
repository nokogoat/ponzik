//! The audio editor's free recording: writing the input into the capture
//! buffer, filling it and handing it back never allocates on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::input::RecordBuffer;
use ponzik_engine::{Command, Engine, Sequence};

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
fn capturing_the_input_never_allocates_on_the_audio_thread() {
    const SR: f32 = 48_000.0;
    let (mut engine, mut handle) = Engine::new(SR, Vec::new(), Sequence::default());
    let input: Vec<f32> = (0..2 * 256).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
    let mut out = vec![0.0f32; 2 * 256];
    for _ in 0..8 {
        engine.process_duplex(&input, 2, &mut out, 2);
    }

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let mut audio_thread = 0;
    let mut returned = 0;
    for round in 0..6 {
        // Interface side: a buffer of a few blocks, sometimes stopped early.
        handle.send(Command::Capture(Some(Box::new(RecordBuffer::new(256 * (3 + round), SR))))).unwrap();
        let before = allocs();
        for _ in 0..(if round % 2 == 0 { 12 } else { 2 }) {
            engine.process_duplex(&input, 2, &mut out, 2);
        }
        audio_thread += allocs() - before;
        if round % 2 == 1 {
            handle.send(Command::Capture(None)).unwrap();
            let before = allocs();
            engine.process_duplex(&input, 2, &mut out, 2);
            audio_thread += allocs() - before;
        }
        if let Some(cap) = handle.take_capture() {
            assert!(cap.len > 0);
            returned += 1;
        }
    }
    assert_eq!(audio_thread, 0, "the audio path allocated");
    assert_eq!(returned, 6, "every capture came back");
}
