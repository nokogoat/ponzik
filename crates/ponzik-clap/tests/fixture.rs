//! Tests avec le plugin de banc d'essai. Prérequis :
//! `cargo build -p ponzik-clap-test-plugin` avant `cargo test -p ponzik-clap`.
//! Sans l'artefact, les tests s'arrêtent en le signalant.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};

use ponzik_clap::{create_effect, create_instrument, kind_for, scan_dir};

const SR: f32 = 48_000.0;

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
