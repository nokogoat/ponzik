//! Tests avec le plugin de banc d'essai. Prérequis :
//! `cargo build -p ponzik-vst3-test-plugin` avant `cargo test -p ponzik-vst3`.
//! Sans l'artefact, les tests s'arrêtent en le signalant.
//!
//! Le plugin interrompt le processus si l'hôte libère un objet en trop ou en
//! laisse un vivant au déchargement du module : chaque test qui se termine
//! valide donc aussi le comptage de références.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};

use ponzik_engine::{EditorEvent, ParentWindow, StateContext};
use ponzik_vst3::abi::uid;
use ponzik_vst3::{
    class_id_hex, create_effect, create_instrument, effect_from_kind, instrument_from_kind, kind_for, scan_dir,
};

const SR: f32 = 48_000.0;

fn gain_id() -> String {
    class_id_hex(&uid(0x504F4E5A, 0x494B5633, 0x4741494E, 0x00000001))
}

fn sine_id() -> String {
    class_id_hex(&uid(0x504F4E5A, 0x494B5633, 0x53494E45, 0x00000003))
}

/// Paquet `test.vst3` (ou fichier unique) dans un dossier temporaire, supprimé en fin de test.
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

/// Dossier d'architecture d'un paquet pour la plateforme courante.
fn arch_dir() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(windows) {
        format!("{}-win", if arch == "aarch64" { "arm64" } else { arch })
    } else {
        format!("{}-linux", if arch == "x86" { "i386" } else { arch })
    }
}

/// Copie l'artefact dans un dossier temporaire propre au test : paquet
/// `test.vst3/Contents/<arch>/test.<so|vst3>` si `bundle`, sinon fichier `test.vst3`.
fn fixture(test: &str, bundle: bool) -> Option<Fixture> {
    let name = format!("{DLL_PREFIX}ponzik_vst3_test_plugin{DLL_SUFFIX}");
    // target/<profil>/deps/<test> → target/<profil>/<artefact>
    let exe = std::env::current_exe().ok()?;
    let built = exe.parent()?.parent()?.join(&name);
    if !built.is_file() {
        eprintln!("note : {} absent, lancer `cargo build -p ponzik-vst3-test-plugin` ; test ignoré", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-vst3-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("test.vst3");
    let dst = if bundle {
        let bin = root.join("Contents").join(arch_dir());
        std::fs::create_dir_all(&bin).ok()?;
        bin.join(if cfg!(windows) { "test.vst3" } else { "test.so" })
    } else {
        std::fs::create_dir_all(&dir).ok()?;
        root.clone()
    };
    std::fs::copy(&built, &dst).ok()?;
    Some(Fixture(root))
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn scan_lists_audio_classes_only() {
    let Some(path) = fixture("scan", true) else { return };
    let mut found = scan_dir(path.parent().unwrap());
    found.sort_by(|a, b| a.id.cmp(&b.id));
    // La classe du contrôleur séparé n'est pas un plugin à part entière.
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!((found[0].id.clone(), found[0].is_instrument), (gain_id(), false));
    assert_eq!((found[1].id.clone(), found[1].is_instrument), (sine_id(), true));
    assert_eq!(found[0].name, "Gain de test");
    assert_eq!(found[1].name, "Sinus de test");
    assert_eq!(found[1].vendor, "Ponzik");
    assert_eq!(found[1].categories, "Instrument|Synth");
    assert_eq!(found[1].path, *path);
    assert_eq!(found[1].kind(), kind_for(&path, &sine_id()));
}

#[test]
fn single_file_module_is_accepted() {
    let Some(path) = fixture("fichier", false) else { return };
    assert_eq!(scan_dir(path.parent().unwrap()).len(), 2);
    let mut fx = create_effect(&path, &gain_id()).unwrap();
    fx.set_sample_rate(SR);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    fx.process(&mut l, &mut r);
    assert!((l[10] - 0.5).abs() < 1e-6);
}

#[test]
fn gain_param_halves_the_level() {
    let Some(path) = fixture("gain", true) else { return };
    let id = gain_id();
    let mut fx = create_effect(&path, &id).unwrap();
    assert_eq!(fx.kind(), kind_for(&path, &id));
    let params = fx.params();
    assert_eq!(params.len(), 1);
    assert_eq!((params[0].id, params[0].name, params[0].min, params[0].max), (7, "Gain", 0.0, 1.0));
    // Valeur lue sur le contrôleur séparé après transmission de l'état du composant.
    assert_eq!(fx.get_param(7), 0.5);
    assert_eq!(fx.latency(), 0, "inactif : latence inconnue");
    fx.set_sample_rate(SR);
    // `getLatencySamples` du banc d'essai (`LATENCY`), lu à l'activation.
    assert_eq!(fx.latency(), 32);

    // Le gain n'est audible que si contrôleur et processeur ont été connectés.
    let input: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin()).collect();
    let (mut l, mut r) = (input.clone(), input.clone());
    fx.process(&mut l, &mut r);
    assert!((rms(&l) - rms(&input)).abs() < 1e-6);

    fx.set_param(7, 0.25);
    assert_eq!(fx.get_param(7), 0.25);
    // Bloc plus long que `MAX_BLOCK` : découpé en plusieurs appels.
    let long: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.05).sin()).collect();
    let (mut l, mut r) = (long.clone(), long.clone());
    fx.process(&mut l, &mut r);
    assert!((rms(&l) / rms(&long) - 0.5).abs() < 1e-5);
    assert_eq!(l, r);

    // Changement de fréquence en cours de route : réactivation propre.
    fx.reset();
    fx.set_sample_rate(44_100.0);
    let (mut l, mut r) = (vec![1.0; 32], vec![1.0; 32]);
    fx.process(&mut l, &mut r);
    assert!((l[0] - 0.5).abs() < 1e-6);
}

/// Fields of a test plugin's state: (gain, blocks, last key, controller edits).
fn test_state(b: &[u8]) -> (f64, u32, i16, u32) {
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    assert_eq!((&b[..4], u32_at(4), u32_at(8), b.len()), (&b"PZV3"[..], 1, 14, 12 + 14 + 4 + 4), "{b:?}");
    assert_eq!(u32_at(26), 4);
    (f64::from_le_bytes(b[12..20].try_into().unwrap()), u32_at(20), i16::from_le_bytes([b[24], b[25]]), u32_at(30))
}

#[test]
fn state_round_trips_component_and_controller() {
    let Some(path) = fixture("etat", true) else { return };
    let mut fx = create_effect(&path, &gain_id()).unwrap();
    fx.set_sample_rate(SR);
    fx.set_param(7, 0.25);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    for _ in 0..3 {
        fx.process(&mut l, &mut r);
    }
    let ed = fx.editor().unwrap();
    // The controller's own state: one host change reached it.
    ed.param_changed(7, 0.25);
    let state = ed.save_state(StateContext::Project).expect("state");
    assert_eq!(test_state(&state), (0.5, 3, -1, 1));

    // A new instance restored before activation: parameter read back from
    // the controller, hidden values of both halves back.
    let mut copy = create_effect(&path, &gain_id()).unwrap();
    assert_eq!(copy.get_param(7), 0.5);
    copy.load_state(&state).unwrap();
    assert_eq!(copy.get_param(7), 0.25);
    let copy_ed = copy.editor().unwrap();
    assert_eq!(copy_ed.save_state(StateContext::Project).unwrap(), state);
    copy.set_sample_rate(SR);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    copy.process(&mut l, &mut r);
    assert!((l[10] - 0.25).abs() < 1e-6, "restored gain applies: {}", l[10]);
    assert!(copy.load_state(b"PZV3 garbage").is_err());

    // Loaded live through the editor: values come back for the host.
    let mut louder = state.clone();
    louder[12..20].copy_from_slice(&1.5f64.to_le_bytes());
    assert_eq!(ed.load_state(&louder, StateContext::Preset).unwrap(), [(7, 0.75)]);
    drop(fx);
    assert!(ed.save_state(StateContext::Project).is_none(), "instance gone");
    assert!(ed.load_state(&state, StateContext::Project).is_err());

    // Single-object instrument: the last key played comes back.
    let mut sine = create_instrument(&path, &sine_id()).unwrap();
    sine.set_sample_rate(SR);
    sine.note_on(64, 1.0);
    sine.render(&mut l, &mut r);
    let state = sine.editor().unwrap().save_state(StateContext::Project).unwrap();
    assert_eq!(test_state(&state).2, 64);
    let mut copy = create_instrument(&path, &sine_id()).unwrap();
    copy.load_state(&state).unwrap();
    assert_eq!(copy.editor().unwrap().save_state(StateContext::Project).unwrap(), state);
}

#[test]
fn two_instances_share_one_module() {
    let Some(path) = fixture("partage", true) else { return };
    let a = create_effect(&path, &gain_id()).unwrap();
    let b = create_instrument(&path, &sine_id()).unwrap();
    assert!(std::ptr::eq(a.params(), create_effect(&path, &gain_id()).unwrap().params()));
    drop(a);
    drop(b);
    // Module déchargé puis rechargé.
    assert!(create_effect(&path, &gain_id()).is_ok());
}

#[test]
fn instrument_plays_the_right_pitch() {
    let Some(path) = fixture("sinus", true) else { return };
    let mut inst = instrument_from_kind(&kind_for(&path, &sine_id())).unwrap();
    assert!(inst.params().is_empty());
    assert!(inst.is_melodic());
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

#[test]
fn wrong_kinds_are_errors() {
    let Some(path) = fixture("erreurs", true) else { return };
    // Classe inconnue, effet chargé comme effet via son identifiant de projet.
    assert!(create_effect(&path, "00000000000000000000000000000000").is_err());
    assert!(effect_from_kind(&kind_for(&path, &gain_id())).is_ok());
    assert!(effect_from_kind("vst3:sans-identifiant").is_err());
}

#[test]
fn renamed_bundle_finds_its_binary() {
    let Some(path) = fixture("renamed", true) else { return };
    let bin = path.join("Contents").join(arch_dir());
    let ext = if cfg!(windows) { "vst3" } else { "so" };
    std::fs::rename(bin.join(format!("test.{ext}")), bin.join(format!("other.{ext}"))).unwrap();
    assert_eq!(scan_dir(path.parent().unwrap()).len(), 2);
}

#[cfg(target_os = "macos")]
const PARENT: ParentWindow = ParentWindow::Cocoa(0x1234);
#[cfg(windows)]
const PARENT: ParentWindow = ParentWindow::Win32(0x1234);
#[cfg(not(any(target_os = "macos", windows)))]
const PARENT: ParentWindow = ParentWindow::X11(0x1234);

#[test]
fn editor_lifecycle_run_loop_and_edits() {
    let Some(path) = fixture("gui", true) else { return };
    // The sine's controller has no view: opening fails cleanly.
    let inst = create_instrument(&path, &sine_id()).unwrap();
    let ed = inst.editor().expect("every VST 3 instance has an editor handle");
    assert!(ed.open(PARENT, 1.0).is_err());
    assert!(!ed.is_open());
    drop(inst);
    assert!(!ed.is_alive());

    let mut fx = create_effect(&path, &gain_id()).unwrap();
    fx.set_sample_rate(SR);
    let ed = fx.editor().unwrap();
    assert!(ed.has_gui() && ed.is_alive());
    // Scale 2 through IPlugViewContentScaleSupport: the view doubles.
    let size = ed.open(PARENT, 2.0).unwrap();
    assert_eq!((size.width, size.height, size.resizable), (600, 400, true));
    assert!(ed.is_open() && ed.resizable());
    // The user's size goes through checkSizeConstraint (10 px steps, at least 100 × 80).
    assert_eq!(ed.set_size(455, 50), Some((450, 80)));

    let mut ev = Vec::new();
    if cfg!(target_os = "linux") {
        // IRunLoop: the timer reports an edit, the readable pipe asks for a resize.
        std::thread::sleep(std::time::Duration::from_millis(5));
        ed.idle(&mut ev);
        assert_eq!(
            ev,
            [
                EditorEvent::GestureBegin(7),
                EditorEvent::Param { id: 7, value: 0.125 },
                EditorEvent::GestureEnd(7),
                EditorEvent::Resize { width: 420, height: 260 },
            ]
        );
        // The size the plugin asked for is applied as is.
        assert_eq!(ed.set_size(420, 260), Some((420, 260)));
        // The host's echo of the edit is not sent back; its own changes are.
        ed.param_changed(7, 0.125);
        ed.param_changed(7, 0.75);
    }

    ed.close();
    assert!(!ed.is_open() && !ed.resizable());
    assert_eq!(ed.set_size(300, 200), None);
    ev.clear();
    std::thread::sleep(std::time::Duration::from_millis(5));
    ed.idle(&mut ev);
    assert!(ev.is_empty(), "{ev:?}");

    // Destroyed with its view attached: the host removes it first (the plugin
    // aborts otherwise) and gives every reference back (checked at ModuleExit).
    ed.open(PARENT, 1.0).unwrap();
    drop(fx);
    assert!(!ed.is_alive() && !ed.is_open() && !ed.has_gui());
    assert!(ed.open(PARENT, 1.0).is_err());
}
