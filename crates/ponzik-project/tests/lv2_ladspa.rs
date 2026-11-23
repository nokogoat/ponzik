//! LV2 and LADSPA devices in a project: built from their kind, their state
//! and values back after a save and a load. Uses the test plugins
//! (`cargo build -p ponzik-lv2-test-plugin -p ponzik-ladspa-test-plugin`
//! first; without them, the tests stop and say so).

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};

use ponzik_engine::StateContext;
use ponzik_project::plugins::{Format, plugin_id};
use ponzik_project::{EffectDef, InstrumentDef, Project, io, presets};

fn built(name: &str) -> Option<PathBuf> {
    let file = format!("{DLL_PREFIX}{name}{DLL_SUFFIX}");
    let p = std::env::current_exe().ok()?.parent()?.parent()?.join(file);
    if !p.is_file() {
        eprintln!("note: {} missing, run `cargo build -p {}`; skipped", p.display(), name.replace('_', "-"));
        return None;
    }
    Some(p)
}

fn temp(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ponzik-project-plugins-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The LV2 test bundle in `dir`.
fn lv2_bundle(dir: &Path) -> Option<PathBuf> {
    let lib = built("ponzik_lv2_test_plugin")?;
    let name = lib.file_name()?.to_string_lossy().into_owned();
    let b = dir.join("test.lv2");
    std::fs::create_dir_all(&b).ok()?;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ponzik-lv2-test-plugin/bundle");
    for f in ["manifest.ttl", "test.ttl", "presets.ttl"] {
        std::fs::write(b.join(f), std::fs::read_to_string(src.join(f)).ok()?.replace("@BINARY@", &name)).ok()?;
    }
    std::fs::copy(&lib, b.join(&name)).ok()?;
    Some(b)
}

#[test]
fn lv2_devices_come_back_with_their_state() {
    let dir = temp("lv2");
    let Some(b) = lv2_bundle(&dir) else { return };
    // The scan tells this process where the bundle is (it is not in the search folders).
    ponzik_lv2::scan_bundle(&b).unwrap();
    let gain = ponzik_lv2::kind_for("urn:ponzik:test:gain");
    let synth = ponzik_lv2::kind_for("urn:ponzik:test:synth");
    assert_eq!(Format::of_kind(&gain), Some(Format::Lv2));
    assert_eq!(plugin_id(&gain), Some("urn:ponzik:test:gain"));
    assert_eq!(presets::preset_folder(&gain), "urn:ponzik:test:gain");

    let mut fx = Project::make_effect(&EffectDef::new(&gain), 48_000.0, 120.0).unwrap();
    fx.set_param(4, 0.5);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    for _ in 0..3 {
        fx.process(&mut l, &mut r);
    }
    let fx_state = fx.editor().unwrap().save_state(StateContext::Project).unwrap();
    let inst = Project::make_instrument(&InstrumentDef::new(&synth), 48_000.0, &|_| None);
    assert_eq!(inst.kind(), synth, "the LV2 synth, not the stand-in");

    let mut p = Project::empty();
    p.add_channel("synth", InstrumentDef::new(&synth), 60);
    let mut def = EffectDef::new(&gain);
    def.state = Some(fx_state.clone().into());
    p.tracks[1].inserts[0] = Some(def);
    let p = io::load(&io::save(&p)).unwrap();

    let mut fx = Project::make_effect(p.tracks[1].inserts[0].as_ref().unwrap(), 48_000.0, 120.0).unwrap();
    assert_eq!(fx.get_param(4), 0.5, "value read back from the state");
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    fx.process(&mut l, &mut r);
    assert!((l[0] - 0.25).abs() < 1e-6);
    let inst = Project::make_instrument(&p.channels.last().unwrap().instrument, 48_000.0, &|_| None);
    assert_eq!(inst.kind(), synth);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ladspa_effects_come_back_with_their_values() {
    let dir = temp("ladspa");
    let Some(lib) = built("ponzik_ladspa_test_plugin") else { return };
    let path = dir.join(format!("test{DLL_SUFFIX}"));
    std::fs::copy(&lib, &path).unwrap();
    assert_eq!(Format::of(&path), Some(Format::Ladspa));
    let kind = ponzik_ladspa::kind_for(&path, "4201");
    assert_eq!(plugin_id(&kind), Some("4201"));
    let mut def = EffectDef::new(&kind);
    def.set(4, 0.25);
    let mut p = Project::empty();
    p.tracks[1].inserts[0] = Some(def);
    let p = io::load(&io::save(&p)).unwrap();
    let mut fx = Project::make_effect(p.tracks[1].inserts[0].as_ref().unwrap(), 48_000.0, 120.0).unwrap();
    assert_eq!(fx.get_param(4), 0.25);
    let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
    fx.process(&mut l, &mut r);
    assert!((l[0] - 0.25).abs() < 1e-6);
    assert!(ponzik_project::plugins::instrument_from_kind(&kind).unwrap().is_err(), "LADSPA: effects only");
    let _ = std::fs::remove_dir_all(dir);
}
