//! Device presets: the JSON format shared by user presets and the factory
//! presets shipped in `assets/presets/<kind>/<name>.json` (embedded at build).
//!
//! A preset is `{"kind": …, "name": …, "params": [[id, value], …]}`; params it
//! leaves out keep their default. A CLAP or VST 3 plugin's preset also keeps
//! the plugin's opaque state (`"state"`, base64): the whole patch, which the
//! params alone cannot rebuild.

use crate::{PluginState, RackDef};
use crate::json::{self, Json};

include!(concat!(env!("OUT_DIR"), "/factory_presets.rs"));

/// Reads the `params` of a preset file.
pub fn parse_preset(text: &str) -> Result<Vec<(u32, f32)>, String> {
    let doc = json::parse(text)?;
    Ok(doc
        .get("params")
        .and_then(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|pair| {
            let a = pair.as_arr()?;
            Some((a.first()?.as_f64()? as u32, a.get(1)?.as_f64()? as f32))
        })
        .collect())
}

/// Writes a preset file.
pub fn preset_json(kind: &str, name: &str, params: &[(u32, f32)]) -> String {
    rack_preset_json(kind, name, params, None)
}

/// Writes a preset file; a rack preset also keeps its chains and mappings
/// (`params` then holds the macro values and every device's settings).
pub fn rack_preset_json(kind: &str, name: &str, params: &[(u32, f32)], rack: Option<&RackDef>) -> String {
    device_preset_json(kind, name, params, rack, None)
}

/// Writes a preset file of any device: a rack's chains, a plugin's state.
pub fn device_preset_json(
    kind: &str,
    name: &str,
    params: &[(u32, f32)],
    rack: Option<&RackDef>,
    state: Option<&[u8]>,
) -> String {
    let mut pairs = vec![
        ("kind".into(), Json::Str(kind.into())),
        ("name".into(), Json::Str(name.into())),
        (
            "params".into(),
            Json::Arr(params.iter().map(|&(id, v)| Json::Arr(vec![Json::Num(id as f64), Json::Num(v as f64)])).collect()),
        ),
    ];
    if let Some(rack) = rack {
        pairs.push(("rack".into(), crate::io::rack_json(rack)));
    }
    if let Some(state) = state {
        pairs.push(("state".into(), crate::io::state_json(state)));
    }
    Json::Obj(pairs).pretty()
}

/// The plugin state of a preset (`None` for other presets, or a malformed one).
pub fn parse_state(text: &str) -> Option<PluginState> {
    let doc = json::parse(text).ok()?;
    crate::io::load_state(doc.get("state"))
}

/// Folder name of a device kind's presets: a plugin's own id (CLAP plugin
/// id, VST 3 class id, LV2 URI, LADSPA id), so that its presets follow it wherever it is
/// installed; the kind itself for native devices.
pub fn preset_folder(kind: &str) -> &str {
    crate::plugins::plugin_id(kind).unwrap_or(kind)
}

/// Chains and mappings of a rack preset (`None` for other presets).
pub fn parse_rack(text: &str) -> Option<RackDef> {
    let doc = json::parse(text).ok()?;
    let rack = doc.get("rack")?;
    Some(crate::io::load_rack(Some(rack)))
}

/// Names of the factory presets of a device kind, sorted.
pub fn factory_preset_names(kind: &str) -> Vec<&'static str> {
    FACTORY_PRESETS.iter().filter(|p| p.0 == kind).map(|p| p.1).collect()
}

/// Params of a factory preset.
pub fn factory_preset(kind: &str, name: &str) -> Option<Vec<(u32, f32)>> {
    FACTORY_PRESETS.iter().find(|p| p.0 == kind && p.1 == name).and_then(|p| parse_preset(p.2).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::registry;

    #[test]
    fn plugin_presets_keep_the_state_under_the_plugin_id() {
        let kind = "clap:/usr/lib/clap/Surge XT.clap#org.surge-synth-team.surge-xt";
        let state = [0u8, 7, 255, 42];
        let text = device_preset_json(kind, "Pad", &[(3, 0.5)], None, Some(&state));
        assert_eq!(parse_state(&text).as_deref(), Some(&state[..]));
        assert_eq!(parse_preset(&text).unwrap(), [(3, 0.5)]);
        assert_eq!(parse_state(&preset_json("eq", "x", &[])), None);
        assert_eq!(parse_state(r#"{"state": "a!"}"#), None);
        assert_eq!(preset_folder(kind), "org.surge-synth-team.surge-xt");
        let vst = "vst3:/usr/lib/vst3/Surge XT.vst3#0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(preset_folder(vst), "0123456789ABCDEF0123456789ABCDEF");
        assert_eq!(preset_folder("eq"), "eq");
    }

    #[test]
    fn rack_presets_keep_chains_mappings_and_macros() {
        let mut fx = crate::EffectDef::new_rack();
        fx.rack_insert(0, None, "eq");
        fx.rack_insert(0, None, "chorus");
        fx.map_macro(4, 1, 2, 1.0, 0.0);
        fx.set(4, 0.6);
        let text = rack_preset_json("rack", "Wide", &fx.params, fx.rack.as_deref());
        assert_eq!(parse_rack(&text).as_ref(), fx.rack.as_deref());
        assert_eq!(parse_preset(&text).unwrap(), fx.params);
        assert!(parse_rack(&preset_json("eq", "x", &[])).is_none());
    }

    #[test]
    fn every_factory_preset_matches_its_device() {
        assert!(FACTORY_PRESETS.len() >= 200, "{} presets", FACTORY_PRESETS.len());
        for &(kind, name, text) in FACTORY_PRESETS {
            let doc = json::parse(text).unwrap_or_else(|e| panic!("{kind}/{name}: {e}"));
            assert_eq!(doc.get("kind").and_then(Json::as_str), Some(kind), "{kind}/{name}: kind");
            assert_eq!(doc.get("name").and_then(Json::as_str), Some(name), "{kind}/{name}: name");
            let raw = doc.get("params").and_then(Json::as_arr).unwrap_or_else(|| panic!("{kind}/{name}: params"));
            let params = parse_preset(text).expect("parsed");
            assert_eq!(params.len(), raw.len(), "{kind}/{name}: malformed pair");
            assert!(!params.is_empty(), "{kind}/{name}: empty");
            let infos = if let Some(k) = registry::instrument_kind(kind) {
                (k.create)().params()
            } else if let Some(k) = registry::effect_kind(kind) {
                (k.create)().params()
            } else {
                panic!("{kind}: no such device");
            };
            for (i, &(id, v)) in params.iter().enumerate() {
                let info = infos.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("{kind}/{name}: no param {id}"));
                assert!(v.is_finite() && v >= info.min && v <= info.max, "{kind}/{name}: {} = {v} out of range", info.name);
                assert!(params[..i].iter().all(|p| p.0 != id), "{kind}/{name}: param {id} twice");
            }
        }
    }

    #[test]
    fn every_native_device_has_factory_presets() {
        // External MIDI n'a pas de son ; Drum Pads se règle par kits d'échantillons (kit de départ).
        for k in registry::instruments().iter().filter(|k| !matches!(k.id, "midi_out" | "drum_pads")) {
            assert!(factory_preset_names(k.id).len() >= 6, "{}", k.id);
        }
        for id in ["eq", "compressor", "reverb", "delay", "saturator", "bus_comp", "vintage_verb", "chorus", "autofilter", "autotune"] {
            assert!(factory_preset_names(id).len() >= 6, "{id}");
        }
        assert!(factory_preset("eq", "Vocal Presence").is_some());
        assert!(factory_preset("eq", "Nope").is_none());
    }

    #[test]
    fn factory_presets_sound_and_stay_sane() {
        const SR: usize = 48_000;
        for &(kind, name, text) in FACTORY_PRESETS {
            let params = parse_preset(text).expect("parsed");
            let (mut l, mut r) = (vec![0.0f32; SR], vec![0.0f32; SR]);
            if let Some(k) = registry::instrument_kind(kind) {
                // Sample-based instruments are silent without a sample.
                if ["sampler", "slicer", "granular"].contains(&kind) {
                    continue;
                }
                let mut inst = (k.create)();
                inst.set_sample_rate(SR as f32);
                params.iter().for_each(|&(id, v)| inst.set_param(id, v));
                let notes: &[u8] = if inst.is_melodic() { &[48, 60, 64] } else { &[36, 38, 42, 60] };
                notes.iter().for_each(|&n| inst.note_on(n, 0.9));
                for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                    inst.render(a, b);
                }
            } else {
                let k = registry::effect_kind(kind).expect("device");
                let mut fx = (k.create)();
                fx.set_sample_rate(SR as f32);
                fx.set_tempo(120.0);
                params.iter().for_each(|&(id, v)| fx.set_param(id, v));
                // Decaying 110 Hz bursts with some noise on top, at −12 dBFS.
                let mut seed = 1u32;
                for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let t = (i % (SR / 4)) as f32 / SR as f32;
                    let x = (std::f32::consts::TAU * 110.0 * t).sin() + 0.2 * (seed as f32 / u32::MAX as f32 - 0.5);
                    *a = 0.25 * x * (-t * 8.0).exp();
                    *b = *a;
                }
                for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                    fx.process(a, b);
                }
            }
            let peak = l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(l.iter().chain(&r).all(|s| s.is_finite()), "{kind}/{name}: not finite");
            assert!(peak > 1e-3, "{kind}/{name}: silent");
            assert!(peak < 4.0, "{kind}/{name}: peak {peak}");
        }
    }

    #[test]
    fn json_roundtrip() {
        let p = [(0, 1.0), (14, -3.5)];
        assert_eq!(parse_preset(&preset_json("eq", "Test", &p)).unwrap(), p);
    }
}
