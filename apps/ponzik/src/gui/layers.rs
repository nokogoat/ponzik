//! Instrument Rack card (layers): the macro knobs with their map buttons,
//! variations and Randomize; the chains as rows with mute, solo, level, pan
//! and a zone bar (keys over a mini keyboard, velocities, or chain selector
//! values with its handle); the devices of the selected chain; and the
//! settings of the open device. Instruments and effects dropped from the
//! browser land on a chain, or make a new one.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::{AutoTarget, ParamInfo, registry};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::InstrumentDef;
use ponzik_project::inst_rack::{
    CHAIN_FX, CHAINS, INST_RACK, InstRackDef, MACROS, P_MACRO, P_PAN, P_SELECTOR, P_VOLUME, RACK_PARAMS,
    SELF_UNIT, SLOTS, Slot, Zone, own_info, sheets, slot_of, split_param, unit_of, unit_param,
};
use ponzik_ui::{Canvas, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, Focus, MenuItem, Popup};
use super::devices::{Device, card_width};
use super::dnd::DropTarget;
use super::widgets::{Icon, draw_icon, wid};

const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GAP: f32 = 18.0;
const CHAINS_W: f32 = 490.0;
const SLOTS_W: f32 = 132.0;
const ROW_MAX: f32 = 26.0;
const RULER_H: f32 = 14.0;

/// Which zone the chain bars show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZoneView {
    #[default]
    Key,
    Vel,
    Sel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Lo,
    Hi,
    Move,
    FadeLo,
    FadeHi,
}

#[derive(Clone, Copy, Debug)]
struct ZoneDrag {
    channel: usize,
    chain: usize,
    part: Part,
    start: Zone,
}

/// Interface state of the racks: selected chain and open device per
/// channel, the macro waiting for a knob to map, the zone being dragged.
#[derive(Default)]
pub struct LayerUi {
    sel: HashMap<usize, usize>,
    open: HashMap<usize, usize>,
    /// (channel, macro) in map mode: the next knob of the rack moved maps to it.
    pub learn: Option<(usize, usize)>,
    pub view: ZoneView,
    drag: Option<ZoneDrag>,
}

/// Edits of an Instrument Rack, from its menus.
#[derive(Clone, Debug)]
pub enum LayerAction {
    Group(usize),
    AddChain { ch: usize, kind: Option<String> },
    RemoveChain { ch: usize, chain: usize },
    SetDevice { ch: usize, chain: usize, slot: usize, kind: Option<String> },
    Bypass { ch: usize, chain: usize, slot: usize },
    SwapFx { ch: usize, chain: usize, a: usize, b: usize },
    Mute { ch: usize, chain: usize },
    Solo { ch: usize, chain: usize },
    Transpose { ch: usize, chain: usize, by: i8 },
    ResetZones { ch: usize, chain: usize },
    Learn { ch: usize, k: usize },
    Map { ch: usize, unit: usize, param: u32, k: usize },
    Unmap { ch: usize, unit: usize, param: u32 },
    Invert { ch: usize, unit: usize, param: u32 },
    FullRange { ch: usize, unit: usize, param: u32 },
    StoreVariation(usize),
    RecallVariation { ch: usize, i: usize },
    DeleteVariation { ch: usize, i: usize },
    Randomize(usize),
    Exclude { ch: usize, k: usize },
    Wide(usize),
}

/// Menus of a rack.
#[derive(Clone, Debug)]
pub enum LayerMenu {
    AddChain(usize),
    Chain { ch: usize, chain: usize },
    Slot { ch: usize, chain: usize, slot: usize },
    Variations(usize),
}

thread_local! {
    static UNIT_SHEETS: RefCell<HashMap<(String, usize), &'static [ParamInfo]>> = RefCell::new(HashMap::new());
}

/// Sheets of the device `kind` on `unit`, under their rack ids (made once
/// per kind and unit), so the usual knobs edit, automate and modulate them.
pub fn unit_sheets(kind: &str, unit: usize) -> &'static [ParamInfo] {
    UNIT_SHEETS.with(|cache| {
        *cache.borrow_mut().entry((kind.to_string(), unit)).or_insert_with(|| {
            let list: Vec<ParamInfo> = sheets(kind, slot_of(unit).1)
                .iter()
                .filter(|p| p.id <= 0xFF_FFFF)
                .map(|p| ParamInfo { id: unit_param(unit, p.id), ..*p })
                .collect();
            Box::leak(list.into_boxed_slice())
        })
    })
}

/// Shown name of a device kind in a slot.
pub fn kind_name(kind: &str, slot: Slot) -> String {
    let native = match slot {
        Slot::Instrument => registry::instrument_kind(kind).map(|k| k.name),
        Slot::NoteFx(_) => registry::note_fx_kind(kind).map(|k| k.name),
        Slot::Effect(_) => registry::effect_kind(kind).map(|k| k.name),
    };
    native.map_or_else(|| super::app::display_kind(kind), |n| tr(n).to_string())
}

/// Sheet of a rack parameter by its id (own or of a device).
fn info_of(def: &InstrumentDef, id: u32) -> Option<ParamInfo> {
    def.param_info(id)
}

fn slot_icon(slot: Slot) -> Icon {
    match slot {
        Slot::Instrument => Icon::Instrument,
        Slot::NoteFx(_) => Icon::Arpeggio,
        Slot::Effect(_) => Icon::Effect,
    }
}

impl App {
    fn layer_def(&self, ch: usize) -> Option<&InstrumentDef> {
        self.project.channels.get(ch).map(|c| &c.instrument).filter(|i| i.kind == INST_RACK)
    }

    fn layer_def_mut(&mut self, ch: usize) -> Option<&mut InstrumentDef> {
        self.project.channels.get_mut(ch).map(|c| &mut c.instrument).filter(|i| i.kind == INST_RACK)
    }

    fn layer_rack(&self, ch: usize) -> Option<&InstRackDef> {
        self.layer_def(ch)?.layers()
    }

    /// Chain selected in the rack of channel `ch`: the chosen one, else the first.
    fn layer_sel(&self, ch: usize) -> Option<usize> {
        let rack = self.layer_rack(ch)?;
        let chosen = self.layers.sel.get(&ch).copied().filter(|&c| rack.chain(c).is_some());
        chosen.or_else(|| rack.chains.first().map(|c| c.index))
    }

    /// Device open in the rack: the chosen one if it is in the selected chain, else its instrument.
    fn layer_open(&self, ch: usize) -> Option<usize> {
        let rack = self.layer_rack(ch)?;
        let chain = self.layer_sel(ch)?;
        let chosen = self.layers.open.get(&ch).copied().filter(|&u| slot_of(u).0 == chain && rack.device_kind(u).is_some());
        chosen.or_else(|| (0..SLOTS).map(|s| unit_of(chain, Slot::from_index(s))).find(|&u| rack.device_kind(u).is_some()))
    }

    fn macro_count(&self, ch: usize) -> usize {
        if self.layer_rack(ch).is_some_and(|r| r.wide) { MACROS } else { 8 }
    }

    pub(super) fn layer_width(&self, ch: usize) -> f32 {
        let macros = (self.macro_count(ch) / 2) as f32 * CELL_W;
        let open = self.layer_open(ch).and_then(|u| Some((u, self.layer_rack(ch)?.device_kind(u)?.to_string())));
        let inner = open.map_or(0.0, |(u, kind)| GAP + card_width("", unit_sheets(&kind, u)) - 28.0);
        28.0 + macros + GAP + CHAINS_W + GAP + SLOTS_W + inner
    }

    pub(super) fn run_layer_action(&mut self, action: LayerAction) {
        use LayerAction as A;
        let zone_edit = |app: &mut App, ch: usize, chain: usize, f: &dyn Fn(&mut ponzik_project::inst_rack::ChainZone)| {
            if let Some(c) = app.layer_def_mut(ch).and_then(|d| d.layers_mut()).and_then(|r| r.chain_mut(chain)) {
                f(&mut c.zone);
            }
        };
        let label = match action {
            A::Group(ch) => {
                if !self.project.group_into_layer_rack(ch) {
                    return;
                }
                self.focus = Focus::Channel(ch);
                tr("Group into an Instrument Rack")
            }
            A::AddChain { ch, kind } => match self.layer_def_mut(ch).and_then(|d| d.layer_add_chain(kind.as_deref())) {
                Some(c) => {
                    self.layers.sel.insert(ch, c);
                    self.layers.open.remove(&ch);
                    tr("Add a chain")
                }
                None => {
                    self.toast(&tr_fmt!("A rack holds {} chains at most", CHAINS));
                    return;
                }
            },
            A::RemoveChain { ch, chain } => {
                self.project.remove_layer_chain(ch, chain);
                tr("Remove a chain")
            }
            A::SetDevice { ch, chain, slot, kind } => {
                let s = Slot::from_index(slot);
                if !self.project.set_layer_device(ch, chain, s, kind.as_deref()) {
                    self.toast(tr("This device cannot go there"));
                    return;
                }
                self.layers.sel.insert(ch, chain);
                if kind.is_some() {
                    self.layers.open.insert(ch, unit_of(chain, s));
                }
                if kind.is_some() { tr("Add a device to the chain") } else { tr("Remove a device from the chain") }
            }
            A::Bypass { ch, chain, slot } => {
                zone_edit(self, ch, chain, &|z| {
                    let s = Slot::from_index(slot);
                    z.set_bypass(s, !z.bypassed(s))
                });
                tr("Bypass a device")
            }
            A::SwapFx { ch, chain, a, b } => {
                self.project.swap_layer_effects(ch, chain, a, b);
                self.layers.open.insert(ch, unit_of(chain, Slot::Effect(b)));
                tr("Move a device in the chain")
            }
            A::Mute { ch, chain } => {
                zone_edit(self, ch, chain, &|z| z.mute = !z.mute);
                tr("Mute a chain")
            }
            A::Solo { ch, chain } => {
                zone_edit(self, ch, chain, &|z| z.solo = !z.solo);
                tr("Solo a chain")
            }
            A::Transpose { ch, chain, by } => {
                zone_edit(self, ch, chain, &|z| z.transpose = if by == 0 { 0 } else { (z.transpose as i32 + by as i32).clamp(-48, 48) as i8 });
                tr("Transpose a chain")
            }
            A::ResetZones { ch, chain } => {
                zone_edit(self, ch, chain, &|z| {
                    z.key = Zone::FULL;
                    z.vel = Zone::FULL;
                    z.sel = Zone::FULL;
                });
                tr("Chain zone")
            }
            A::Learn { ch, k } => {
                if self.layers.learn == Some((ch, k)) {
                    self.layers.learn = None;
                } else {
                    self.layers.learn = Some((ch, k));
                    self.toast(&tr_fmt!("Move a knob of the rack to map it to Macro {}", k + 1));
                }
                return;
            }
            A::Map { ch, unit, param, k } => {
                if let Some(d) = self.layer_def_mut(ch) {
                    d.layer_map(k, unit, param, 0.0, 1.0);
                }
                tr("Map to a macro")
            }
            A::Unmap { ch, unit, param } => {
                if let Some(d) = self.layer_def_mut(ch) {
                    d.layer_unmap(unit, param);
                }
                tr("Remove a macro mapping")
            }
            A::Invert { ch, unit, param } | A::FullRange { ch, unit, param } => {
                let invert = matches!(action, A::Invert { .. });
                if let Some(m) = self.layer_def_mut(ch).and_then(|d| d.layers_mut()).and_then(|r| r.maps.iter_mut().find(|m| m.unit == unit && m.param == param)) {
                    if invert {
                        std::mem::swap(&mut m.min, &mut m.max);
                    } else {
                        (m.min, m.max) = if m.min > m.max { (1.0, 0.0) } else { (0.0, 1.0) };
                    }
                }
                tr("Macro range")
            }
            A::StoreVariation(ch) => {
                let n = self.layer_rack(ch).map_or(0, |r| r.variations.len());
                if let Some(d) = self.layer_def_mut(ch) {
                    d.store_variation(&tr_fmt!("Variation {}", n + 1));
                }
                tr("Store a variation")
            }
            A::RecallVariation { ch, i } => {
                if let Some(d) = self.layer_def_mut(ch) {
                    d.recall_variation(i);
                }
                tr("Recall a variation")
            }
            A::DeleteVariation { ch, i } => {
                if let Some(r) = self.layer_def_mut(ch).and_then(|d| d.layers_mut())
                    && i < r.variations.len()
                {
                    r.variations.remove(i);
                }
                tr("Delete a variation")
            }
            A::Randomize(ch) => {
                let seed = (self.time * 1000.0) as u32 ^ 0x9e37_79b9;
                if let Some(d) = self.layer_def_mut(ch) {
                    d.randomize_macros(seed);
                }
                tr("Randomize the macros")
            }
            A::Exclude { ch, k } => {
                if let Some(r) = self.layer_def_mut(ch).and_then(|d| d.layers_mut()) {
                    r.rand_exclude ^= 1 << k;
                }
                tr("Randomize leaves a macro alone")
            }
            A::Wide(ch) => {
                if let Some(r) = self.layer_def_mut(ch).and_then(|d| d.layers_mut()) {
                    r.wide = !r.wide;
                }
                tr("Sixteen macros")
            }
        };
        self.commit(label);
    }

    /// Map mode: a knob of the rack moved while a macro waits maps to it
    /// (true: the move is taken by the mapping).
    pub(super) fn layer_learn_hit(&mut self, device: Device, p: &ParamInfo) -> bool {
        let Some((ch, k)) = self.layers.learn else { return false };
        if device != Device::Instrument(ch) || self.layer_def(ch).is_none() {
            return false;
        }
        let (unit, param) = split_param(p.id).unwrap_or((SELF_UNIT, p.id));
        if unit == SELF_UNIT && !(P_SELECTOR..RACK_PARAMS).contains(&param) {
            return false;
        }
        self.layers.learn = None;
        self.drag = None;
        self.run_layer_action(LayerAction::Map { ch, unit, param, k });
        self.toast(&tr_fmt!("{} mapped to Macro {}", tr(p.name), k + 1));
        true
    }

    /// Menu lines of a knob of a rack: map it to a macro.
    pub(super) fn layer_macro_items(&self, ch: usize, id: u32) -> Vec<MenuItem> {
        let Some(rack) = self.layer_rack(ch) else { return Vec::new() };
        let (unit, param) = split_param(id).unwrap_or((SELF_UNIT, id));
        if unit == SELF_UNIT && !(P_SELECTOR..RACK_PARAMS).contains(&param) {
            return Vec::new();
        }
        let current = rack.mapping(unit, param).map(|m| m.macro_index);
        let mut v: Vec<MenuItem> = (0..self.macro_count(ch))
            .map(|k| MenuItem::new(Icon::Rack, &tr_fmt!("Map to Macro {}", k + 1), Action::Layer(LayerAction::Map { ch, unit, param, k })).on(current == Some(k)))
            .collect();
        if let Some(first) = v.first_mut() {
            first.sep = true;
        }
        if current.is_some() {
            v.push(MenuItem::new(Icon::Swap, tr("Invert the range"), Action::Layer(LayerAction::Invert { ch, unit, param })).tip(tr("The knob goes down when the macro goes up")));
            v.push(MenuItem::new(Icon::Reset, tr("Full range"), Action::Layer(LayerAction::FullRange { ch, unit, param })));
            v.push(MenuItem::new(Icon::Close, tr("Remove the macro mapping"), Action::Layer(LayerAction::Unmap { ch, unit, param })));
        }
        v
    }

    pub(super) fn layer_menu(&self, menu: &LayerMenu) -> (String, Vec<MenuItem>) {
        use LayerAction as A;
        match *menu {
            LayerMenu::AddChain(ch) => {
                let rows = registry::instruments()
                    .iter()
                    .filter(|k| k.id != INST_RACK)
                    .map(|k| (k.category, MenuItem::new(Icon::Instrument, tr(k.name), Action::Layer(A::AddChain { ch, kind: Some(k.id.to_string()) })).tip(tr(k.description))))
                    .collect();
                let mut v = super::menu::by_category(rows, super::menu::category_icon);
                v.push(MenuItem::new(Icon::Plus, tr("Empty chain"), Action::Layer(A::AddChain { ch, kind: None })).sep());
                (tr("Add a chain").into(), v)
            }
            LayerMenu::Chain { ch, chain } => {
                let Some(c) = self.layer_rack(ch).and_then(|r| r.chain(chain)) else { return (String::new(), Vec::new()) };
                let z = c.zone;
                let v = vec![
                    MenuItem::new(Icon::Mute, tr("Mute"), Action::Layer(A::Mute { ch, chain })).on(z.mute),
                    MenuItem::new(Icon::Solo, tr("Solo"), Action::Layer(A::Solo { ch, chain })).on(z.solo),
                    MenuItem::new(Icon::Up, tr("Octave up"), Action::Layer(A::Transpose { ch, chain, by: 12 })).detail(format!("{:+}", z.transpose)).sep(),
                    MenuItem::new(Icon::ChevronDown, tr("Octave down"), Action::Layer(A::Transpose { ch, chain, by: -12 })),
                    MenuItem::new(Icon::Plus, tr("Semitone up"), Action::Layer(A::Transpose { ch, chain, by: 1 })),
                    MenuItem::new(Icon::Minus, tr("Semitone down"), Action::Layer(A::Transpose { ch, chain, by: -1 })),
                    MenuItem::new(Icon::Reset, tr("No transpose"), Action::Layer(A::Transpose { ch, chain, by: 0 })).dim(z.transpose == 0),
                    MenuItem::new(Icon::Reset, tr("Reset the zones"), Action::Layer(A::ResetZones { ch, chain })).tip(tr("Every key, velocity and selector value")).sep(),
                    MenuItem::new(Icon::Trash, tr("Remove the chain"), Action::Layer(A::RemoveChain { ch, chain })).sep(),
                ];
                (tr_fmt!("Chain {}", chain + 1), v)
            }
            LayerMenu::Slot { ch, chain, slot } => {
                let s = Slot::from_index(slot);
                let set = move |id: &'static str| Action::Layer(A::SetDevice { ch, chain, slot, kind: Some(id.to_string()) });
                let mut v = match s {
                    Slot::Instrument => {
                        let rows = registry::instruments()
                            .iter()
                            .filter(|k| k.id != INST_RACK)
                            .map(|k| (k.category, MenuItem::new(Icon::Instrument, tr(k.name), set(k.id)).tip(tr(k.description))))
                            .collect();
                        super::menu::by_category(rows, super::menu::category_icon)
                    }
                    Slot::NoteFx(_) => registry::note_fx()
                        .iter()
                        .map(|k| MenuItem::new(super::notefx::icon(k.id), tr(k.name), set(k.id)).tip(tr(k.description)))
                        .collect(),
                    Slot::Effect(_) => super::menu::effect_items(|k| k != ponzik_project::rack::RACK, set),
                };
                let kind = self.layer_rack(ch).and_then(|r| r.device_kind(unit_of(chain, s)));
                if kind.is_some() {
                    let bypassed = self.layer_rack(ch).and_then(|r| r.chain(chain)).is_some_and(|c| c.zone.bypassed(s));
                    let mut extra = vec![MenuItem::new(Icon::Power, if bypassed { tr("Enable") } else { tr("Bypass") }, Action::Layer(A::Bypass { ch, chain, slot }))];
                    if let Slot::Effect(k) = s {
                        if k > 0 {
                            extra.push(MenuItem::new(Icon::ChevronLeft, tr("Move up"), Action::Layer(A::SwapFx { ch, chain, a: k, b: k - 1 })));
                        }
                        if k + 1 < CHAIN_FX {
                            extra.push(MenuItem::new(Icon::ChevronRight, tr("Move down"), Action::Layer(A::SwapFx { ch, chain, a: k, b: k + 1 })));
                        }
                    }
                    extra.push(MenuItem::new(Icon::Trash, tr("Remove"), Action::Layer(A::SetDevice { ch, chain, slot, kind: None })));
                    if let Some(first) = v.first_mut() {
                        first.sep = true;
                    }
                    extra.push(MenuItem::header(tr("Replace with")));
                    extra.extend(v);
                    v = extra;
                }
                let title = kind.map_or_else(|| tr("Add a device").to_string(), |k| kind_name(k, s));
                (title, v)
            }
            LayerMenu::Variations(ch) => {
                let Some(rack) = self.layer_rack(ch) else { return (String::new(), Vec::new()) };
                let mut v = vec![MenuItem::new(Icon::Save, tr("Store the macros"), Action::Layer(A::StoreVariation(ch))).tip(tr("Keeps the values of every macro as a variation"))];
                for (i, var) in rack.variations.iter().enumerate() {
                    let it = MenuItem::new(Icon::History, &var.name, Action::Layer(A::RecallVariation { ch, i })).tip(tr("Recall: the macros take these values"));
                    v.push(if i == 0 { it.sep() } else { it });
                }
                if !rack.variations.is_empty() {
                    let del = rack.variations.iter().enumerate().map(|(i, var)| MenuItem::new(Icon::Trash, &var.name, Action::Layer(A::DeleteVariation { ch, i }))).collect();
                    v.push(MenuItem::sub(Icon::Trash, tr("Delete a variation"), del));
                }
                v.push(MenuItem::new(Icon::Dice, tr("Randomize the macros"), Action::Layer(A::Randomize(ch))).sep());
                let keep = (0..self.macro_count(ch))
                    .map(|k| MenuItem::new(Icon::Check, &self.macro_label(ch, k), Action::Layer(A::Exclude { ch, k })).on(rack.excluded(k)))
                    .collect();
                v.push(MenuItem::sub(Icon::Dice, tr("Randomize leaves alone"), keep));
                (tr("Variations").into(), v)
            }
        }
    }

    /// Name of macro `k`: its own, or what it drives first.
    fn macro_label(&self, ch: usize, k: usize) -> String {
        let fallback = tr_fmt!("Macro {}", k + 1);
        let (Some(def), Some(rack)) = (self.layer_def(ch), self.layer_rack(ch)) else { return fallback };
        if let Some(n) = rack.macro_name(k) {
            return n.to_string();
        }
        let Some(m) = rack.macro_maps(k).next() else { return fallback };
        let id = if m.unit == SELF_UNIT { m.param } else { unit_param(m.unit, m.param) };
        info_of(def, id).map_or(fallback, |p| tr(p.name).to_string())
    }

    /// A mapped knob of a rack: dragging moves the end of its range (Alt:
    /// the start) instead of the value; its range in the macro's color.
    pub(super) fn layer_range_knob(&mut self, c: &mut Canvas, cell: Rect, knob: u64, ch: usize, p: &ParamInfo, changed: &mut Option<f32>, alt: bool) {
        let (unit, param) = split_param(p.id).unwrap_or((SELF_UNIT, p.id));
        let Some(map) = self.layer_rack(ch).and_then(|r| r.mapping(unit, param)).copied() else { return };
        let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
        let ang = |n: f32| 0.75 * std::f32::consts::PI + 1.5 * std::f32::consts::PI * n;
        let (a, b) = (ang(map.min), ang(map.max));
        c.stroke_arc(cx, cy, 21.0, a.min(b), a.max(b), 2.5, self.theme.select);
        let dir = if map.min > map.max { "↓" } else { "↑" };
        self.text_center(c, self.f.mono_med, 9.0, cell.x + 10.0, cell.y + 6.0, &format!("M{}{}", map.macro_index + 1, dir), self.theme.select);
        if self.is_dragging(knob)
            && let Some(v) = changed.take()
        {
            let n = p.to_norm(v);
            if let Some(m) = self.layer_def_mut(ch).and_then(|d| d.layers_mut()).and_then(|r| r.maps.iter_mut().find(|m| m.unit == unit && m.param == param)) {
                if alt { m.min = n } else { m.max = n }
            }
            self.commit(tr("Macro range"));
        }
    }

    /// Body of a rack card.
    pub(super) fn layer_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, ch: usize) {
        let Some(def) = self.layer_def(ch).cloned() else { return };
        let rack = def.layers().cloned().unwrap_or_default();
        let x = self.layer_macros(c, input, body, ch, &def, &rack);
        let x = self.layer_chains(c, input, Rect::new(x, body.y, CHAINS_W, body.h), ch, &def, &rack);
        let x = self.layer_slots(c, input, Rect::new(x, body.y, SLOTS_W, body.h), ch, &rack);
        // The open device.
        let Some(unit) = self.layer_open(ch) else { return };
        let Some(kind) = rack.device_kind(unit).map(str::to_string) else { return };
        let params = unit_sheets(&kind, unit);
        let t = self.theme;
        c.fill_rect(Rect::new(x - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        let w = card_width("", params) - 28.0;
        self.device_body(c, input, Rect::new(x, body.y, w, body.h), Device::Instrument(ch), "", params);
    }

    /// Macro knobs, their map buttons, variations and Randomize; the right edge.
    fn layer_macros(&mut self, c: &mut Canvas, input: &Input, body: Rect, ch: usize, def: &InstrumentDef, rack: &InstRackDef) -> f32 {
        let t = self.theme;
        let device = Device::Instrument(ch);
        let n = self.macro_count(ch);
        let cols = n / 2;
        let w = cols as f32 * CELL_W;
        self.text(c, self.f.sans_med, 11.0, body.x + 4.0, body.y + 6.0, tr("Macros"), t.muted);
        // Variations, Randomize, eight or sixteen.
        let mut bx = body.x + w - 22.0;
        let b = Rect::new(bx, body.y - 4.0, 22.0, 20.0);
        if self.icon_button(c, input, wid("lyr-wide", ch, 0), b, Icon::Columns, rack.wide, (tr("Sixteen macros"), "")) {
            self.run_layer_action(LayerAction::Wide(ch));
        }
        bx -= 24.0;
        let b = Rect::new(bx, body.y - 4.0, 22.0, 20.0);
        if self.icon_button(c, input, wid("lyr-rand", ch, 0), b, Icon::Dice, false, (tr("Randomize the macros\nExcluded macros keep their value"), "")) {
            self.run_layer_action(LayerAction::Randomize(ch));
        }
        bx -= 24.0;
        let b = Rect::new(bx, body.y - 4.0, 22.0, 20.0);
        let has = !rack.variations.is_empty();
        if self.icon_button(c, input, wid("lyr-var", ch, 0), b, Icon::History, has, (tr("Variations: store and recall the macro values"), "")) {
            self.open_popup(Popup::Layer(LayerMenu::Variations(ch)), b);
        }
        if has {
            self.text_right(c, self.f.mono, 9.0, bx - 3.0, body.y + 6.0, &rack.variations.len().to_string(), t.muted);
        }
        for k in 0..n {
            let Some(p) = own_info(P_MACRO + k as u32) else { continue };
            let cell = Rect::new(body.x + (k % cols) as f32 * CELL_W, body.y + 16.0 + (k / cols) as f32 * CELL_H, CELL_W, CELL_H);
            let maps = rack.macro_maps(k).count();
            let label: &'static str = match rack.macro_maps(k).next() {
                Some(m) => {
                    let id = if m.unit == SELF_UNIT { m.param } else { unit_param(m.unit, m.param) };
                    info_of(def, id).map_or(p.name, |i| i.name)
                }
                None => p.name,
            };
            let shown = ParamInfo { name: label, ..*p };
            let value = def.get(p.id).unwrap_or(p.default);
            let id = wid("lyr-macro", ch, k);
            let learning = self.layers.learn == Some((ch, k));
            if learning {
                let pulse = 0.5 + 0.3 * (self.time * 5.0).sin();
                c.fill_rrect(Rect::new(cell.x + 2.0, cell.y + 8.0, cell.w - 4.0, cell.h - 10.0), 10.0, t.select.alpha(0.18 * pulse + 0.06));
            }
            let mut changed = self.knob(c, input, id, cell, &shown, value);
            if maps > 0 {
                let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
                c.fill_circle(cx + 15.0, cy - 14.0, 7.0, t.select);
                self.text_center(c, self.f.mono, 9.0, cx + 15.0, cy - 14.0, &maps.to_string(), t.bg_deep);
            }
            if rack.excluded(k) {
                draw_icon(c, Icon::Dice, cell.right() - 10.0, cell.bottom() - 12.0, 9.0, t.muted.alpha(0.6));
            }
            // Map button: then move any knob of the rack.
            let map = Rect::new(cell.x + 2.0, cell.y + 10.0, 18.0, 18.0);
            if self.icon_button(c, input, wid("lyr-map", ch, k), map, Icon::Route, learning, (tr("Map: click, then move a knob of the rack"), "")) {
                self.run_layer_action(LayerAction::Learn { ch, k });
            }
            self.mod_knob(c, input, id, cell, device, p, value, &mut changed);
            if let Some(v) = changed {
                self.set_param_value(device, p, v);
            }
            let target = AutoTarget::InstrumentParam { channel: ch, id: p.id };
            if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                self.open_popup(Popup::Param { target, value, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
            }
            self.param_mark(c, cell, target);
            self.auto_mark(c, input, (cell.right() - 10.0, cell.y + 6.0), Rect::new(cell.x, cell.y + 8.0, cell.w, 54.0), target);
        }
        body.x + w + GAP
    }

    /// Chain rows with their zone bars; the right edge.
    fn layer_chains(&mut self, c: &mut Canvas, input: &Input, area: Rect, ch: usize, def: &InstrumentDef, rack: &InstRackDef) -> f32 {
        let t = self.theme;
        let device = Device::Instrument(ch);
        c.fill_rect(Rect::new(area.x - GAP * 0.5, area.y + 20.0, 1.0, area.h - 28.0), t.hairline);
        self.text(c, self.f.sans_med, 11.0, area.x + 4.0, area.y + 6.0, tr("Chains"), t.muted);
        // Zone view: keys, velocities, chain selector.
        let views = [(ZoneView::Key, tr("Notes"), tr("Key zones: which notes each chain plays")), (ZoneView::Vel, tr("Vel"), tr("Velocity zones: soft and hard layers")), (ZoneView::Sel, tr("Chain"), tr("Chain selector zones: one value picks the chains, a macro morphs"))];
        let mut tx = area.x + 62.0;
        for (i, (v, name, tip)) in views.into_iter().enumerate() {
            let tw = self.fonts.measure(self.f.sans_med, 11.0, name) + 14.0;
            let r = Rect::new(tx, area.y - 4.0, tw, 20.0);
            let id = wid("lyr-view", ch, i);
            let on = self.layers.view == v;
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(r, 6.0, if on { t.accent.alpha(0.22) } else { t.raised.alpha(0.3 + hv * 0.5) });
            self.text_center(c, self.f.sans_med, 11.0, r.center().0, r.center().1, name, if on { t.accent } else { t.text2.mix(t.text, hv) });
            if hov {
                self.set_tip(tip, "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.layers.view = v;
                }
            }
            tx += tw + 4.0;
        }
        // Add a chain.
        let add = Rect::new(area.right() - 24.0, area.y - 4.0, 22.0, 20.0);
        if rack.chains.len() < CHAINS && self.icon_button(c, input, wid("lyr-add", ch, 0), add, Icon::Plus, false, (tr("Add a chain\nor drop an instrument from the browser"), "")) {
            self.open_popup(Popup::Layer(LayerMenu::AddChain(ch)), add);
        }

        let n = rack.chains.len();
        let sel = self.layer_sel(ch);
        let top = area.y + 18.0;
        let room = area.h - 22.0 - RULER_H - 26.0;
        let row_h = ROW_MAX.min(room / n.max(1) as f32).max(9.0);
        let name_w = 96.0;
        let levels_w = 100.0;
        let zx = area.x + 20.0 + name_w + 40.0;
        let za_w = area.right() - levels_w - 4.0 - zx;
        let ruler = Rect::new(zx, top, za_w, RULER_H);
        let rows_top = ruler.bottom() + 3.0;
        let rows_h = n as f32 * row_h;
        let xv = |v: f32| zx + v / 128.0 * za_w;
        let selector = def.effective(P_SELECTOR, own_info(P_SELECTOR).expect("selector"));
        self.layer_ruler(c, input, ruler, ch, selector);
        for (i, chain) in rack.chains.iter().enumerate() {
            let row = Rect::new(area.x, rows_top + i as f32 * row_h, area.w, row_h - 2.0);
            let ci = chain.index;
            let selected = sel == Some(ci);
            let color = t.patterns[ci % t.patterns.len()];
            c.fill_rrect(row, 6.0, if selected { t.raised_hi.alpha(0.7) } else { t.well });
            self.drop_zone(input, row, DropTarget::LayerChain { channel: ch, chain: Some(ci) });
            let cy = row.center().1;
            let small = row.h < 16.0;
            let fs = if small { 9.0 } else { 11.0 };
            c.fill_circle(row.x + 9.0, cy, 3.0, if chain.zone.mute { t.muted } else { color });
            let name = if !chain.name.is_empty() {
                chain.name.clone()
            } else {
                chain.instrument.as_ref().map_or_else(|| tr("Empty chain").to_string(), |i| kind_name(&i.kind, Slot::Instrument))
            };
            let name = if chain.zone.transpose != 0 { format!("{name} {:+}", chain.zone.transpose) } else { name };
            let name_r = Rect::new(row.x + 16.0, row.y, name_w, row.h);
            self.text_fit(c, self.f.sans_med, fs, name_r.x, cy, &name, name_w - 4.0, if chain.zone.mute { t.muted } else if selected { t.text } else { t.text2 });
            let nid = wid("lyr-name", ch, ci);
            if self.hovered(input, nid, name_r) {
                self.set_tip(&name, "");
                self.set_tip(tr("Click: show its devices · right-click: menu"), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.layers.sel.insert(ch, ci);
                    self.layers.open.remove(&ch);
                }
                if input.right_pressed {
                    self.open_popup(Popup::Layer(LayerMenu::Chain { ch, chain: ci }), name_r);
                }
            }
            // Mute and solo.
            let bs = (row.h - 2.0).min(18.0);
            let m = Rect::new(name_r.right(), cy - bs * 0.5, bs, bs);
            if self.icon_button(c, input, wid("lyr-mute", ch, ci), m, Icon::Mute, chain.zone.mute, (tr("Mute the chain"), "")) {
                self.run_layer_action(LayerAction::Mute { ch, chain: ci });
            }
            let s = Rect::new(m.right() + 2.0, m.y, bs, bs);
            if self.icon_button(c, input, wid("lyr-solo", ch, ci), s, Icon::Solo, chain.zone.solo, (tr("Solo the chain"), "")) {
                self.run_layer_action(LayerAction::Solo { ch, chain: ci });
            }
            // Zone bar.
            let za = Rect::new(zx, row.y + 1.0, za_w, row.h - 2.0);
            c.fill_rrect(za, 3.0, t.bg_deep.alpha(0.5));
            let zone = match self.layers.view {
                ZoneView::Key => chain.zone.key,
                ZoneView::Vel => chain.zone.vel,
                ZoneView::Sel => chain.zone.sel,
            };
            let (lo, hi) = (zone.lo as f32, zone.hi as f32 + 1.0);
            let alpha = if chain.zone.mute { 0.3 } else { 0.85 };
            let core = Rect::new(xv(lo), za.y + 1.0, (xv(hi) - xv(lo)).max(2.0), za.h - 2.0);
            if zone.fade_lo > 0 {
                let mut p = Path::new();
                p.polygon(&[(xv(lo - zone.fade_lo as f32), core.bottom()), (core.x, core.y), (core.x, core.bottom())]);
                c.fill_path(&p, color.alpha(alpha * 0.45));
            }
            if zone.fade_hi > 0 {
                let mut p = Path::new();
                p.polygon(&[(core.right(), core.y), (xv(hi + zone.fade_hi as f32), core.bottom()), (core.right(), core.bottom())]);
                c.fill_path(&p, color.alpha(alpha * 0.45));
            }
            c.fill_rrect(core, 2.0, color.alpha(alpha));
            let zid = wid("lyr-zone", ch, ci);
            let hov = self.hovered(input, zid, za);
            if hov && self.layers.drag.is_none() {
                let near_lo = (input.x - core.x).abs() < 5.0;
                let near_hi = (input.x - core.right()).abs() < 5.0;
                self.set_cursor(if near_lo || near_hi { Cursor::ResizeH } else { Cursor::Hand });
                let what = match self.layers.view {
                    ZoneView::Key => tr_fmt!("Keys {} to {}", super::rack::note_name(zone.lo), super::rack::note_name(zone.hi)),
                    ZoneView::Vel => tr_fmt!("Velocities {} to {}", zone.lo, zone.hi),
                    ZoneView::Sel => tr_fmt!("Selector {} to {}", zone.lo, zone.hi),
                };
                self.set_tip(&what, "");
                self.set_tip(tr("Drag the edges or the zone · Alt: fades · double-click: whole range"), "");
                if input.pressed {
                    if input.double {
                        self.set_layer_zone(ch, ci, Zone::FULL);
                    } else {
                        let part = match (near_lo, near_hi, input.mods.alt) {
                            (true, _, true) => Part::FadeLo,
                            (_, true, true) => Part::FadeHi,
                            (true, _, _) => Part::Lo,
                            (_, true, _) => Part::Hi,
                            _ if input.x < core.x => Part::Lo,
                            _ if input.x > core.right() => Part::Hi,
                            _ => Part::Move,
                        };
                        self.begin_drag(zid, input, 0.0);
                        self.layers.drag = Some(ZoneDrag { channel: ch, chain: ci, part, start: zone });
                        self.layers.sel.insert(ch, ci);
                    }
                }
            }
            if let Some(d) = self.layers.drag.filter(|d| d.channel == ch && d.chain == ci)
                && let Some(drag) = self.drag.as_ref().filter(|g| g.id == zid)
            {
                let dv = ((input.x - drag.x0) / za_w * 128.0).round() as i32;
                let s = d.start;
                let (slo, shi) = (s.lo as i32, s.hi as i32);
                let mut z = s;
                match d.part {
                    Part::Lo => z.lo = (slo + dv).clamp(0, shi) as u8,
                    Part::Hi => z.hi = (shi + dv).clamp(slo, 127) as u8,
                    Part::Move => {
                        let dv = dv.clamp(-slo, 127 - shi);
                        z.lo = (slo + dv) as u8;
                        z.hi = (shi + dv) as u8;
                    }
                    Part::FadeLo => z.fade_lo = (s.fade_lo as i32 - dv).clamp(0, slo) as u8,
                    Part::FadeHi => z.fade_hi = (s.fade_hi as i32 + dv).clamp(0, 127 - shi) as u8,
                }
                let z = z.sanitized();
                if z != zone {
                    self.set_layer_zone(ch, ci, z);
                }
            }
            // Level and pan, dragged up and down.
            for (k, base) in [P_VOLUME, P_PAN].into_iter().enumerate() {
                let Some(p) = own_info(base + ci as u32) else { continue };
                let field = Rect::new(area.right() - levels_w + k as f32 * 58.0, row.y + 1.0, if k == 0 { 56.0 } else { 40.0 }, row.h - 2.0);
                let value = def.effective(p.id, p);
                self.mini_value(c, input, wid("lyr-lvl", ch, p.id as usize), field, device, p, value);
                if self.layer_rack(ch).and_then(|r| r.mapping(SELF_UNIT, p.id)).is_some() {
                    c.fill_circle(field.x + 4.0, field.y + 4.0, 2.0, t.select);
                }
            }
        }
        if self.layers.drag.is_some() && input.released {
            self.layers.drag = None;
        }
        // Selector line across the rows.
        if self.layers.view == ZoneView::Sel && n > 0 {
            let sx = xv(selector + 0.5);
            c.fill_rect(Rect::new(sx - 0.75, rows_top, 1.5, rows_h), t.text.alpha(0.8));
        }
        // Drop here: a new chain.
        let below = rows_top + rows_h + 4.0;
        let drop = Rect::new(area.x, below, area.w, (area.bottom() - below).clamp(16.0, 24.0));
        if rack.chains.len() < CHAINS {
            self.drop_zone(input, drop, DropTarget::LayerChain { channel: ch, chain: None });
            if self.dnd_active() {
                c.stroke_rrect(drop, 6.0, 1.0, t.accent.alpha(0.6));
                self.text_center(c, self.f.sans, 11.0, drop.center().0, drop.center().1, tr("Drop here: a new chain"), t.accent);
            } else if n == 0 {
                self.text(c, self.f.sans, 12.0, area.x + 4.0, below + 10.0, tr("Drop instruments here, or click +"), t.muted);
            }
        }
        area.right() + GAP
    }

    /// Ruler over the zone bars: a keyboard, velocities, or the selector handle.
    fn layer_ruler(&mut self, c: &mut Canvas, input: &Input, r: Rect, ch: usize, selector: f32) {
        let t = self.theme;
        let key_w = r.w / 128.0;
        match self.layers.view {
            ZoneView::Key => {
                c.fill_rrect(r, 2.0, t.text2.alpha(0.85));
                for k in 0..128u32 {
                    let x = r.x + k as f32 * key_w;
                    if matches!(k % 12, 1 | 3 | 6 | 8 | 10) {
                        c.fill_rect(Rect::new(x, r.y, key_w, r.h * 0.62), t.bg_deep);
                    } else if k % 12 == 0 {
                        c.fill_rect(Rect::new(x, r.y + r.h * 0.55, 1.0, r.h * 0.45), t.muted);
                        if k % 24 == 0 && k > 0 {
                            self.text_center(c, self.f.mono, 7.0, x + 1.0, r.y - 5.0, &format!("C{}", k / 12 - 1), t.muted);
                        }
                    }
                }
            }
            ZoneView::Vel => {
                for i in 0..32 {
                    let x = r.x + i as f32 * r.w / 32.0;
                    let h = r.h * (0.25 + 0.75 * i as f32 / 31.0);
                    c.fill_rect(Rect::new(x + 0.5, r.bottom() - h, r.w / 32.0 - 1.0, h), t.accent.alpha(0.15 + 0.5 * i as f32 / 31.0));
                }
            }
            ZoneView::Sel => {
                c.fill_rrect(r, 2.0, t.raised.alpha(0.6));
                for v in (0..=127).step_by(16) {
                    c.fill_rect(Rect::new(r.x + v as f32 / 128.0 * r.w, r.bottom() - 4.0, 1.0, 4.0), t.muted);
                }
                let x = r.x + (selector + 0.5) / 128.0 * r.w;
                let mut p = Path::new();
                p.polygon(&[(x - 5.0, r.y), (x + 5.0, r.y), (x, r.bottom())]);
                c.fill_path(&p, t.text);
                let id = wid("lyr-selector", ch, 0);
                let hit = Rect::new(r.x - 4.0, r.y - 2.0, r.w + 8.0, r.h + 4.0);
                let hov = self.hovered(input, id, hit);
                if hov {
                    self.set_tip(&tr_fmt!("Chain selector: {}", selector.round() as i32), "");
                    self.set_tip(tr("Drag: pick the chains that play · map it to a macro to morph"), "");
                    self.set_cursor(Cursor::ResizeH);
                    if input.pressed {
                        self.begin_drag(id, input, selector);
                    }
                    if input.right_pressed && let Some(p) = own_info(P_SELECTOR) {
                        let target = AutoTarget::InstrumentParam { channel: ch, id: P_SELECTOR };
                        self.open_popup(Popup::Param { target, value: selector, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
                    }
                }
                if self.is_dragging(id)
                    && let Some(p) = own_info(P_SELECTOR)
                {
                    let v = ((input.x - r.x) / r.w * 128.0 - 0.5).clamp(0.0, 127.0);
                    if (v - selector).abs() > 0.01 {
                        self.set_param_value(Device::Instrument(ch), p, v);
                    }
                }
            }
        }
    }

    fn set_layer_zone(&mut self, ch: usize, chain: usize, z: Zone) {
        let view = self.layers.view;
        if let Some(c) = self.layer_def_mut(ch).and_then(|d| d.layers_mut()).and_then(|r| r.chain_mut(chain)) {
            match view {
                ZoneView::Key => c.zone.key = z,
                ZoneView::Vel => c.zone.vel = z,
                ZoneView::Sel => c.zone.sel = z,
            }
        }
        self.commit(tr("Chain zone"));
    }

    /// Devices of the selected chain, one line per place; the right edge.
    fn layer_slots(&mut self, c: &mut Canvas, input: &Input, area: Rect, ch: usize, rack: &InstRackDef) -> f32 {
        let t = self.theme;
        c.fill_rect(Rect::new(area.x - GAP * 0.5, area.y + 20.0, 1.0, area.h - 28.0), t.hairline);
        let Some(chain) = self.layer_sel(ch).and_then(|i| rack.chain(i)) else { return area.right() + GAP };
        let ci = chain.index;
        self.text(c, self.f.sans_med, 11.0, area.x + 4.0, area.y + 6.0, &tr_fmt!("Chain {}", ci + 1), t.muted);
        let open = self.layer_open(ch);
        let line_h = ((area.h - 22.0) / SLOTS as f32).min(22.0);
        for s in 0..SLOTS {
            let slot = Slot::from_index(s);
            let unit = unit_of(ci, slot);
            let r = Rect::new(area.x, area.y + 18.0 + s as f32 * line_h, area.w, line_h - 3.0);
            let kind = chain.kind(slot);
            let id = wid("lyr-slot", ch * CHAINS + ci, s);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let selected = open == Some(unit) && kind.is_some();
            let bypassed = chain.zone.bypassed(slot);
            match kind {
                Some(k) => {
                    c.fill_rrect(r, 5.0, if selected { t.raised_hi } else { t.raised.mix(t.raised_hi, hv) });
                    if selected {
                        c.stroke_rrect(r, 5.0, 1.2, t.accent);
                    }
                    draw_icon(c, slot_icon(slot), r.x + 9.0, r.center().1, 10.0, if bypassed { t.muted } else { t.accent });
                    let name = kind_name(k, slot);
                    self.text_fit(c, self.f.sans_med, 10.0, r.x + 18.0, r.center().1, &name, r.w - 22.0, if bypassed { t.muted } else { t.text2.mix(t.text, hv) });
                    if hov {
                        self.set_tip(&name, "");
                        self.set_tip(tr("Click: open · right-click: menu"), "");
                    }
                }
                None => {
                    c.stroke_rrect(r, 5.0, 1.0, t.hairline.mix(t.accent, hv * 0.6));
                    draw_icon(c, Icon::Plus, r.x + 9.0, r.center().1, 9.0, t.muted.mix(t.accent, hv));
                    let what = match slot {
                        Slot::Instrument => tr("Instrument"),
                        Slot::NoteFx(_) => tr("Note effect"),
                        Slot::Effect(_) => tr("Effect"),
                    };
                    self.text_fit(c, self.f.sans, 10.0, r.x + 18.0, r.center().1, what, r.w - 22.0, t.muted);
                    if hov {
                        self.set_tip(&tr_fmt!("Add: {}", what), "");
                    }
                }
            }
            if !matches!(slot, Slot::NoteFx(_)) {
                self.drop_zone(input, r, DropTarget::LayerChain { channel: ch, chain: Some(ci) });
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    if kind.is_some() {
                        self.layers.open.insert(ch, unit);
                    } else {
                        self.open_popup(Popup::Layer(LayerMenu::Slot { ch, chain: ci, slot: s }), r);
                    }
                }
                if input.right_pressed {
                    self.open_popup(Popup::Layer(LayerMenu::Slot { ch, chain: ci, slot: s }), r);
                }
            }
        }
        area.right() + GAP
    }

    /// Drop of a browser item on a chain (`None`: a new chain).
    pub(super) fn drop_on_layer(&mut self, ch: usize, chain: Option<usize>, kind: &str, instrument: bool) {
        if instrument {
            if !ponzik_project::inst_rack::layer_accepts(kind, Slot::Instrument) {
                self.toast(tr("This device cannot go there"));
                return;
            }
            match chain {
                Some(c) => self.run_layer_action(LayerAction::SetDevice { ch, chain: c, slot: 0, kind: Some(kind.into()) }),
                None => self.run_layer_action(LayerAction::AddChain { ch, kind: Some(kind.into()) }),
            }
            return;
        }
        let Some(c) = chain.or_else(|| self.layer_sel(ch)) else {
            self.toast(tr("Add a chain first"));
            return;
        };
        let free = self.layer_rack(ch).and_then(|r| r.chain(c)).and_then(|chain| (0..CHAIN_FX).find(|&k| chain.kind(Slot::Effect(k)).is_none()));
        match free {
            Some(k) => self.run_layer_action(LayerAction::SetDevice { ch, chain: c, slot: Slot::Effect(k).index(), kind: Some(kind.into()) }),
            None => self.toast(&tr_fmt!("A chain holds {} effects at most", CHAIN_FX)),
        }
    }

    /// Saves the whole rack as a preset.
    pub(super) fn save_layer_preset(&mut self, ch: usize, name: &str) {
        let Some(def) = self.layer_def(ch) else { return };
        let text = ponzik_project::inst_rack::layer_preset_json(name, def);
        let dir = super::compat::presets_dir(INST_RACK);
        let path = dir.join(format!("{}.json", super::compat::file_name(name)));
        let done = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text));
        match done {
            Ok(()) => self.toast(&tr_fmt!("Preset saved: {}", name)),
            Err(e) => self.toast(&format!("{}: {e}", path.display())),
        }
    }

    /// Loads a rack preset: its chains, mappings and values.
    pub(super) fn apply_layer_preset(&mut self, device: Device, params: &[(u32, f32)], rack: InstRackDef) {
        let Device::Instrument(ch) = device else { return };
        let Some(def) = self.layer_def_mut(ch) else {
            self.toast(tr("This preset is for an Instrument Rack"));
            return;
        };
        def.params = params.to_vec();
        def.rack = Some(Box::new(rack));
        self.layers.open.remove(&ch);
        self.commit(tr("Load a preset"));
    }

    /// Captures: channel `spec` (default 0) becomes a layered piano and
    /// strings with a key split, a pad on the chain selector, macros mapped;
    /// focused. `PONZIK_LAYERS_VIEW=vel|sel` picks the zone view,
    /// `PONZIK_LAYERS_LEARN=1` shows map mode.
    pub fn screenshot_layers(&mut self, spec: &str) {
        let ch = spec.trim().parse::<usize>().ok().filter(|&c| c < self.project.channels.len()).unwrap_or(0);
        let mut def = InstrumentDef::new_layer_rack();
        def.layer_add_chain(Some("piano"));
        def.layer_add_chain(Some("strings"));
        def.layer_add_chain(Some("pulse"));
        def.layer_set_device(1, Slot::Effect(0), Some("reverb"));
        def.layer_set_device(1, Slot::Effect(1), Some("eq"));
        def.layer_set_device(2, Slot::NoteFx(0), Some("arpeggiator"));
        def.layer_set_device(2, Slot::Effect(0), Some("delay"));
        if let Some(r) = def.layers_mut() {
            r.chains[0].zone.key = Zone::new(0, 59).with_fades(0, 4);
            r.chains[1].zone.key = Zone::new(60, 127).with_fades(5, 0);
            r.chains[1].zone.vel = Zone::new(30, 127).with_fades(20, 0);
            r.chains[1].zone.transpose = 12;
            r.chains[2].zone.key = Zone::new(48, 96);
            r.chains[2].zone.vel = Zone::new(90, 127).with_fades(25, 0);
            r.chains[2].zone.sel = Zone::new(70, 127).with_fades(40, 0);
            r.chains[0].zone.sel = Zone::new(0, 60).with_fades(0, 30);
        }
        def.set(P_VOLUME + 1, -4.5);
        def.set(P_PAN + 1, 0.2);
        def.set(P_PAN + 2, -0.35);
        let rev = unit_of(1, Slot::Effect(0));
        def.set(unit_param(rev, 5), 0.4);
        def.layer_map(0, SELF_UNIT, P_SELECTOR, 0.0, 1.0);
        def.layer_map(1, SELF_UNIT, P_VOLUME + 1, 0.55, 0.95);
        def.layer_map(1, rev, 5, 0.1, 0.7);
        def.layer_map(2, unit_of(2, Slot::Instrument), 3, 0.1, 0.9);
        def.set(P_MACRO, 0.42);
        def.set(P_MACRO + 1, 0.6);
        def.set(P_MACRO + 2, 0.3);
        def.store_variation(&tr_fmt!("Variation {}", 1));
        let c = &mut self.project.channels[ch];
        c.instrument = def;
        c.name = tr("Piano and strings").into();
        self.layers.sel.insert(ch, 1);
        self.layers.open.insert(ch, unit_of(1, Slot::Effect(0)));
        self.layers.view = match std::env::var("PONZIK_LAYERS_VIEW").as_deref() {
            Ok("vel") => ZoneView::Vel,
            Ok("sel") => ZoneView::Sel,
            _ => ZoneView::Key,
        };
        if std::env::var("PONZIK_LAYERS_LEARN").is_ok() {
            self.layers.learn = Some((ch, 3));
        }
        self.focus = Focus::Channel(ch);
        self.panel = true;
        self.commit(tr("Capture layers"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_sheets_carry_rack_ids() {
        let unit = unit_of(3, Slot::Effect(1));
        let sheet = unit_sheets("eq", unit);
        assert!(!sheet.is_empty());
        assert!(sheet.iter().all(|p| split_param(p.id).is_some_and(|(u, _)| u == unit)));
        assert!(std::ptr::eq(sheet, unit_sheets("eq", unit)), "made once");
        assert!(unit < ponzik_project::inst_rack::UNITS);
    }

    #[test]
    fn group_map_and_zone_edits_go_through_undo() {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        let before = app.project.channels[0].instrument.clone();
        app.run_layer_action(LayerAction::Group(0));
        assert_eq!(app.project.channels[0].instrument.kind, INST_RACK);
        app.run_layer_action(LayerAction::AddChain { ch: 0, kind: Some("strings".into()) });
        assert_eq!(app.layer_rack(0).unwrap().chains.len(), 2);
        // Map mode: the next knob moved maps to the macro.
        app.run_layer_action(LayerAction::Learn { ch: 0, k: 2 });
        let p = own_info(P_VOLUME + 1).unwrap();
        assert!(app.layer_learn_hit(Device::Instrument(0), p));
        assert!(app.layer_rack(0).unwrap().mapping(SELF_UNIT, P_VOLUME + 1).is_some_and(|m| m.macro_index == 2));
        assert!(app.layers.learn.is_none());
        assert!(!app.layer_learn_hit(Device::Instrument(0), p), "once");
        app.run_layer_action(LayerAction::Transpose { ch: 0, chain: 1, by: 12 });
        assert_eq!(app.layer_rack(0).unwrap().chain(1).unwrap().zone.transpose, 12);
        app.run_layer_action(LayerAction::StoreVariation(0));
        assert_eq!(app.layer_rack(0).unwrap().variations.len(), 1);
        for _ in 0..6 {
            app.undo();
        }
        assert_eq!(app.project.channels[0].instrument, before);
    }
}
