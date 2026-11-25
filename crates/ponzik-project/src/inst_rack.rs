//! Instrument Racks (layers) in the project model.
//!
//! A rack is an [`InstrumentDef`] of kind [`INST_RACK`] with an
//! [`InstRackDef`]: its chains (each an instrument, note effects and
//! effects, by kind, and its zones), its macro mappings, macro names and
//! variations. Like the Effect Rack, every value lives in the rack's own
//! `params`: the macros, selector and chain levels under their rack ids, the
//! settings of each device under [`unit_param`] ids. Automation, modulators,
//! presets, undo and the sync to the engine treat them like any other
//! instrument parameter.

use std::cell::RefCell;
use std::collections::HashMap;

pub use ponzik_engine::instruments::inst_rack::{
    CHAIN_FX, CHAIN_NOTE_FX, CHAINS, ChainZone, KIND as INST_RACK, LayerDevice, MACROS, MAX_MAPPINGS, P_MACRO, P_PAN,
    P_SELECTOR, P_VOLUME, RACK_PARAMS, SELF_UNIT, SLOTS, Slot, UNITS, Zone, rack_params, slot_of, split_param, unit_of,
    unit_param,
};
use ponzik_engine::instruments::InstrumentRack;
use ponzik_engine::{Command, Instrument, MacroMap, ParamInfo, Params, registry};

use crate::json::Json;
use crate::{AutoTarget, EffectDef, InstrumentDef, NoteFxDef, Project, SampleLoader};

/// One chain of a rack.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerChain {
    /// Stable number of the chain (0..[`CHAINS`]): its units and level ids.
    pub index: usize,
    /// Empty: named after its instrument.
    pub name: String,
    /// Its instrument: kind, sample, pads and plugin state (its values are
    /// in the rack's params).
    pub instrument: Option<InstrumentDef>,
    /// Note effects, by kind ([`CHAIN_NOTE_FX`] places).
    pub note_fx: Vec<Option<NoteFxDef>>,
    /// Effects after the instrument ([`CHAIN_FX`] places).
    pub effects: Vec<Option<EffectDef>>,
    pub zone: ChainZone,
}

impl LayerChain {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            name: String::new(),
            instrument: None,
            note_fx: vec![None; CHAIN_NOTE_FX],
            effects: vec![None; CHAIN_FX],
            zone: ChainZone::active(),
        }
    }

    /// Kind of the device in `slot`.
    pub fn kind(&self, slot: Slot) -> Option<&str> {
        match slot {
            Slot::Instrument => self.instrument.as_ref().map(|i| i.kind.as_str()),
            Slot::NoteFx(k) => self.note_fx.get(k)?.as_ref().map(|f| f.kind.as_str()),
            Slot::Effect(k) => self.effects.get(k)?.as_ref().map(|f| f.kind.as_str()),
        }
    }
}

/// Stored macro values, recalled at once.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerVariation {
    pub name: String,
    pub macros: Vec<f32>,
}

/// Chains, mappings and macro settings of an Instrument Rack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstRackDef {
    /// Sorted by index, at most [`CHAINS`].
    pub chains: Vec<LayerChain>,
    pub maps: Vec<MacroMap>,
    /// Names given to the macros (empty: named after what they drive).
    pub macro_names: Vec<String>,
    pub variations: Vec<LayerVariation>,
    /// One bit per macro that Randomize leaves alone.
    pub rand_exclude: u16,
    /// Shows sixteen macros instead of eight.
    pub wide: bool,
}

impl InstRackDef {
    pub fn chain(&self, index: usize) -> Option<&LayerChain> {
        self.chains.iter().find(|c| c.index == index)
    }

    pub fn chain_mut(&mut self, index: usize) -> Option<&mut LayerChain> {
        self.chains.iter_mut().find(|c| c.index == index)
    }

    pub fn free_chain(&self) -> Option<usize> {
        (0..CHAINS).find(|&i| self.chain(i).is_none())
    }

    /// Kind of the device on `unit`.
    pub fn device_kind(&self, unit: usize) -> Option<&str> {
        let (c, slot) = slot_of(unit);
        self.chain(c)?.kind(slot)
    }

    /// Zone of chain `index` as the engine plays it (off when missing).
    pub fn zone(&self, index: usize) -> ChainZone {
        self.chain(index).map_or_else(ChainZone::default, |c| ChainZone { on: true, ..c.zone })
    }

    pub fn mapping(&self, unit: usize, param: u32) -> Option<&MacroMap> {
        self.maps.iter().find(|m| m.unit == unit && m.param == param)
    }

    pub fn macro_maps(&self, k: usize) -> impl Iterator<Item = &MacroMap> {
        self.maps.iter().filter(move |m| m.macro_index == k)
    }

    pub fn macro_name(&self, k: usize) -> Option<&str> {
        self.macro_names.get(k).map(String::as_str).filter(|s| !s.is_empty())
    }

    pub fn excluded(&self, k: usize) -> bool {
        k < 16 && self.rand_exclude & (1 << k) != 0
    }
}

thread_local! {
    static SHEETS: RefCell<HashMap<(u8, String), &'static [ParamInfo]>> = RefCell::new(HashMap::new());
}

/// Parameter sheets of a device kind for a slot (instrument, note effect,
/// effect), native or plugin, cached.
pub fn sheets(kind: &str, slot: Slot) -> &'static [ParamInfo] {
    let what = match slot {
        Slot::Instrument => 0u8,
        Slot::NoteFx(_) => 1,
        Slot::Effect(_) => 2,
    };
    SHEETS.with(|cache| {
        *cache.borrow_mut().entry((what, kind.to_string())).or_insert_with(|| {
            use crate::plugins;
            let plugin = plugins::Format::of_kind(kind).is_some();
            match what {
                0 if kind == INST_RACK => &[],
                0 if plugin => plugins::instrument_from_kind(kind).and_then(Result::ok).map_or(&[], |i| i.params()),
                0 => registry::create_instrument(kind).map_or(&[], |i| i.params()),
                1 => registry::create_note_fx(kind).map_or(&[], |f| f.params()),
                _ if plugin => plugins::effect_from_kind(kind).and_then(Result::ok).map_or(&[], |e| e.params()),
                _ => registry::create_effect(kind).map_or(&[], |e| e.params()),
            }
        })
    })
}

/// True if `kind` can play in a chain's `slot` (no rack inside a rack).
pub fn layer_accepts(kind: &str, slot: Slot) -> bool {
    let plugin = crate::plugins::Format::of_kind(kind).is_some();
    match slot {
        Slot::Instrument => kind != INST_RACK && (plugin || registry::instrument_kind(kind).is_some()),
        Slot::NoteFx(_) => registry::note_fx_kind(kind).is_some(),
        Slot::Effect(_) => kind != crate::rack::RACK && (plugin || registry::effect_kind(kind).is_some()),
    }
}

/// Sheet of one of the rack's own parameters.
pub fn own_info(id: u32) -> Option<&'static ParamInfo> {
    rack_params().get(id as usize)
}

impl InstrumentDef {
    /// An empty Instrument Rack.
    pub fn new_layer_rack() -> Self {
        Self::new(INST_RACK)
    }

    pub fn layers(&self) -> Option<&InstRackDef> {
        self.rack.as_deref().filter(|_| self.kind == INST_RACK)
    }

    pub fn layers_mut(&mut self) -> Option<&mut InstRackDef> {
        if self.kind != INST_RACK {
            return None;
        }
        self.rack.as_deref_mut()
    }

    /// Sheet of a parameter of this instrument, rack devices included
    /// (a plugin's own parameters only inside a rack).
    pub fn param_info(&self, id: u32) -> Option<ParamInfo> {
        let Some(rack) = self.layers() else {
            return crate::rack::native_params(&self.kind, true).iter().find(|p| p.id == id).copied();
        };
        match split_param(id) {
            Some((unit, local)) => {
                let kind = rack.device_kind(unit)?;
                sheets(kind, slot_of(unit).1).iter().find(|p| p.id == local).copied()
            }
            None => own_info(id).copied(),
        }
    }

    pub fn macro_value(&self, k: usize) -> f32 {
        let id = P_MACRO + k as u32;
        self.get(id).or_else(|| own_info(id).map(|p| p.default)).unwrap_or(0.0)
    }

    /// Value a parameter really has: one driven by a macro follows the macro.
    pub fn effective(&self, id: u32, info: &ParamInfo) -> f32 {
        if let Some(rack) = self.layers() {
            let (unit, param) = split_param(id).unwrap_or((SELF_UNIT, id));
            if let Some(map) = rack.mapping(unit, param) {
                return map.value(info, self.macro_value(map.macro_index));
            }
        }
        self.get(id).unwrap_or(info.default)
    }

    /// Adds a chain (the first free number) playing `kind` (an instrument),
    /// or empty; its index.
    pub fn layer_add_chain(&mut self, kind: Option<&str>) -> Option<usize> {
        let rack = self.layers_mut()?;
        let index = rack.free_chain()?;
        let mut chain = LayerChain::new(index);
        chain.instrument = kind.filter(|k| layer_accepts(k, Slot::Instrument)).map(InstrumentDef::new);
        let at = rack.chains.partition_point(|c| c.index < index);
        rack.chains.insert(at, chain);
        self.clear_chain_values(index);
        Some(index)
    }

    /// Values of chain `index`: its devices, level and pan.
    fn clear_chain_values(&mut self, index: usize) {
        let of_chain = |id: u32| match split_param(id) {
            Some((u, _)) => slot_of(u).0 == index,
            None => id == P_VOLUME + index as u32 || id == P_PAN + index as u32,
        };
        self.params.retain(|(id, _)| !of_chain(*id));
        if let Some(rack) = self.layers_mut() {
            rack.maps.retain(|m| {
                let own = m.unit == SELF_UNIT && (m.param == P_VOLUME + index as u32 || m.param == P_PAN + index as u32);
                !own && (m.unit >= UNITS || slot_of(m.unit).0 != index)
            });
        }
    }

    /// Removes chain `index`, its devices, values and mappings.
    pub fn layer_remove_chain(&mut self, index: usize) -> bool {
        let Some(rack) = self.layers_mut() else { return false };
        let Some(at) = rack.chains.iter().position(|c| c.index == index) else { return false };
        rack.chains.remove(at);
        self.clear_chain_values(index);
        true
    }

    /// Places a device of `kind` (`None`: empties it) on a slot of chain
    /// `index`; it starts from its defaults, without mappings.
    pub fn layer_set_device(&mut self, index: usize, slot: Slot, kind: Option<&str>) -> bool {
        if kind.is_some_and(|k| !layer_accepts(k, slot)) {
            return false;
        }
        let Some(chain) = self.layers_mut().and_then(|r| r.chain_mut(index)) else { return false };
        match slot {
            Slot::Instrument => chain.instrument = kind.map(InstrumentDef::new),
            Slot::NoteFx(k) if k < CHAIN_NOTE_FX => {
                chain.note_fx.resize(CHAIN_NOTE_FX, None);
                chain.note_fx[k] = kind.map(NoteFxDef::new);
            }
            Slot::Effect(k) if k < CHAIN_FX => {
                chain.effects.resize(CHAIN_FX, None);
                chain.effects[k] = kind.map(EffectDef::new);
            }
            _ => return false,
        }
        chain.zone.set_bypass(slot, false);
        let unit = unit_of(index, slot);
        self.params.retain(|(id, _)| split_param(*id).is_none_or(|(u, _)| u != unit));
        if let Some(rack) = self.layers_mut() {
            rack.maps.retain(|m| m.unit != unit);
        }
        true
    }

    /// Maps macro `k` onto a parameter (`unit` [`SELF_UNIT`]: one of the
    /// rack's own, not a macro) between `min` and `max`, positions in its
    /// range; replaces an earlier mapping of it.
    pub fn layer_map(&mut self, k: usize, unit: usize, param: u32, min: f32, max: f32) -> bool {
        let ok = match unit {
            SELF_UNIT => (P_SELECTOR..RACK_PARAMS).contains(&param),
            u => self.layers().and_then(|r| r.device_kind(u)).is_some(),
        };
        let Some(rack) = self.layers_mut() else { return false };
        if k >= MACROS || !ok || rack.maps.len() >= MAX_MAPPINGS && rack.mapping(unit, param).is_none() {
            return false;
        }
        rack.maps.retain(|m| !(m.unit == unit && m.param == param));
        rack.maps.push(MacroMap { macro_index: k, unit, param, min: min.clamp(0.0, 1.0), max: max.clamp(0.0, 1.0) });
        true
    }

    pub fn layer_unmap(&mut self, unit: usize, param: u32) {
        if let Some(rack) = self.layers_mut() {
            rack.maps.retain(|m| !(m.unit == unit && m.param == param));
        }
    }

    /// Stores the macro values as a new variation; its index.
    pub fn store_variation(&mut self, name: &str) -> Option<usize> {
        let macros: Vec<f32> = (0..MACROS).map(|k| self.macro_value(k)).collect();
        let rack = self.layers_mut()?;
        rack.variations.push(LayerVariation { name: name.to_string(), macros });
        Some(rack.variations.len() - 1)
    }

    /// Sets the macros to variation `i`.
    pub fn recall_variation(&mut self, i: usize) -> bool {
        let Some(v) = self.layers().and_then(|r| r.variations.get(i)).cloned() else { return false };
        for (k, &m) in v.macros.iter().enumerate().take(MACROS) {
            self.set(P_MACRO + k as u32, m.clamp(0.0, 1.0));
        }
        true
    }

    /// Random values for the macros not excluded (`seed`: any number).
    pub fn randomize_macros(&mut self, seed: u32) {
        let Some(rack) = self.layers() else { return };
        let exclude = rack.rand_exclude;
        let mut x = seed.max(1);
        for k in 0..MACROS {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if exclude & (1 << k) == 0 {
                self.set(P_MACRO + k as u32, (x >> 8) as f32 / (1u32 << 24) as f32);
            }
        }
    }

    /// This instrument, its values and plugin state, as the first chain of a
    /// new rack (the values move to the chain's unit).
    pub fn into_layer_rack(self) -> InstrumentDef {
        if self.kind == INST_RACK {
            return self;
        }
        let mut rack = InstrumentDef::new_layer_rack();
        let unit = unit_of(0, Slot::Instrument);
        rack.params = self.params.iter().filter(|(id, _)| *id <= 0xFF_FFFF).map(|&(id, v)| (unit_param(unit, id), v)).collect();
        let mut chain = LayerChain::new(0);
        chain.instrument = Some(InstrumentDef { params: Vec::new(), ab: None, rack: None, ..self });
        if let Some(r) = rack.layers_mut() {
            r.chains.push(chain);
        }
        rack
    }

    /// The chain instrument a project file of another program keeps: the
    /// first chain's, with its values (`self` when not a rack).
    pub fn first_layer(&self) -> Option<InstrumentDef> {
        let Some(rack) = self.layers() else { return Some(self.clone()) };
        let chain = rack.chains.iter().find(|c| c.instrument.is_some())?;
        let mut inst = chain.instrument.clone()?;
        inst.params = unit_values(self, unit_of(chain.index, Slot::Instrument));
        Some(inst)
    }
}

/// Values of the device on `unit`, under its own ids.
pub fn unit_values(def: &InstrumentDef, unit: usize) -> Vec<(u32, f32)> {
    def.params.iter().filter_map(|&(id, v)| split_param(id).filter(|&(u, _)| u == unit).map(|(_, local)| (local, v))).collect()
}

/// A device of rack `def`, created and set (sample rate, tempo, values).
pub fn make_device(def: &InstrumentDef, unit: usize, sample_rate: f32, bpm: f32, loader: SampleLoader) -> LayerDevice {
    let Some(rack) = def.layers() else { return LayerDevice::None };
    let (c, slot) = slot_of(unit);
    let Some(chain) = rack.chain(c) else { return LayerDevice::None };
    let values = unit_values(def, unit);
    match slot {
        Slot::Instrument => match chain.instrument.as_ref().filter(|i| layer_accepts(&i.kind, slot)) {
            Some(i) => {
                let inst = InstrumentDef { params: values, ..i.clone() };
                let mut made = Project::make_instrument(&inst, sample_rate, loader);
                made.set_tempo(bpm);
                LayerDevice::Instrument(made)
            }
            None => LayerDevice::None,
        },
        Slot::NoteFx(k) => chain
            .note_fx
            .get(k)
            .and_then(Option::as_ref)
            .and_then(|f| Project::make_note_fx(&NoteFxDef { params: values, ..f.clone() }, sample_rate, bpm))
            .map_or(LayerDevice::None, LayerDevice::NoteFx),
        Slot::Effect(k) => chain
            .effects
            .get(k)
            .and_then(Option::as_ref)
            .filter(|f| layer_accepts(&f.kind, slot))
            .and_then(|f| Project::make_effect(&EffectDef { params: values, ..f.clone() }, sample_rate, bpm))
            .map_or(LayerDevice::None, LayerDevice::Effect),
    }
}

/// The whole rack, ready for the engine.
pub fn make_layer_rack(def: &InstrumentDef, sample_rate: f32, bpm: f32, loader: SampleLoader) -> Box<dyn Instrument> {
    let mut rack = InstrumentRack::default();
    rack.set_sample_rate(sample_rate);
    rack.set_tempo(bpm);
    if let Some(r) = def.layers() {
        for chain in &r.chains {
            for s in 0..SLOTS {
                let unit = unit_of(chain.index, Slot::from_index(s));
                let device = make_device(def, unit, sample_rate, bpm, loader);
                if !device.is_none() {
                    rack.set_device(unit, device);
                }
            }
            rack.set_zone(chain.index, r.zone(chain.index));
        }
        for (i, m) in r.maps.iter().enumerate() {
            rack.set_mapping(i, Some(*m));
        }
    }
    // The rack's own values last: the macros drive their mappings.
    let mut own: Vec<(u32, f32)> = def.params.iter().copied().filter(|(id, _)| *id < RACK_PARAMS).collect();
    own.sort_by_key(|&(id, _)| (id < P_SELECTOR, id));
    for (id, v) in own {
        rack.set_param(id, v);
    }
    rack.set_sample_rate(sample_rate);
    Box::new(rack)
}

/// Default value of a rack parameter (device parameters included).
pub fn layer_default(def: &InstrumentDef, id: u32) -> f32 {
    def.param_info(id).map_or(0.0, |p| p.default)
}

/// What makes two chain devices the same running device.
fn same_device(a: &InstRackDef, b: &InstRackDef, unit: usize) -> bool {
    let (c, slot) = slot_of(unit);
    let (x, y) = (a.chain(c), b.chain(c));
    match slot {
        Slot::Instrument => {
            let (i, j) = (x.and_then(|c| c.instrument.as_ref()), y.and_then(|c| c.instrument.as_ref()));
            match (i, j) {
                (Some(i), Some(j)) => i.kind == j.kind && i.sample == j.sample && i.pads == j.pads && i.state == j.state,
                (None, None) => true,
                _ => false,
            }
        }
        Slot::NoteFx(_) | Slot::Effect(_) => x.and_then(|c| c.kind(slot)) == y.and_then(|c| c.kind(slot)),
    }
}

/// Commands that turn rack `old` into rack `new` in place (same channel).
pub fn layer_commands(channel: usize, old: &InstrumentDef, new: &InstrumentDef, sample_rate: f32, bpm: f32, loader: SampleLoader) -> Vec<Command> {
    let mut cmds = Vec::new();
    let (ra, rb) = (old.layers().cloned().unwrap_or_default(), new.layers().cloned().unwrap_or_default());
    let mut fresh = Vec::new();
    for unit in 0..UNITS {
        if !same_device(&ra, &rb, unit) {
            let device = make_device(new, unit, sample_rate, bpm, loader);
            cmds.push(Command::SetLayerDevice { channel, unit, device });
            fresh.push(unit);
        }
    }
    for c in 0..CHAINS {
        if ra.zone(c) != rb.zone(c) {
            cmds.push(Command::SetLayerZone { channel, chain: c, zone: rb.zone(c) });
        }
    }
    for i in 0..ra.maps.len().max(rb.maps.len()) {
        let (a, b) = (ra.maps.get(i), rb.maps.get(i));
        let refresh = b.is_some_and(|m| fresh.contains(&m.unit));
        if a != b || refresh {
            cmds.push(Command::SetLayerMapping { channel, index: i, map: b.copied() });
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
        let gone = split_param(id).is_some_and(|(u, _)| fresh.contains(&u) || rb.device_kind(u).is_none());
        if new.get(id).is_none() && !gone {
            changes.push((id, layer_default(new, id)));
        }
    }
    changes.sort_by_key(|&(id, _)| (id < RACK_PARAMS, id < P_SELECTOR, id));
    for (id, value) in changes {
        cmds.push(Command::SetInstrumentParam { channel, id, value });
    }
    cmds
}

impl Project {
    /// Turns channel `c`'s instrument into an Instrument Rack whose first
    /// chain plays it; its automation and modulations follow it.
    pub fn group_into_layer_rack(&mut self, c: usize) -> bool {
        let Some(ch) = self.channels.get_mut(c) else { return false };
        if ch.instrument.kind == INST_RACK {
            return false;
        }
        let inst = std::mem::replace(&mut ch.instrument, InstrumentDef::new("pulse"));
        ch.instrument = inst.into_layer_rack();
        let unit = unit_of(0, Slot::Instrument);
        for m in &mut ch.modulators {
            m.targets.retain(|t| t.param <= 0xFF_FFFF);
            for t in &mut m.targets {
                t.param = unit_param(unit, t.param);
            }
        }
        for a in &mut self.automation {
            if let AutoTarget::InstrumentParam { channel, id } = &mut a.target
                && *channel == c
            {
                *id = unit_param(unit, *id);
            }
        }
        self.automation.retain(|a| !matches!(a.target, AutoTarget::InstrumentParam { channel, id } if channel == c && split_param(id).is_none()));
        true
    }

    /// Removes chain `index` of channel `c`'s rack, with the automation and
    /// modulations of its devices and levels.
    pub fn remove_layer_chain(&mut self, c: usize, index: usize) -> bool {
        let Some(ch) = self.channels.get_mut(c) else { return false };
        if !ch.instrument.layer_remove_chain(index) {
            return false;
        }
        self.prune_layer_targets(c, |id| match split_param(id) {
            Some((u, _)) => slot_of(u).0 == index,
            None => id == P_VOLUME + index as u32 || id == P_PAN + index as u32,
        });
        true
    }

    /// Empties (or replaces, `kind`) a device of a chain, with the automation
    /// and modulations of its parameters.
    pub fn set_layer_device(&mut self, c: usize, index: usize, slot: Slot, kind: Option<&str>) -> bool {
        let Some(ch) = self.channels.get_mut(c) else { return false };
        if !ch.instrument.layer_set_device(index, slot, kind) {
            return false;
        }
        let unit = unit_of(index, slot);
        self.prune_layer_targets(c, |id| split_param(id).is_some_and(|(u, _)| u == unit));
        true
    }

    fn prune_layer_targets(&mut self, c: usize, gone: impl Fn(u32) -> bool) {
        self.automation.retain(|a| !matches!(a.target, AutoTarget::InstrumentParam { channel, id } if channel == c && gone(id)));
        if let Some(ch) = self.channels.get_mut(c) {
            for m in &mut ch.modulators {
                m.targets.retain(|t| !gone(t.param));
            }
        }
    }

    /// Swaps two effect places of a chain, with their values, mappings,
    /// automation and modulations.
    pub fn swap_layer_effects(&mut self, c: usize, index: usize, a: usize, b: usize) -> bool {
        if a >= CHAIN_FX || b >= CHAIN_FX || a == b {
            return false;
        }
        let Some(ch) = self.channels.get_mut(c) else { return false };
        let Some(chain) = ch.instrument.layers_mut().and_then(|r| r.chain_mut(index)) else { return false };
        chain.effects.resize(CHAIN_FX, None);
        chain.effects.swap(a, b);
        let (sa, sb) = (Slot::Effect(a), Slot::Effect(b));
        let (ba, bb) = (chain.zone.bypassed(sa), chain.zone.bypassed(sb));
        chain.zone.set_bypass(sa, bb);
        chain.zone.set_bypass(sb, ba);
        let (ua, ub) = (unit_of(index, sa), unit_of(index, sb));
        let swap = |u: usize| if u == ua { ub } else if u == ub { ua } else { u };
        let swap_id = |id: u32| match split_param(id) {
            Some((u, local)) => unit_param(swap(u), local),
            None => id,
        };
        for p in &mut ch.instrument.params {
            p.0 = swap_id(p.0);
        }
        if let Some(r) = ch.instrument.layers_mut() {
            for m in &mut r.maps {
                if m.unit < UNITS {
                    m.unit = swap(m.unit);
                }
            }
        }
        for m in &mut ch.modulators {
            for t in &mut m.targets {
                t.param = swap_id(t.param);
            }
        }
        for au in &mut self.automation {
            if let AutoTarget::InstrumentParam { channel, id } = &mut au.target
                && *channel == c
            {
                *id = swap_id(*id);
            }
        }
        true
    }

    /// What an export to another program does to the Instrument Racks: one
    /// line per rack, or nothing.
    pub fn layer_export_notes(&self) -> Vec<String> {
        self.channels
            .iter()
            .filter_map(|c| {
                let rack = c.instrument.layers()?;
                let kept = rack.chains.iter().find(|ch| ch.instrument.is_some()).map(|ch| ch.index + 1);
                Some(match kept {
                    Some(k) if rack.chains.len() > 1 => {
                        ponzik_i18n::tr_fmt!("{}: Instrument Rack, only chain {} kept", c.name, k)
                    }
                    Some(_) => ponzik_i18n::tr_fmt!("{}: Instrument Rack, its instrument kept", c.name),
                    None => ponzik_i18n::tr_fmt!("{}: empty Instrument Rack skipped", c.name),
                })
            })
            .collect()
    }
}

// --- JSON ---

fn zone_json(z: &Zone) -> Json {
    Json::Arr(vec![z.lo.into(), z.hi.into(), z.fade_lo.into(), z.fade_hi.into()])
}

fn load_zone(v: Option<&Json>) -> Zone {
    let a = v.and_then(Json::as_arr).unwrap_or(&[]);
    let n = |i: usize, d: u8| a.get(i).and_then(Json::as_f64).map_or(d, |x| x.clamp(0.0, 127.0) as u8);
    Zone { lo: n(0, 0), hi: n(1, 127), fade_lo: n(2, 0), fade_hi: n(3, 0) }.sanitized()
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Chains, mappings, macro names and variations of a rack.
pub(crate) fn layer_rack_json(r: &InstRackDef) -> Json {
    let chains = r
        .chains
        .iter()
        .map(|c| {
            let mut pairs = vec![("index", c.index.into())];
            if !c.name.is_empty() {
                pairs.push(("name", c.name.as_str().into()));
            }
            if let Some(i) = &c.instrument {
                pairs.push(("instrument", crate::io::instrument_json(i)));
            }
            let kinds = |list: Vec<Option<String>>| Json::Arr(list.into_iter().map(|k| k.map_or(Json::Null, Json::Str)).collect());
            if c.note_fx.iter().any(Option::is_some) {
                pairs.push(("note_fx", kinds(c.note_fx.iter().map(|f| f.as_ref().map(|f| f.kind.clone())).collect())));
            }
            if c.effects.iter().any(Option::is_some) {
                let fx = c.effects.iter().map(|f| match f {
                    Some(f) => {
                        let mut p = vec![("kind", f.kind.as_str().into())];
                        if let Some(state) = &f.state {
                            p.push(("state", crate::io::state_json(state)));
                        }
                        obj(p)
                    }
                    None => Json::Null,
                });
                pairs.push(("effects", Json::Arr(fx.collect())));
            }
            let z = &c.zone;
            pairs.push(("key", zone_json(&z.key)));
            pairs.push(("vel", zone_json(&z.vel)));
            pairs.push(("sel", zone_json(&z.sel)));
            for (name, on) in [("mute", z.mute), ("solo", z.solo)] {
                if on {
                    pairs.push((name, true.into()));
                }
            }
            if z.transpose != 0 {
                pairs.push(("transpose", (z.transpose as f64).into()));
            }
            if z.bypass != 0 {
                pairs.push(("bypass", (z.bypass as f64).into()));
            }
            obj(pairs)
        })
        .collect();
    let maps = r.maps.iter().map(|m| Json::Arr(vec![m.macro_index.into(), m.unit.into(), m.param.into(), m.min.into(), m.max.into()])).collect();
    let mut pairs = vec![("chains", Json::Arr(chains)), ("maps", Json::Arr(maps))];
    if r.macro_names.iter().any(|n| !n.is_empty()) {
        pairs.push(("macro_names", Json::Arr(r.macro_names.iter().map(|n| n.as_str().into()).collect())));
    }
    if !r.variations.is_empty() {
        let vars = r
            .variations
            .iter()
            .map(|v| obj(vec![("name", v.name.as_str().into()), ("macros", Json::Arr(v.macros.iter().map(|&m| m.into()).collect()))]))
            .collect();
        pairs.push(("variations", Json::Arr(vars)));
    }
    if r.rand_exclude != 0 {
        pairs.push(("rand_exclude", (r.rand_exclude as f64).into()));
    }
    if r.wide {
        pairs.push(("wide", true.into()));
    }
    obj(pairs)
}

/// A rack written by [`layer_rack_json`]: unknown kinds, repeated or out of
/// range chains and mappings of missing devices are dropped.
pub(crate) fn load_layer_rack(v: Option<&Json>) -> InstRackDef {
    let mut rack = InstRackDef::default();
    let arr = |v: Option<&Json>| v.and_then(Json::as_arr).unwrap_or(&[]).to_vec();
    let num = |v: Option<&Json>, d: f64| v.and_then(Json::as_f64).unwrap_or(d);
    let flag = |v: Option<&Json>| matches!(v, Some(Json::Bool(true)));
    for c in arr(v.and_then(|v| v.get("chains"))) {
        let index = num(c.get("index"), -1.0);
        if !(0.0..CHAINS as f64).contains(&index) || rack.chain(index as usize).is_some() {
            continue;
        }
        let mut chain = LayerChain::new(index as usize);
        chain.name = c.get("name").and_then(Json::as_str).unwrap_or("").to_string();
        chain.instrument = c.get("instrument").map(crate::io::load_instrument).filter(|i| layer_accepts(&i.kind, Slot::Instrument));
        if let Some(i) = chain.instrument.as_mut() {
            i.params.clear();
            i.rack = None;
        }
        for (k, f) in arr(c.get("note_fx")).iter().enumerate().take(CHAIN_NOTE_FX) {
            chain.note_fx[k] = f.as_str().filter(|k| layer_accepts(k, Slot::NoteFx(0))).map(NoteFxDef::new);
        }
        for (k, f) in arr(c.get("effects")).iter().enumerate().take(CHAIN_FX) {
            chain.effects[k] = f.get("kind").and_then(Json::as_str).filter(|kind| layer_accepts(kind, Slot::Effect(0))).map(|kind| EffectDef {
                state: crate::io::load_state(f.get("state")),
                ..EffectDef::new(kind)
            });
        }
        chain.zone = ChainZone {
            on: true,
            mute: flag(c.get("mute")),
            solo: flag(c.get("solo")),
            key: load_zone(c.get("key")),
            vel: load_zone(c.get("vel")),
            sel: load_zone(c.get("sel")),
            transpose: num(c.get("transpose"), 0.0).clamp(-48.0, 48.0) as i8,
            bypass: num(c.get("bypass"), 0.0).clamp(0.0, ((1u32 << SLOTS) - 1) as f64) as u16,
        };
        rack.chains.push(chain);
    }
    rack.chains.sort_by_key(|c| c.index);
    for m in arr(v.and_then(|v| v.get("maps"))).iter().take(MAX_MAPPINGS) {
        let m = m.as_arr().unwrap_or(&[]);
        let n = |i: usize| m.get(i).and_then(Json::as_f64);
        let (Some(k), Some(unit), Some(param)) = (n(0), n(1), n(2)) else { continue };
        if !(0.0..MACROS as f64).contains(&k) || !(0.0..=SELF_UNIT as f64).contains(&unit) || !(0.0..=u32::MAX as f64).contains(&param) {
            continue;
        }
        let (k, unit, param) = (k as usize, unit as usize, param as u32);
        let ok = if unit == SELF_UNIT { (P_SELECTOR..RACK_PARAMS).contains(&param) } else { rack.device_kind(unit).is_some() };
        if ok && rack.mapping(unit, param).is_none() {
            let r = |i: usize, d: f64| n(i).unwrap_or(d).clamp(0.0, 1.0) as f32;
            rack.maps.push(MacroMap { macro_index: k, unit, param, min: r(3, 0.0), max: r(4, 1.0) });
        }
    }
    rack.macro_names = arr(v.and_then(|v| v.get("macro_names"))).iter().take(MACROS).map(|n| n.as_str().unwrap_or("").to_string()).collect();
    for var in arr(v.and_then(|v| v.get("variations"))).iter().take(64) {
        let macros: Vec<f32> = arr(var.get("macros")).iter().take(MACROS).map(|m| m.as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32).collect();
        rack.variations.push(LayerVariation { name: var.get("name").and_then(Json::as_str).unwrap_or("").to_string(), macros });
    }
    rack.rand_exclude = num(v.and_then(|v| v.get("rand_exclude")), 0.0).clamp(0.0, u16::MAX as f64) as u16;
    rack.wide = flag(v.and_then(|v| v.get("wide")));
    rack
}

/// A preset of a whole rack: its values, chains and mappings.
pub fn layer_preset_json(name: &str, def: &InstrumentDef) -> String {
    let mut doc = crate::json::parse(&crate::presets::preset_json(INST_RACK, name, &def.params)).unwrap_or(Json::Null);
    if let (Json::Obj(pairs), Some(rack)) = (&mut doc, def.layers()) {
        pairs.push(("layers".into(), layer_rack_json(rack)));
    }
    doc.pretty()
}

/// Chains and mappings of a rack preset (`None` for other presets).
pub fn parse_layer_preset(text: &str) -> Option<InstRackDef> {
    let doc = crate::json::parse(text).ok()?;
    let rack = doc.get("layers")?;
    Some(load_layer_rack(Some(rack)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModulatorDef;
    use ponzik_engine::ModKind;

    fn piano_strings() -> InstrumentDef {
        let mut def = InstrumentDef::new_layer_rack();
        assert_eq!(def.layer_add_chain(Some("piano")), Some(0));
        assert_eq!(def.layer_add_chain(Some("strings")), Some(1));
        let r = def.layers_mut().unwrap();
        r.chains[0].zone.key = Zone::new(0, 59).with_fades(0, 3);
        r.chains[1].zone.key = Zone::new(60, 127).with_fades(3, 0);
        r.chains[1].zone.transpose = 12;
        def.layer_set_device(1, Slot::Effect(0), Some("reverb"));
        def.layer_set_device(1, Slot::NoteFx(0), Some("chord"));
        def
    }

    #[test]
    fn chains_keep_their_numbers_and_values() {
        let mut def = piano_strings();
        assert_eq!(def.layer_add_chain(Some(INST_RACK)), Some(2), "no rack in a rack: an empty chain");
        assert!(def.layers().unwrap().chain(2).unwrap().instrument.is_none());
        def.set(P_VOLUME + 2, -6.0);
        def.set(unit_param(unit_of(2, Slot::Effect(1)), 3), 0.5);
        assert!(def.layer_remove_chain(1));
        assert_eq!(def.layer_add_chain(None), Some(1), "the freed number comes back");
        assert!(def.layers().unwrap().chain(1).unwrap().kind(Slot::Effect(0)).is_none());
        assert!(def.layer_remove_chain(2));
        assert!(def.get(P_VOLUME + 2).is_none());
        assert!(def.params.is_empty());
        assert!(!def.layer_set_device(0, Slot::Effect(0), Some("rack")), "no effect rack inside");
        assert!(!def.layer_set_device(0, Slot::Instrument, Some("nope")));
    }

    #[test]
    fn macro_mapping_ranges() {
        let mut def = piano_strings();
        let rev = unit_of(1, Slot::Effect(0));
        let info = def.param_info(unit_param(rev, 5)).expect("reverb param");
        assert!(def.layer_map(2, rev, 5, 0.8, 0.2));
        assert!(def.layer_map(2, SELF_UNIT, P_SELECTOR, 0.0, 1.0));
        assert!(!def.layer_map(2, SELF_UNIT, P_MACRO + 3, 0.0, 1.0), "no macro on a macro");
        assert!(!def.layer_map(2, unit_of(5, Slot::Instrument), 0, 0.0, 1.0), "no device there");
        def.set(P_MACRO + 2, 0.0);
        assert!((info.to_norm(def.effective(unit_param(rev, 5), &info)) - 0.8).abs() < 1e-4);
        def.set(P_MACRO + 2, 1.0);
        assert!((info.to_norm(def.effective(unit_param(rev, 5), &info)) - 0.2).abs() < 1e-4);
        let sel = *own_info(P_SELECTOR).unwrap();
        assert_eq!(def.effective(P_SELECTOR, &sel), 127.0);
        def.set(P_MACRO + 2, 0.5);
        assert!((info.to_norm(def.effective(unit_param(rev, 5), &info)) - 0.5).abs() < 1e-4);
        // The built rack agrees.
        let built = make_layer_rack(&def, 48_000.0, 120.0, &|_| None);
        assert!((info.to_norm(built.get_param(unit_param(rev, 5))) - 0.5).abs() < 1e-4);
        assert!((built.get_param(P_SELECTOR) - 63.5).abs() < 1e-3);
        // Replacing the device drops its mapping.
        def.layer_set_device(1, Slot::Effect(0), Some("delay"));
        assert!(def.layers().unwrap().mapping(rev, 5).is_none());
    }

    #[test]
    fn variations_recall_and_randomize_spares_excluded_macros() {
        let mut def = piano_strings();
        def.set(P_MACRO, 0.25);
        def.set(P_MACRO + 5, 0.9);
        assert_eq!(def.store_variation("Soft"), Some(0));
        def.set(P_MACRO, 1.0);
        def.set(P_MACRO + 5, 0.0);
        assert_eq!(def.store_variation("Bright"), Some(1));
        assert!(def.recall_variation(0));
        assert_eq!((def.macro_value(0), def.macro_value(5)), (0.25, 0.9));
        assert!(def.recall_variation(1));
        assert_eq!((def.macro_value(0), def.macro_value(5)), (1.0, 0.0));
        assert!(!def.recall_variation(7));
        def.layers_mut().unwrap().rand_exclude = 1 << 5;
        def.randomize_macros(1234);
        assert_eq!(def.macro_value(5), 0.0, "excluded");
        assert_ne!(def.macro_value(0), 1.0);
        assert!((0..MACROS).all(|k| (0.0..=1.0).contains(&def.macro_value(k))));
        let a: Vec<f32> = (0..MACROS).map(|k| def.macro_value(k)).collect();
        def.randomize_macros(99);
        assert_ne!(a, (0..MACROS).map(|k| def.macro_value(k)).collect::<Vec<_>>());
    }

    #[test]
    fn edits_become_minimal_commands() {
        let a = piano_strings();
        let names = |c: &[Command]| c.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>();
        let mut b = a.clone();
        b.set(unit_param(unit_of(0, Slot::Instrument), 1), 0.3);
        b.set(P_MACRO + 1, 0.7);
        let none: SampleLoader = &|_| None;
        assert_eq!(names(&layer_commands(3, &a, &b, 48_000.0, 120.0, none)), ["SetInstrumentParam", "SetInstrumentParam"]);
        let mut c = b.clone();
        c.layers_mut().unwrap().chains[1].zone.vel = Zone::new(40, 127);
        assert_eq!(names(&layer_commands(3, &b, &c, 48_000.0, 120.0, none)), ["SetLayerZone"]);
        let mut d = c.clone();
        d.layer_set_device(0, Slot::Effect(2), Some("chorus"));
        d.layer_map(0, unit_of(0, Slot::Effect(2)), 1, 0.0, 1.0);
        assert_eq!(names(&layer_commands(3, &c, &d, 48_000.0, 120.0, none)), ["SetLayerDevice", "SetLayerMapping"]);
        let mut e = d.clone();
        e.layer_remove_chain(1);
        let cmds = names(&layer_commands(3, &d, &e, 48_000.0, 120.0, none));
        assert_eq!(cmds.iter().filter(|c| *c == "SetLayerDevice").count(), 3, "{cmds:?}");
        assert!(cmds.contains(&"SetLayerZone".to_string()));
    }

    #[test]
    fn grouping_a_channel_keeps_its_sound_and_curves() {
        let mut p = Project::empty();
        let ch = p.add_channel("keys", InstrumentDef::new("pulse").with(3, 0.42), 60);
        let mut lfo = ModulatorDef::new(ModKind::Lfo);
        lfo.connect(0, 3, 0.5);
        p.channels[ch].modulators.push(lfo);
        p.add_automation_point(AutoTarget::InstrumentParam { channel: ch, id: 3 }, ponzik_engine::AutoScope::Song, 0, 0.1);
        assert!(p.group_into_layer_rack(ch));
        assert!(!p.group_into_layer_rack(ch), "already a rack");
        let def = &p.channels[ch].instrument;
        let id = unit_param(unit_of(0, Slot::Instrument), 3);
        assert_eq!(def.kind, INST_RACK);
        assert_eq!(def.get(id), Some(0.42));
        assert_eq!(def.layers().unwrap().chain(0).unwrap().kind(Slot::Instrument), Some("pulse"));
        assert_eq!(p.channels[ch].modulators[0].targets[0].param, id);
        assert!(p.automation.iter().any(|a| a.target == AutoTarget::InstrumentParam { channel: ch, id }));
        assert!(p.make_channel_mods(ch).is_some_and(|m| m.params.len() == 1));
        assert_eq!(def.first_layer().unwrap().params, [(3, 0.42)]);
        // Removing the chain takes its curves and modulations along.
        assert!(p.remove_layer_chain(ch, 0));
        assert!(p.automation.is_empty());
        assert!(p.channels[ch].modulators[0].targets.is_empty());
        assert_eq!(p.layer_export_notes().len(), 1);
    }

    #[test]
    fn swapping_effects_moves_their_values_and_curves() {
        let mut p = Project::empty();
        let ch = p.add_channel("layers", piano_strings(), 60);
        p.set_layer_device(ch, 1, Slot::Effect(1), Some("delay"));
        let (ua, ub) = (unit_of(1, Slot::Effect(0)), unit_of(1, Slot::Effect(1)));
        p.channels[ch].instrument.set(unit_param(ua, 2), 0.7);
        p.channels[ch].instrument.layer_map(0, ua, 2, 0.0, 1.0);
        p.add_automation_point(AutoTarget::InstrumentParam { channel: ch, id: unit_param(ub, 1) }, ponzik_engine::AutoScope::Song, 0, 0.3);
        assert!(p.swap_layer_effects(ch, 1, 0, 1));
        let def = &p.channels[ch].instrument;
        let chain = def.layers().unwrap().chain(1).unwrap();
        assert_eq!((chain.kind(Slot::Effect(0)), chain.kind(Slot::Effect(1))), (Some("delay"), Some("reverb")));
        assert_eq!(def.get(unit_param(ub, 2)), Some(0.7));
        assert!(def.layers().unwrap().mapping(ub, 2).is_some());
        assert!(p.automation.iter().any(|a| a.target == AutoTarget::InstrumentParam { channel: ch, id: unit_param(ua, 1) }));
    }

    #[test]
    fn racks_round_trip_and_tolerate_damage() {
        let mut p = Project::empty();
        let mut def = piano_strings();
        def.layers_mut().unwrap().chains[0].zone.vel = Zone::new(10, 100).with_fades(5, 7);
        def.layers_mut().unwrap().chains[0].zone.mute = true;
        def.layers_mut().unwrap().chains[1].zone.sel = Zone::new(64, 127).with_fades(20, 0);
        def.layers_mut().unwrap().chains[1].zone.set_bypass(Slot::Effect(0), true);
        def.layers_mut().unwrap().chains[1].name = "Pad".into();
        def.layer_map(1, unit_of(1, Slot::Effect(0)), 5, 0.1, 0.9);
        def.layer_map(0, SELF_UNIT, P_SELECTOR, 0.0, 1.0);
        def.set(P_MACRO, 0.3);
        def.set(unit_param(unit_of(0, Slot::Instrument), 2), 0.6);
        def.store_variation("A");
        let r = def.layers_mut().unwrap();
        r.macro_names = vec!["Morph".into()];
        r.rand_exclude = 3;
        r.wide = true;
        let ch = p.add_channel("layers", def, 60);
        let q = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(q.channels[ch].instrument, p.channels[ch].instrument);
        // Damaged: unknown kinds, repeated chains, bad mappings.
        let text = r#"{"format": "ponzik-project", "channels": [{"instrument": {"kind": "instrument_rack", "rack": {
            "chains": [{"index": 0, "instrument": {"kind": "nope"}, "key": [70, 20, 300]},
                       {"index": 0}, {"index": 99}, {"index": 3, "effects": [{"kind": "rack"}, {"kind": "eq"}], "note_fx": ["arpeggiator", 5]}],
            "maps": [[0, 0, 1], [1, 25, 2, 0.5, 0.2], [99, 25, 2], [2, 128, 3], [2, 128, 16, 0.0, 1.0]],
            "variations": [{"macros": [2, -1]}]}}}]}"#;
        let q = crate::io::load(text).unwrap();
        let r = q.channels[0].instrument.layers().expect("rack");
        assert_eq!(r.chains.iter().map(|c| c.index).collect::<Vec<_>>(), [0, 3]);
        assert!(r.chains[0].instrument.is_none());
        assert_eq!(r.chains[0].zone.key, Zone { lo: 20, hi: 70, fade_lo: 20, fade_hi: 0 });
        assert_eq!((r.chains[1].kind(Slot::Effect(0)), r.chains[1].kind(Slot::Effect(1))), (None, Some("eq")));
        assert_eq!(r.chains[1].kind(Slot::NoteFx(0)), Some("arpeggiator"));
        assert_eq!(r.maps, [MacroMap { macro_index: 1, unit: 25, param: 2, min: 0.5, max: 0.2 }, MacroMap { macro_index: 2, unit: SELF_UNIT, param: 16, min: 0.0, max: 1.0 }]);
        assert_eq!(r.variations[0].macros, [1.0, 0.0]);
        // An old file without a rack gets an empty one.
        let q = crate::io::load(r#"{"format": "ponzik-project", "channels": [{"instrument": {"kind": "instrument_rack"}}]}"#).unwrap();
        assert!(q.channels[0].instrument.layers().is_some_and(|r| r.chains.is_empty()));
    }

    #[test]
    fn presets_keep_the_whole_rack() {
        let mut def = piano_strings();
        def.set(P_MACRO + 4, 0.8);
        def.layer_map(4, unit_of(1, Slot::Effect(0)), 5, 0.0, 1.0);
        let text = layer_preset_json("Keys and pad", &def);
        assert_eq!(crate::presets::parse_preset(&text).unwrap(), def.params);
        assert_eq!(parse_layer_preset(&text).as_ref(), def.layers());
        assert!(parse_layer_preset(&crate::presets::preset_json("pulse", "x", &[])).is_none());
    }
}
