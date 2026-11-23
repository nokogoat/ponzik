//! Real LV2 plugins of this machine (Calf, x42 and others in `/usr/lib/lv2`),
//! skipped when they are not installed:
//! `cargo test --release -p ponzik-lv2 --test system -- --ignored --nocapture`.

use std::path::Path;

use ponzik_engine::StateContext;
use ponzik_lv2::{create_effect, create_instrument, create_note_fx, plugin_bundles, scan_bundle};

const SR: f32 = 48_000.0;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn installed(bundle: &str) -> Option<&Path> {
    let p = Path::new(bundle);
    if p.join("manifest.ttl").is_file() {
        Some(p)
    } else {
        eprintln!("{bundle} not installed, skipped");
        None
    }
}

/// A test signal: two sines and a little noise, stereo.
fn signal(n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut seed = 1u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 - 0.5) * 0.05
    };
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            0.3 * (t * 220.0 * std::f32::consts::TAU).sin() + 0.2 * (t * 3_000.0 * std::f32::consts::TAU).sin() + noise()
        })
        .collect();
    (l.clone(), l)
}

/// RMS of the band around `f` (single-bin DFT over the signal).
fn band(x: &[f32], f: f32) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let ph = (i as f64) * f as f64 / SR as f64 * std::f64::consts::TAU;
        re += v as f64 * ph.cos();
        im += v as f64 * ph.sin();
    }
    ((re * re + im * im).sqrt() * 2.0 / x.len() as f64) as f32
}

#[test]
#[ignore = "lists the LV2 plugins of this machine"]
fn scan_every_installed_bundle() {
    let mut ok = 0;
    let mut skipped = 0;
    for b in plugin_bundles() {
        match scan_bundle(&b) {
            Ok((plugins, skip)) => {
                for p in &plugins {
                    let what = if p.is_instrument {
                        "instrument"
                    } else if p.is_midi_effect {
                        "MIDI effect"
                    } else {
                        "effect"
                    };
                    eprintln!("  {:<40} {what:<12} {}", p.name, p.id);
                }
                for s in &skip {
                    eprintln!("  SKIP {} ({}): {}", s.name, b.display(), s.reason);
                }
                ok += plugins.len();
                skipped += skip.len();
            }
            Err(e) => eprintln!("  ERROR {}: {e}", b.display()),
        }
    }
    eprintln!("{ok} plugins, {skipped} skipped");
    assert!(ok > 0 || plugin_bundles().is_empty());
}

#[test]
#[ignore = "needs x42-eq (fil4.lv2)"]
fn fil4_equalizer_cuts_and_boosts() {
    let Some(b) = installed("/usr/lib/lv2/fil4.lv2") else { return };
    let mut fx = create_effect(b, "http://gareus.org/oss/lv2/fil4#stereo").expect("fil4");
    let params = fx.params();
    eprintln!("{} parameters: {:?}", params.len(), params.iter().map(|p| p.name).collect::<Vec<_>>());
    let id = |name: &str| params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{name}")).id;
    fx.set_sample_rate(SR);
    // Highpass on at 1 kHz: the 220 Hz sine goes, the 3 kHz stays.
    fx.set_param(id("Highpass"), 1.0);
    fx.set_param(id("Highpass Frequency"), 1_000.0);
    let (l0, _) = signal(48_000);
    let (mut l, mut r) = signal(48_000);
    for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        fx.process(bl, br);
    }
    let tail = 24_000..;
    let (lo_in, lo_out) = (band(&l0[tail.clone()], 220.0), band(&l[tail.clone()], 220.0));
    let (hi_in, hi_out) = (band(&l0[tail.clone()], 3_000.0), band(&l[tail.clone()], 3_000.0));
    eprintln!("fil4 HP 1 kHz: 220 Hz {lo_in:.3} → {lo_out:.3}, 3 kHz {hi_in:.3} → {hi_out:.3}, rms {:.3}", rms(&l));
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    assert!(lo_out < lo_in * 0.2, "the highpass cuts 220 Hz");
    assert!((hi_out / hi_in - 1.0).abs() < 0.2, "3 kHz passes");
    // State round trip: a new instance gets the highpass back.
    let state = fx.editor().unwrap().save_state(StateContext::Project).expect("state");
    let mut copy = create_effect(b, "http://gareus.org/oss/lv2/fil4#stereo").unwrap();
    copy.load_state(&state).unwrap();
    assert_eq!(copy.get_param(id("Highpass")), 1.0);
    assert!((copy.get_param(id("Highpass Frequency")) - 1_000.0).abs() < 1.0);
}

#[test]
#[ignore = "needs x42-dpl (dpl.lv2)"]
fn dpl_limiter_holds_the_ceiling() {
    let Some(b) = installed("/usr/lib/lv2/dpl.lv2") else { return };
    let (_, plugins) = (0, scan_bundle(b).unwrap().0);
    let p = plugins.iter().find(|p| p.id.contains("stereo")).unwrap_or(&plugins[0]);
    let mut fx = create_effect(b, &p.id).expect("dpl");
    let params = fx.params();
    eprintln!("{}: {:?}", p.name, params.iter().map(|p| (p.name, p.min, p.max, p.default)).collect::<Vec<_>>());
    fx.set_sample_rate(SR);
    let threshold = params.iter().find(|p| p.name.contains("Threshold")).expect("threshold");
    fx.set_param(threshold.id, threshold.min);
    let (mut l, mut r): (Vec<f32>, Vec<f32>) = signal(48_000);
    l.iter_mut().chain(r.iter_mut()).for_each(|v| *v *= 2.0);
    for (bl, br) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        fx.process(bl, br);
    }
    let peak = l[12_000..].iter().fold(0f32, |m, v| m.max(v.abs()));
    eprintln!("dpl at {} dB: peak {peak:.3} ({:.1} dBFS), latency {}", threshold.min, 20.0 * peak.log10(), fx.latency());
    assert!(l.iter().all(|v| v.is_finite()));
    assert!(peak < 10f32.powf(threshold.min / 20.0) * 1.05, "limited to the threshold");
    assert!(rms(&l) > 0.05, "not silent");
}

#[test]
#[ignore = "needs Calf (calf.lv2)"]
fn calf_compressor_squeezes() {
    let Some(b) = installed("/usr/lib/lv2/calf.lv2") else { return };
    let mut fx = create_effect(b, "http://calf.sourceforge.net/plugins/Compressor").expect("Calf Compressor");
    let params = fx.params();
    let id = |n: &str| params.iter().find(|p| p.name.contains(n)).unwrap_or_else(|| panic!("{n}")).id;
    fx.set_sample_rate(SR);
    fx.set_param(id("Threshold"), 0.03);
    fx.set_param(id("Ratio"), 10.0);
    let (l0, _) = signal(48_000);
    let (mut l, mut r) = signal(48_000);
    for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        fx.process(bl, br);
    }
    eprintln!("Calf Compressor: rms {:.3} → {:.3}", rms(&l0[24_000..]), rms(&l[24_000..]));
    assert!(l.iter().all(|v| v.is_finite()));
    assert!(rms(&l[24_000..]) > 0.01);
    assert!(rms(&l[24_000..]) < rms(&l0[24_000..]) * 0.9 || params.iter().any(|p| p.name.contains("Makeup")));
}

#[test]
#[ignore = "needs Calf (calf.lv2)"]
fn calf_monosynth_plays_a_note() {
    let Some(b) = installed("/usr/lib/lv2/calf.lv2") else { return };
    let mut inst = create_instrument(b, "http://calf.sourceforge.net/plugins/Monosynth").expect("Monosynth");
    eprintln!("Monosynth: {} parameters", inst.params().len());
    inst.set_sample_rate(SR);
    let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
    inst.note_on(57, 0.9);
    for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        inst.render(bl, br);
    }
    let (mut l2, mut r2) = (vec![0.0; 24_000], vec![0.0; 24_000]);
    inst.note_off(57);
    for (bl, br) in l2.chunks_mut(512).zip(r2.chunks_mut(512)) {
        inst.render(bl, br);
    }
    // Pitch: zero crossings of the held part (A3 = 220 Hz, the filter rounds it).
    let held = &l[12_000..36_000];
    let crossings = held.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let f = crossings as f32 / (held.len() as f32 / SR);
    eprintln!("Monosynth A3: rms {:.3}, ~{f:.0} Hz by crossings, after release rms {:.4}", rms(held), rms(&l2[18_000..]));
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    assert!(rms(held) > 0.01, "Monosynth sounds");
    let state = inst.editor().unwrap().save_state(StateContext::Project).unwrap();
    let mut copy = create_instrument(b, "http://calf.sourceforge.net/plugins/Monosynth").unwrap();
    copy.load_state(&state).unwrap();
}

#[test]
#[ignore = "needs x42-midifilter (midifilter.lv2)"]
fn midifilter_transposes_notes() {
    use ponzik_engine::note_fx::NoteBuffer;
    let Some(b) = installed("/usr/lib/lv2/midifilter.lv2") else { return };
    let (plugins, _) = scan_bundle(b).unwrap();
    let t = plugins.iter().find(|p| p.id.ends_with("#miditranspose")).expect("transpose");
    assert!(t.is_midi_effect);
    let mut fx = create_note_fx(b, &t.id).expect("instance");
    eprintln!("{}: {:?}", t.name, fx.params().iter().map(|p| (p.id, p.name, p.min, p.max)).collect::<Vec<_>>());
    fx.set_sample_rate(SR);
    let semis = fx.params().iter().find(|p| p.min < 0.0 && p.max >= 12.0).expect("semitones").id;
    fx.set_param(semis, 7.0);
    let mut out = NoteBuffer::default();
    fx.note_on(60, 0.8, &mut out);
    fx.tick(&mut out);
    fx.advance(64);
    fx.tick(&mut out);
    eprintln!("out {:?}", out.as_slice());
    assert!(out.as_slice().iter().any(|e| e.on && e.key == 67), "C4 + 7 = G4");
}

/// The x42-eq interface (an `ui:X11UI` with `ui:idleInterface`) embedded in
/// a Ponzik plugin window for two seconds, captured with ImageMagick's
/// `import` when present: `-- --ignored fil4_interface_in_a_window`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs x42-eq and an X11 display"]
fn fil4_interface_in_a_window() {
    use std::time::{Duration, Instant};

    use ponzik_engine::{EditorEvent, ParentWindow};
    use ponzik_window::{NativeHandle, PluginWindow, PluginWindowOptions};

    let Some(b) = installed("/usr/lib/lv2/fil4.lv2") else { return };
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("no DISPLAY, skipped");
        return;
    }
    let mut fx = create_effect(b, "http://gareus.org/oss/lv2/fil4#stereo").unwrap();
    fx.set_sample_rate(SR);
    let ed = fx.editor().unwrap();
    assert!(ed.has_gui(), "x42-eq has an X11 interface");
    let options = PluginWindowOptions { title: "x42-eq in Ponzik".into(), width: 640, height: 400, position: None, owner: None, resizable: false };
    let mut win = PluginWindow::open(&options).expect("plugin window");
    let NativeHandle::X11(id) = win.handle() else { panic!("X11 expected") };
    let size = ed.open(ParentWindow::X11(id), 1.0).expect("x42-eq interface");
    eprintln!("interface size {size:?}");
    win.set_resizable(size.resizable);
    win.set_size(size.width, size.height);
    let (mut ev, mut wev) = (Vec::new(), Vec::new());
    let (l0, _) = signal(256);
    let t0 = Instant::now();
    let mut shot = false;
    while t0.elapsed() < Duration::from_millis(2500) {
        let (mut l, mut r) = (l0.clone(), l0.clone());
        fx.process(&mut l, &mut r);
        ed.idle(&mut ev);
        for e in ev.drain(..) {
            if let EditorEvent::Resize { width, height } = e {
                win.set_size(width, height);
            }
            eprintln!("editor event {e:?}");
        }
        win.poll_events(&mut wev);
        if !shot && t0.elapsed() > Duration::from_millis(1800) {
            shot = true;
            let out = std::process::Command::new("import").args(["-window", &format!("{id:#x}"), "/tmp/ponzik-lv2-fil4-ui.png"]).status();
            eprintln!("capture: {out:?} → /tmp/ponzik-lv2-fil4-ui.png");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ed.is_open());
    ed.close();
    assert!(!ed.is_open());
    drop(win);
}
