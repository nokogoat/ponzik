//! The LV2 host on the audio thread: `run`, notes into the atom sequence,
//! parameter changes, worker requests and responses, the fixed-block FIFO
//! never allocate. Needs `cargo build -p ponzik-lv2-test-plugin`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};

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

fn bundle() -> Option<PathBuf> {
    let name = format!("{DLL_PREFIX}ponzik_lv2_test_plugin{DLL_SUFFIX}");
    let built = std::env::current_exe().ok()?.parent()?.parent()?.join(&name);
    if !built.is_file() {
        eprintln!("note: {} missing, run `cargo build -p ponzik-lv2-test-plugin`; skipped", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-lv2-alloc-{}", std::process::id())).join("test.lv2");
    std::fs::create_dir_all(&dir).ok()?;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ponzik-lv2-test-plugin/bundle");
    for f in ["manifest.ttl", "test.ttl", "presets.ttl"] {
        std::fs::write(dir.join(f), std::fs::read_to_string(src.join(f)).ok()?.replace("@BINARY@", &name)).ok()?;
    }
    std::fs::copy(&built, dir.join(&name)).ok()?;
    Some(dir)
}

#[test]
fn processing_never_allocates() {
    let Some(b) = bundle() else { return };
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");

    let mut gain = ponzik_lv2::create_effect(&b, "urn:ponzik:test:gain").unwrap();
    let mut fixed = ponzik_lv2::create_effect(&b, "urn:ponzik:test:fixed").unwrap();
    let mut synth = ponzik_lv2::create_instrument(&b, "urn:ponzik:test:synth").unwrap();
    gain.set_sample_rate(48_000.0);
    fixed.set_sample_rate(48_000.0);
    synth.set_sample_rate(48_000.0);
    let (mut l, mut r) = (vec![0.1f32; 1024], vec![0.1f32; 1024]);
    let before = allocs();
    for block in 0..500usize {
        let n = 1 + (block * 97) % 1024;
        // Mode changes go through the worker and come back.
        gain.set_param(5, (block / 50 % 3) as f32);
        gain.set_param(4, (block % 7) as f32 * 0.25);
        gain.process(&mut l[..n], &mut r[..n]);
        fixed.process(&mut l[..n], &mut r[..n]);
        synth.note_on(40 + (block % 40) as u8, 0.7);
        synth.controller(ponzik_engine::Controller::PitchBend(0.3));
        synth.render(&mut l[..n], &mut r[..n]);
        if block % 3 == 0 {
            synth.note_off(40 + (block % 40) as u8);
        }
        if block % 100 == 0 {
            synth.all_notes_off();
        }
    }
    assert_eq!(allocs() - before, 0, "allocations on the audio path");
    let _ = std::fs::remove_dir_all(b.parent().unwrap());
}
