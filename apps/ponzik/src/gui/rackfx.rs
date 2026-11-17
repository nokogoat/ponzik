//! Effect rack card: eight macro knobs, the chains as rows of mini cards
//! (click to open a device, drag to reorder, drop effects from the browser),
//! and the settings of the open device.

use ponzik_engine::effects::fx_rack::{CHAIN_LEN, CHAINS, MACROS, P_MACRO, P_MIX, P_VOLUME, split_param};
use ponzik_engine::{AutoTarget, ParamInfo, registry};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::rack::{RACK, rack_accepts};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, MenuItem, Popup};
use super::devices::{Device, card_width, params_of};
use super::dnd::DropTarget;
use super::widgets::{Icon, draw_icon, wid};

const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GAP: f32 = 18.0;
const CHAINS_W: f32 = 340.0;
const ROW_H: f32 = 30.0;

/// Edits of a rack, from its menus.
#[derive(Clone, Debug)]
pub enum RackAction {
    Add { track: usize, slot: usize, chain: usize, kind: String },
    Remove { track: usize, slot: usize, unit: usize },
    Bypass { track: usize, slot: usize, unit: usize },
    Move { track: usize, slot: usize, unit: usize, chain: usize, pos: usize },
    AddChain { track: usize, slot: usize },
    RemoveChain { track: usize, slot: usize, chain: usize },
    Map { track: usize, slot: usize, unit: usize, param: u32, k: usize },
    Unmap { track: usize, slot: usize, unit: usize, param: u32 },
    Invert { track: usize, slot: usize, unit: usize, param: u32 },
    FullRange { track: usize, slot: usize, unit: usize, param: u32 },
}

/// A chip being dragged in a rack: (track, slot, unit).
pub type ChipDrag = (usize, usize, usize);

impl App {
    fn rack_def(&self, track: usize, slot: usize) -> Option<&ponzik_project::EffectDef> {
        self.project.tracks.get(track)?.inserts.get(slot)?.as_ref().filter(|d| d.kind == RACK)
    }

    fn rack_def_mut(&mut self, track: usize, slot: usize) -> Option<&mut ponzik_project::EffectDef> {
        self.project.tracks.get_mut(track)?.inserts.get_mut(slot)?.as_mut().filter(|d| d.kind == RACK)
    }

    /// Device open in the rack: the chosen one, else the first.
    pub(super) fn rack_open(&self, track: usize, slot: usize) -> Option<usize> {
        let rack = self.rack_def(track, slot)?.rack.as_deref()?;
        let chosen = self.rack_sel.get(&(track, slot)).copied().filter(|&u| rack.device(u).is_some());
        chosen.or_else(|| rack.devices().next().map(|d| d.unit))
    }

    pub(super) fn rack_width(&self, track: usize, slot: usize) -> f32 {
        let open = self.rack_open(track, slot).and_then(|u| self.rack_def(track, slot)?.rack.as_deref()?.device(u));
        let inner = open.map_or(0.0, |d| GAP + card_width(&d.kind, params_of(&d.kind, false)) - 28.0);
        28.0 + 4.0 * CELL_W + GAP + CHAINS_W + inner
    }

    pub(super) fn run_rack_action(&mut self, action: RackAction) {
        let label = match action {
            RackAction::Add { track, slot, chain, kind } => {
                let unit = self.rack_def_mut(track, slot).and_then(|d| d.rack_insert(chain, None, &kind));
                match unit {
                    Some(u) => {
                        self.rack_sel.insert((track, slot), u);
                        tr("Add an effect to the rack")
                    }
                    None => {
                        self.toast(&tr_fmt!("A chain holds {} effects at most", CHAIN_LEN));
                        return;
                    }
                }
            }
            RackAction::Remove { track, slot, unit } => {
                self.project.remove_rack_device(track, slot, unit);
                tr("Remove an effect from the rack")
            }
            RackAction::Bypass { track, slot, unit } => {
                if let Some(d) = self.rack_def_mut(track, slot) {
                    let on = d.rack.as_deref().and_then(|r| r.device(unit)).is_some_and(|x| x.bypass);
                    d.rack_set_bypass(unit, !on);
                }
                tr("Bypass an effect")
            }
            RackAction::Move { track, slot, unit, chain, pos } => {
                if let Some(d) = self.rack_def_mut(track, slot) {
                    d.rack_move(unit, chain, pos);
                }
                tr("Move an effect in the rack")
            }
            RackAction::AddChain { track, slot } => {
                if let Some(d) = self.rack_def_mut(track, slot) {
                    d.rack_add_chain();
                }
                tr("Add a parallel chain")
            }
            RackAction::RemoveChain { track, slot, chain } => {
                let units = self.rack_def(track, slot).and_then(|d| d.rack.as_deref()).map_or(Vec::new(), |r| {
                    r.chains.get(chain).map_or(Vec::new(), |c| c.iter().map(|d| d.unit).collect())
                });
                for u in units {
                    self.project.remove_rack_device(track, slot, u);
                }
                if let Some(d) = self.rack_def_mut(track, slot) {
                    d.rack_remove_chain(chain);
                }
                tr("Remove a chain")
            }
            RackAction::Map { track, slot, unit, param, k } => {
                if let Some(d) = self.rack_def_mut(track, slot) {
                    d.map_macro(k, unit, param, 0.0, 1.0);
                }
                tr("Map to a macro")
            }
            RackAction::Unmap { track, slot, unit, param } => {
                if let Some(d) = self.rack_def_mut(track, slot) {
                    d.unmap(unit, param);
                }
                tr("Remove a macro mapping")
            }
            RackAction::Invert { track, slot, unit, param } | RackAction::FullRange { track, slot, unit, param } => {
                let invert = matches!(action, RackAction::Invert { .. });
                if let Some(m) = self
                    .rack_def_mut(track, slot)
                    .and_then(|d| d.rack.as_deref_mut())
                    .and_then(|r| r.maps.iter_mut().find(|m| m.unit == unit && m.param == param))
                {
                    if invert {
                        std::mem::swap(&mut m.min, &mut m.max);
                    } else {
                        (m.min, m.max) = if m.min > m.max { (1.0, 0.0) } else { (0.0, 1.0) };
                    }
                }
                tr("Macro range")
            }
        };
        self.commit(label);
    }

    /// Menu lines of a knob inside a rack: map it to a macro.
    pub(super) fn macro_menu_items(&self, target: AutoTarget) -> Vec<MenuItem> {
        let AutoTarget::EffectParam { track, slot, id } = target else { return Vec::new() };
        let Some((unit, param)) = split_param(id) else { return Vec::new() };
        let Some(rack) = self.rack_def(track, slot).and_then(|d| d.rack.as_deref()) else { return Vec::new() };
        let mut v = Vec::new();
        let current = rack.mapping(unit, param).map(|m| m.macro_index);
        for k in 0..MACROS {
            let it = MenuItem::new(Icon::Rack, &tr_fmt!("Map to Macro {}", k + 1), Action::Rack(RackAction::Map { track, slot, unit, param, k }))
                .on(current == Some(k));
            v.push(if k == 0 { it.sep() } else { it });
        }
        if current.is_some() {
            v.push(MenuItem::new(Icon::Swap, tr("Invert the range"), Action::Rack(RackAction::Invert { track, slot, unit, param })).tip(tr("The knob goes down when the macro goes up")));
            v.push(MenuItem::new(Icon::Reset, tr("Full range"), Action::Rack(RackAction::FullRange { track, slot, unit, param })));
            v.push(MenuItem::new(Icon::Close, tr("Remove the macro mapping"), Action::Rack(RackAction::Unmap { track, slot, unit, param })));
        }
        v
    }

    pub(super) fn rack_add_items(&self, track: usize, slot: usize, chain: usize) -> Vec<MenuItem> {
        registry::effects()
            .iter()
            .filter(|k| rack_accepts(k.id))
            .map(|k| MenuItem::new(Icon::Effect, tr(k.name), Action::Rack(RackAction::Add { track, slot, chain, kind: k.id.to_string() })).tip(tr(k.description)))
            .collect()
    }

    pub(super) fn rack_device_items(&self, track: usize, slot: usize, unit: usize) -> Vec<MenuItem> {
        let Some(rack) = self.rack_def(track, slot).and_then(|d| d.rack.as_deref()) else { return Vec::new() };
        let Some(d) = rack.device(unit) else { return Vec::new() };
        let Some((c, p)) = rack.position(unit) else { return Vec::new() };
        let bypass = if d.bypass { tr("Enable") } else { tr("Bypass") };
        let mut v = vec![MenuItem::new(Icon::Power, bypass, Action::Rack(RackAction::Bypass { track, slot, unit }))];
        if p > 0 {
            v.push(MenuItem::new(Icon::ChevronLeft, tr("Move left"), Action::Rack(RackAction::Move { track, slot, unit, chain: c, pos: p - 1 })));
        }
        if p + 1 < rack.chains[c].len() {
            v.push(MenuItem::new(Icon::ChevronRight, tr("Move right"), Action::Rack(RackAction::Move { track, slot, unit, chain: c, pos: p + 1 })));
        }
        for (k, chain) in rack.chains.iter().enumerate().filter(|&(k, _)| k != c) {
            if chain.len() < CHAIN_LEN {
                v.push(MenuItem::new(Icon::Route, &tr_fmt!("Move to chain {}", k + 1), Action::Rack(RackAction::Move { track, slot, unit, chain: k, pos: chain.len() })));
            }
        }
        v.push(MenuItem::new(Icon::Trash, tr("Remove"), Action::Rack(RackAction::Remove { track, slot, unit })).sep());
        v
    }

    /// Body of a rack card.
    pub(super) fn rack_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, track: usize, slot: usize) {
        let t = self.theme;
        let Some(def) = self.rack_def(track, slot).cloned() else { return };
        let rack = def.rack.as_deref().cloned().unwrap_or_default();
        let device = Device::Effect { track, slot };
        let rack_params = params_of(RACK, false);

        // Macros: the name of the first parameter each one drives.
        self.text(c, self.f.sans_med, 11.0, body.x + 4.0, body.y + 6.0, tr("Macros"), t.muted);
        for k in 0..MACROS {
            let Some(p) = rack_params.iter().find(|p| p.id == P_MACRO + k as u32) else { continue };
            let maps: Vec<_> = rack.macro_maps(k).copied().collect();
            let mut shown: ParamInfo = *p;
            if let Some(first) = maps.first()
                && let Some(d) = rack.device(first.unit)
                && let Some(info) = params_of(&d.kind, false).iter().find(|i| i.id == first.param)
            {
                shown.name = info.name;
            }
            let cell = Rect::new(body.x + (k % 4) as f32 * CELL_W, body.y + 16.0 + (k / 4) as f32 * CELL_H, CELL_W, CELL_H);
            let value = def.get(p.id).unwrap_or(p.default);
            let id = wid("rack-macro", track * 16 + slot, k);
            let mut changed = self.knob(c, input, id, cell, &shown, value);
            if !maps.is_empty() {
                let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
                c.fill_circle(cx + 15.0, cy - 14.0, 7.0, t.select);
                self.text_center(c, self.f.mono, 9.0, cx + 15.0, cy - 14.0, &maps.len().to_string(), t.bg_deep);
            }
            self.mod_knob(c, input, id, cell, device, p, value, &mut changed);
            if let Some(v) = changed {
                self.set_param_value(device, p, v);
            }
            let target = AutoTarget::EffectParam { track, slot, id: p.id };
            if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                self.open_popup(Popup::Param { target, value, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
            }
            self.param_mark(c, cell, target);
            if self.is_automated(target) {
                c.fill_circle(cell.right() - 10.0, cell.y + 6.0, 3.0, t.select);
            }
        }

        // Chains.
        let open = self.rack_open(track, slot);
        let x0 = body.x + 4.0 * CELL_W + GAP;
        c.fill_rect(Rect::new(x0 - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        self.text(c, self.f.sans_med, 11.0, x0 + 4.0, body.y + 6.0, tr("Chains"), t.muted);
        let n = rack.chains.len();
        let row_h = ROW_H.min((body.h - 54.0) / n as f32);
        let mut drop: Option<(usize, usize)> = None;
        let dragging = self.rack_chip.filter(|d| d.0 == track && d.1 == slot).map(|d| d.2);
        for (ci, chain) in rack.chains.iter().enumerate() {
            let row = Rect::new(x0, body.y + 18.0 + ci as f32 * (row_h + 6.0), CHAINS_W, row_h);
            c.fill_rrect(row, 8.0, t.well);
            self.drop_zone(input, row, DropTarget::RackChain { track, slot, chain: ci });
            let mut x = row.x + 4.0;
            if n > 1 {
                self.text_center(c, self.f.mono_med, 11.0, x + 9.0, row.y + row.h * 0.5, &(ci + 1).to_string(), t.muted);
                x += 20.0;
            }
            let levels_w = if n > 1 { 112.0 } else { 0.0 };
            let room = row.right() - levels_w - 30.0 - x;
            let chip_w = (room / chain.len().max(1) as f32).clamp(40.0, 96.0);
            for (pos, d) in chain.iter().enumerate() {
                let chip = Rect::new(x, row.y + 3.0, chip_w - 4.0, row.h - 6.0);
                // Dropping a dragged chip: before the one under the pointer.
                if dragging.is_some() && input.inside && row.contains(input.x, input.y) && input.x < chip.center().0 && drop.is_none() {
                    drop = Some((ci, pos));
                    c.fill_rect(Rect::new(chip.x - 3.0, chip.y, 2.0, chip.h), t.accent);
                }
                let id = wid("rack-chip", (track * 16 + slot) * 64 + d.unit, 0);
                let hov = self.hovered(input, id, chip);
                let hv = self.hover_anim(id, hov);
                let selected = open == Some(d.unit);
                c.fill_rrect(chip, 6.0, if selected { t.raised_hi } else { t.raised.mix(t.raised_hi, hv) });
                if selected {
                    c.stroke_rrect(chip, 6.0, 1.2, t.accent);
                }
                let name = registry::effect_kind(&d.kind).map_or_else(|| d.kind.clone(), |k| tr(k.name).to_string());
                let col = if d.bypass { t.muted } else if selected { t.text } else { t.text2 };
                c.fill_circle(chip.x + 8.0, chip.center().1, 2.5, if d.bypass { t.muted } else { t.accent });
                self.text_fit(c, self.f.sans_med, 11.0, chip.x + 15.0, chip.center().1, &name, chip.w - 19.0, col);
                if hov {
                    self.set_tip(&name, "");
                    self.set_tip(tr("Click: open · drag: move · right-click: menu"), "");
                    self.set_cursor(Cursor::Hand);
                    if input.pressed {
                        self.rack_sel.insert((track, slot), d.unit);
                        self.begin_drag(id, input, 0.0);
                        self.rack_chip = Some((track, slot, d.unit));
                    }
                    if input.right_pressed {
                        self.open_popup(Popup::RackDevice { track, slot, unit: d.unit }, chip);
                    }
                }
                x += chip_w;
            }
            if dragging.is_some() && input.inside && row.contains(input.x, input.y) && drop.is_none() {
                drop = Some((ci, chain.len()));
                c.fill_rect(Rect::new(x - 3.0, row.y + 3.0, 2.0, row.h - 6.0), t.accent);
            }
            // Add an effect to this chain.
            if chain.len() < CHAIN_LEN {
                let add = Rect::new(x, row.y + 3.0, 24.0, row.h - 6.0);
                if self.icon_button(c, input, wid("rack-add", track * 16 + slot, ci), add, Icon::Plus, false, (tr("Add an effect to this chain\nor drag one from the browser"), "")) {
                    self.open_popup(Popup::RackAdd { track, slot, chain: ci }, add);
                }
            }
            if n > 1 {
                // Volume and mix of the chain: drag up and down.
                for (k, base) in [P_VOLUME, P_MIX].into_iter().enumerate() {
                    let Some(p) = rack_params.iter().find(|p| p.id == base + ci as u32) else { continue };
                    let field = Rect::new(row.right() - levels_w + k as f32 * 55.0, row.y + 4.0, 52.0, row.h - 8.0);
                    let value = def.get(p.id).unwrap_or(p.default);
                    self.mini_value(c, input, wid("rack-lvl", track * 16 + slot, (p.id) as usize), field, device, p, value);
                }
            }
        }
        // Release of a dragged chip.
        if let Some(unit) = dragging
            && input.released
        {
            self.rack_chip = None;
            if let Some((chain, mut pos)) = drop
                && let Some((c0, p0)) = rack.position(unit)
            {
                if chain == c0 && pos > p0 {
                    pos -= 1;
                }
                if (chain, pos) != (c0, p0) {
                    self.run_rack_action(RackAction::Move { track, slot, unit, chain, pos });
                }
            }
        }
        let below = body.y + 18.0 + n as f32 * (row_h + 6.0);
        if n < CHAINS {
            let b = Rect::new(x0, below, 120.0, 24.0);
            let id = wid("rack-chain", track * 16 + slot, 0);
            let hov = self.hovered(input, id, b);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(b, 8.0, t.raised.alpha(0.4 + hv * 0.6));
            draw_icon(c, Icon::Plus, b.x + 12.0, b.center().1, 12.0, t.muted.mix(t.accent, hv));
            self.text(c, self.f.sans, 11.0, b.x + 22.0, b.center().1, tr("Parallel chain"), t.text2.mix(t.text, hv));
            if hov {
                self.set_tip(tr("Splits the sound into chains side by side, mixed back together"), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.run_rack_action(RackAction::AddChain { track, slot });
                    return;
                }
            }
        }
        if n > 1 {
            let b = Rect::new(x0 + 128.0, below, 120.0, 24.0);
            let id = wid("rack-unchain", track * 16 + slot, 0);
            let hov = self.hovered(input, id, b);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(b, 8.0, t.raised.alpha(0.4 + hv * 0.6));
            draw_icon(c, Icon::Close, b.x + 12.0, b.center().1, 11.0, t.muted.mix(t.text, hv));
            self.text(c, self.f.sans, 11.0, b.x + 22.0, b.center().1, &tr_fmt!("Remove chain {}", n), t.text2.mix(t.text, hv));
            if hov && input.pressed {
                self.run_rack_action(RackAction::RemoveChain { track, slot, chain: n - 1 });
                return;
            }
        }
        if rack.devices().next().is_none() {
            self.text(c, self.f.sans, 12.0, x0 + 4.0, below + 44.0, tr("Drop effects on a chain, or click +"), t.muted);
        }

        // The open device.
        let Some(unit) = open else { return };
        let Some(d) = rack.device(unit) else { return };
        let kind = d.kind.clone();
        let x = x0 + CHAINS_W + GAP;
        c.fill_rect(Rect::new(x - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        let w = card_width(&kind, params_of(&kind, false)) - 28.0;
        let inner = Rect::new(x, body.y, w, body.h);
        let unit_device = Device::RackUnit { track, slot, unit };
        self.device_body(c, input, inner, unit_device, &kind, params_of(&kind, false));
    }

    /// Small value field: drag up and down; double-click resets.
    fn mini_value(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, device: Device, p: &ParamInfo, value: f32) {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        c.fill_rrect(r, 6.0, t.raised.mix(t.raised_hi, hv));
        let n = p.to_norm(value);
        c.fill_rrect(Rect::new(r.x + 2.0, r.bottom() - 3.0, (r.w - 4.0) * n, 2.0), 1.0, t.accent.alpha(0.8));
        let label = self.fit(self.f.mono, 10.0, &p.format(value), r.w - 4.0);
        self.text_center(c, self.f.mono, 10.0, r.center().0, r.center().1 - 1.0, &label, t.text2.mix(t.text, hv));
        if hov {
            self.set_tip(tr(p.name), "");
            self.set_cursor(Cursor::ResizeV);
            if input.pressed {
                if input.double {
                    self.set_param_value(device, p, p.default);
                } else {
                    self.begin_drag(id, input, n);
                }
            }
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let per_px = if d.fine { 0.0015 } else { 0.007 };
            let v = p.from_norm(d.v0 + (d.y0 - input.y) * per_px);
            if v != value {
                self.set_param_value(device, p, v);
            }
        }
    }

    /// A mapped knob inside a rack: dragging moves the end of its range
    /// (Alt: the start) instead of the value; draws the range in the macro's color.
    pub(super) fn macro_range_knob(&mut self, c: &mut Canvas, cell: Rect, knob: u64, track: usize, slot: usize, unit: usize, p: &ParamInfo, changed: &mut Option<f32>, alt: bool) {
        let Some(map) = self.rack_def(track, slot).and_then(|d| d.rack.as_deref()).and_then(|r| r.mapping(unit, p.id)).copied() else { return };
        let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
        let ang = |n: f32| 0.75 * std::f32::consts::PI + 1.5 * std::f32::consts::PI * n;
        let (a, b) = (ang(map.min), ang(map.max));
        c.stroke_arc(cx, cy, 21.0, a.min(b), a.max(b), 2.5, self.theme.select);
        let tip = if map.min > map.max { "↓" } else { "↑" };
        self.text_center(c, self.f.mono_med, 9.0, cell.x + 10.0, cell.y + 6.0, &format!("M{}{}", map.macro_index + 1, tip), self.theme.select);
        if self.is_dragging(knob)
            && let Some(v) = changed.take()
        {
            let n = p.to_norm(v);
            if let Some(m) = self
                .rack_def_mut(track, slot)
                .and_then(|d| d.rack.as_deref_mut())
                .and_then(|r| r.maps.iter_mut().find(|m| m.unit == unit && m.param == p.id))
            {
                if alt { m.min = n } else { m.max = n }
            }
            self.commit(tr("Macro range"));
        }
    }

    /// Captures: a rack on track `spec` (EQ → saturator → chorus, a reverb in
    /// parallel, macros mapped), focused, the saturator open.
    pub fn screenshot_rack(&mut self, spec: &str) {
        use ponzik_engine::effects::fx_rack::unit_param;
        let track = spec.trim().parse::<usize>().ok().filter(|&t| t > 0 && t < self.project.tracks.len()).unwrap_or(1);
        let Some(slot) = self.project.tracks[track].inserts.iter().position(Option::is_none) else { return };
        let mut fx = ponzik_project::EffectDef::new_rack();
        for kind in ["eq", "saturator", "chorus"] {
            fx.rack_insert(0, None, kind);
        }
        fx.rack_add_chain();
        fx.rack_insert(1, None, "reverb");
        fx.set(P_VOLUME + 1, -4.0);
        fx.set(P_MIX + 1, 0.8);
        fx.set(unit_param(1, 1), 9.0);
        fx.map_macro(0, 1, 1, 0.1, 0.85);
        fx.map_macro(0, 3, 5, 0.6, 0.0);
        fx.map_macro(1, 2, 2, 0.0, 1.0);
        fx.map_macro(2, 0, 1, 0.0, 0.6);
        fx.set(P_MACRO, 0.62);
        fx.set(P_MACRO + 1, 0.35);
        fx.set(P_MACRO + 2, 0.2);
        self.project.tracks[track].inserts[slot] = Some(fx);
        self.rack_sel.insert((track, slot), 1);
        self.focus = super::app::Focus::Track(track);
        self.panel = true;
        self.commit(tr("Capture effects"));
    }
}
