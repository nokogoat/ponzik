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
