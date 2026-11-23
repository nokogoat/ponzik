//! What the bundles say: plugins, ports, interfaces and presets, read from
//! their Turtle files (see [`crate::ttl`]).
//!
//! A bundle is a folder `<name>.lv2` with a `manifest.ttl` naming its
//! plugins and pointing (`rdfs:seeAlso`) to the files that describe them.

use std::path::{Path, PathBuf};

use crate::abi::*;
use crate::ttl::{Graph, RDF_TYPE, Term, uri_to_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    Audio,
    Control,
    Atom,
    Cv,
    /// The obsolete LV2 event port.
    Event,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct PortDesc {
    pub index: u32,
    pub symbol: String,
    pub name: String,
    pub kind: PortKind,
    pub input: bool,
    pub default: Option<f32>,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub toggled: bool,
    pub integer: bool,
    pub enumeration: bool,
    pub logarithmic: bool,
    /// Bounds and default are fractions of the sample rate.
    pub sample_rate: bool,
    pub not_on_gui: bool,
    pub optional: bool,
    pub reports_latency: bool,
    pub side_chain: bool,
    /// `lv2:designation` (`lv2:latency`, `lv2:enabled`, `time:beatsPerMinute`…), empty without.
    pub designation: String,
    /// (value, label), in the order of the file.
    pub scale_points: Vec<(f32, String)>,
    /// `units:unit`, empty without.
    pub unit: String,
    /// An atom port that carries MIDI events.
    pub midi: bool,
    /// `rsz:minimumSize` of an atom port, in bytes (0 without).
    pub min_size: usize,
}

impl PortDesc {
    pub fn is_audio_in(&self) -> bool {
        self.kind == PortKind::Audio && self.input
    }

    pub fn is_audio_out(&self) -> bool {
        self.kind == PortKind::Audio && !self.input
    }
}

#[derive(Clone, Debug)]
pub struct UiDesc {
    pub uri: String,
    /// `ui:X11UI`, `ui:WindowsUI`, `ui:CocoaUI`, `ui:GtkUI`…
    pub class: String,
    pub binary: PathBuf,
    pub required: Vec<String>,
    /// `ui:noUserResize` or `ui:fixedSize`.
    pub fixed_size: bool,
}

#[derive(Clone, Debug)]
pub struct PluginDesc {
    pub uri: String,
    pub name: String,
    pub vendor: String,
    pub bundle: PathBuf,
    pub binary: PathBuf,
    pub classes: Vec<String>,
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub extension_data: Vec<String>,
    /// By index.
    pub ports: Vec<PortDesc>,
    pub uis: Vec<UiDesc>,
    /// `state:state` of the plugin: its default state (key, value).
    pub default_state: Vec<(String, Term)>,
}

/// Properties of a `state:state` node: (key, value).
fn state_of(g: &Graph, s: &Term) -> Vec<(String, Term)> {
    let Some(node) = g.object(s, STATE_STATE) else { return Vec::new() };
    g.about(node).map(|t| (t.p.clone(), t.o.clone())).collect()
}

/// Features a plugin may require that Ponzik provides.
pub const SUPPORTED_FEATURES: &[&str] = &[
    URID_MAP,
    URID_UNMAP,
    OPTIONS_OPTIONS,
    BUF_BOUNDED,
    BUF_FIXED,
    BUF_POWER_OF_2,
    BUF_COARSE,
    LOG_LOG,
    WORKER_SCHEDULE,
    LV2_HARD_RT_CAPABLE,
    LV2_IN_PLACE_BROKEN,
    LV2_IS_LIVE,
    STATE_LOAD_DEFAULT,
    STATE_THREAD_SAFE_RESTORE,
];

/// Features an interface may require that Ponzik provides.
pub const SUPPORTED_UI_FEATURES: &[&str] = &[
    URID_MAP,
    URID_UNMAP,
    OPTIONS_OPTIONS,
    LOG_LOG,
    UI_PARENT,
    UI_RESIZE,
    UI_IDLE,
    UI_TOUCH,
    UI_PORT_MAP,
    UI_NO_USER_RESIZE,
    UI_FIXED_SIZE,
    UI_MAKE_RESIDENT,
    UI_MAKE_SO_RESIDENT,
    INSTANCE_ACCESS,
    DATA_ACCESS,
];

/// What a plugin is for Ponzik.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Instrument,
    Effect,
    /// MIDI in and out, no audio: a note effect.
    MidiEffect,
}

/// Short name of a URI for messages: what follows its last `#` or `/`.
pub fn short(uri: &str) -> &str {
    uri.rsplit(['#', '/']).find(|s| !s.is_empty()).unwrap_or(uri)
}

impl PluginDesc {
    pub fn port(&self, symbol: &str) -> Option<&PortDesc> {
        self.ports.iter().find(|p| p.symbol == symbol)
    }

    fn midi_in(&self) -> bool {
        self.ports.iter().any(|p| p.kind == PortKind::Atom && p.input && p.midi)
    }

    fn midi_out(&self) -> bool {
        self.ports.iter().any(|p| p.kind == PortKind::Atom && !p.input && p.midi)
    }

    pub fn role(&self) -> Role {
        let audio_in = self.ports.iter().filter(|p| p.is_audio_in() && !p.side_chain).count();
        let audio_out = self.ports.iter().filter(|p| p.is_audio_out()).count();
        if audio_in == 0 && audio_out == 0 && self.midi_in() && self.midi_out() {
            Role::MidiEffect
        } else if self.classes.iter().any(|c| c == LV2_INSTRUMENT_PLUGIN) || (self.midi_in() && audio_in == 0 && audio_out > 0) {
            Role::Instrument
        } else {
            Role::Effect
        }
    }

    pub fn is_instrument(&self) -> bool {
        self.role() == Role::Instrument
    }

    pub fn has_extension(&self, uri: &str) -> bool {
        self.extension_data.iter().any(|e| e == uri)
    }

    /// Why Ponzik cannot host this plugin, `None` if it can.
    pub fn unsupported(&self) -> Option<String> {
        if let Some(f) = self.required.iter().find(|f| !SUPPORTED_FEATURES.contains(&f.as_str())) {
            return Some(format!("needs the feature {}", short(f)));
        }
        for p in &self.ports {
            let usable = match p.kind {
                PortKind::Audio | PortKind::Control | PortKind::Cv => true,
                PortKind::Atom => true,
                PortKind::Event | PortKind::Unknown => p.optional,
            };
            if !usable {
                let what = if p.kind == PortKind::Event { "an obsolete event port" } else { "a port of unknown type" };
                return Some(format!("has {what} ({})", p.symbol));
            }
        }
        let audio_out = self.ports.iter().filter(|p| p.is_audio_out()).count();
        if audio_out == 0 && self.role() != Role::MidiEffect {
            return Some("has no audio output".into());
        }
        None
    }

    /// The embeddable interface for this platform, if any.
    pub fn native_ui(&self) -> Option<&UiDesc> {
        let class = if cfg!(windows) {
            UI_WINDOWS
        } else if cfg!(target_os = "macos") {
            UI_COCOA
        } else {
            UI_X11
        };
        self.uis.iter().find(|u| u.class == class && u.required.iter().all(|f| SUPPORTED_UI_FEATURES.contains(&f.as_str())))
    }
}

fn iri_strings(g: &Graph, s: &Term, p: &str) -> Vec<String> {
    g.objects(s, p).filter_map(Term::as_iri).map(str::to_string).collect()
}

fn text(g: &Graph, s: &Term, p: &str) -> Option<String> {
    // An untagged or English literal first.
    let mut best: Option<String> = None;
    for o in g.objects(s, p) {
        if let Term::Literal { value, lang, .. } = o {
            if lang.is_empty() || lang.starts_with("en") {
                return Some(value.clone());
            }
            best.get_or_insert_with(|| value.clone());
        }
    }
    best
}

fn person_name(g: &Graph, s: &Term) -> Option<String> {
    for p in [DOAP_MAINTAINER, DOAP_DEVELOPER] {
        for who in g.objects(s, p) {
            if let Some(n) = text(g, who, FOAF_NAME) {
                return Some(n);
            }
        }
    }
    None
}

fn read_port(g: &Graph, node: &Term) -> Option<PortDesc> {
    let index = g.object(node, LV2_INDEX)?.as_f64()?;
    if index < 0.0 || index.fract() != 0.0 || index > 65_535.0 {
        return None;
    }
    let is = |c: &str| g.has_type(node, c);
    let kind = if is(LV2_AUDIO_PORT) {
        PortKind::Audio
    } else if is(LV2_CONTROL_PORT) {
        PortKind::Control
    } else if is(ATOM_ATOM_PORT) {
        PortKind::Atom
    } else if is(LV2_CV_PORT) {
        PortKind::Cv
    } else if is(EVENT_EVENT_PORT) {
        PortKind::Event
    } else {
        PortKind::Unknown
    };
    let prop = |p: &str| g.has(node, LV2_PORT_PROPERTY, p);
    let num = |p: &str| g.object(node, p).and_then(Term::as_f32);
    let symbol = text(g, node, LV2_SYMBOL).unwrap_or_else(|| format!("port{index}"));
    let mut scale_points: Vec<(f32, String)> = g
        .objects(node, LV2_SCALE_POINT)
        .filter_map(|sp| Some((g.object(sp, RDF_VALUE)?.as_f32()?, text(g, sp, RDFS_LABEL)?)))
        .collect();
    scale_points.sort_by(|a, b| a.0.total_cmp(&b.0));
    scale_points.dedup_by(|a, b| a.0 == b.0);
    Some(PortDesc {
        index: index as u32,
        name: text(g, node, LV2_NAME).unwrap_or_else(|| symbol.clone()),
        symbol,
        kind,
        input: is(LV2_INPUT_PORT),
        default: num(LV2_DEFAULT),
        min: num(LV2_MINIMUM),
        max: num(LV2_MAXIMUM),
        toggled: prop(LV2_TOGGLED),
        integer: prop(LV2_INTEGER),
        enumeration: prop(LV2_ENUMERATION),
        logarithmic: prop(PPROPS_LOGARITHMIC),
        sample_rate: prop(LV2_SAMPLE_RATE),
        not_on_gui: prop(PPROPS_NOT_ON_GUI),
        optional: prop(LV2_CONNECTION_OPTIONAL),
        reports_latency: prop(LV2_REPORTS_LATENCY),
        side_chain: prop(LV2_IS_SIDE_CHAIN),
        designation: g.object(node, LV2_DESIGNATION).and_then(Term::as_iri).unwrap_or("").to_string(),
        scale_points,
        unit: g.object(node, UNITS_UNIT).and_then(Term::as_iri).unwrap_or("").to_string(),
        midi: g.has(node, ATOM_SUPPORTS, MIDI_EVENT),
        min_size: num(RSZ_MINIMUM_SIZE).map_or(0, |v| v.max(0.0) as usize),
    })
}

fn read_ui(g: &Graph, node: &Term) -> Option<UiDesc> {
    let uri = node.as_iri()?.to_string();
    let class = [UI_X11, UI_WINDOWS, UI_COCOA]
        .into_iter()
        .find(|c| g.has_type(node, c))
        .map(str::to_string)
        .or_else(|| g.objects(node, RDF_TYPE).filter_map(Term::as_iri).next().map(str::to_string))?;
    let binary = uri_to_path(g.object(node, UI_BINARY)?.as_iri()?)?;
    let required = iri_strings(g, node, LV2_REQUIRED_FEATURE);
    let fixed_size = [UI_NO_USER_RESIZE, UI_FIXED_SIZE]
        .iter()
        .any(|f| required.iter().any(|r| r == f) || g.has(node, LV2_OPTIONAL_FEATURE, f));
    Some(UiDesc { uri, class, binary, required, fixed_size })
}

/// Loads a bundle's manifest and every file it points to for its plugins
/// and their interfaces.
pub fn load_bundle(g: &mut Graph, bundle: &Path) -> Result<(), String> {
    g.load_file(&bundle.join("manifest.ttl"))?;
    // Files pointed to may point further (a file per plugin, then a file for the presets).
    for _ in 0..4 {
        let mut more: Vec<PathBuf> = Vec::new();
        for class in [LV2_PLUGIN, UI_X11, UI_WINDOWS, UI_COCOA] {
            for s in g.subjects(RDF_TYPE, &Term::iri(class)) {
                more.extend(g.objects(&s, RDFS_SEE_ALSO).filter_map(Term::as_iri).filter_map(uri_to_path));
            }
        }
        let before = g.len();
        for f in more {
            // A missing or broken file costs only what it would have said.
            if let Err(e) = g.load_file(&f) {
                eprintln!("LV2: {e}");
            }
        }
        if g.len() == before {
            break;
        }
    }
    Ok(())
}

/// The plugin `uri` as described in the graph (bundle already loaded).
pub fn describe(g: &Graph, uri: &str, bundle: &Path) -> Result<PluginDesc, String> {
    let s = Term::iri(uri);
    let binary = g
        .object(&s, LV2_BINARY)
        .and_then(Term::as_iri)
        .and_then(uri_to_path)
        .ok_or_else(|| format!("{}: no lv2:binary", short(uri)))?;
    let name = text(g, &s, DOAP_NAME).unwrap_or_else(|| short(uri).to_string());
    let vendor = person_name(g, &s)
        .or_else(|| {
            let project = g.object(&s, LV2_PROJECT)?;
            person_name(g, project).or_else(|| text(g, project, DOAP_NAME))
        })
        .unwrap_or_default();
    let mut ports: Vec<PortDesc> = g.objects(&s, LV2_PORT).filter_map(|p| read_port(g, p)).collect();
    ports.sort_by_key(|p| p.index);
    ports.dedup_by_key(|p| p.index);
    if ports.iter().enumerate().any(|(i, p)| p.index as usize != i) {
        return Err(format!("{name}: port indices are not 0 to {}", ports.len().saturating_sub(1)));
    }
    let uis = g.objects(&s, UI_UI).filter_map(|u| read_ui(g, u)).collect();
    Ok(PluginDesc {
        uri: uri.to_string(),
        name,
        vendor,
        bundle: bundle.to_path_buf(),
        binary,
        classes: iri_strings(g, &s, RDF_TYPE),
        required: iri_strings(g, &s, LV2_REQUIRED_FEATURE),
        optional: iri_strings(g, &s, LV2_OPTIONAL_FEATURE),
        extension_data: iri_strings(g, &s, LV2_EXTENSION_DATA),
        ports,
        uis,
        default_state: state_of(g, &s),
    })
}

/// URIs of the plugins whose binary lives in `bundle` (graph already loaded).
pub fn plugin_uris(g: &Graph, bundle: &Path) -> Vec<String> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let dir = canon(bundle);
    let mut out: Vec<String> = g
        .subjects(RDF_TYPE, &Term::iri(LV2_PLUGIN))
        .into_iter()
        .filter_map(|s| {
            let bin = g.object(&s, LV2_BINARY).and_then(Term::as_iri).and_then(uri_to_path)?;
            (canon(bin.parent()?) == dir || bin.starts_with(bundle)).then(|| s.as_iri().map(str::to_string)).flatten()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every plugin of a bundle, in the order of their URIs.
pub fn read_bundle(bundle: &Path) -> Result<Vec<Result<PluginDesc, String>>, String> {
    let mut g = Graph::new();
    load_bundle(&mut g, bundle)?;
    Ok(plugin_uris(&g, bundle).iter().map(|u| describe(&g, u, bundle)).collect())
}

// --- Presets -------------------------------------------------------------------

/// A preset of the plugin: port values by symbol, and its state if it has one.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub uri: String,
    pub name: String,
    pub values: Vec<(String, f32)>,
    /// Its `state:state` properties, if any.
    pub state: Vec<(String, Term)>,
}

/// The presets that apply to `plugin`, from every bundle of `bundles`
/// (preset bundles may live apart from the plugin), by name.
pub fn presets(plugin: &str, bundles: &[PathBuf]) -> Vec<Preset> {
    let target = Term::iri(plugin);
    let mut out = Vec::new();
    for b in bundles {
        let mut g = Graph::new();
        if g.load_file(&b.join("manifest.ttl")).is_err() {
            continue;
        }
        let applies = g.subjects(LV2_APPLIES_TO, &target);
        if applies.is_empty() {
            continue;
        }
        for p in &applies {
            for f in g.objects(p, RDFS_SEE_ALSO).filter_map(Term::as_iri).filter_map(uri_to_path).collect::<Vec<_>>() {
                let _ = g.load_file(&f);
            }
        }
        for p in g.subjects(LV2_APPLIES_TO, &target) {
            if !g.has_type(&p, PSET_PRESET) {
                continue;
            }
            let Some(uri) = p.as_iri() else { continue };
            let values = g
                .objects(&p, LV2_PORT)
                .filter_map(|port| Some((text(&g, port, LV2_SYMBOL)?, g.object(port, PSET_VALUE)?.as_f32()?)))
                .collect();
            let name = text(&g, &p, RDFS_LABEL).unwrap_or_else(|| short(uri).to_string());
            out.push(Preset { uri: uri.to_string(), name, values, state: state_of(&g, &p) });
        }
    }
    out.sort_by(|a, b| a.uri.cmp(&b.uri));
    out.dedup_by(|a, b| a.uri == b.uri);
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ponzik-lv2-world-{}-{name}", std::process::id())).join(format!("{name}.lv2"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, text) in files {
            std::fs::write(dir.join(f), text).unwrap();
        }
        dir
    }

    const MANIFEST: &str = r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
@prefix pset: <http://lv2plug.in/ns/ext/presets#> .
<urn:t:amp> a lv2:Plugin ; lv2:binary <amp.so> ; rdfs:seeAlso <amp.ttl> .
<urn:t:amp#ui> a ui:X11UI ; ui:binary <amp_ui.so> ; rdfs:seeAlso <amp.ttl> .
<urn:t:amp#loud> a pset:Preset ; lv2:appliesTo <urn:t:amp> ; rdfs:seeAlso <presets.ttl> .
"#;

    const AMP: &str = r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
@prefix pprops: <http://lv2plug.in/ns/ext/port-props#> .
@prefix units: <http://lv2plug.in/ns/extensions/units#> .
<urn:t:amp> a lv2:Plugin, lv2:AmplifierPlugin ;
  doap:name "Test Amp" ; doap:maintainer [ foaf:name "Ponzik" ] ;
  lv2:requiredFeature <http://lv2plug.in/ns/ext/urid#map> ;
  ui:ui <urn:t:amp#ui> ;
  lv2:port [ a lv2:OutputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out" ; lv2:name "Out" ] ,
           [ a lv2:InputPort, lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in" ; lv2:name "In" ] ,
           [ a lv2:InputPort, lv2:ControlPort ; lv2:index 2 ; lv2:symbol "gain" ; lv2:name "Gain" ;
             lv2:default 0 ; lv2:minimum -20 ; lv2:maximum 20 ; units:unit units:db ] ,
           [ a lv2:InputPort, lv2:ControlPort ; lv2:index 3 ; lv2:symbol "freq" ; lv2:name "Freq" ;
             lv2:default 0.01 ; lv2:minimum 0.001 ; lv2:maximum 0.5 ;
             lv2:portProperty lv2:sampleRate, pprops:logarithmic ] ,
           [ a lv2:InputPort, lv2:ControlPort ; lv2:index 4 ; lv2:symbol "mode" ; lv2:name "Mode" ;
             lv2:default 2 ; lv2:minimum 1 ; lv2:maximum 4 ; lv2:portProperty lv2:enumeration, lv2:integer ;
             lv2:scalePoint [ rdfs:label "Four" ; rdf:value 4 ] , [ rdfs:label "One" ; rdf:value 1 ] ,
                            [ rdfs:label "Two" ; rdf:value 2 ] ] ,
           [ a atom:AtomPort, lv2:InputPort ; lv2:index 5 ; lv2:symbol "midi" ; atom:bufferType atom:Sequence ;
             atom:supports midi:MidiEvent ] .
<urn:t:amp#ui> lv2:requiredFeature ui:idleInterface .
"#;

    #[test]
    fn a_bundle_describes_its_plugin() {
        let presets_ttl = r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix pset: <http://lv2plug.in/ns/ext/presets#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:t:amp#loud> a pset:Preset ; rdfs:label "Loud" ; lv2:port [ lv2:symbol "gain" ; pset:value 12.0 ] .
"#;
        let dir = bundle("amp", &[("manifest.ttl", MANIFEST), ("amp.ttl", AMP), ("presets.ttl", presets_ttl)]);
        let all = read_bundle(&dir).unwrap();
        assert_eq!(all.len(), 1);
        let p = all[0].as_ref().unwrap();
        assert_eq!((p.uri.as_str(), p.name.as_str(), p.vendor.as_str()), ("urn:t:amp", "Test Amp", "Ponzik"));
        assert_eq!(p.binary, dir.join("amp.so"));
        assert_eq!(p.ports.iter().map(|p| p.symbol.as_str()).collect::<Vec<_>>(), ["in", "out", "gain", "freq", "mode", "midi"]);
        let gain = p.port("gain").unwrap();
        assert_eq!((gain.kind, gain.input, gain.min, gain.max, gain.default), (PortKind::Control, true, Some(-20.0), Some(20.0), Some(0.0)));
        assert_eq!(gain.unit, UNITS_DB);
        let freq = p.port("freq").unwrap();
        assert!(freq.sample_rate && freq.logarithmic);
        let mode = p.port("mode").unwrap();
        assert!(mode.enumeration && mode.integer);
        assert_eq!(mode.scale_points, [(1.0, "One".to_string()), (2.0, "Two".into()), (4.0, "Four".into())]);
        assert!(p.port("midi").unwrap().midi);
        // MIDI in, audio in: an effect. Its UI is an X11 one.
        assert_eq!(p.role(), Role::Effect);
        assert_eq!(p.uis.len(), 1);
        assert_eq!(p.uis[0].class, UI_X11);
        assert_eq!(p.uis[0].required, [UI_IDLE]);
        assert!(p.unsupported().is_none());
        let presets = presets("urn:t:amp", std::slice::from_ref(&dir));
        assert_eq!(presets, [Preset { uri: "urn:t:amp#loud".into(), name: "Loud".into(), values: vec![("gain".into(), 12.0)], state: vec![] }]);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn unsupported_plugins_say_why() {
        let mut p = PluginDesc {
            uri: "urn:x".into(),
            name: "X".into(),
            vendor: String::new(),
            bundle: PathBuf::new(),
            binary: PathBuf::new(),
            classes: vec![],
            required: vec!["http://example.org/ns#magic".into()],
            optional: vec![],
            extension_data: vec![],
            ports: vec![],
            uis: vec![],
            default_state: vec![],
        };
        assert_eq!(p.unsupported().as_deref(), Some("needs the feature magic"));
        p.required.clear();
        assert_eq!(p.unsupported().as_deref(), Some("has no audio output"));
        assert_eq!(short("http://lv2plug.in/ns/ext/urid#map"), "map");
        assert_eq!(short("http://a.org/b/"), "b");
    }
}
