//! LV2 plugin host for Ponzik: bundles read with an in-house Turtle reader
//! ([`ttl`]), the C ABI written by hand ([`abi`]), binaries loaded with
//! `dlopen` / `LoadLibraryExW`.
//!
//! Supported: audio, control (bounds, default, toggled, integer,
//! enumeration, logarithmic, sample rate), atom (MIDI) and CV ports (CV
//! silent); the features `urid:map` / `urid:unmap`, `options:options`,
//! `buf-size` (bounded and fixed block lengths), `log:log`,
//! `worker:schedule` and `state:interface`; presets; embedded interfaces
//! (`ui:X11UI`, `ui:WindowsUI`, `ui:CocoaUI`).
//!
//! A plugin is identified in a project by its URI, `lv2:<uri>` (see
//! [`kind_for`]): LV2 URIs do not depend on where the bundle is installed,
//! so the bundle is looked up again when a project is opened.

pub mod abi;
mod atom;
mod bytes;
mod host;
mod plugin;
mod state;
pub mod ttl;
mod ui;
pub mod urid;
pub mod world;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ponzik_engine::{Effect, Instrument, NoteFx};

pub use crate::plugin::{FIXED_BLOCK, Lv2Effect, Lv2Instrument, Lv2NoteFx, MAX_BLOCK};
use crate::world::Role;

/// Prefix of LV2 kinds in a project.
pub const KIND_PREFIX: &str = "lv2:";

/// A plugin found by the scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    /// The plugin's URI.
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// Its bundle (`<name>.lv2` folder).
    pub path: PathBuf,
    pub is_instrument: bool,
    /// MIDI in and out, no audio: a note effect.
    pub is_midi_effect: bool,
}

impl PluginInfo {
    pub fn kind(&self) -> String {
        kind_for(&self.id)
    }
}

/// A plugin of a bundle Ponzik cannot host, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

pub fn kind_for(uri: &str) -> String {
    format!("{KIND_PREFIX}{uri}")
}

/// The URI of an `lv2:<uri>` kind.
pub fn parse_kind(kind: &str) -> Option<&str> {
    kind.strip_prefix(KIND_PREFIX).filter(|u| !u.is_empty())
}

/// Search folders: `$LV2_PATH` if set, otherwise the platform's.
pub fn search_paths() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("LV2_PATH").filter(|v| !v.is_empty()) {
        return std::env::split_paths(&v).collect();
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut dirs = Vec::new();
    if cfg!(windows) {
        dirs.extend(env("APPDATA").map(|p| p.join("LV2")));
        dirs.extend(env("COMMONPROGRAMFILES").map(|p| p.join("LV2")));
    } else if cfg!(target_os = "macos") {
        dirs.extend(env("HOME").map(|p| p.join("Library/Audio/Plug-Ins/LV2")));
        dirs.extend(env("HOME").map(|p| p.join(".lv2")));
        dirs.push("/Library/Audio/Plug-Ins/LV2".into());
    } else {
        dirs.extend(env("HOME").map(|p| p.join(".lv2")));
        dirs.push("/usr/local/lib/lv2".into());
        dirs.push("/usr/lib/lv2".into());
        dirs.push("/usr/lib64/lv2".into());
    }
    dirs
}

/// Bundles (folders with a `manifest.ttl`) of the search folders, without
/// duplicates (links included).
pub fn plugin_bundles() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen = Vec::new();
    for dir in search_paths() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("manifest.ttl").is_file()).collect();
        found.sort();
        for b in found {
            let c = b.canonicalize().unwrap_or_else(|_| b.clone());
            if !seen.contains(&c) {
                seen.push(c);
                out.push(b);
            }
        }
    }
    out
}

/// Where each plugin URI was found, in this process.
static KNOWN: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

fn remember(uri: &str, bundle: &Path) {
    host::lock(&KNOWN).get_or_insert_with(HashMap::new).insert(uri.to_string(), bundle.to_path_buf());
}

/// The bundle of plugin `uri`: one seen in this process, otherwise the
/// first of the search folders whose manifest names it.
pub fn find_bundle(uri: &str) -> Option<PathBuf> {
    if let Some(b) = host::lock(&KNOWN).as_ref().and_then(|m| m.get(uri).cloned()) {
        return Some(b);
    }
    let target = ttl::Term::iri(uri);
    for b in plugin_bundles() {
        let mut g = ttl::Graph::new();
        if g.load_file(&b.join("manifest.ttl")).is_ok() && g.has_type(&target, abi::LV2_PLUGIN) {
            remember(uri, &b);
            return Some(b);
        }
    }
    None
}

/// The plugins of a bundle Ponzik can host, and those it cannot (why).
/// Each binary is loaded to check it holds what its description says:
/// the app does it in a child process.
pub fn scan_bundle(bundle: &Path) -> Result<(Vec<PluginInfo>, Vec<Skipped>), String> {
    let all = world::read_bundle(bundle)?;
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    let mut libs: HashMap<PathBuf, Result<Vec<String>, String>> = HashMap::new();
    for d in all {
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                let (name, reason) = e.split_once(": ").map_or((String::new(), e.clone()), |(a, b)| (a.to_string(), b.to_string()));
                skipped.push(Skipped { name, reason });
                continue;
            }
        };
        if let Some(reason) = d.unsupported() {
            skipped.push(Skipped { name: d.name.clone(), reason });
            continue;
        }
        let uris = libs.entry(d.binary.clone()).or_insert_with(|| plugin::library_uris(&d.binary));
        match uris {
            Err(e) => {
                skipped.push(Skipped { name: d.name.clone(), reason: e.clone() });
                continue;
            }
            Ok(u) if !u.contains(&d.uri) => {
                skipped.push(Skipped { name: d.name.clone(), reason: "not in its library".into() });
                continue;
            }
            Ok(_) => {}
        }
        remember(&d.uri, bundle);
        let role = d.role();
        ok.push(PluginInfo {
            id: d.uri,
            name: d.name,
            vendor: d.vendor,
            path: bundle.to_path_buf(),
            is_instrument: role == Role::Instrument,
            is_midi_effect: role == Role::MidiEffect,
        });
    }
    Ok((ok, skipped))
}

fn model_of(bundle: &Path, uri: &str) -> Result<std::sync::Arc<plugin::Model>, String> {
    remember(uri, bundle);
    plugin::model(bundle, uri)
}

/// The instrument `uri` of `bundle`.
pub fn create_instrument(bundle: &Path, uri: &str) -> Result<Box<dyn Instrument>, String> {
    Ok(Box::new(Lv2Instrument::new(model_of(bundle, uri)?)?))
}

/// The effect `uri` of `bundle`.
pub fn create_effect(bundle: &Path, uri: &str) -> Result<Box<dyn Effect>, String> {
    Ok(Box::new(Lv2Effect::new(model_of(bundle, uri)?)?))
}

/// The MIDI effect `uri` of `bundle`, as a note effect.
pub fn create_note_fx(bundle: &Path, uri: &str) -> Result<Box<dyn NoteFx>, String> {
    Ok(Box::new(Lv2NoteFx::new(model_of(bundle, uri)?)?))
}

fn bundle_of(kind: &str) -> Result<(PathBuf, &str), String> {
    let uri = parse_kind(kind).ok_or_else(|| format!("invalid LV2 kind: {kind}"))?;
    let bundle = find_bundle(uri).ok_or_else(|| format!("LV2 plugin {uri} not installed"))?;
    Ok((bundle, uri))
}

pub fn instrument_from_kind(kind: &str) -> Result<Box<dyn Instrument>, String> {
    let (b, uri) = bundle_of(kind)?;
    create_instrument(&b, uri)
}

pub fn effect_from_kind(kind: &str) -> Result<Box<dyn Effect>, String> {
    let (b, uri) = bundle_of(kind)?;
    create_effect(&b, uri)
}

pub fn note_fx_from_kind(kind: &str) -> Result<Box<dyn NoteFx>, String> {
    let (b, uri) = bundle_of(kind)?;
    create_note_fx(&b, uri)
}

/// Presets of the plugin of `kind`: name and parameter values.
pub fn presets_of(kind: &str) -> Vec<(String, Vec<(u32, f32)>)> {
    let Ok((b, uri)) = bundle_of(kind) else { return Vec::new() };
    let Ok(m) = plugin::model(&b, uri) else { return Vec::new() };
    let presets = world::presets(uri, &plugin_bundles());
    presets
        .into_iter()
        .map(|p| {
            let values = p
                .values
                .iter()
                .filter_map(|(sym, v)| {
                    let port = m.desc.port(sym)?.index;
                    let k = m.param(port)?;
                    Some((port, m.convs[k].1.to_host(*v, 48_000.0)))
                })
                .collect();
            (p.name, values)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_carry_the_uri() {
        let k = kind_for("http://gareus.org/oss/lv2/fil4#stereo");
        assert_eq!(k, "lv2:http://gareus.org/oss/lv2/fil4#stereo");
        assert_eq!(parse_kind(&k), Some("http://gareus.org/oss/lv2/fil4#stereo"));
        assert_eq!(parse_kind("lv2:"), None);
        assert_eq!(parse_kind("clap:x#y"), None);
    }

    #[test]
    fn missing_plugins_are_errors() {
        assert!(effect_from_kind("lv2:urn:ponzik:nowhere").is_err());
        assert!(scan_bundle(Path::new("/nonexistent/x.lv2")).is_err());
    }
}
