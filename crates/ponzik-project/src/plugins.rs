//! Plugin scan in child processes, with a cache.
//!
//! Loading a plugin runs its code: a broken one can crash or hang the process
//! that loads it. Each `.clap` file and `.vst3` bundle is therefore scanned by
//! a child process (`ponzik scan-plugin <path>`, see [`scan_child`]) watched
//! with a timeout; its answer is kept in a cache keyed by the file's size and
//! date, so only new or changed plugins are loaded again.
//!
//! The child writes one line per plugin found, prefixed so that what plugins
//! print themselves on standard output is ignored.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use ponzik_i18n::{tr, tr_fmt};

/// Prefix of the child's answer lines.
const PREFIX: &str = "PONZIK-SCAN";
const CACHE_HEADER: &str = "ponzik-plugin-cache 1";
/// Default time a plugin gets to load (bridged Windows plugins start Wine).
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Clap,
    Vst3,
    Lv2,
    Ladspa,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Clap, Format::Vst3, Format::Lv2, Format::Ladspa];

    /// Format of a plugin file or bundle, from its extension (a shared
    /// library is a LADSPA one: the other formats have their own extension).
    pub fn of(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "clap" => Some(Format::Clap),
            "vst3" => Some(Format::Vst3),
            "lv2" => Some(Format::Lv2),
            _ if ponzik_ladspa::is_library(path) => Some(Format::Ladspa),
            _ => None,
        }
    }

    /// Format of a device kind (`clap:`, `vst3:`, `lv2:`, `ladspa:`), none for native devices.
    pub fn of_kind(kind: &str) -> Option<Format> {
        [
            (ponzik_clap::KIND_PREFIX, Format::Clap),
            (ponzik_vst3::KIND_PREFIX, Format::Vst3),
            (ponzik_lv2::KIND_PREFIX, Format::Lv2),
            (ponzik_ladspa::KIND_PREFIX, Format::Ladspa),
        ]
        .into_iter()
        .find(|(p, _)| kind.starts_with(p))
        .map(|(_, f)| f)
    }

    /// Short label shown in the browser.
    pub fn label(self) -> &'static str {
        match self {
            Format::Clap => "CLAP",
            Format::Vst3 => "VST3",
            Format::Lv2 => "LV2",
            Format::Ladspa => "LADSPA",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Format::Clap => "clap",
            Format::Vst3 => "vst3",
            Format::Lv2 => "lv2",
            Format::Ladspa => "ladspa",
        }
    }

    fn from_tag(tag: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.tag() == tag)
    }
}

/// The instrument of a plugin kind, `None` for a native kind.
pub fn instrument_from_kind(kind: &str) -> Option<Result<Box<dyn ponzik_engine::Instrument>, String>> {
    Some(match Format::of_kind(kind)? {
        Format::Clap => ponzik_clap::instrument_from_kind(kind),
        Format::Vst3 => ponzik_vst3::instrument_from_kind(kind),
        Format::Lv2 => ponzik_lv2::instrument_from_kind(kind),
        Format::Ladspa => Err("LADSPA plugins are effects".into()),
    })
}

/// The effect of a plugin kind, `None` for a native kind.
pub fn effect_from_kind(kind: &str) -> Option<Result<Box<dyn ponzik_engine::Effect>, String>> {
    Some(match Format::of_kind(kind)? {
        Format::Clap => ponzik_clap::effect_from_kind(kind),
        Format::Vst3 => ponzik_vst3::effect_from_kind(kind),
        Format::Lv2 => ponzik_lv2::effect_from_kind(kind),
        Format::Ladspa => ponzik_ladspa::effect_from_kind(kind),
    })
}

/// The note effect of a plugin kind (LV2 MIDI effects), `None` for a native kind.
pub fn note_fx_from_kind(kind: &str) -> Option<Result<Box<dyn ponzik_engine::NoteFx>, String>> {
    Some(match Format::of_kind(kind)? {
        Format::Lv2 => ponzik_lv2::note_fx_from_kind(kind),
        f => Err(format!("{} plugins are not note effects", f.label())),
    })
}

/// The plugin's own id in a kind (CLAP id, VST 3 class, LV2 URI, LADSPA
/// unique id): what stays the same wherever it is installed.
pub fn plugin_id(kind: &str) -> Option<&str> {
    match Format::of_kind(kind)? {
        Format::Clap => ponzik_clap::parse_kind(kind).map(|k| k.1),
        Format::Vst3 => ponzik_vst3::parse_kind(kind).map(|k| k.1),
        Format::Lv2 => ponzik_lv2::parse_kind(kind),
        Format::Ladspa => ponzik_ladspa::parse_kind(kind).map(|k| k.1),
    }
}

/// One plugin of a file, as the scan reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// VST 3 subcategories (`Fx|Delay`…), `MIDI` for an LV2 MIDI effect,
    /// the label of a LADSPA plugin, empty for CLAP.
    pub categories: String,
    pub is_instrument: bool,
    /// Why Ponzik cannot host this plugin of the file (empty: it can).
    pub skipped: String,
}

/// A file that could not be scanned, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    pub format: Format,
    /// The plugin skipped, when the file holds others that load (empty: the whole file).
    pub name: String,
    pub message: String,
}

/// Everything the scan found.
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub clap: Vec<ponzik_clap::PluginInfo>,
    pub vst3: Vec<ponzik_vst3::PluginInfo>,
    pub lv2: Vec<ponzik_lv2::PluginInfo>,
    pub ladspa: Vec<ponzik_ladspa::PluginInfo>,
    pub errors: Vec<ScanError>,
    /// Files actually loaded by a child (the others came from the cache).
    pub scanned: usize,
}

impl ScanReport {
    fn add(&mut self, format: Format, path: &Path, result: &Result<Vec<Found>, String>) {
        match result {
            Ok(found) => {
                for f in found {
                    if !f.skipped.is_empty() {
                        let e = ScanError { path: path.to_path_buf(), format, name: f.name.clone(), message: f.skipped.clone() };
                        self.errors.push(e);
                        continue;
                    }
                    match format {
                        Format::Clap => self.clap.push(ponzik_clap::PluginInfo {
                            id: f.id.clone(),
                            name: f.name.clone(),
                            vendor: f.vendor.clone(),
                            path: path.to_path_buf(),
                            is_instrument: f.is_instrument,
                        }),
                        Format::Vst3 => self.vst3.push(ponzik_vst3::PluginInfo {
                            id: f.id.clone(),
                            name: f.name.clone(),
                            vendor: f.vendor.clone(),
                            categories: f.categories.clone(),
                            path: path.to_path_buf(),
                            is_instrument: f.is_instrument,
                        }),
                        Format::Lv2 => self.lv2.push(ponzik_lv2::PluginInfo {
                            id: f.id.clone(),
                            name: f.name.clone(),
                            vendor: f.vendor.clone(),
                            path: path.to_path_buf(),
                            is_instrument: f.is_instrument,
                            is_midi_effect: f.categories == "MIDI",
                        }),
                        Format::Ladspa => self.ladspa.push(ponzik_ladspa::PluginInfo {
                            id: f.id.clone(),
                            label: f.categories.clone(),
                            name: f.name.clone(),
                            vendor: f.vendor.clone(),
                            path: path.to_path_buf(),
                        }),
                    }
                }
            }
            Err(message) => {
                self.errors.push(ScanError { path: path.to_path_buf(), format, name: String::new(), message: message.clone() })
            }
        }
    }

    pub fn plugin_count(&self) -> usize {
        self.clap.len() + self.vst3.len() + self.lv2.len() + self.ladspa.len()
    }
}

/// Plugin files and bundles of the standard folders (bridges such as
/// yabridge, which live under them, included).
pub fn candidates() -> Vec<(Format, PathBuf)> {
    let clap = ponzik_clap::plugin_files().into_iter().map(|p| (Format::Clap, p));
    let vst3 = ponzik_vst3::plugin_bundles().into_iter().map(|p| (Format::Vst3, p));
    let lv2 = ponzik_lv2::plugin_bundles().into_iter().map(|p| (Format::Lv2, p));
    let ladspa = ponzik_ladspa::plugin_files().into_iter().map(|p| (Format::Ladspa, p));
    clap.chain(vst3).chain(lv2).chain(ladspa).collect()
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

fn found_line(f: &Found) -> String {
    let line = format!("{}\t{}\t{}\t{}\t{}", clean(&f.id), f.is_instrument as u8, clean(&f.name), clean(&f.vendor), clean(&f.categories));
    if f.skipped.is_empty() { line } else { format!("{line}\t{}", clean(&f.skipped)) }
}

fn parse_found(fields: &[&str]) -> Option<Found> {
    let [id, inst, name, vendor, categories, rest @ ..] = fields else { return None };
    (!id.is_empty()).then(|| Found {
        id: id.to_string(),
        is_instrument: *inst == "1",
        name: name.to_string(),
        vendor: vendor.to_string(),
        categories: categories.to_string(),
        // Older caches have no sixth field.
        skipped: rest.first().map_or(String::new(), |s| s.to_string()),
    })
}

/// Lines for the plugins a file holds that Ponzik cannot host.
fn skipped_found(skipped: impl Iterator<Item = (String, String)>) -> impl Iterator<Item = Found> {
    skipped.enumerate().map(|(i, (name, reason))| Found {
        id: format!("skipped-{i}"),
        name,
        vendor: String::new(),
        categories: String::new(),
        is_instrument: false,
        skipped: reason,
    })
}

/// Work of the child process: loads `path` here and returns its answer
/// (standard output) and whether it succeeded.
pub fn scan_child(path: &Path) -> (String, bool) {
    let result: Result<Vec<Found>, String> = match Format::of(path) {
        Some(Format::Clap) => ponzik_clap::scan_file(path).map(|v| {
            v.into_iter()
                .map(|p| Found {
                    id: p.id,
                    name: p.name,
                    vendor: p.vendor,
                    categories: String::new(),
                    is_instrument: p.is_instrument,
                    skipped: String::new(),
                })
                .collect()
        }),
        Some(Format::Vst3) => ponzik_vst3::scan_bundle(path).map(|v| {
            v.into_iter()
                .map(|p| Found {
                    id: p.id,
                    name: p.name,
                    vendor: p.vendor,
                    categories: p.categories,
                    is_instrument: p.is_instrument,
                    skipped: String::new(),
                })
                .collect()
        }),
        Some(Format::Lv2) => ponzik_lv2::scan_bundle(path).map(|(ok, skipped)| {
            let ok = ok.into_iter().map(|p| Found {
                categories: if p.is_midi_effect { "MIDI".into() } else { String::new() },
                id: p.id,
                name: p.name,
                vendor: p.vendor,
                is_instrument: p.is_instrument,
                skipped: String::new(),
            });
            ok.chain(skipped_found(skipped.into_iter().map(|s| (s.name, s.reason)))).collect()
        }),
        Some(Format::Ladspa) => ponzik_ladspa::scan_file(path).map(|(ok, skipped)| {
            let ok = ok.into_iter().map(|p| Found {
                id: p.id,
                name: p.name,
                vendor: p.vendor,
                categories: p.label,
                is_instrument: false,
                skipped: String::new(),
            });
            ok.chain(skipped_found(skipped.into_iter())).collect()
        }),
        None => Err(tr("not a CLAP, VST 3, LV2 or LADSPA plugin").into()),
    };
    match result {
        Ok(found) => {
            let mut out = String::new();
            for f in &found {
                out.push_str(&format!("{PREFIX}\tplugin\t{}\n", found_line(f)));
            }
            out.push_str(&format!("{PREFIX}\tdone\t{}\n", found.len()));
            (out, true)
        }
        Err(e) => (format!("{PREFIX}\terror\t{}\n", clean(&e)), false),
    }
}

/// Reads the child's answer; `Err` carries its error message, `Ok(None)` an
/// answer cut short (crash before the end).
fn parse_answer(out: &str) -> Result<Option<Vec<Found>>, String> {
    let mut found = Vec::new();
    for line in out.lines() {
        let Some(rest) = line.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('\t')) else { continue };
        let fields: Vec<&str> = rest.split('\t').collect();
        match fields.first().copied() {
            Some("plugin") => found.extend(parse_found(&fields[1..])),
            Some("error") => return Err(fields.get(1).unwrap_or(&"").to_string()),
            Some("done") => return Ok(Some(found)),
            _ => {}
        }
    }
    Ok(None)
}

/// Runs a scanner (normally `ponzik scan-plugin <path>`) and waits for it at
/// most `timeout`. A crash, an error exit or a timeout becomes a readable
/// message; the plugin can never take the caller down.
pub fn run_scanner(mut cmd: Command, timeout: Duration) -> Result<Vec<Found>, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| tr_fmt!("could not start the scan: {}", e.to_string()))?;
    // Read on the side: a chatty plugin would block on a full pipe.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    };
    // A killed child may leave grandchildren holding the pipe: don't wait for them.
    let out = if status.is_some() { reader.join().unwrap_or_default() } else { String::new() };
    let Some(status) = status else {
        return Err(tr_fmt!("did not answer within {} s: skipped", timeout.as_secs()));
    };
    match parse_answer(&out) {
        Err(e) => Err(e),
        // A complete answer stands even if the plugin crashes while unloading.
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(exit_message(status)),
    }
}

#[cfg(unix)]
fn exit_message(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(sig) => tr_fmt!("crashed while loading (signal {})", sig),
        None => tr_fmt!("the scan failed (exit code {})", status.code().unwrap_or(-1)),
    }
}

#[cfg(not(unix))]
fn exit_message(status: std::process::ExitStatus) -> String {
    tr_fmt!("crashed while loading (exit code {})", status.code().unwrap_or(-1))
}

// --- Cache ---------------------------------------------------------------------

/// Size and date of a file, or of everything inside a bundle.
fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    fn walk(p: &Path, depth: u32, acc: &mut (u64, u64)) {
        let Ok(meta) = std::fs::metadata(p) else { return };
        let date = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        acc.0 = acc.0.max(date);
        if meta.is_dir() {
            if depth < 4
                && let Ok(entries) = std::fs::read_dir(p)
            {
                for e in entries.flatten() {
                    walk(&e.path(), depth + 1, acc);
                }
            }
        } else {
            acc.1 += meta.len();
        }
    }
    std::fs::metadata(path).ok()?;
    let mut acc = (0, 0);
    walk(path, 0, &mut acc);
    Some(acc)
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    format: Format,
    stamp: (u64, u64),
    result: Result<Vec<Found>, String>,
}

type Cache = HashMap<PathBuf, Entry>;

fn load_cache(path: &Path) -> Cache {
    let mut cache = Cache::new();
    let Ok(text) = std::fs::read_to_string(path) else { return cache };
    let mut lines = text.lines();
    if lines.next() != Some(CACHE_HEADER) {
        return cache;
    }
    let mut current: Option<(PathBuf, Entry)> = None;
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["F", file, format, date, size] => {
                cache.extend(current.take());
                let (Some(format), Ok(date), Ok(size)) = (Format::from_tag(format), date.parse(), size.parse()) else {
                    continue;
                };
                current = Some((PathBuf::from(file), Entry { format, stamp: (date, size), result: Ok(Vec::new()) }));
            }
            ["P", rest @ ..] => {
                if let Some((_, Entry { result: Ok(found), .. })) = &mut current {
                    found.extend(parse_found(rest));
                }
            }
            ["E", message] => {
                if let Some((_, e)) = &mut current {
                    e.result = Err(message.to_string());
                }
            }
            _ => {}
        }
    }
    cache.extend(current);
    cache
}

fn save_cache(path: &Path, cache: &Cache) -> std::io::Result<()> {
    let mut files: Vec<_> = cache.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = format!("{CACHE_HEADER}\n");
    for (file, e) in files {
        out.push_str(&format!("F\t{}\t{}\t{}\t{}\n", clean(&file.to_string_lossy()), e.format.tag(), e.stamp.0, e.stamp.1));
        match &e.result {
            Ok(found) => found.iter().for_each(|f| out.push_str(&format!("P\t{}\n", found_line(f)))),
            Err(m) => out.push_str(&format!("E\t{}\n", clean(m))),
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(tmp, path)
}

/// What the cache knows of the current plugins, without loading anything
/// (changed or new files are missing until the next [`scan`]).
pub fn cached(cache_path: &Path) -> ScanReport {
    let cache = load_cache(cache_path);
    let mut report = ScanReport::default();
    for (format, path) in candidates() {
        if let Some(e) = cache.get(&path).filter(|e| e.format == format && Some(e.stamp) == fingerprint(&path)) {
            report.add(format, &path, &e.result);
        }
    }
    report
}

/// Scans every plugin of the standard folders: unchanged files come from the
/// cache (unless `force`), the others are loaded by `scanner(path)` children,
/// a few at a time. The cache is rewritten when `cache_path` is given.
pub fn scan(
    scanner: &(dyn Fn(&Path) -> Command + Sync),
    cache_path: Option<&Path>,
    force: bool,
    timeout: Duration,
) -> ScanReport {
    scan_paths(candidates(), scanner, cache_path, force, timeout)
}

/// [`scan`] on a given list of files and bundles.
pub fn scan_paths(
    files: Vec<(Format, PathBuf)>,
    scanner: &(dyn Fn(&Path) -> Command + Sync),
    cache_path: Option<&Path>,
    force: bool,
    timeout: Duration,
) -> ScanReport {
    let old = if force { Cache::new() } else { cache_path.map(load_cache).unwrap_or_default() };
    let todo: Vec<(Format, PathBuf, (u64, u64))> =
        files.into_iter().filter_map(|(f, p)| fingerprint(&p).map(|stamp| (f, p, stamp))).collect();
    let fresh: Mutex<Cache> = Mutex::new(Cache::new());
    let next = Mutex::new(0usize);
    let scanned = Mutex::new(0usize);
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap_or_else(|e| e.into_inner());
                        *n += 1;
                        *n - 1
                    };
                    let Some((format, path, stamp)) = todo.get(i) else { break };
                    let entry = match old.get(path).filter(|e| e.format == *format && e.stamp == *stamp) {
                        Some(e) => e.clone(),
                        None => {
                            *scanned.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                            Entry { format: *format, stamp: *stamp, result: run_scanner(scanner(path), timeout) }
                        }
                    };
                    fresh.lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone(), entry);
                }
            });
        }
    });
    let fresh = fresh.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Some(cache_path) = cache_path
        && let Err(e) = save_cache(cache_path, &fresh)
    {
        eprintln!("plugin cache {}: {e}", cache_path.display());
    }
    let mut report = ScanReport { scanned: scanned.into_inner().unwrap_or(0), ..ScanReport::default() };
    for (format, path, _) in &todo {
        if let Some(e) = fresh.get(path) {
            report.add(*format, path, &e.result);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(id: &str, inst: bool) -> Found {
        Found {
            id: id.into(),
            name: format!("Name {id}"),
            vendor: "V\tendor".into(),
            categories: "Fx".into(),
            is_instrument: inst,
            skipped: String::new(),
        }
    }

    #[test]
    fn answers_ignore_plugin_noise() {
        let out = format!(
            "hello from the plugin\n{PREFIX}\tplugin\t{}\nnoise\n{PREFIX}\tplugin\t{}\n{PREFIX}\tdone\t2\n",
            found_line(&found("a", true)),
            found_line(&found("b", false))
        );
        let got = parse_answer(&out).unwrap().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].id.as_str(), got[0].is_instrument, got[0].vendor.as_str()), ("a", true, "V endor"));
        assert_eq!(parse_answer(&format!("{PREFIX}\terror\tno factory\n")), Err("no factory".into()));
        // Cut before `done`: a crash in the middle.
        assert_eq!(parse_answer(&format!("{PREFIX}\tplugin\t{}\n", found_line(&found("a", true)))), Ok(None));
    }

    #[test]
    fn child_answer_for_a_missing_file() {
        let (out, ok) = scan_child(Path::new("/nonexistent/x.clap"));
        assert!(!ok);
        assert!(parse_answer(&out).is_err());
        let (out, ok) = scan_child(Path::new("/nonexistent/x.txt"));
        assert!(!ok && out.contains("error"));
    }

    #[cfg(unix)]
    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    #[cfg(unix)]
    #[test]
    fn crashes_hangs_and_errors_are_contained() {
        ponzik_i18n::with_lang(ponzik_i18n::Lang::EN, || {
            let ok = sh(&format!("echo noise; printf '{PREFIX}\\tplugin\\tid.x\\t1\\tX\\tV\\t\\n{PREFIX}\\tdone\\t1\\n'"));
            let got = run_scanner(ok, Duration::from_secs(5)).unwrap();
            assert_eq!((got[0].id.as_str(), got[0].is_instrument), ("id.x", true));

            let crash = sh(&format!("printf '{PREFIX}\\tplugin\\tid.x\\t1\\tX\\tV\\t\\n'; kill -SEGV $$"));
            assert_eq!(run_scanner(crash, Duration::from_secs(5)), Err("crashed while loading (signal 11)".into()));

            let t0 = Instant::now();
            let hang = sh("sleep 30");
            assert_eq!(run_scanner(hang, Duration::from_millis(200)), Err("did not answer within 0 s: skipped".into()));
            assert!(t0.elapsed() < Duration::from_secs(5));

            let failed = sh("exit 3");
            assert_eq!(run_scanner(failed, Duration::from_secs(5)), Err("the scan failed (exit code 3)".into()));
            let error = sh(&format!("printf '{PREFIX}\\terror\\tbroken\\n'; exit 2"));
            assert_eq!(run_scanner(error, Duration::from_secs(5)), Err("broken".into()));
        });
    }

    #[cfg(unix)]
    #[test]
    fn cache_skips_unchanged_files() {
        let dir = std::env::temp_dir().join(format!("ponzik-scan-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("b.vst3/Contents")).unwrap();
        std::fs::write(dir.join("a.clap"), b"x").unwrap();
        std::fs::write(dir.join("b.vst3/Contents/b.so"), b"yy").unwrap();
        std::fs::write(dir.join("c.clap"), b"z").unwrap();
        let files = vec![
            (Format::Clap, dir.join("a.clap")),
            (Format::Vst3, dir.join("b.vst3")),
            (Format::Clap, dir.join("c.clap")),
        ];
        let cache = dir.join("cache.txt");
        // The fake scanner answers by file name; `c` crashes.
        let scanner = |p: &Path| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            if name == "c" {
                sh("kill -SEGV $$")
            } else {
                sh(&format!("printf '{PREFIX}\\tplugin\\tid.{name}\\t0\\tPlug {name}\\tV\\tFx\\n{PREFIX}\\tdone\\t1\\n'"))
            }
        };
        let r = scan_paths(files.clone(), &scanner, Some(&cache), false, Duration::from_secs(5));
        assert_eq!((r.scanned, r.clap.len(), r.vst3.len(), r.errors.len()), (3, 1, 1, 1));
        assert_eq!(r.vst3[0].categories, "Fx");
        assert_eq!(r.errors[0].path, dir.join("c.clap"));
        // Second run: everything from the cache, crash included, nothing loaded.
        let never = |_: &Path| -> Command { panic!("cached files are not scanned again") };
        let r2 = scan_paths(files.clone(), &never, Some(&cache), false, Duration::from_secs(5));
        assert_eq!((r2.scanned, r2.clap, r2.vst3, r2.errors), (0, r.clap.clone(), r.vst3.clone(), r.errors.clone()));
        // A changed file is scanned again; `force` scans everything.
        std::fs::write(dir.join("a.clap"), b"xxx").unwrap();
        assert_eq!(scan_paths(files.clone(), &scanner, Some(&cache), false, Duration::from_secs(5)).scanned, 1);
        assert_eq!(scan_paths(files, &scanner, Some(&cache), true, Duration::from_secs(5)).scanned, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
