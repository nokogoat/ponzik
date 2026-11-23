//! Tests with the test plugins. Needs `cargo build -p ponzik-ladspa-test-plugin`
//! first; without the library the tests stop and say so.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};

use ponzik_engine::{Effect, Params, Unit};
use ponzik_ladspa::{LadspaEffect, create_effect, effect_from_kind, kind_for, scan_file};

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

/// `test.so` in a temporary folder of its own, removed at the end.
struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn fixture(test: &str) -> Option<Fixture> {
    let name = format!("{DLL_PREFIX}ponzik_ladspa_test_plugin{DLL_SUFFIX}");
    let built = std::env::current_exe().ok()?.parent()?.parent()?.join(&name);
    if !built.is_file() {
        eprintln!("note: {} missing, run `cargo build -p ponzik-ladspa-test-plugin`; skipped", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-ladspa-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join(format!("test{DLL_SUFFIX}"));
    std::fs::copy(&built, &dst).ok()?;
    Some(Fixture(dst))
}

fn path(f: &Fixture) -> &Path {
    &f.0
}

#[test]
fn scan_lists_both_plugins() {
    let Some(f) = fixture("scan") else { return };
    let (found, skipped) = scan_file(path(&f)).unwrap();
    assert!(skipped.is_empty());
    let got: Vec<_> = found.iter().map(|p| (p.id.as_str(), p.label.as_str(), p.name.as_str(), p.vendor.as_str())).collect();
    assert_eq!(got, [("4201", "ponzik_gain", "Ponzik Test Gain", "Ponzik"), ("4202", "ponzik_lowpass", "Ponzik Test Lowpass", "Ponzik")]);
    assert_eq!(found[0].kind(), kind_for(path(&f), "4201"));
    assert!(scan_file(Path::new("/nonexistent/x.so")).is_err());
}

#[test]
fn hints_become_parameters() {
    let Some(f) = fixture("hints") else { return };
    let fx = create_effect(path(&f), "4201").unwrap();
    let p = fx.params();
    assert_eq!(p.iter().map(|p| p.name).collect::<Vec<_>>(), ["Gain", "Invert", "Steps", "Tone"]);
    assert_eq!((p[0].id, p[0].min, p[0].max, p[0].default), (4, 0.0, 2.0, 1.0));
    assert_eq!((p[1].unit, p[1].default), (Unit::Choice, 0.0));
    assert_eq!((p[2].min, p[2].max, p[2].default), (1.0, 8.0, 5.0));
    // Tone: 0.001 to 0.5 of the rate, shown in hertz at 48 kHz, default low (logarithmic).
    assert_eq!(p[3].unit, Unit::Hz);
    assert!(p[3].log);
    assert!((p[3].min - 48.0).abs() < 1e-3 && (p[3].max - 24_000.0).abs() < 1e-2);
    let low = (48f32.ln() * 0.75 + 24_000f32.ln() * 0.25).exp();
    assert!((p[3].default - low).abs() < 0.1, "{} vs {low}", p[3].default);
}

#[test]
fn gain_processes_reports_latency_and_restores_values() {
    let Some(f) = fixture("gain") else { return };
    let mut fx = LadspaEffect::new(path(&f), "4201").unwrap();
    fx.set_sample_rate(44_100.0);
    fx.set_param(4, 0.5);
    let (mut l, mut r) = (vec![0.8f32; 6000], vec![-0.4f32; 6000]);
    fx.process(&mut l, &mut r);
    assert!(l.iter().all(|v| (v - 0.4).abs() < 1e-6) && r.iter().all(|v| (v + 0.2).abs() < 1e-6));
    assert_eq!(fx.latency(), 16);
    fx.set_param(5, 1.0);
    fx.process(&mut l, &mut r);
    assert!((l[0] + 0.2).abs() < 1e-6, "inverted");
    // The state is the parameter values: a new instance with them sounds the same.
    let values: Vec<(u32, f32)> = fx.params().iter().map(|p| (p.id, fx.get_param(p.id))).collect();
    let mut copy = effect_from_kind(&kind_for(path(&f), "4201")).unwrap();
    copy.set_sample_rate(44_100.0);
    for (id, v) in values {
        copy.set_param(id, v);
    }
    let (mut l2, mut r2) = (vec![0.8f32; 64], vec![-0.4f32; 64]);
    copy.process(&mut l2, &mut r2);
    assert!((l2[0] + 0.4).abs() < 1e-6);
}

#[test]
fn lowpass_cutoff_follows_the_sample_rate() {
    let Some(f) = fixture("lowpass") else { return };
    let mut fx = create_effect(path(&f), "4202").unwrap();
    fx.set_sample_rate(48_000.0);
    fx.set_param(2, 500.0);
    // Mono plugin: the mid goes in, both sides come out.
    let n = 48_000;
    let sine = |f: f32| -> Vec<f32> { (0..n).map(|i| (i as f32 * f / 48_000.0 * std::f32::consts::TAU).sin()).collect() };
    let rms = |x: &[f32]| (x[n / 2..].iter().map(|v| v * v).sum::<f32>() / (n / 2) as f32).sqrt();
    let (mut l, mut r) = (sine(100.0), sine(100.0));
    fx.process(&mut l, &mut r);
    let low = rms(&l);
    let (mut l, mut r) = (sine(8_000.0), sine(8_000.0));
    fx.process(&mut l, &mut r);
    let high = rms(&l);
    assert_eq!(l, r);
    assert!(low > 0.6 && high < 0.1, "100 Hz {low}, 8 kHz {high}");
    // Another rate: the cutoff stays 500 Hz.
    fx.set_sample_rate(96_000.0);
    assert_eq!(fx.get_param(2), 500.0);
}

#[test]
fn processing_never_allocates() {
    let Some(f) = fixture("alloc") else { return };
    let mut a = create_effect(path(&f), "4201").unwrap();
    let mut b = create_effect(path(&f), "4202").unwrap();
    a.set_sample_rate(48_000.0);
    b.set_sample_rate(48_000.0);
    let (mut l, mut r) = (vec![0.1f32; 5000], vec![0.1f32; 5000]);
    let before = ALLOCS.with(Cell::get);
    for k in 0..200usize {
        let n = 1 + k * 37 % 5000;
        a.set_param(4, (k % 5) as f32 * 0.3);
        b.set_param(2, 100.0 + k as f32 * 10.0);
        a.process(&mut l[..n], &mut r[..n]);
        b.process(&mut l[..n], &mut r[..n]);
        std::hint::black_box(a.latency());
    }
    assert_eq!(ALLOCS.with(Cell::get) - before, 0);
}
