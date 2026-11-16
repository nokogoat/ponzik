//! Effect racks and modulators in the project model.
//!
//! A rack is an [`EffectDef`] of kind `"rack"` with a [`RackDef`]: its chains
//! of devices and its macro mappings. Every value lives in the rack's own
//! `params`: the macros and chain levels under their rack ids, the settings
//! of each device under [`unit_param`] ids, so automation, presets and the
//! sync to the engine treat them like any other parameter.
//!
//! Modulators live on a track ([`crate::TrackDef::modulators`], targets: its
//! insert slots) or a channel ([`crate::ChannelDef::modulators`], targets: its
//! instrument, slot 0).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ponzik_engine::effects::EffectRack;
use ponzik_engine::effects::fx_rack::{CHAIN_LEN, CHAINS, MACROS, P_MACRO, P_MIX, P_VOLUME, RACK_PARAMS, UNITS, split_param};
use ponzik_engine::{Effect, MacroMap, ModKind, ModLink, ModMeters, ModParam, ModSet, Modulator, ParamInfo, Params, RackLayout, registry};

use crate::{AutoTarget, EffectDef, Project};

/// Kind of the effect rack in the registry.
pub const RACK: &str = "rack";

/// One device of a rack.
#[derive(Clone, Debug, PartialEq)]
pub struct RackDevice {
    /// Stable number of the device in the rack (its parameter ids).
    pub unit: usize,
    pub kind: String,
    pub bypass: bool,
}

/// Chains and macro mappings of a rack.
#[derive(Clone, Debug, PartialEq)]
pub struct RackDef {
    /// One to [`CHAINS`] chains of at most [`CHAIN_LEN`] devices.
    pub chains: Vec<Vec<RackDevice>>,
    pub maps: Vec<MacroMap>,
}

impl Default for RackDef {
    fn default() -> Self {
        Self { chains: vec![Vec::new()], maps: Vec::new() }
    }
}

impl RackDef {
    pub fn devices(&self) -> impl Iterator<Item = &RackDevice> {
        self.chains.iter().flatten()
    }

    pub fn device(&self, unit: usize) -> Option<&RackDevice> {
        self.devices().find(|d| d.unit == unit)
    }

    /// (chain, position) of a device.
    pub fn position(&self, unit: usize) -> Option<(usize, usize)> {
        self.chains.iter().enumerate().find_map(|(c, ch)| ch.iter().position(|d| d.unit == unit).map(|p| (c, p)))
    }

    pub fn free_unit(&self) -> Option<usize> {
        (0..UNITS).find(|&u| self.device(u).is_none())
    }

    /// The engine's view of the chains.
    pub fn layout(&self) -> RackLayout {
        let mut layout = RackLayout::default();
        layout.chains = self.chains.len().clamp(1, CHAINS);
        for (c, chain) in self.chains.iter().enumerate().take(CHAINS) {
            let units: Vec<usize> = chain.iter().take(CHAIN_LEN).map(|d| d.unit).collect();
            layout.set_chain(c, &units);
            for d in chain {
                layout.set_bypass(d.unit, d.bypass);
            }
        }
        layout
    }

    /// Mapping of a device parameter, if a macro drives it.
    pub fn mapping(&self, unit: usize, param: u32) -> Option<&MacroMap> {
        self.maps.iter().find(|m| m.unit == unit && m.param == param)
    }

    /// Mappings of macro `k`.
    pub fn macro_maps(&self, k: usize) -> impl Iterator<Item = &MacroMap> {
        self.maps.iter().filter(move |m| m.macro_index == k)
    }
}

/// True if `kind` can go inside a rack (native effects, no nested rack).
pub fn rack_accepts(kind: &str) -> bool {
    kind != RACK && registry::effect_kind(kind).is_some()
}

thread_local! {
    static NATIVE: RefCell<HashMap<(bool, String), &'static [ParamInfo]>> = RefCell::new(HashMap::new());
}

/// Parameter sheets of a native device kind, cached (creating a device can
/// allocate a lot). Plugins have none here.
pub fn native_params(kind: &str, instrument: bool) -> &'static [ParamInfo] {
    NATIVE.with(|cache| {
        *cache.borrow_mut().entry((instrument, kind.to_string())).or_insert_with(|| {
            if instrument {
                registry::create_instrument(kind).map_or(&[], |i| i.params())
            } else {
                registry::create_effect(kind).map_or(&[], |e| e.params())
            }
        })
    })
}

fn rack_info(id: u32) -> Option<&'static ParamInfo> {
    native_params(RACK, false).iter().find(|p| p.id == id)
}

impl EffectDef {
    /// An empty rack, one chain.
    pub fn new_rack() -> Self {
        Self { rack: Some(Box::default()), ..Self::new(RACK) }
    }

    /// Sheet of a parameter of this effect, rack devices included.
    pub fn param_info(&self, id: u32) -> Option<ParamInfo> {
        match (self.rack.as_deref(), split_param(id)) {
            (Some(rack), Some((unit, local))) => {
                let d = rack.device(unit)?;
                native_params(&d.kind, false).iter().find(|p| p.id == local).copied()
            }
            _ => native_params(&self.kind, false).iter().find(|p| p.id == id).copied(),
        }
    }

    /// Value a parameter really has: a rack device parameter driven by a
    /// macro follows the macro.
    pub fn effective(&self, id: u32, info: &ParamInfo) -> f32 {
        if let (Some(rack), Some((unit, local))) = (self.rack.as_deref(), split_param(id))
            && let Some(map) = rack.mapping(unit, local)
        {
            return map.value(info, self.macro_value(map.macro_index));
        }
        self.get(id).unwrap_or(info.default)
    }

    pub fn macro_value(&self, k: usize) -> f32 {
        let id = P_MACRO + k as u32;
        self.get(id).or_else(|| rack_info(id).map(|p| p.default)).unwrap_or(0.0)
    }

    /// Adds a device to chain `chain` at `pos` (the end if `None`); its unit.
    pub fn rack_insert(&mut self, chain: usize, pos: Option<usize>, kind: &str) -> Option<usize> {
        if !rack_accepts(kind) {
            return None;
        }
        let rack = self.rack.as_deref_mut()?;
        let unit = rack.free_unit()?;
        let row = rack.chains.get_mut(chain)?;
        if row.len() >= CHAIN_LEN {
            return None;
        }
        let pos = pos.unwrap_or(row.len()).min(row.len());
        row.insert(pos, RackDevice { unit, kind: kind.into(), bypass: false });
        // A unit freed earlier starts from the device's defaults.
        self.params.retain(|(id, _)| split_param(*id).is_none_or(|(u, _)| u != unit));
        Some(unit)
    }

    /// Removes a device, its settings and its mappings.
    pub fn rack_remove(&mut self, unit: usize) -> bool {
        let Some(rack) = self.rack.as_deref_mut() else { return false };
        let Some((c, p)) = rack.position(unit) else { return false };
        rack.chains[c].remove(p);
        rack.maps.retain(|m| m.unit != unit);
        self.params.retain(|(id, _)| split_param(*id).is_none_or(|(u, _)| u != unit));
        true
    }

    /// Moves a device to chain `chain`, position `pos`.
    pub fn rack_move(&mut self, unit: usize, chain: usize, pos: usize) -> bool {
        let Some(rack) = self.rack.as_deref_mut() else { return false };
        let Some((c, p)) = rack.position(unit) else { return false };
        if chain >= rack.chains.len() || (chain != c && rack.chains[chain].len() >= CHAIN_LEN) {
            return false;
        }
        let d = rack.chains[c].remove(p);
        let pos = pos.min(rack.chains[chain].len());
        rack.chains[chain].insert(pos, d);
        true
    }

    pub fn rack_set_bypass(&mut self, unit: usize, bypass: bool) {
        if let Some(d) = self.rack.as_deref_mut().and_then(|r| r.chains.iter_mut().flatten().find(|d| d.unit == unit)) {
            d.bypass = bypass;
        }
    }

    /// Adds a parallel chain; its index.
    pub fn rack_add_chain(&mut self) -> Option<usize> {
        let rack = self.rack.as_deref_mut()?;
        if rack.chains.len() >= CHAINS {
            return None;
        }
        rack.chains.push(Vec::new());
        let c = rack.chains.len() - 1;
        self.params.retain(|(id, _)| *id != P_VOLUME + c as u32 && *id != P_MIX + c as u32);
        Some(c)
    }

    /// Removes chain `c` and its devices (the last chain stays).
    pub fn rack_remove_chain(&mut self, c: usize) -> Vec<usize> {
        let Some(rack) = self.rack.as_deref() else { return Vec::new() };
        if rack.chains.len() <= 1 || c >= rack.chains.len() {
            return Vec::new();
        }
        let units: Vec<usize> = rack.chains[c].iter().map(|d| d.unit).collect();
        for &u in &units {
            self.rack_remove(u);
        }
        let n = self.rack.as_deref().map_or(0, |r| r.chains.len());
        if let Some(rack) = self.rack.as_deref_mut() {
            rack.chains.remove(c);
        }
        // The chains after it move up, with their levels.
        let mut moved = Vec::new();
        for k in c..n {
            for base in [P_VOLUME, P_MIX] {
                let id = base + k as u32;
                let v = self.get(id);
                self.params.retain(|(i, _)| *i != id);
                if k > c
                    && let Some(v) = v
                {
                    moved.push((base + k as u32 - 1, v));
                }
            }
        }
        self.params.extend(moved);
        units
    }

    /// Maps macro `k` onto a device parameter between `min` and `max`
    /// (positions in its range); replaces an earlier mapping of it.
    pub fn map_macro(&mut self, k: usize, unit: usize, param: u32, min: f32, max: f32) {
        let Some(rack) = self.rack.as_deref_mut() else { return };
        if k >= MACROS || rack.device(unit).is_none() {
            return;
        }
        rack.maps.retain(|m| !(m.unit == unit && m.param == param));
        rack.maps.push(MacroMap { macro_index: k, unit, param, min: min.clamp(0.0, 1.0), max: max.clamp(0.0, 1.0) });
    }

    pub fn unmap(&mut self, unit: usize, param: u32) {
        if let Some(rack) = self.rack.as_deref_mut() {
            rack.maps.retain(|m| !(m.unit == unit && m.param == param));
        }
    }
}

/// A device of rack `def`, created and set (sample rate, tempo, its params).
pub fn make_unit(def: &EffectDef, unit: usize, sample_rate: f32, bpm: f32) -> Option<Box<dyn Effect>> {
    let d = def.rack.as_deref()?.device(unit)?;
    if !rack_accepts(&d.kind) {
        return None;
    }
    let mut fx = registry::create_effect(&d.kind)?;
    fx.set_sample_rate(sample_rate);
    fx.set_tempo(bpm);
    for &(id, v) in &def.params {
        if let Some((u, local)) = split_param(id)
            && u == unit
        {
            fx.set_param(local, v);
        }
    }
    Some(fx)
}

/// The whole rack, ready for the engine.
pub fn make_rack(def: &EffectDef, sample_rate: f32, bpm: f32) -> Box<dyn Effect> {
    let mut rack = EffectRack::default();
    rack.set_sample_rate(sample_rate);
    rack.set_tempo(bpm);
    if let Some(r) = def.rack.as_deref() {
        for d in r.devices() {
            rack.set_unit(d.unit, make_unit(def, d.unit, sample_rate, bpm));
        }
        rack.set_layout(r.layout());
        for (i, m) in r.maps.iter().enumerate() {
            rack.set_mapping(i, Some(*m));
        }
    }
    // The rack's own values last: the macros drive their mappings.
    for &(id, v) in def.params.iter().filter(|(id, _)| *id < RACK_PARAMS) {
        rack.set_param(id, v);
    }
    Box::new(rack)
}

/// Default value of a rack parameter (device parameters included).
pub fn rack_default(def: &EffectDef, id: u32) -> f32 {
    def.param_info(id).map_or(0.0, |p| p.default)
}

/// Commands that turn rack `old` into rack `new` in place (same insert).
pub fn rack_commands(track: usize, slot: usize, old: &EffectDef, new: &EffectDef, sample_rate: f32, bpm: f32) -> Vec<ponzik_engine::Command> {
    use ponzik_engine::Command;
    let mut cmds = Vec::new();
    let (ra, rb) = (old.rack.as_deref().cloned().unwrap_or_default(), new.rack.as_deref().cloned().unwrap_or_default());
    let mut fresh = Vec::new();
    for unit in 0..UNITS {
        let (a, b) = (ra.device(unit).map(|d| &d.kind), rb.device(unit).map(|d| &d.kind));
        if a != b {
            let effect = make_unit(new, unit, sample_rate, bpm);
            cmds.push(Command::SetRackUnit { track, slot, unit, effect });
            fresh.push(unit);
        }
    }
    if ra.layout() != rb.layout() {
        cmds.push(Command::SetRackLayout { track, slot, layout: rb.layout() });
    }
    for i in 0..ra.maps.len().max(rb.maps.len()) {
        let (a, b) = (ra.maps.get(i), rb.maps.get(i));
        // A new device takes its mappings again.
        let refresh = b.is_some_and(|m| fresh.contains(&m.unit));
        if a != b || refresh {
            cmds.push(Command::SetRackMapping { track, slot, index: i, map: b.copied() });
        }
    }
    // Values: devices first, then the rack's own (macros last).
    let mut changes: Vec<(u32, f32)> = Vec::new();
    for &(id, v) in &new.params {
        if split_param(id).is_some_and(|(u, _)| fresh.contains(&u)) {
            continue;
        }
        if old.get(id) != Some(v) {
            changes.push((id, v));
        }
    }
    for &(id, _) in &old.params {
        if new.get(id).is_none() && split_param(id).is_none_or(|(u, _)| !fresh.contains(&u) && rb.device(u).is_some()) {
            changes.push((id, rack_default(new, id)));
        }
    }
    changes.sort_by_key(|&(id, _)| (id < RACK_PARAMS, id));
    for (id, value) in changes {
        cmds.push(Command::SetEffectParam { track, slot, id, value });
    }
    cmds
}

// --- Modulators ---

/// A parameter driven by a modulator, with its bipolar depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModTarget {
    /// Insert slot (track modulators); 0 for a channel's instrument.
    pub slot: usize,
    pub param: u32,
    /// -1..1, in positions of the parameter's range.
    pub depth: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModulatorDef {
    pub kind: ModKind,
    /// Values that differ from the defaults.
    pub params: Vec<(u32, f32)>,
    pub targets: Vec<ModTarget>,
}

impl ModulatorDef {
    pub fn new(kind: ModKind) -> Self {
        Self { kind, params: Vec::new(), targets: Vec::new() }
    }

    pub fn get(&self, id: u32) -> f32 {
        self.params.iter().find(|p| p.0 == id).map(|p| p.1).or_else(|| self.info(id).map(|p| p.default)).unwrap_or(0.0)
    }

    pub fn set(&mut self, id: u32, value: f32) {
        let Some(info) = self.info(id) else { return };
        let value = info.clamp(value);
        match self.params.iter_mut().find(|(i, _)| *i == id) {
            Some(p) => p.1 = value,
            None => self.params.push((id, value)),
        }
    }

    pub fn info(&self, id: u32) -> Option<&'static ParamInfo> {
        self.kind.params().iter().find(|p| p.id == id)
    }

    pub fn target(&self, slot: usize, param: u32) -> Option<&ModTarget> {
        self.targets.iter().find(|t| t.slot == slot && t.param == param)
    }

    /// Connects (or changes the depth of) a target.
    pub fn connect(&mut self, slot: usize, param: u32, depth: f32) {
        let depth = depth.clamp(-1.0, 1.0);
        match self.targets.iter_mut().find(|t| t.slot == slot && t.param == param) {
            Some(t) => t.depth = depth,
            None => self.targets.push(ModTarget { slot, param, depth }),
        }
    }

    pub fn disconnect(&mut self, slot: usize, param: u32) {
        self.targets.retain(|t| !(t.slot == slot && t.param == param));
    }

    /// A running modulator, set like this one.
    pub fn build(&self, seed: u32) -> Modulator {
        let mut m = Modulator::new(self.kind).with_seed(seed);
        for &(id, v) in &self.params {
            m.set_param(id, v);
        }
        m
    }
}

/// Which modulators: a track's (`false`) or a channel's (`true`).
pub type MeterKey = (bool, usize);

/// Outputs of the modulators of a track or channel, shared between every set
/// the interface builds for it, so it keeps reading the running one.
pub fn mod_meters(key: MeterKey) -> Arc<ModMeters> {
    static METERS: OnceLock<Mutex<HashMap<MeterKey, Arc<ModMeters>>>> = OnceLock::new();
    let map = METERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key).or_default().clone()
}

/// Builds a modulator set; `param(slot, id)` gives the sheet and base value of
/// a target (`None`: the target is skipped). `None` without modulators.
pub fn make_mods(mods: &[ModulatorDef], key: MeterKey, param: impl Fn(usize, u32) -> Option<(ParamInfo, f32)>) -> Option<Box<ModSet>> {
    if mods.is_empty() {
        return None;
    }
    let mut params: Vec<ModParam> = Vec::new();
    let mut links = Vec::new();
    for (i, m) in mods.iter().enumerate() {
        for t in &m.targets {
            let at = match params.iter().position(|p| p.slot == t.slot && p.id == t.param) {
                Some(at) => at,
                None => {
                    let Some((info, base)) = param(t.slot, t.param) else { continue };
                    params.push(ModParam::new(t.slot, t.param, info, base));
                    params.len() - 1
                }
            };
            links.push(ModLink { modulator: i, param: at, depth: t.depth });
        }
    }
    let built = mods.iter().enumerate().map(|(i, m)| m.build(0x9e37 + i as u32 + 31 * key.1 as u32)).collect();
    Some(Box::new(ModSet::new(built, params, links, mod_meters(key))))
}

impl Project {
    /// Modulator set of track `t`, ready for the engine.
    pub fn make_track_mods(&self, t: usize) -> Option<Box<ModSet>> {
        let track = self.tracks.get(t)?;
        make_mods(&track.modulators, (false, t), |slot, id| {
            let fx = track.inserts.get(slot)?.as_ref()?;
            let info = fx.param_info(id)?;
            Some((info, fx.get(id).unwrap_or(info.default)))
        })
    }

    /// Modulator set of channel `c`, ready for the engine.
    pub fn make_channel_mods(&self, c: usize) -> Option<Box<ModSet>> {
        let ch = self.channels.get(c)?;
        make_mods(&ch.modulators, (true, c), |_, id| {
            let info = *native_params(&ch.instrument.kind, true).iter().find(|p| p.id == id)?;
            Some((info, ch.instrument.get(id).unwrap_or(info.default)))
        })
    }

    /// Removes a device from the rack in an insert slot, with its automation
    /// and the modulations that target it.
    pub fn remove_rack_device(&mut self, track: usize, slot: usize, unit: usize) {
        let Some(Some(fx)) = self.tracks.get_mut(track).map(|t| &mut t.inserts[slot]) else { return };
        if !fx.rack_remove(unit) {
            return;
        }
        let of_unit = |id: u32| split_param(id).is_some_and(|(u, _)| u == unit);
        self.automation.retain(|a| !matches!(a.target, AutoTarget::EffectParam { track: t, slot: s, id } if t == track && s == slot && of_unit(id)));
        for m in &mut self.tracks[track].modulators {
            m.targets.retain(|t| !(t.slot == slot && of_unit(t.param)));
        }
    }

    /// Drops the modulation targets that no longer point at a parameter
    /// (effect removed or replaced, instrument changed).
    pub fn prune_modulators(&mut self) {
        for t in 0..self.tracks.len() {
            let inserts = self.tracks[t].inserts.clone();
            for m in &mut self.tracks[t].modulators {
                m.targets.retain(|tg| inserts.get(tg.slot).and_then(Option::as_ref).is_some_and(|fx| fx.param_info(tg.param).is_some()));
            }
        }
        for ch in &mut self.channels {
            let params = native_params(&ch.instrument.kind, true);
            for m in &mut ch.modulators {
                m.targets.retain(|tg| params.iter().any(|p| p.id == tg.param));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::effects::fx_rack::unit_param;

    fn rack() -> EffectDef {
        let mut fx = EffectDef::new_rack();
        assert_eq!(fx.rack_insert(0, None, "eq"), Some(0));
        assert_eq!(fx.rack_insert(0, None, "saturator"), Some(1));
        assert_eq!(fx.rack_add_chain(), Some(1));
        assert_eq!(fx.rack_insert(1, None, "reverb"), Some(2));
        fx
    }

    #[test]
    fn editing_keeps_units_stable() {
        let mut fx = rack();
        assert_eq!(fx.rack_insert(0, None, RACK), None, "no rack in a rack");
        fx.set(unit_param(1, 1), 12.0);
        fx.map_macro(0, 1, 1, 0.2, 0.8);
        assert!(fx.rack_move(1, 1, 0));
        let r = fx.rack.as_deref().unwrap();
        assert_eq!(r.position(1), Some((1, 0)));
        assert_eq!(fx.get(unit_param(1, 1)), Some(12.0));
        let layout = r.layout();
        assert_eq!(layout.chain(1).collect::<Vec<_>>(), [1, 2]);
        assert!(fx.rack_remove(1));
        assert!(fx.get(unit_param(1, 1)).is_none());
        assert!(fx.rack.as_deref().unwrap().maps.is_empty());
        // The freed unit is reused, from its defaults.
        assert_eq!(fx.rack_insert(0, Some(0), "delay"), Some(1));
        assert_eq!(fx.rack.as_deref().unwrap().position(1), Some((0, 0)));
    }

    #[test]
    fn macro_ranges_and_inversion() {
        let mut fx = rack();
        let info = fx.param_info(unit_param(0, 1)).unwrap();
        fx.map_macro(3, 0, 1, 0.9, 0.1);
        fx.set(P_MACRO + 3, 0.0);
        assert!((info.to_norm(fx.effective(unit_param(0, 1), &info)) - 0.9).abs() < 1e-4);
        fx.set(P_MACRO + 3, 1.0);
        assert!((info.to_norm(fx.effective(unit_param(0, 1), &info)) - 0.1).abs() < 1e-4);
        fx.set(P_MACRO + 3, 0.25);
        assert!((info.to_norm(fx.effective(unit_param(0, 1), &info)) - 0.7).abs() < 1e-4);
    }

    #[test]
    fn removing_a_chain_moves_the_next_levels_up() {
        let mut fx = rack();
        fx.rack_add_chain();
        fx.set(P_VOLUME + 2, -6.0);
        fx.set(P_MIX + 1, 0.3);
        assert_eq!(fx.rack_remove_chain(1), [2]);
        assert_eq!(fx.rack.as_deref().unwrap().chains.len(), 2);
        assert_eq!(fx.get(P_VOLUME + 1), Some(-6.0));
        assert_eq!(fx.get(P_MIX + 1), None);
    }

    #[test]
    fn built_rack_matches_the_def() {
        let mut fx = rack();
        fx.set(unit_param(0, 1), 300.0);
        fx.map_macro(0, 2, 5, 0.0, 1.0);
        fx.set(P_MACRO, 0.4);
        let mut built = make_rack(&fx, 48_000.0, 120.0);
        assert_eq!(built.get_param(unit_param(0, 1)), 300.0);
        let info = fx.param_info(unit_param(2, 5)).unwrap();
        assert!((built.get_param(unit_param(2, 5)) - fx.effective(unit_param(2, 5), &info)).abs() < 1e-5);
        let r = built.as_rack().unwrap();
        assert_eq!(r.layout(), fx.rack.as_deref().unwrap().layout());
    }

    #[test]
    fn rack_edits_become_minimal_commands() {
        let a = rack();
        let mut b = a.clone();
        b.set(unit_param(0, 1), 500.0);
        b.set(P_MACRO + 1, 0.7);
        let names = |c: &[ponzik_engine::Command]| c.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>();
        assert_eq!(names(&rack_commands(1, 0, &a, &b, 48_000.0, 120.0)), ["SetEffectParam", "SetEffectParam"]);
        let mut c = b.clone();
        c.rack_insert(1, None, "delay");
        c.map_macro(1, 3, 0, 0.0, 1.0);
        assert_eq!(names(&rack_commands(1, 0, &b, &c, 48_000.0, 120.0)), ["SetRackUnit", "SetRackLayout", "SetRackMapping"]);
        let mut d = c.clone();
        d.rack_set_bypass(0, true);
        assert_eq!(names(&rack_commands(1, 0, &c, &d, 48_000.0, 120.0)), ["SetRackLayout"]);
    }

    #[test]
    fn modulators_build_their_targets() {
        let mut p = Project::empty();
        p.tracks[2].inserts[1] = Some(rack());
        let mut lfo = ModulatorDef::new(ModKind::Lfo);
        lfo.connect(1, P_MACRO, 0.5);
        lfo.connect(1, unit_param(0, 1), -0.25);
        lfo.connect(5, 0, 1.0); // empty slot: skipped
        p.tracks[2].modulators.push(lfo);
        let set = p.make_track_mods(2).unwrap();
        assert_eq!(set.params.len(), 2);
        assert_eq!(set.links.len(), 2);
        assert_eq!(set.links[1].depth, -0.25);
        p.prune_modulators();
        assert_eq!(p.tracks[2].modulators[0].targets.len(), 2);
        p.remove_rack_device(2, 1, 0);
        assert_eq!(p.tracks[2].modulators[0].targets.len(), 1);
    }
}
