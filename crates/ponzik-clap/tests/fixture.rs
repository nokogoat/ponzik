//! Tests avec le plugin de banc d'essai. Prérequis :
//! `cargo build -p ponzik-clap-test-plugin` avant `cargo test -p ponzik-clap`.
//! Sans l'artefact, les tests s'arrêtent en le signalant.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ponzik_clap::{create_effect, create_instrument, kind_for, scan_dir};
use ponzik_engine::{EditorEvent, ParentWindow, StateContext};

const SR: f32 = 48_000.0;

/// Fields of the test gain's state: (gain, blocks, context).
fn gain_state(b: &[u8]) -> (f64, u32, u32) {
    assert_eq!((b.len(), &b[..4]), (20, &b"PZTG"[..]), "{b:?}");
    (
        f64::from_le_bytes(b[4..12].try_into().unwrap()),
        u32::from_le_bytes(b[12..16].try_into().unwrap()),
        u32::from_le_bytes(b[16..20].try_into().unwrap()),
    )
}

#[test]
fn state_round_trips_with_hidden_values() {
    let Some(path) = fixture("state") else { return };
    let mut fx = create_effect(&path, "ponzik.test.gain").unwrap();
    fx.set_sample_rate(SR);
    fx.set_param(7, 0.5);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    for _ in 0..3 {
        fx.process(&mut l, &mut r);
    }
    let ed = fx.editor().unwrap();
    // `clap.state-context` is used: the context reaches the plugin (project 3, preset 1).
    let project = ed.save_state(StateContext::Project).expect("state");
    assert_eq!(gain_state(&project), (0.5, 3, 3));
    assert_eq!(gain_state(&ed.save_state(StateContext::Preset).unwrap()).2, 1);

    // A new instance restored before activation: the parameter is read back,
    // the hidden counter comes back.
    let mut copy = create_effect(&path, "ponzik.test.gain").unwrap();
    assert_eq!(copy.get_param(7), 1.0);
    copy.load_state(&project).unwrap();
    assert_eq!(copy.get_param(7), 0.5);
    copy.set_sample_rate(SR);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    copy.process(&mut l, &mut r);
    assert!((l[0] - 0.25).abs() < 1e-6, "restored gain applies");
    assert_eq!(gain_state(&copy.editor().unwrap().save_state(StateContext::Project).unwrap()), (0.5, 4, 3));
    assert!(copy.load_state(b"garbage").is_err());

    // Loaded live through the editor: values come back for the host.
    let mut other = project.clone();
    other[4..12].copy_from_slice(&1.5f64.to_le_bytes());
    assert_eq!(ed.load_state(&other, StateContext::Preset).unwrap(), [(7, 1.5)]);
    drop(fx);
    assert!(ed.save_state(StateContext::Project).is_none(), "instance gone");
    assert!(ed.load_state(&project, StateContext::Project).is_err());

    // The sine only has plain `clap.state`: its last key comes back.
    let mut sine = create_instrument(&path, "ponzik.test.sine").unwrap();
    sine.set_sample_rate(SR);
    sine.note_on(64, 1.0);
    sine.render(&mut l, &mut r);
    let state = sine.editor().unwrap().save_state(StateContext::Project).unwrap();
    assert_eq!(state, 64i16.to_le_bytes());
    let mut copy = create_instrument(&path, "ponzik.test.sine").unwrap();
    assert_eq!(copy.editor().unwrap().save_state(StateContext::Project).unwrap(), (-1i16).to_le_bytes());
    copy.load_state(&state).unwrap();
    assert_eq!(copy.editor().unwrap().save_state(StateContext::Project).unwrap(), state);
}

/// Fichier `test.clap` dans un dossier temporaire, supprimé en fin de test.
struct Fixture(PathBuf);

impl std::ops::Deref for Fixture {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Copie l'artefact dans un dossier temporaire propre au test, sous le nom `test.clap`.
fn fixture(test: &str) -> Option<Fixture> {
    let name = format!("{DLL_PREFIX}ponzik_clap_test_plugin{DLL_SUFFIX}");
    // target/<profil>/deps/<test> → target/<profil>/<artefact>
    let exe = std::env::current_exe().ok()?;
    let built = exe.parent()?.parent()?.join(&name);
    if !built.is_file() {
        eprintln!("note : {} absent, lancer `cargo build -p ponzik-clap-test-plugin` ; test ignoré", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-clap-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("test.clap");
    std::fs::copy(&built, &dst).ok()?;
    Some(Fixture(dst))
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn scan_lists_both_plugins() {
    let Some(path) = fixture("scan") else { return };
    let mut found = scan_dir(path.parent().unwrap());
    found.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].id.as_str(), found[0].is_instrument), ("ponzik.test.gain", false));
    assert_eq!((found[1].id.as_str(), found[1].is_instrument), ("ponzik.test.sine", true));
    assert_eq!(found[1].name, "Sinus de test");
    assert_eq!(found[1].vendor, "Ponzik");
    assert_eq!(found[1].path, *path);
}

#[test]
fn gain_param_halves_the_level() {
    let Some(path) = fixture("gain") else { return };
    let mut fx = create_effect(&path, "ponzik.test.gain").unwrap();
    assert_eq!(fx.kind(), kind_for(&path, "ponzik.test.gain"));
    let params = fx.params();
    assert_eq!(params.len(), 1);
    assert_eq!((params[0].id, params[0].name, params[0].min, params[0].max), (7, "Gain", 0.0, 2.0));
    assert_eq!(fx.get_param(7), 1.0);
    assert_eq!(fx.latency(), 0, "inactif : latence inconnue");
    fx.set_sample_rate(SR);
    // `clap.latency` du banc d'essai (`GAIN_LATENCY`), lue à l'activation.
    assert_eq!(fx.latency(), 64);

    let input: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin()).collect();
    let (mut l, mut r) = (input.clone(), input.clone());
    fx.process(&mut l, &mut r);
    assert!((rms(&l) - rms(&input)).abs() < 1e-6);

    fx.set_param(7, 0.5);
    assert_eq!(fx.get_param(7), 0.5);
    // Bloc plus long que `MAX_BLOCK` : découpé en plusieurs appels.
    let long: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.05).sin()).collect();
    let (mut l, mut r) = (long.clone(), long.clone());
    fx.process(&mut l, &mut r);
    assert!((rms(&l) / rms(&long) - 0.5).abs() < 1e-5);
    assert_eq!(l, r);
}

#[test]
fn instrument_plays_the_right_pitch() {
    let Some(path) = fixture("sine") else { return };
    let mut inst = create_instrument(&path, "ponzik.test.sine").unwrap();
    assert!(inst.params().is_empty());
    inst.set_sample_rate(SR);

    let (mut l, mut r) = (vec![0.0; SR as usize], vec![0.0; SR as usize]);
    inst.render(&mut l, &mut r);
    assert_eq!(rms(&l), 0.0);

    inst.note_on(69, 1.0);
    inst.render(&mut l, &mut r);
    let rising = l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!((439..=441).contains(&rising), "{rising} passages par zéro");
    assert!((rms(&r) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-2);

    // `render` ajoute au tampon existant.
    let mut l2 = l.clone();
    inst.render(&mut l2[..100], &mut r[..100]);
    assert!(l2[..100].iter().zip(&l[..100]).any(|(a, b)| a != b));

    inst.note_off(69);
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    inst.render(&mut l, &mut r);
    assert_eq!(rms(&l) + rms(&r), 0.0);

    inst.note_on(60, 0.5);
    inst.all_notes_off();
    inst.render(&mut l, &mut r);
    assert_eq!(rms(&l), 0.0);
}

#[cfg(target_os = "macos")]
const PARENT: ParentWindow = ParentWindow::Cocoa(0x1234);
#[cfg(windows)]
const PARENT: ParentWindow = ParentWindow::Win32(0x1234);
#[cfg(not(any(target_os = "macos", windows)))]
const PARENT: ParentWindow = ParentWindow::X11(0x1234);

#[test]
fn editor_lifecycle_timers_and_fds() {
    let Some(path) = fixture("gui") else { return };
    let inst = create_instrument(&path, "ponzik.test.sine").unwrap();
    let ed = inst.editor().expect("every CLAP instance has an editor handle");
    assert!(!ed.has_gui());
    assert!(ed.open(PARENT, 1.0).is_err());

    let mut fx = create_effect(&path, "ponzik.test.gain").unwrap();
    fx.set_sample_rate(SR);
    let ed = fx.editor().unwrap();
    assert!(ed.has_gui() && ed.is_alive() && !ed.is_open());
    // Scale 2: the interface doubles.
    let size = ed.open(PARENT, 2.0).unwrap();
    assert_eq!((size.width, size.height, size.resizable), (600, 400, true));
    assert!(ed.is_open() && ed.resizable());
    assert_eq!(ed.open(PARENT, 2.0).unwrap().width, 600, "already open: same interface");
    // Size adjusted to the plugin's steps (10 px, at least 100 × 80).
    assert_eq!(ed.set_size(455, 50), Some((450, 80)));

    // Pump: the readable pipe asks for a resize and a rescan, the timer for an
    // edit that comes out of the next `process`.
    std::thread::sleep(Duration::from_millis(5));
    let mut ev = Vec::new();
    ed.idle(&mut ev);
    assert_eq!(ev, [EditorEvent::Resize { width: 420, height: 260 }]);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    fx.process(&mut l, &mut r);
    ev.clear();
    ed.idle(&mut ev);
    // The rescan asked along with the resize reads the values again (the edit
    // has just been applied), then come the edit's gesture and value.
    assert_eq!(
        ev,
        [
            EditorEvent::Param { id: 7, value: 0.25 },
            EditorEvent::GestureBegin(7),
            EditorEvent::Param { id: 7, value: 0.25 },
            EditorEvent::GestureEnd(7)
        ]
    );
    assert_eq!(fx.get_param(7), 0.25);
    assert!((l[0] - 0.125).abs() < 1e-6);

    // Closed: timer and pipe are given back, nothing comes anymore.
    ed.close();
    assert!(!ed.is_open() && !ed.resizable());
    assert_eq!(ed.set_size(300, 200), None);
    std::thread::sleep(Duration::from_millis(5));
    ev.clear();
    ed.idle(&mut ev);
    fx.process(&mut l, &mut r);
    ed.idle(&mut ev);
    assert!(ev.is_empty(), "{ev:?}");

    // Destroyed with its interface open: the host closes it first (the plugin
    // aborts the process otherwise), then the editor turns inert.
    ed.open(PARENT, 1.0).unwrap();
    drop(fx);
    assert!(!ed.is_alive() && !ed.is_open() && !ed.has_gui());
    assert!(ed.open(PARENT, 1.0).is_err());
    ed.idle(&mut ev);
}
