//! Modulator cards in the device panel (LFO, envelope follower, step
//! sequencer, random), their live trace, and how they reach the knobs: drag a
//! card's handle onto a knob, or pick "Modulate with…" in a knob's menu; with
//! a card in depth mode, dragging a knob sets that modulator's depth on it.
//! Modulated knobs show a depth arc per modulator and a dot that moves with
//! the current value.

use std::collections::HashMap;
use std::f32::consts::PI;

use ponzik_engine::modulation::{STEP_COUNT_P, STEP_FIRST, STEP_RATE};
use ponzik_engine::{AutoTarget, ModKind, ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::ModulatorDef;
use ponzik_project::rack::{MeterKey, mod_meters};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, Focus, MenuItem, Popup};
use super::devices::Device;
use super::playlist::pattern_color;
use super::widgets::{Icon, draw_icon, wid};

/// Where modulators live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModOwner {
    /// Targets: the track's insert slots.
    Track(usize),
    /// Targets: the channel's instrument (slot 0).
    Channel(usize),
}

impl ModOwner {
    pub fn key(self) -> MeterKey {
        match self {
            ModOwner::Track(t) => (false, t),
            ModOwner::Channel(c) => (true, c),
        }
    }

    /// Who owns the parameter behind an automation target, and its (slot, id).
    pub fn of_target(target: AutoTarget) -> Option<(ModOwner, usize, u32)> {
        match target {
            AutoTarget::InstrumentParam { channel, id } => Some((ModOwner::Channel(channel), 0, id)),
            AutoTarget::EffectParam { track, slot, id } => Some((ModOwner::Track(track), slot, id)),
            _ => None,
        }
    }
}

/// Live trace length of a modulator, in frames.
const TRACE: usize = 120;
const CARD_W: f32 = 340.0;
const CELL: f32 = 56.0;
const SCOPE_H: f32 = 46.0;

/// Interface state of the modulators (not saved).
#[derive(Default)]
pub struct ModUi {
    /// A card's handle being dragged towards a knob.
    pub dragging: Option<(ModOwner, usize)>,
    /// Card in depth mode: dragging a knob sets this modulator's depth.
    pub edit: Option<(ModOwner, usize)>,
    traces: HashMap<(ModOwner, usize), (Vec<f32>, usize)>,
}

#[derive(Clone, Debug)]
pub enum ModAction {
    Add(ModOwner, ModKind, Option<(usize, u32)>),
    Remove(ModOwner, usize),
    Connect(ModOwner, usize, usize, u32, f32),
    Disconnect(ModOwner, usize, usize, u32),
}

/// Color of modulator `i`.
pub fn mod_color(i: usize) -> Color {
    pattern_color([1, 7, 5, 3, 6, 2, 4, 0][i % 8])
}

fn angle(n: f32) -> f32 {
    0.75 * PI + 1.5 * PI * n.clamp(0.0, 1.0)
}

impl App {
    pub(super) fn mods_of(&self, owner: ModOwner) -> &[ModulatorDef] {
        match owner {
            ModOwner::Track(t) => self.project.tracks.get(t).map_or(&[], |t| &t.modulators),
            ModOwner::Channel(c) => self.project.channels.get(c).map_or(&[], |c| &c.modulators),
        }
    }

    fn mods_mut(&mut self, owner: ModOwner) -> Option<&mut Vec<ModulatorDef>> {
        match owner {
            ModOwner::Track(t) => self.project.tracks.get_mut(t).map(|t| &mut t.modulators),
            ModOwner::Channel(c) => self.project.channels.get_mut(c).map(|c| &mut c.modulators),
        }
    }

    /// Display name: "LFO 2" (second LFO of its owner).
    pub(super) fn mod_name(&self, owner: ModOwner, i: usize) -> String {
        let mods = self.mods_of(owner);
        let Some(m) = mods.get(i) else { return String::new() };
        let same = mods[..i].iter().filter(|o| o.kind == m.kind).count();
        format!("{} {}", tr(m.kind.name()), same + 1)
    }

    /// Owner, slot and full id of a device parameter.
    pub(super) fn mod_slot(device: Device, id: u32) -> (ModOwner, usize, u32) {
        match device {
            Device::Instrument(ch) => (ModOwner::Channel(ch), 0, id),
            Device::Effect { track, slot } => (ModOwner::Track(track), slot, id),
            Device::RackUnit { track, slot, unit } => {
                (ModOwner::Track(track), slot, ponzik_engine::effects::fx_rack::unit_param(unit, id))
            }
        }
    }

    pub(super) fn run_mod_action(&mut self, action: ModAction) {
        match action {
            ModAction::Add(owner, kind, target) => {
                let Some(mods) = self.mods_mut(owner) else { return };
                if mods.len() >= ponzik_engine::modulation::MAX_MODULATORS {
                    self.toast(tr("No room for another modulator here"));
                    return;
                }
                let mut m = ModulatorDef::new(kind);
                if let Some((slot, id)) = target {
                    m.connect(slot, id, 0.35);
                }
                mods.push(m);
                let i = mods.len() - 1;
                self.mod_ui.edit = Some((owner, i));
                self.commit(tr("Add a modulator"));
            }
            ModAction::Remove(owner, i) => {
                if let Some(mods) = self.mods_mut(owner)
                    && i < mods.len()
                {
                    mods.remove(i);
                }
                self.mod_ui.edit = None;
                self.commit(tr("Remove a modulator"));
            }
            ModAction::Connect(owner, i, slot, id, depth) => {
                if let Some(m) = self.mods_mut(owner).and_then(|m| m.get_mut(i)) {
                    m.connect(slot, id, depth);
                }
                self.commit(tr("Modulate a parameter"));
            }
            ModAction::Disconnect(owner, i, slot, id) => {
                if let Some(m) = self.mods_mut(owner).and_then(|m| m.get_mut(i)) {
                    m.disconnect(slot, id);
                }
                self.commit(tr("Remove a modulation"));
            }
        }
    }

    /// Menu lines of a knob for its modulators.
    pub(super) fn mod_menu_items(&self, target: AutoTarget) -> Vec<MenuItem> {
        let Some((owner, slot, id)) = ModOwner::of_target(target) else { return Vec::new() };
        let mut v = Vec::new();
        for (i, m) in self.mods_of(owner).iter().enumerate() {
            let name = self.mod_name(owner, i);
            let mut it = if m.target(slot, id).is_some() {
                MenuItem::new(Icon::Close, &tr_fmt!("Remove the modulation by {}", name), Action::Mod(ModAction::Disconnect(owner, i, slot, id)))
            } else {
                MenuItem::new(Icon::Automation, &tr_fmt!("Modulate with {}", name), Action::Mod(ModAction::Connect(owner, i, slot, id, 0.35)))
            };
            it.color = Some(mod_color(i));
            v.push(it);
        }
        for kind in ModKind::ALL {
            let it = MenuItem::new(Icon::Plus, &tr_fmt!("New {} on this parameter", tr(kind.name())), Action::Mod(ModAction::Add(owner, kind, Some((slot, id)))))
                .tip(tr(kind.description()));
            v.push(it);
        }
        if let Some(first) = v.first_mut() {
            first.sep = true;
        }
        v
    }

    /// Menu to add a modulator: on the instrument (channel focus) or the track.
    pub(super) fn add_mod_items(&self, track: usize, channel: Option<usize>) -> Vec<MenuItem> {
        let mut v = Vec::new();
        let section = |owner: ModOwner, label: &str, v: &mut Vec<MenuItem>| {
            for (k, kind) in ModKind::ALL.into_iter().enumerate() {
                let it = MenuItem::new(Icon::Automation, tr(kind.name()), Action::Mod(ModAction::Add(owner, kind, None)))
                    .detail(label.to_string())
                    .tip(tr(kind.description()));
                v.push(if k == 0 && !v.is_empty() { it.sep() } else { it });
            }
        };
        if let Some(ch) = channel {
            section(ModOwner::Channel(ch), tr("Instrument"), &mut v);
        }
        section(ModOwner::Track(track), tr("Track effects"), &mut v);
        v
    }

    /// After a knob is drawn: depth arcs and the moving value, a modulator
    /// handle dropped on it, and depth editing. `changed` is the knob's new
    /// value, taken over in depth mode.
    pub(super) fn mod_knob(&mut self, c: &mut Canvas, input: &Input, knob: u64, cell: Rect, device: Device, p: &ParamInfo, value: f32, changed: &mut Option<f32>) {
        if p.unit == Unit::Choice {
            return;
        }
        let (owner, slot, id) = Self::mod_slot(device, p.id);
        let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
        let base = p.to_norm(value);
        let over = input.inside && Rect::new(cell.x, cell.y + 8.0, cell.w, 54.0).contains(input.x, input.y);

        // Depth mode: the knob's drag sets the depth.
        if let Some((o, i)) = self.mod_ui.edit.filter(|e| e.0 == owner)
            && i < self.mods_of(o).len()
        {
            if over && !self.is_dragging(knob) && self.drag.is_none() {
                self.set_tip(&tr_fmt!("Drag: depth of {}", self.mod_name(o, i)), "");
            }
            if self.is_dragging(knob)
                && let Some(v) = changed.take()
            {
                let depth = ((p.to_norm(v) - base) * 100.0).round() / 100.0;
                let label = tr("Modulation depth");
                if let Some(m) = self.mods_mut(o).and_then(|m| m.get_mut(i)) {
                    if depth == 0.0 { m.disconnect(slot, id) } else { m.connect(slot, id, depth) }
                }
                self.commit(label);
            }
            c.stroke_arc(cx, cy, 23.0, angle(0.0), angle(1.0), 1.0, mod_color(i).alpha(0.25));
        }

        // A handle dropped here connects.
        if let Some((o, i)) = self.mod_ui.dragging.filter(|d| d.0 == owner) {
            if over {
                c.stroke_circle(cx, cy, 24.0, 2.0, mod_color(i));
                if input.released {
                    self.run_mod_action(ModAction::Connect(o, i, slot, id, 0.35));
                    self.mod_ui.edit = Some((o, i));
                }
            } else {
                c.stroke_circle(cx, cy, 23.0, 1.0, mod_color(i).alpha(0.35));
            }
        }

        // Arcs and the moving value.
        let meters = mod_meters(owner.key());
        let mut offset = 0.0;
        let mut any = false;
        let edit = self.mod_ui.edit;
        for (i, m) in self.mods_of(owner).iter().enumerate() {
            let Some(target) = m.target(slot, id) else { continue };
            any = true;
            let col = mod_color(i);
            let r = 21.5 + 2.5 * (i % 3) as f32;
            let (a, b) = (angle(base), angle(base + target.depth));
            let strong = edit == Some((owner, i));
            c.stroke_arc(cx, cy, r, a.min(b), a.max(b), if strong { 3.0 } else { 2.0 }, col.alpha(if strong { 1.0 } else { 0.75 }));
            offset += target.depth * meters.get(i);
        }
        if any {
            let n = (base + offset).clamp(0.0, 1.0);
            let (a, b) = (angle(base), angle(n));
            c.stroke_arc(cx, cy, 17.0, a.min(b), a.max(b), 3.0, Color::hex(0xffffff).alpha(0.55));
            let ang = angle(n);
            c.fill_circle(cx + ang.cos() * 17.0, cy + ang.sin() * 17.0, 3.2, Color::hex(0xffffff));
        }
    }

    /// Modulator cards of the panel, from `x`; returns the x after them.
    pub(super) fn modulator_cards(&mut self, c: &mut Canvas, input: &Input, mut x: f32, area: Rect) -> f32 {
        let t = self.theme;
        let track = self.focused_track();
        let channel = match self.focus {
            Focus::Channel(ch) if ch < self.project.channels.len() => Some(ch),
            _ => None,
        };
        let mut owners = Vec::new();
        if let Some(ch) = channel {
            owners.push(ModOwner::Channel(ch));
        }
        owners.push(ModOwner::Track(track));
        for owner in owners {
            for i in 0..self.mods_of(owner).len() {
                let card = Rect::new(x, area.y, CARD_W, area.h);
                if card.right() > area.x && card.x < area.right() {
                    self.modulator_card(c, input, card, owner, i);
                }
                x += CARD_W + 10.0;
            }
        }
        // Add a modulator.
        let add = Rect::new(x, area.y, 64.0, area.h);
        let id = wid("mod-add", track, channel.unwrap_or(usize::MAX));
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(add, 14.0, t.panel.alpha(hv * 0.6));
        c.stroke_rrect(add, 14.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Automation, ax, ay - 8.0, 18.0, t.muted.mix(t.select, hv));
        draw_icon(c, Icon::Plus, ax, ay + 14.0, 12.0, t.muted.mix(t.select, hv));
        if hov {
            self.set_tip(tr("Add a modulator\nLFO, envelope follower, steps, random"), "");
        }
        if hov && input.pressed {
            self.open_popup(Popup::AddModulator { track, channel }, add);
        }
        x + 64.0
    }

    fn modulator_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, owner: ModOwner, i: usize) {
        let t = self.theme;
        let Some(def) = self.mods_of(owner).get(i).cloned() else { return };
        let col = mod_color(i);
        let editing = self.mod_ui.edit == Some((owner, i));
        c.fill_rrect(card, 14.0, t.panel);
        c.stroke_rrect(card, 14.0, if editing { 1.5 } else { 1.0 }, if editing { col } else { t.hairline });
        let key = owner.key();
        let base_id = (key.0 as usize) << 20 | key.1 << 8 | i;

        // Header: name (click: depth mode), owner, handle, close.
        let head = Rect::new(card.x, card.y, card.w, 34.0);
        c.fill_rrect(Rect::new(head.x + 12.0, head.y + 12.0, 3.0, 12.0), 1.5, col);
        let name = self.mod_name(owner, i);
        let name_r = Rect::new(head.x + 6.0, head.y + 4.0, card.w - 110.0, 26.0);
        let nid = wid("mod-name", base_id, 0);
        let nhov = self.hovered(input, nid, name_r);
        if nhov {
            self.set_tip(tr("Click: depth mode (drag knobs to set the depth)"), "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.mod_ui.edit = if editing { None } else { Some((owner, i)) };
            }
        }
        let w = self.text(c, self.f.sans_med, 13.0, head.x + 22.0, head.y + 18.0, &name, if editing { col } else { t.text });
        let where_ = match owner {
            ModOwner::Track(_) => tr("track"),
            ModOwner::Channel(_) => tr("instrument"),
        };
        let n = def.targets.len();
        let info = match n {
            0 => where_.to_string(),
            1 => tr_fmt!("{} · 1 target", where_),
            n => tr_fmt!("{} · {} targets", where_, n),
        };
        self.text_fit(c, self.f.sans, 11.0, head.x + 30.0 + w, head.y + 18.0, &info, card.w - w - 140.0, t.muted);

        // Handle: drag onto a knob.
        let handle = Rect::new(head.right() - 62.0, head.y + 5.0, 26.0, 26.0);
        let hid = wid("mod-handle", base_id, 0);
        let hhov = self.hovered(input, hid, handle);
        let dragging = self.mod_ui.dragging == Some((owner, i));
        c.fill_circle(handle.center().0, handle.center().1, if hhov || dragging { 9.0 } else { 7.0 }, col.alpha(if hhov || dragging { 1.0 } else { 0.8 }));
        c.fill_circle(handle.center().0, handle.center().1, 3.0, t.panel);
        if hhov {
            self.set_tip(tr("Drag onto a knob to modulate it"), "");
            self.set_cursor(Cursor::Grab);
            if input.pressed {
                self.mod_ui.dragging = Some((owner, i));
            }
        }
        let close = Rect::new(head.right() - 32.0, head.y + 5.0, 26.0, 26.0);
        if self.icon_button(c, input, wid("mod-close", base_id, 0), close, Icon::Close, false, (tr("Remove"), "")) {
            self.run_mod_action(ModAction::Remove(owner, i));
            return;
        }

        // Live trace, or the steps.
        let body = Rect::new(card.x + 14.0, head.bottom() + 2.0, card.w - 28.0, card.h - head.h - 8.0);
        let scope = Rect::new(body.x, body.y + 2.0, body.w, SCOPE_H);
        c.fill_rrect(scope, 8.0, t.well);
        let value = mod_meters(key).get(i);
        if def.kind == ModKind::Steps {
            self.step_editor(c, input, scope, owner, i, &def, base_id);
        } else {
            let (buf, at) = self.mod_ui.traces.entry((owner, i)).or_insert_with(|| (vec![0.0; TRACE], 0));
            buf[*at] = value;
            *at = (*at + 1) % TRACE;
            let pts: Vec<(f32, f32)> = (0..TRACE)
                .map(|k| {
                    let v = buf[(*at + k) % TRACE];
                    let y = if def.kind.bipolar() { scope.y + scope.h * 0.5 * (1.0 - v) } else { scope.bottom() - scope.h * v };
                    (scope.x + 4.0 + (scope.w - 8.0) * k as f32 / (TRACE - 1) as f32, y.clamp(scope.y + 2.0, scope.bottom() - 2.0))
                })
                .collect();
            if def.kind.bipolar() {
                c.fill_rect(Rect::new(scope.x + 4.0, scope.y + scope.h * 0.5, scope.w - 8.0, 1.0), t.hairline);
            }
            c.stroke_polyline(&pts, 1.8, col);
            if let Some(&(px, py)) = pts.last() {
                c.fill_circle(px, py, 3.0, col);
            }
        }

        // Settings.
        // Choices take one and a half knob widths.
        let params: Vec<&'static ParamInfo> = def.kind.params().iter().filter(|p| p.group.is_empty()).collect();
        let weight = |p: &ParamInfo| if p.unit == Unit::Choice { 1.5 } else { 1.0 };
        let units: f32 = params.iter().map(|p| weight(p)).sum();
        let unit_w = (body.w / units.max(1.0)).min(CELL + 8.0);
        let mut px = body.x;
        for p in params {
            let cell_w = unit_w * weight(p);
            let cell = Rect::new(px, scope.bottom() + 4.0, cell_w, 76.0);
            px += cell_w;
            let v = def.get(p.id);
            let id = wid("mod-prm", base_id, p.id as usize);
            let changed = if p.unit == Unit::Choice { self.choice(c, input, id, cell, p, v) } else { self.knob(c, input, id, cell, p, v) };
            if let Some(v) = changed
                && let Some(m) = self.mods_mut(owner).and_then(|m| m.get_mut(i))
            {
                m.set(p.id, v);
                self.commit(&tr_fmt!("Setting: {}", tr(p.name)));
            }
        }
    }

    /// Bars of the step sequencer: click or drag to draw.
    fn step_editor(&mut self, c: &mut Canvas, input: &Input, r: Rect, owner: ModOwner, i: usize, def: &ModulatorDef, base_id: usize) {
        let t = self.theme;
        let col = mod_color(i);
        let count = [4, 8, 12, 16][(def.get(STEP_COUNT_P).round() as usize).min(3)];
        let beats = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0][(def.get(STEP_RATE).round() as usize).min(5)];
        let now = if self.playing {
            Some(((self.position / ponzik_engine::PPQ as f64) / beats).floor().rem_euclid(count as f64) as usize)
        } else {
            None
        };
        let inner = Rect::new(r.x + 4.0, r.y + 3.0, r.w - 8.0, r.h - 6.0);
        let bw = inner.w / count as f32;
        let mid = inner.y + inner.h * 0.5;
        c.fill_rect(Rect::new(inner.x, mid, inner.w, 1.0), t.hairline);
        for s in 0..count {
            let v = def.get(STEP_FIRST + s as u32);
            let x = inner.x + s as f32 * bw;
            let y = mid - v * inner.h * 0.5;
            let bar = Rect::new(x + 1.5, y.min(mid), bw - 3.0, (y - mid).abs().max(1.5));
            let hot = now == Some(s);
            c.fill_rrect(bar, 2.0, col.alpha(if hot { 1.0 } else { 0.55 }));
        }
        let id = wid("mod-steps", base_id, 0);
        let hov = self.hovered(input, id, inner);
        if hov {
            self.set_cursor(Cursor::Hand);
            self.set_tip(tr("Click or drag to draw the steps"), "");
            if input.pressed {
                self.begin_drag(id, input, 0.0);
            }
        }
        if self.is_dragging(id) && input.down && inner.w > 0.0 {
            let s = (((input.x - inner.x) / bw).floor().max(0.0) as usize).min(count - 1);
            let v = ((mid - input.y) / (inner.h * 0.5)).clamp(-1.0, 1.0);
            let v = (v * 20.0).round() / 20.0;
            if let Some(m) = self.mods_mut(owner).and_then(|m| m.get_mut(i))
                && m.get(STEP_FIRST + s as u32) != v
            {
                m.set(STEP_FIRST + s as u32, v);
                self.commit(tr("Draw the steps"));
            }
        }
    }

    /// Line from the dragged handle to the pointer; the gesture ends on release.
    pub(super) fn mod_drag_layer(&mut self, c: &mut Canvas, input: &Input) {
        let Some((_, i)) = self.mod_ui.dragging else { return };
        let col = mod_color(i);
        c.fill_circle(input.x, input.y, 6.0, col.alpha(0.9));
        c.stroke_circle(input.x, input.y, 11.0, 1.5, col.alpha(0.5));
        self.set_cursor(Cursor::Grab);
        if input.released || !input.down {
            self.mod_ui.dragging = None;
        }
    }

    /// Captures: an LFO on the first rack's macro and a device, a step
    /// sequencer and a follower, on the focused track (and channel).
    pub fn screenshot_mods(&mut self, spec: &str) {
        use ponzik_engine::effects::fx_rack::{P_MACRO, unit_param};
        let track = self.focused_track();
        let rack = self.project.tracks[track].inserts.iter().position(|f| f.as_ref().is_some_and(|f| f.kind == ponzik_project::rack::RACK));
        let mut lfo = ModulatorDef::new(ModKind::Lfo);
        lfo.set(ponzik_engine::modulation::LFO_SHAPE, 1.0);
        lfo.set(ponzik_engine::modulation::LFO_RATE, 1.5);
        let mut steps = ModulatorDef::new(ModKind::Steps);
        steps.set(ponzik_engine::modulation::STEP_COUNT_P, 3.0);
        let mut follow = ModulatorDef::new(ModKind::Follower);
        if let Some(slot) = rack {
            lfo.connect(slot, P_MACRO + 1, 0.4);
            lfo.connect(slot, unit_param(1, 2), -0.3);
            steps.connect(slot, unit_param(1, 1), 0.25);
            follow.connect(slot, unit_param(3, 5), 0.5);
        }
        let mods = &mut self.project.tracks[track].modulators;
        mods.push(lfo);
        mods.push(steps);
        if spec.contains("all") {
            mods.push(follow);
            mods.push(ModulatorDef::new(ModKind::Random));
        }
        if spec.contains("edit") {
            self.mod_ui.edit = Some((ModOwner::Track(track), 0));
        }
        // An LFO on the instrument's first two knobs.
        if spec.contains("channel")
            && let Focus::Channel(ch) = self.focus
            && let Some(c) = self.project.channels.get_mut(ch)
        {
            let params = super::devices::params_of(&c.instrument.kind, true);
            let mut lfo = ModulatorDef::new(ModKind::Lfo);
            lfo.set(ponzik_engine::modulation::LFO_RATE, 0.8);
            for (k, p) in params.iter().filter(|p| p.unit != Unit::Choice).take(2).enumerate() {
                lfo.connect(0, p.id, if k == 0 { 0.45 } else { -0.25 });
            }
            c.modulators.push(lfo);
            if spec.contains("edit") {
                self.mod_ui.edit = Some((ModOwner::Channel(ch), 0));
            }
        }
        self.commit(tr("Add a modulator"));
    }
}
