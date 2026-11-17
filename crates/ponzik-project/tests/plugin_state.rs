//! A plugin's state saved in a project comes back when the project builds its
//! instances. Uses the CLAP test plugin: `cargo build -p ponzik-clap-test-plugin`
//! first (without it, the test stops and says so).

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::PathBuf;

use ponzik_engine::StateContext;
use ponzik_project::{EffectDef, InstrumentDef, Project, io};

/// `test.clap` copied into a temporary folder of its own.
fn fixture(test: &str) -> Option<PathBuf> {
    let name = format!("{DLL_PREFIX}ponzik_clap_test_plugin{DLL_SUFFIX}");
    let built = std::env::current_exe().ok()?.parent()?.parent()?.join(name);
    if !built.is_file() {
        eprintln!("note: {} missing, run `cargo build -p ponzik-clap-test-plugin`; test skipped", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-project-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("test.clap");
    std::fs::copy(&built, &dst).ok()?;
    Some(dst)
}

#[test]
fn project_restores_plugin_states() {
    let Some(path) = fixture("state") else { return };
    let gain = ponzik_clap::kind_for(&path, "ponzik.test.gain");
    let sine = ponzik_clap::kind_for(&path, "ponzik.test.sine");

    // A gain at 0.5 that processed 3 blocks; a sine that last played 64.
    let mut fx = Project::make_effect(&EffectDef::new(&gain), 48_000.0, 120.0).unwrap();
    fx.set_param(7, 0.5);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    for _ in 0..3 {
        fx.process(&mut l, &mut r);
    }
    let fx_state = fx.editor().unwrap().save_state(StateContext::Project).unwrap();
    let mut inst = Project::make_instrument(&InstrumentDef::new(&sine), 48_000.0, &|_| None);
    inst.note_on(64, 1.0);
    inst.render(&mut l, &mut r);
    let inst_state = inst.editor().unwrap().save_state(StateContext::Project).unwrap();

    // Through the project file and back.
    let mut p = Project::empty();
    let ch = p.add_channel("synth", InstrumentDef { state: Some(inst_state.clone().into()), ..InstrumentDef::new(&sine) }, 60);
    let mut def = EffectDef::new(&gain);
    def.state = Some(fx_state.clone().into());
    p.tracks[1].inserts[0] = Some(def);
    let p = io::load(&io::save(&p)).unwrap();

    let fx = Project::make_effect(p.tracks[1].inserts[0].as_ref().unwrap(), 48_000.0, 120.0).unwrap();
    assert_eq!(fx.get_param(7), 0.5, "value read back from the restored plugin");
    assert_eq!(fx.editor().unwrap().save_state(StateContext::Project).unwrap(), fx_state);
    let inst = Project::make_instrument(&p.channels[ch].instrument, 48_000.0, &|_| None);
    assert_eq!(inst.editor().unwrap().save_state(StateContext::Project).unwrap(), inst_state);

    // Values saved after the state (an undo step) win over it.
    let mut newer = p.tracks[1].inserts[0].clone().unwrap();
    newer.set(7, 1.5);
    let mut fx = Project::make_effect(&newer, 48_000.0, 120.0).unwrap();
    assert_eq!(fx.get_param(7), 1.5);
    let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
    fx.process(&mut l, &mut r);
    assert!((l[0] - 0.75).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
