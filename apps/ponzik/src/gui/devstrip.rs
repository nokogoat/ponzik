//! Finding one's way in the device panel with many devices: a strip of
//! chips listing them (click: jump there, drag: reorder, right-click: their
//! menu), cards that collapse to a slim card, Ctrl+←/→ from device to
//! device, a "+" between cards to insert an effect right there, and the
//! wheel scrolling the panel anywhere.

use ponzik_engine::mixer::INSERT_SLOTS;
use ponzik_engine::registry;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::{Action, App, Focus, MenuItem, Popup};
use super::devices::Device;
use super::theme::*;
use super::visuals::device_key;
use super::widgets::{Icon, draw_icon, wid};

/// Width of a collapsed card.
pub const COLLAPSED_W: f32 = 132.0;
/// Width of the « Add an effect » card at the end of the chain.
pub const ADD_W: f32 = 132.0;
/// Seconds a wheel gesture keeps scrolling the panel, even over knobs.
const WHEEL_LATCH: f32 = 0.4;

/// What a chip of the strip stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripItem {
    /// The note effects card of the channel.
    Notes,
    Dev(Device),
    /// The modulator cards.
    Mods,
}

/// An edit of a device from its menu or the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceOp {
    Bypass,
    Remove,
    /// Detach into a window, or dock back.
    Float,
    Collapse,
    /// One place to the left (-1) or to the right (+1) among the effects.
    Move(i8),
    /// Scrolls the panel to it.
    Show,
}

/// A chip dragged along the strip to reorder the effects.
#[derive(Clone, Copy, Debug)]
pub struct StripDrag {
    pub slot: usize,
    pub grab_dx: f32,
}

/// The panel's chain laid out: each item and where it starts in the chain
/// (0: the left edge of the content), with its width.
pub struct ChainLayout {
    pub items: Vec<(StripItem, f32, f32)>,
    pub total: f32,
}

impl ChainLayout {
    pub fn offset(&self, it: StripItem) -> Option<f32> {
        self.items.iter().find(|(i, _, _)| *i == it).map(|&(_, x, _)| x)
    }
}

impl App {
    /// Short name of a device, as its card shows it.
    pub(super) fn device_name(&self, d: Device) -> String {
        let (kind, inst) = self.device_kind(d);
        let found = if inst { registry::instrument_kind(&kind).map(|k| tr(k.name).to_string()) } else { registry::effect_kind(&kind).map(|k| tr(k.name).to_string()) };
        found.or_else(|| self.plugin_name(&kind)).unwrap_or_else(|| super::app::display_kind(&kind))
    }

    fn device_bypassed(&self, d: Device) -> bool {
        match d {
            Device::Effect { track, slot } => self.project.tracks[track].inserts[slot].as_ref().is_some_and(|e| e.bypass),
            _ => false,
        }
    }

    pub(super) fn is_collapsed(&self, d: Device) -> bool {
        self.collapsed.contains(&d)
    }

    /// The wheel over the panel scrolls it sideways, unless it starts on a
    /// knob (which it turns); once scrolling, it keeps scrolling for a moment
    /// even over knobs. Returns true when this frame's wheel scrolled.
    pub(super) fn panel_wheel(&mut self, input: &Input, panel: Rect, header: Rect) -> bool {
        if !(input.inside && panel.contains(input.x, input.y)) || input.wheel == 0.0 {
            return false;
        }
        let latched = self.time < self.wheel_latch;
        if input.mods.shift || latched || !self.wheel_hot || header.contains(input.x, input.y) {
            self.chain_scroll -= input.wheel * 80.0;
            self.wheel_latch = self.time + WHEEL_LATCH;
            return true;
        }
        false
    }

    /// A control under the pointer takes the wheel (this frame).
    pub(super) fn claim_wheel(&mut self) {
        self.wheel_hot_now = true;
    }

    /// Scrolls the chain so that `item` starts near the left edge.
    pub(super) fn reveal(&mut self, lay: &ChainLayout, item: StripItem) {
        if let Some(x) = lay.offset(item) {
            self.chain_scroll = (x - 8.0).max(0.0);
        }
        self.panel_cursor = Some(item);
    }

    /// Ctrl+← / Ctrl+→: the previous or next device; with Shift, the
    /// selected effect moves.
    pub(super) fn panel_keys(&mut self, input: &Input, lay: &ChainLayout, view_w: f32) {
        for &(key, mods) in &input.keys {
            let dir: i32 = match key {
                Key::Left if mods.ctrl => -1,
                Key::Right if mods.ctrl => 1,
                _ => continue,
            };
            if mods.shift {
                if let Some(StripItem::Dev(d @ Device::Effect { .. })) = self.panel_cursor {
                    self.device_op(d, DeviceOp::Move(dir as i8));
                }
                continue;
            }
            let n = lay.items.len() as i32;
            if n == 0 {
                continue;
            }
            // From the selected item, or the first one in view.
            let cur = self.panel_cursor.and_then(|c| lay.items.iter().position(|(i, _, _)| *i == c)).map(|i| i as i32).unwrap_or_else(|| {
                let first = lay.items.iter().position(|&(_, x, w)| x + w > self.chain_scroll + 4.0 && x < self.chain_scroll + view_w).unwrap_or(0) as i32;
                first - dir
            });
            let next = (cur + dir).clamp(0, n - 1) as usize;
            let item = lay.items[next].0;
            self.reveal(lay, item);
            let name = match item {
                StripItem::Notes => tr("Notes").to_string(),
                StripItem::Mods => tr("Modulators").to_string(),
                StripItem::Dev(d) => self.device_name(d),
            };
            self.toast(&tr_fmt!("{} · {} of {}", name, next + 1, n));
        }
    }

    /// The strip of chips in `r`: one per item of the chain.
    pub(super) fn device_strip(&mut self, c: &mut Canvas, input: &Input, r: Rect, lay: &ChainLayout, view_w: f32) {
        let t = self.theme;
        if lay.items.is_empty() || r.w < 40.0 {
            return;
        }
        let track = self.focused_track();
        // Labels and natural widths; names shrink, then go, when the strip is full.
        let labels: Vec<(Icon, String, bool)> = lay
            .items
            .iter()
            .map(|&(it, _, _)| match it {
                StripItem::Notes => (Icon::Arpeggio, tr("Notes").to_string(), false),
                StripItem::Mods => (Icon::Automation, tr("Modulators").to_string(), false),
                StripItem::Dev(d @ Device::Instrument(_)) => (Icon::Instrument, self.device_name(d), false),
                StripItem::Dev(d) => (Icon::Effect, self.device_name(d), self.device_bypassed(d)),
            })
            .collect();
        let gap = 4.0;
        let natural: Vec<f32> = labels.iter().map(|(_, s, _)| (self.fonts.measure(self.f.sans_med, 12.0, s) + 34.0).min(150.0)).collect();
        let avail = r.w - gap * (labels.len() as f32 - 1.0);
        let sum: f32 = natural.iter().sum();
        let widths: Vec<f32> = if sum <= avail {
            natural
        } else {
            // Every chip gets the same share, at least its icon.
            let share = (avail / labels.len() as f32).max(30.0);
            natural.iter().map(|&w| w.min(share)).collect()
        };
        let scroll = self.chain_scroll;
        let in_view = |x: f32, w: f32| x + w > scroll + 2.0 && x < scroll + view_w - 2.0;
        let mut x = r.x;
        let mut effect_chips: Vec<(usize, Rect)> = Vec::new();
        let dragging = self.strip_drag;
        for (k, (&(item, ox, ow), (icon, name, bypass))) in lay.items.iter().zip(&labels).enumerate() {
            let w = widths[k];
            let chip = Rect::new(x, r.y + 2.0, w, r.h - 4.0);
            x += w + gap;
            if chip.right() > r.right() + 0.5 {
                break;
            }
            if let StripItem::Dev(Device::Effect { slot, .. }) = item {
                effect_chips.push((slot, chip));
            }
            let id = wid("strip-chip", k, device_item_key(item));
            let hov = self.hovered(input, id, chip);
            let hv = self.hover_anim(id, hov);
            let seen = in_view(ox, ow);
            let sel = self.panel_cursor == Some(item);
            let on = self.anim.to(id ^ 0x5345_454e, seen as u8 as f32, T_HOVER);
            let held = matches!((dragging, item), (Some(d), StripItem::Dev(Device::Effect { slot, .. })) if d.slot == slot);
            let fill = t.raised.alpha(0.5 + 0.5 * hv).mix(t.accent, on * 0.14);
            c.fill_rrect(chip, 8.0, if held { fill.alpha(0.35) } else { fill });
            if sel {
                c.stroke_rrect(chip, 8.0, 1.5, t.accent.alpha(0.9));
            } else if on > 0.01 {
                c.stroke_rrect(chip, 8.0, 1.0, t.accent.alpha(0.35 * on));
            }
            let col = if *bypass { t.muted } else { t.text2.mix(t.text, hv.max(on)) };
            draw_icon(c, *icon, chip.x + 14.0, chip.y + chip.h * 0.5, 12.0, if *bypass { t.muted } else { t.accent.mix(col, 0.4) });
            if chip.w > 40.0 {
                self.text_fit(c, self.f.sans_med, 12.0, chip.x + 26.0, chip.y + chip.h * 0.5, name, chip.w - 32.0, col);
            }
            if *bypass {
                c.fill_rect(Rect::new(chip.x + 24.0, chip.y + chip.h * 0.5, chip.w - 30.0, 1.0), t.muted.alpha(0.8));
            }
            if hov && self.strip_drag.is_none() {
                let tip = match item {
                    StripItem::Dev(Device::Effect { .. }) => tr_fmt!("{}\nClick: go to it · drag: move it · right-click: bypass, remove, detach, presets", name),
                    StripItem::Dev(_) => tr_fmt!("{}\nClick: go to it · right-click: its menu", name),
                    _ => tr_fmt!("{}\nClick: go to it", name),
                };
                self.set_tip(&tip, "Ctrl ← →");
                self.set_cursor(Cursor::Hand);
            }
            if hov && input.pressed && self.drag.is_none() {
                self.reveal(lay, item);
                if let StripItem::Dev(Device::Effect { slot, .. }) = item {
                    self.begin_drag(id, input, 0.0);
                    self.strip_drag = Some(StripDrag { slot, grab_dx: input.x - chip.x });
                }
            }
            if hov && input.right_pressed && self.drag.is_none() {
                if let StripItem::Dev(d) = item {
                    self.panel_cursor = Some(item);
                    self.open_popup(Popup::DeviceMenu(d), Rect::new(chip.x, chip.bottom(), chip.w, 0.0));
                }
            }
        }
        // Reordering: a ghost of the chip follows the mouse, a bar shows where it lands.
        if let Some(d) = self.strip_drag {
            let moved = self.drag.as_ref().is_some_and(|g| (input.x - g.x0).abs() > 6.0);
            let others: Vec<&(usize, Rect)> = effect_chips.iter().filter(|(s, _)| *s != d.slot).collect();
            let target = others.iter().filter(|(_, r)| r.x + r.w * 0.5 < input.x).count();
            if moved {
                self.set_cursor(Cursor::Grab);
                if let Some(&(_, from)) = effect_chips.iter().find(|(s, _)| *s == d.slot) {
                    let ghost = Rect::new(input.x - d.grab_dx, from.y - 3.0, from.w, from.h);
                    c.fill_rrect(ghost, 8.0, t.raised_hi.alpha(0.9));
                    c.stroke_rrect(ghost, 8.0, 1.5, t.accent);
                }
                let bar_x = match (others.get(target.wrapping_sub(1)), others.get(target)) {
                    (_, Some((_, r))) => r.x - gap * 0.5,
                    (Some((_, r)), None) => r.right() + gap * 0.5,
                    _ => r.x,
                };
                c.fill_rrect(Rect::new(bar_x - 1.0, r.y, 2.0, r.h), 1.0, t.accent);
            }
            if input.released || !input.down {
                self.strip_drag = None;
                if moved {
                    // The new order of the occupied slots, gaps kept where they are.
                    let occupied: Vec<usize> = (0..INSERT_SLOTS).filter(|&s| self.project.tracks[track].inserts[s].is_some()).collect();
                    let mut order: Vec<usize> = occupied.iter().copied().filter(|&s| s != d.slot).collect();
                    order.insert(target.min(order.len()), d.slot);
                    let mut perm: [usize; INSERT_SLOTS] = std::array::from_fn(|i| i);
                    for (k, &s) in occupied.iter().enumerate() {
                        perm[s] = order[k];
                    }
                    if self.project.permute_inserts(track, &perm) {
                        let to = occupied[occupied.iter().position(|&s| perm[s] == d.slot).unwrap_or(0)];
                        self.panel_cursor = Some(StripItem::Dev(Device::Effect { track, slot: to }));
                        self.commit(tr("Move an effect"));
                    }
                }
            }
        }
    }

    /// Menu of a device (right-click on its chip).
    pub(super) fn device_menu(&self, d: Device) -> (String, Vec<MenuItem>) {
        let name = self.device_name(d);
        let mut v = Vec::new();
        let (kind, _) = self.device_kind(d);
        if let Device::Effect { track, slot } = d {
            let bypass = self.device_bypassed(d);
            v.push(MenuItem::new(Icon::Power, if bypass { tr("Enable") } else { tr("Bypass") }, Action::Device(d, DeviceOp::Bypass)));
            let first = (0..slot).all(|s| self.project.tracks[track].inserts[s].is_none());
            let last = (slot + 1..INSERT_SLOTS).all(|s| self.project.tracks[track].inserts[s].is_none());
            v.push(MenuItem::new(Icon::ChevronLeft, tr("Move left"), Action::Device(d, DeviceOp::Move(-1))).detail("Ctrl Shift ←".into()).dim(first));
            v.push(MenuItem::new(Icon::ChevronRight, tr("Move right"), Action::Device(d, DeviceOp::Move(1))).detail("Ctrl Shift →".into()).dim(last));
        }
        let collapsed = self.is_collapsed(d);
        v.push(MenuItem::new(if collapsed { Icon::Expand } else { Icon::Collapse }, if collapsed { tr("Expand") } else { tr("Collapse") }, Action::Device(d, DeviceOp::Collapse)).sep());
        let floating = self.layout.floating(d);
        v.push(MenuItem::new(if floating { Icon::Dock } else { Icon::Detach }, if floating { tr("Dock") } else { tr("Detach into a window") }, Action::Device(d, DeviceOp::Float)));
        if !kind.is_empty() {
            v.push(MenuItem::new(Icon::Preset, tr("Presets"), Action::Popup(Popup::Presets { device: d, kind })));
        }
        if let Device::Effect { track, .. } = d {
            let insert = match d {
                Device::Effect { slot, .. } => Popup::InsertEffect { track, slot },
                _ => Popup::AddEffect { track },
            };
            v.push(MenuItem::new(Icon::Plus, tr("Insert an effect before"), Action::Popup(insert)).sep());
            v.push(MenuItem::new(Icon::Trash, tr("Remove"), Action::Device(d, DeviceOp::Remove)).sep());
        }
        (name, v)
    }

    pub(super) fn device_op(&mut self, d: Device, op: DeviceOp) {
        match (op, d) {
            (DeviceOp::Bypass, Device::Effect { track, slot }) => {
                if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                    fx.bypass = !fx.bypass;
                    self.commit(tr("Bypass an effect"));
                }
            }
            (DeviceOp::Remove, Device::Effect { track, slot }) => {
                self.project.tracks[track].inserts[slot] = None;
                self.collapsed.remove(&d);
                self.commit(tr("Remove an effect"));
            }
            (DeviceOp::Float, _) => {
                if self.layout.floating(d) {
                    self.dock(d);
                } else {
                    self.detach(d, None);
                }
            }
            (DeviceOp::Collapse, _) => {
                if !self.collapsed.remove(&d) {
                    self.collapsed.insert(d);
                }
                self.panel_cursor = Some(StripItem::Dev(d));
            }
            (DeviceOp::Move(dir), Device::Effect { track, slot }) => {
                let occupied: Vec<usize> = (0..INSERT_SLOTS).filter(|&s| self.project.tracks[track].inserts[s].is_some()).collect();
                let Some(k) = occupied.iter().position(|&s| s == slot) else { return };
                let j = k as i32 + dir as i32;
                if j < 0 || j as usize >= occupied.len() {
                    return;
                }
                let other = occupied[j as usize];
                let mut perm: [usize; INSERT_SLOTS] = std::array::from_fn(|i| i);
                perm.swap(slot, other);
                if self.project.permute_inserts(track, &perm) {
                    self.panel_cursor = Some(StripItem::Dev(Device::Effect { track, slot: other }));
                    self.commit(tr("Move an effect"));
                }
            }
            (DeviceOp::Show, _) => {
                self.panel = true;
                self.panel_reveal = Some(StripItem::Dev(d));
            }
            _ => {}
        }
    }

    /// Collapse or expand every device of the panel.
    pub(super) fn collapse_all(&mut self, collapse: bool) {
        for d in self.panel_devices() {
            if collapse {
                self.collapsed.insert(d);
            } else {
                self.collapsed.remove(&d);
            }
        }
    }

    /// A collapsed card: its name, on/off and a way back.
    pub(super) fn collapsed_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, d: Device) {
        let t = self.theme;
        let key = device_key(d);
        let id = wid("dev-folded", key, 0);
        let hov = self.hovered(input, id, card);
        let hv = self.hover_anim(id, hov);
        let bypass = self.device_bypassed(d);
        c.fill_rrect(card, 14.0, t.panel.mix(t.raised, hv * 0.5));
        c.stroke_rrect(card, 14.0, 1.0, if self.panel_cursor == Some(StripItem::Dev(d)) { t.accent.alpha(0.8) } else { t.hairline });
        let color = if bypass { t.muted } else if matches!(d, Device::Instrument(_)) { t.accent } else { super::playlist::pattern_color(1) };
        c.fill_rrect(Rect::new(card.x + 12.0, card.y + 12.0, 3.0, 12.0), 1.5, color);
        let name = self.device_name(d);
        self.text_fit(c, self.f.sans_med, 13.0, card.x + 22.0, card.y + 18.0, &name, card.w - 30.0, if bypass { t.muted } else { t.text });
        let (cx, cy) = card.center();
        // On/off, big enough to hit.
        if let Device::Effect { track, slot } = d {
            let power = Rect::new(cx - 22.0, cy - 30.0, 44.0, 44.0);
            let tip = if bypass { (tr("Enable"), "") } else { (tr("Bypass"), "") };
            if self.icon_button(c, input, wid("fx-power-folded", track, slot), power, Icon::Power, !bypass, tip) {
                self.device_op(d, DeviceOp::Bypass);
            }
        } else {
            draw_icon(c, Icon::Instrument, cx, cy - 8.0, 22.0, t.text2);
        }
        let expand = Rect::new(card.x + 8.0, card.bottom() - 44.0, card.w - 16.0, 34.0);
        if self.lab_button(c, input, wid("dev-expand", key, 0), expand, Some(Icon::Expand), tr("Expand"), 0, tr("Show its settings again")) {
            self.device_op(d, DeviceOp::Collapse);
        }
        if hov && input.right_pressed && self.drag.is_none() {
            self.open_popup(Popup::DeviceMenu(d), Rect::new(input.x, input.y, 0.0, 0.0));
        }
        if hov && input.double && self.drag.is_none() {
            self.device_op(d, DeviceOp::Collapse);
        }
    }

    /// The « + » between two cards: an effect inserted right there.
    pub(super) fn insert_gap(&mut self, c: &mut Canvas, input: &Input, x: f32, chain: Rect, track: usize, slot: usize) {
        let t = self.theme;
        let zone = Rect::new(x - 9.0, chain.y, 18.0, chain.h);
        let id = wid("chain-gap", track, slot);
        let near = input.inside && zone.contains(input.x, input.y) && self.drag.is_none();
        let a = self.anim.to(id, near as u8 as f32, T_HOVER);
        if a < 0.01 {
            return;
        }
        let (cx, cy) = (x, chain.y + chain.h * 0.5);
        c.fill_rect(Rect::new(cx - 1.0, chain.y + 10.0, 2.0, chain.h - 20.0), t.accent.alpha(0.5 * a));
        c.fill_circle(cx, cy, 13.0, t.raised_hi.mix(t.accent, 0.25).alpha(a));
        c.stroke_circle(cx, cy, 13.0, 1.0, t.accent.alpha(a));
        draw_icon(c, Icon::Plus, cx, cy, 14.0, t.text.alpha(a));
        if near {
            self.set_tip(tr("Insert an effect here"), "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.open_popup(Popup::InsertEffect { track, slot }, Rect::new(cx - 13.0, cy - 13.0, 26.0, 26.0));
            }
        }
    }

    /// An effect at `slot` of `track`, those after it shifting right.
    pub fn insert_effect_at(&mut self, track: usize, slot: usize, kind: &str) {
        let Some(info) = registry::effect_kind(kind) else { return };
        let Some(at) = self.project.open_insert(track, slot) else {
            self.toast(tr("No free slot left on this track"));
            return;
        };
        self.project.tracks[track].inserts[at] = Some(ponzik_project::EffectDef::new(kind));
        self.panel_cursor = Some(StripItem::Dev(Device::Effect { track, slot: at }));
        self.commit(tr("Add an effect"));
        let name = self.project.tracks[track].name.clone();
        self.toast(&tr_fmt!("{} added on {}", tr(info.name), name));
    }

    /// The panel's header switch: the channel's instrument, or its mixer
    /// track's effects (a channel's panel shows both; a track's panel goes
    /// back to the channel playing into it).
    pub(super) fn panel_switch(&mut self, c: &mut Canvas, input: &Input, x: f32, cy: f32, lay: &ChainLayout) -> f32 {
        let t = self.theme;
        let track = self.focused_track();
        let (label, icon, tip, go): (String, Icon, String, Option<Focus>) = match self.focus {
            Focus::Channel(_) => {
                let n = self.project.tracks[track].inserts.iter().flatten().count();
                let name = self.project.tracks[track].name.clone();
                (ponzik_i18n::tr_fmt_n!("Track {} · {} effect", "Track {} · {} effects", n, track, n), Icon::Mixer, tr_fmt!("Mixer track {}: {}\nClick: go to its effects · double-click: open the track alone", track, name), None)
            }
            Focus::Track(t) => {
                let ch = self.panel_return.filter(|&c| self.project.channels.get(c).is_some_and(|ch| ch.track == t)).or_else(|| self.project.channels.iter().position(|ch| ch.track == t));
                let Some(ch) = ch else { return x };
                let name = self.project.channels[ch].name.clone();
                (name.clone(), Icon::Instrument, tr_fmt!("Back to the channel {}: its instrument and these effects", name), Some(Focus::Channel(ch)))
            }
        };
        let w = self.fonts.measure(self.f.sans_med, 12.0, &label) + 36.0;
        let r = Rect::new(x, cy - 13.0, w, 26.0);
        let id = wid("panel-switch", track, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let tint = self.track_tint(track);
        c.fill_rrect(r, 13.0, tint.alpha(0.12 + 0.12 * hv));
        c.stroke_rrect(r, 13.0, 1.0, tint.alpha(0.5 + 0.3 * hv));
        draw_icon(c, icon, r.x + 15.0, cy, 12.0, tint.mix(t.text, 0.3));
        self.text(c, self.f.sans_med, 12.0, r.x + 27.0, cy, &label, t.text.mix(tint, 0.15));
        if hov {
            self.set_tip(&tip, "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                match go {
                    Some(f) => self.focus = f,
                    None if input.double => {
                        self.panel_return = self.selected_channel();
                        self.focus = Focus::Track(track);
                    }
                    None => {
                        let first = lay.items.iter().find(|(i, _, _)| matches!(i, StripItem::Dev(Device::Effect { .. })));
                        match first {
                            Some(&(item, _, _)) => self.reveal(lay, item),
                            None => self.open_popup(Popup::AddEffect { track }, r),
                        }
                    }
                }
            }
        }
        r.right()
    }

    /// The tint of the panel's owner, for its title.
    pub(super) fn panel_tint(&self) -> Color {
        self.track_tint(self.focused_track())
    }
}

/// A stable number per strip item, for widget identities.
fn device_item_key(it: StripItem) -> usize {
    match it {
        StripItem::Notes => 1,
        StripItem::Mods => 2,
        StripItem::Dev(d) => device_key(d) + 3,
    }
}

impl App {
    /// Screenshot setup (`PONZIK_DEVICES=collapse:1,collapse:2,menu:3,cursor:2,seen`):
    /// the panel's devices (counted from 0) collapsed, a chip's menu, the
    /// keyboard cursor, the strip hint dismissed.
    pub fn screenshot_devices(&mut self, spec: &str) {
        let devices = self.panel_devices();
        for part in spec.split(',').map(str::trim) {
            let (key, arg) = part.split_once(':').unwrap_or((part, ""));
            let d = arg.parse::<usize>().ok().and_then(|i| devices.get(i).copied());
            match (key, d) {
                ("collapse", Some(d)) => {
                    self.collapsed.insert(d);
                }
                ("cursor", Some(d)) => self.panel_cursor = Some(StripItem::Dev(d)),
                ("menu", Some(d)) => {
                    self.panel_cursor = Some(StripItem::Dev(d));
                    self.open_popup(Popup::DeviceMenu(d), Rect::new(700.0, 765.0, 90.0, 0.0));
                }
                ("insert", Some(Device::Effect { track, slot })) => {
                    self.open_popup(Popup::InsertEffect { track, slot }, Rect::new(700.0, 640.0, 26.0, 26.0));
                }
                ("seen", _) => self.hint_seen(super::hints::Hint::DeviceStrip),
                _ => {}
            }
        }
    }
}
