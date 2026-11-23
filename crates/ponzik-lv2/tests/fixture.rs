//! Tests with the test bundle. Needs `cargo build -p ponzik-lv2-test-plugin`
//! first; without the library the tests stop and say so.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ponzik_engine::{StateContext, Unit};
use ponzik_lv2::{create_effect, create_instrument, kind_for, open_effect, presets_of, scan_bundle};

const SR: f32 = 48_000.0;

/// `test.lv2` in a temporary folder of its own, removed at the end.
struct Bundle(PathBuf);

impl Drop for Bundle {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl std::ops::Deref for Bundle {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

fn bundle(test: &str) -> Option<Bundle> {
    let name = format!("{DLL_PREFIX}ponzik_lv2_test_plugin{DLL_SUFFIX}");
    // target/<profile>/deps/<test> → target/<profile>/<library>
    let built = std::env::current_exe().ok()?.parent()?.parent()?.join(&name);
    if !built.is_file() {
        eprintln!("note: {} missing, run `cargo build -p ponzik-lv2-test-plugin`; skipped", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-lv2-{}-{test}", std::process::id())).join("test.lv2");
    std::fs::create_dir_all(&dir).ok()?;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ponzik-lv2-test-plugin/bundle");
    for f in ["manifest.ttl", "test.ttl", "presets.ttl"] {
        let text = std::fs::read_to_string(src.join(f)).ok()?.replace("@BINARY@", &name);
        std::fs::write(dir.join(f), text).ok()?;
    }
    std::fs::copy(&built, dir.join(&name)).ok()?;
    Some(Bundle(dir))
}

const GAIN: &str = "urn:ponzik:test:gain";
const SYNTH: &str = "urn:ponzik:test:synth";

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn scan_lists_what_it_can_host() {
    let Some(b) = bundle("scan") else { return };
    let (plugins, skipped) = scan_bundle(&b).unwrap();
    let got: Vec<_> = plugins.iter().map(|p| (p.id.as_str(), p.is_instrument, p.name.as_str())).collect();
    assert_eq!(
        got,
        [("urn:ponzik:test:fixed", false, "Ponzik Test Fixed"), (GAIN, false, "Ponzik Test Gain"), (SYNTH, true, "Ponzik Test Synth")]
    );
    assert_eq!(plugins[1].vendor, "Ponzik");
    assert_eq!(plugins[1].path, *b);
    assert_eq!(skipped.len(), 1);
    assert_eq!((skipped[0].name.as_str(), skipped[0].reason.as_str()), ("Ponzik Test Magic", "needs the feature urn:ponzik:magic"));
}

#[test]
fn ports_become_parameters() {
    let Some(b) = bundle("params") else { return };
    let fx = create_effect(&b, GAIN).unwrap();
    assert_eq!(fx.kind(), kind_for(GAIN));
    let p = fx.params();
    let names: Vec<_> = p.iter().map(|p| p.name).collect();
    // Outputs and `notOnGUI` ports are not parameters.
    assert_eq!(names, ["Gain", "Mode", "Frequency", "Mute", "Delay"]);
    assert_eq!((p[0].id, p[0].min, p[0].max, p[0].default), (4, 0.0, 2.0, 1.0));
    assert_eq!((p[1].unit, p[1].choices), (Unit::Choice, &["One", "Two", "Four"][..]));
    // A fraction of the sample rate shows in hertz, logarithmic.
    assert_eq!(p[2].unit, Unit::Hz);
    assert!(p[2].log);
    assert!((p[2].default - 480.0).abs() < 1e-3 && (p[2].max - 24_000.0).abs() < 1e-2);
    assert_eq!((p[3].unit, p[3].choices.len()), (Unit::Choice, 2));
    // Milliseconds become seconds.
    assert_eq!(p[4].unit, Unit::Seconds);
    assert!((p[4].default - 0.02).abs() < 1e-6);
    assert_eq!(fx.get_param(5), 0.0, "mode One");
}

#[test]
fn gain_processes_and_the_worker_answers() {
    let Some(b) = bundle("gain") else { return };
    let mut fx = open_effect(&b, GAIN).unwrap();
    use ponzik_engine::{Effect, Params};
    fx.set_sample_rate(SR);
    fx.set_param(4, 0.5);
    let (mut l, mut r) = (vec![0.8f32; 512], vec![-0.4f32; 512]);
    fx.process(&mut l, &mut r);
    assert!(l.iter().all(|v| (v - 0.4).abs() < 1e-6) && r.iter().all(|v| (v + 0.2).abs() < 1e-6));
    assert_eq!(fx.latency(), 32, "read from the latency port");
    // Mode Four (index 2): the plugin schedules work, the answer comes in a later block.
    fx.set_param(5, 2.0);
    let t0 = Instant::now();
    while fx.port_value("worked") != Some(40.0) {
        assert!(t0.elapsed() < Duration::from_secs(5), "the worker never answered");
        fx.process(&mut l, &mut r);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fx.port_value("mode"), Some(4.0));
    fx.set_param(7, 1.0);
    let (mut l, mut r) = (vec![0.8f32; 64], vec![0.8f32; 64]);
    fx.process(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|&v| v == 0.0), "muted");
    // Blocks longer than the longest the plugin takes are cut.
    fx.set_param(7, 0.0);
    let (mut l, mut r) = (vec![1.0f32; 10_000], vec![1.0f32; 10_000]);
    fx.process(&mut l, &mut r);
    assert!(l.iter().all(|v| (v - 0.5).abs() < 1e-6));
}

#[test]
fn state_round_trips_with_the_plugin_properties() {
    let Some(b) = bundle("state") else { return };
    let mut fx = create_effect(&b, GAIN).unwrap();
    fx.set_sample_rate(SR);
    fx.set_param(4, 1.5);
    fx.set_param(6, 1_000.0);
    let (mut l, mut r) = (vec![0.1f32; 128], vec![0.1f32; 128]);
    for _ in 0..3 {
        fx.process(&mut l, &mut r);
    }
    let ed = fx.editor().unwrap();
    assert!(!ed.has_gui());
    let state = ed.save_state(StateContext::Project).expect("state");

    let mut copy = create_effect(&b, GAIN).unwrap();
    copy.load_state(&state).unwrap();
    assert_eq!(copy.get_param(4), 1.5);
    assert!((copy.get_param(6) - 1_000.0).abs() < 0.01);
    copy.set_sample_rate(SR);
    copy.process(&mut l, &mut r);
    // The hidden counter came back through `state:interface`: 3 blocks, then one more.
    let again = copy.editor().unwrap().save_state(StateContext::Project).unwrap();
    let blocks = |s: &[u8]| {
        let key = b"urn:ponzik:test:gain#blocks";
        let at = s.windows(key.len()).position(|w| w == key).expect("blocks property");
        let rest = &s[at + key.len()..];
        // Type string, flags, then the value's length and the value.
        let tlen = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let v = &rest[4 + tlen + 4 + 4..];
        i32::from_le_bytes(v[..4].try_into().unwrap())
    };
    assert_eq!((blocks(&state), blocks(&again)), (3, 4));
    assert!(copy.load_state(b"garbage").is_err());

    // Loaded live through the editor: values come back, the plugin hears them at its next block.
    let live = copy.editor().unwrap();
    let values = live.load_state(&state, StateContext::Preset).unwrap();
    assert!(values.contains(&(4, 1.5)));
    drop(copy);
    assert!(!live.is_alive());
    assert!(live.save_state(StateContext::Project).is_none());
}

#[test]
fn a_new_sample_rate_keeps_the_settings() {
    let Some(b) = bundle("rate") else { return };
    let mut fx = create_effect(&b, GAIN).unwrap();
    fx.set_sample_rate(48_000.0);
    fx.set_param(6, 2_000.0);
    fx.set_param(4, 0.25);
    fx.set_sample_rate(44_100.0);
    assert_eq!(fx.get_param(4), 0.25);
    assert!((fx.get_param(6) - 2_000.0).abs() < 0.1, "{}", fx.get_param(6));
    let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
    fx.process(&mut l, &mut r);
    assert!((l[0] - 0.25).abs() < 1e-6, "the new instance runs");
}

#[test]
fn synth_plays_midi_notes() {
    let Some(b) = bundle("synth") else { return };
    let mut inst = create_instrument(&b, SYNTH).unwrap();
    inst.set_sample_rate(SR);
    inst.set_param(3, 1.0);
    let (mut l, mut r) = (vec![0.0f32; 48_000], vec![0.0f32; 48_000]);
    inst.note_on(69, 1.0);
    for (bl, br) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        inst.render(bl, br);
    }
    let crossings = l.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    assert!((crossings as i32 - 440).abs() <= 1, "A4: {crossings}");
    assert!((rms(&l) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
    inst.note_off(69);
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    inst.render(&mut l, &mut r);
    assert_eq!(rms(&l), 0.0);
    // Notes held then the panic: silence.
    inst.note_on(60, 1.0);
    inst.all_notes_off();
    inst.render(&mut l, &mut r);
    assert_eq!(rms(&l), 0.0);
}

#[test]
fn fixed_blocks_go_through_a_fifo() {
    let Some(b) = bundle("fixed") else { return };
    let mut fx = open_effect(&b, "urn:ponzik:test:fixed").unwrap();
    use ponzik_engine::Effect;
    fx.set_sample_rate(SR);
    assert_eq!(fx.latency(), ponzik_lv2::FIXED_BLOCK);
    let input: Vec<f32> = (0..3_000).map(|i| (i as f32 * 0.01).sin()).collect();
    let (mut l, mut r) = (input.clone(), input.clone());
    // Ragged blocks from the host, fixed ones for the plugin.
    let mut at = 0;
    for n in [100, 37, 500, 1, 256, 1000, 1106] {
        let end = at + n;
        let (bl, br) = (&mut l[at..end], &mut r[at..end]);
        fx.process(bl, br);
        at = end;
    }
    let d = ponzik_lv2::FIXED_BLOCK;
    assert!(l[..d].iter().all(|&v| v == 0.0));
    for i in d..3_000 {
        assert!((l[i] - 0.5 * (input[i - d] + input[i - d])).abs() < 1e-6, "{i}");
    }
    assert_eq!(fx.port_value("bad"), Some(0.0), "every block had the nominal length");
}

#[test]
fn presets_carry_values_in_ponzik_units() {
    let Some(b) = bundle("presets") else { return };
    // Through the instance (its own bundle), then by kind (the bundle this
    // process knows for the URI: other tests' copies may come and go).
    let presets = open_effect(&b, GAIN).unwrap().presets();
    assert!(presets_of(&kind_for(GAIN)).iter().all(|p| p.0 == "Half"));
    let half = presets.iter().find(|p| p.0 == "Half").unwrap_or_else(|| panic!("{presets:?}"));
    assert!(half.1.contains(&(4, 0.5)));
    assert!(half.1.contains(&(5, 2.0)), "mode Four is the third choice");
}
