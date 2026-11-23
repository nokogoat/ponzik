//! The plugin scan through the real `ponzik scan-plugin` child process, with
//! the CLAP test plugin (built by `cargo build -p ponzik-clap-test-plugin`;
//! skipped without it). `PONZIK_TEST_PLUGIN_CRASH` makes that plugin abort
//! while loading: the scan must report it and carry on.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ponzik_project::plugins::{Format, scan_paths};

/// `test.clap` in a folder of its own, removed at the end.
struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn fixture(test: &str) -> Option<Fixture> {
    let name = format!("{DLL_PREFIX}ponzik_clap_test_plugin{DLL_SUFFIX}");
    let built = Path::new(env!("CARGO_BIN_EXE_ponzik")).parent()?.join(name);
    if !built.is_file() {
        eprintln!("note: {} missing, run `cargo build -p ponzik-clap-test-plugin`; skipped", built.display());
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ponzik-scan-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("test.clap");
    std::fs::copy(&built, &dst).ok()?;
    Some(Fixture(dst))
}

fn scanner(crash: bool) -> impl Fn(&Path) -> Command + Sync {
    move |path: &Path| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ponzik"));
        c.arg("scan-plugin").arg(path).env("PONZIK_LANG", "en");
        if crash {
            c.env("PONZIK_TEST_PLUGIN_CRASH", "1");
        } else {
            c.env_remove("PONZIK_TEST_PLUGIN_CRASH");
        }
        c
    }
}

#[test]
fn child_process_scan_lists_the_plugins() {
    let Some(f) = fixture("ok") else { return };
    let r = scan_paths(vec![(Format::Clap, f.0.clone())], &scanner(false), None, false, Duration::from_secs(30));
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let mut ids: Vec<_> = r.clap.iter().map(|p| (p.id.as_str(), p.is_instrument)).collect();
    ids.sort();
    assert_eq!(ids, [("ponzik.test.gain", false), ("ponzik.test.sine", true)]);
    assert!(r.clap.iter().all(|p| p.path == f.0 && p.vendor == "Ponzik"));
}

#[test]
fn a_crashing_plugin_does_not_take_the_scan_down() {
    let Some(f) = fixture("crash") else { return };
    let cache = f.0.with_file_name("cache.txt");
    let r = scan_paths(vec![(Format::Clap, f.0.clone())], &scanner(true), Some(&cache), false, Duration::from_secs(30));
    assert!(r.clap.is_empty());
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].path, f.0);
    #[cfg(unix)]
    assert_eq!(r.errors[0].message, "crashed while loading (signal 6)");
    // Remembered: the next start does not load it again; a rescan does.
    let again = scan_paths(vec![(Format::Clap, f.0.clone())], &scanner(true), Some(&cache), false, Duration::from_secs(30));
    assert_eq!((again.scanned, again.errors.len()), (0, 1));
    let fixed = scan_paths(vec![(Format::Clap, f.0.clone())], &scanner(false), Some(&cache), true, Duration::from_secs(30));
    assert_eq!((fixed.scanned, fixed.clap.len(), fixed.errors.len()), (1, 2, 0));
}

#[test]
fn scan_plugin_subcommand_reports_errors() {
    let out = Command::new(env!("CARGO_BIN_EXE_ponzik"))
        .args(["scan-plugin", "/nonexistent/x.clap"])
        .env("PONZIK_LANG", "en")
        .output()
        .expect("ponzik runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("PONZIK-SCAN\terror\t"));
}

/// A library built next to `ponzik`, or `None` (said) when missing.
fn built(name: &str) -> Option<PathBuf> {
    let file = format!("{DLL_PREFIX}{name}{DLL_SUFFIX}");
    let p = Path::new(env!("CARGO_BIN_EXE_ponzik")).parent()?.join(file);
    if !p.is_file() {
        eprintln!("note: {} missing, run `cargo build -p {}`; skipped", p.display(), name.replace('_', "-"));
        return None;
    }
    Some(p)
}

#[test]
fn lv2_bundles_are_scanned_in_a_child_with_skipped_plugins_reported() {
    let Some(lib) = built("ponzik_lv2_test_plugin") else { return };
    let dir = std::env::temp_dir().join(format!("ponzik-scan-{}-lv2", std::process::id()));
    let b = dir.join("test.lv2");
    std::fs::create_dir_all(&b).unwrap();
    let name = lib.file_name().unwrap().to_string_lossy().into_owned();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/ponzik-lv2-test-plugin/bundle");
    for f in ["manifest.ttl", "test.ttl", "presets.ttl"] {
        std::fs::write(b.join(f), std::fs::read_to_string(src.join(f)).unwrap().replace("@BINARY@", &name)).unwrap();
    }
    std::fs::copy(&lib, b.join(&name)).unwrap();
    let cache = dir.join("cache.txt");
    let r = scan_paths(vec![(Format::Lv2, b.clone())], &scanner(false), Some(&cache), false, Duration::from_secs(30));
    let mut ids: Vec<_> = r.lv2.iter().map(|p| (p.id.as_str(), p.is_instrument)).collect();
    ids.sort();
    assert_eq!(ids, [("urn:ponzik:test:fixed", false), ("urn:ponzik:test:gain", false), ("urn:ponzik:test:synth", true)]);
    assert_eq!(r.errors.len(), 1);
    assert_eq!((r.errors[0].name.as_str(), r.errors[0].message.as_str()), ("Ponzik Test Magic", "needs the feature urn:ponzik:magic"));
    // From the cache the second time, skipped plugin included.
    let again = scan_paths(vec![(Format::Lv2, b.clone())], &scanner(false), Some(&cache), false, Duration::from_secs(30));
    assert_eq!((again.scanned, again.lv2.len(), again.errors.len()), (0, 3, 1));
    assert_eq!(again.errors[0].name, "Ponzik Test Magic");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ladspa_libraries_are_scanned_in_a_child_and_crashes_contained() {
    let Some(lib) = built("ponzik_ladspa_test_plugin") else { return };
    let dir = std::env::temp_dir().join(format!("ponzik-scan-{}-ladspa", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("test{DLL_SUFFIX}"));
    std::fs::copy(&lib, &f).unwrap();
    let r = scan_paths(vec![(Format::Ladspa, f.clone())], &scanner(false), None, false, Duration::from_secs(30));
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let got: Vec<_> = r.ladspa.iter().map(|p| (p.id.as_str(), p.label.as_str())).collect();
    assert_eq!(got, [("4201", "ponzik_gain"), ("4202", "ponzik_lowpass")]);
    let crashed = scan_paths(vec![(Format::Ladspa, f.clone())], &scanner(true), None, false, Duration::from_secs(30));
    assert!(crashed.ladspa.is_empty());
    #[cfg(unix)]
    assert_eq!(crashed.errors[0].message, "crashed while loading (signal 6)");
    let _ = std::fs::remove_dir_all(dir);
}
