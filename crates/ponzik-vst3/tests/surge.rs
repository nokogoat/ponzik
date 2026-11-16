//! Surge XT, a real plugin, loaded headless when it is installed
//! (`/usr/lib/vst3/Surge XT.vst3`, distro package `surge-xt`):
//! `cargo test -p ponzik-vst3 --test surge -- --ignored --nocapture`.

use std::path::Path;
use std::time::{Duration, Instant};

use ponzik_vst3::{create_instrument, scan_bundle};

const SURGE: &str = "/usr/lib/vst3/Surge XT.vst3";

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
#[ignore = "needs Surge XT installed"]
fn surge_xt_plays_headless() {
    let path = Path::new(SURGE);
    if !path.exists() {
        eprintln!("{SURGE} not installed, skipped");
        return;
    }
    let found = scan_bundle(path).expect("scan");
    eprintln!("{found:#?}");
    let synth = found.iter().find(|p| p.is_instrument).expect("Surge XT instrument");
    let t0 = Instant::now();
    let mut inst = create_instrument(path, &synth.id).expect("instance");
    eprintln!("created in {:?}, {} parameters", t0.elapsed(), inst.params().len());
    assert!(inst.params().len() > 100);
    inst.set_sample_rate(48_000.0);
    let ed = inst.editor().expect("editor handle");
    assert!(ed.has_gui());
    let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
    inst.note_on(60, 0.9);
    let mut ev = Vec::new();
    for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        inst.render(bl, br);
        ed.idle(&mut ev);
    }
    eprintln!("rms {:.4} / {:.4}, editor events {}", rms(&l), rms(&r), ev.len());
    assert!(rms(&l) > 1e-3, "Surge XT is silent");
    drop(inst);
    assert!(!ed.is_alive());
}

/// Opens the real Surge XT view in a Ponzik plugin window for a moment
/// (needs a display): `-- --ignored surge_xt_editor_in_a_window`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs Surge XT installed and an X11 display"]
fn surge_xt_editor_in_a_window() {
    use ponzik_engine::ParentWindow;
    use ponzik_window::{NativeHandle, PluginWindow, PluginWindowOptions};

    let path = Path::new(SURGE);
    if !path.exists() || std::env::var_os("DISPLAY").is_none() {
        eprintln!("Surge XT or DISPLAY missing, skipped");
        return;
    }
    let synth = scan_bundle(path).unwrap().into_iter().find(|p| p.is_instrument).unwrap();
    let mut inst = create_instrument(path, &synth.id).unwrap();
    inst.set_sample_rate(48_000.0);
    let ed = inst.editor().unwrap();
    let options = PluginWindowOptions {
        title: "Surge XT (VST 3) — test".into(),
        width: 800,
        height: 600,
        position: None,
        owner: None,
        resizable: false,
    };
    let mut win = PluginWindow::open(&options).expect("plugin window");
    let NativeHandle::X11(id) = win.handle() else { panic!("X11 expected") };
    let size = ed.open(ParentWindow::X11(id), win.scale_factor() as f64).expect("Surge XT view");
    eprintln!("view size {size:?}");
    win.set_resizable(size.resizable);
    win.set_size(size.width, size.height);
    let t0 = Instant::now();
    let (mut ev, mut wev) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    while t0.elapsed() < Duration::from_millis(1500) {
        inst.render(&mut l, &mut r);
        ed.idle(&mut ev);
        win.poll_events(&mut wev);
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("editor events {ev:?}, window events {wev:?}");
    ed.close();
    drop(win);
    assert!(!ed.is_open());
}
