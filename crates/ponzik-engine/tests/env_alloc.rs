//! The native synths' envelopes with delay, hold and curves on the audio
//! thread: notes, parameter changes, rendering and the envelope probe never
//! allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use ponzik_engine::registry::create_instrument;
use ponzik_engine::{EnvStage, Sample};

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

const EXTRA: [&str; 5] = ["Delay", "Hold", "Attack curve", "Decay curve", "Release curve"];

#[test]
fn curved_envelopes_never_allocate() {
    let mut l = vec![0.0f32; 1_024];
    let mut r = vec![0.0f32; 1_024];
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe, "the counter sees allocations");
    let sine: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let sample = Arc::new(Sample { name: "sine".into(), sample_rate: 48_000.0, left: sine.clone(), right: sine, markers: Vec::new() });
    for id in ["pulse", "wavetable", "fm", "sampler", "granular"] {
        let mut inst = create_instrument(id).expect(id);
        inst.set_sample_rate(48_000.0);
        inst.set_sample(sample.clone());
        let env = inst.env_probe().expect("an envelope probe");
        let extra: Vec<(u32, f32, f32)> =
            inst.params().iter().filter(|p| EXTRA.contains(&p.name)).map(|p| (p.id, p.min, p.max)).collect();
        assert!(extra.len() >= 5, "{id}: {} extra envelope parameters", extra.len());
        for &(pid, min, _) in &extra {
            inst.set_param(pid, if min < 0.0 { 0.7 } else { 0.01 });
        }
        let before = allocs();
        let mut stages = 0u32;
        for block in 0..400u32 {
            let note = 40 + (block * 5 % 36) as u8;
            if block % 4 == 0 {
                inst.note_on(note, 0.8);
            }
            if block % 3 == 0 {
                let (pid, min, max) = extra[block as usize % extra.len()];
                let t = (block % 7) as f32 / 6.0;
                inst.set_param(pid, if min < 0.0 { min + (max - min) * t } else { 0.05 * t });
            }
            let n = 64 + (block as usize % 4) * 128;
            l.fill(0.0);
            r.fill(0.0);
            inst.render(&mut l[..n], &mut r[..n]);
            let (stage, progress, level) = env.read();
            assert!((0.0..=1.0).contains(&progress) && (0.0..=1.0).contains(&level), "{id}: {stage:?} {progress} {level}");
            stages |= 1 << stage.index();
            if block % 4 == 2 {
                inst.note_off(note);
            }
            assert!(l[..n].iter().chain(&r[..n]).all(|s| s.is_finite() && s.abs() < 4.0), "{id}");
        }
        inst.all_notes_off();
        assert_eq!(allocs() - before, 0, "{id}: the audio path allocated");
        assert!(stages & !(1 << EnvStage::Idle.index()) != 0, "{id}: the probe never saw a note");
        assert!(env.notes() >= 100, "{id}");
    }
}
