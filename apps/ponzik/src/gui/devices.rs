//! Panneau d'appareils : l'instrument du canal et les effets de sa piste, en chaîne.
//! Chaque appareil montre un visuel (courbe d'EQ, transfert, enveloppe, filtre)
//! en plus de ses réglages.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::effects::fx_rack::{split_param, unit_param};
use ponzik_engine::mixer::{INSERT_SLOTS, MASTER};
use ponzik_engine::{ParamInfo, Unit, registry};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{App, Focus, Popup, Rename};
use super::dnd::DropTarget;
use super::playlist::pattern_color;
use super::theme::*;
use super::visuals::device_key;
use super::widgets::{Icon, draw_icon, wid};

const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GROUP_GAP: f32 = 18.0;
const CARD_GAP: f32 = 10.0;
const TRANSFER: f32 = 168.0;
const EQ_W: f32 = 640.0;

/// Ce que montre une carte d'appareil.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Device {
    Instrument(usize),
    Effect { track: usize, slot: usize },
    /// A device inside the rack of an insert slot.
    RackUnit { track: usize, slot: usize, unit: usize },
}

/// Visuel d'un groupe de paramètres, déduit des noms.
#[derive(Clone, Copy)]
enum GroupVisual {
    None,
    Envelope([&'static ParamInfo; 4]),
    Filter { cutoff: &'static ParamInfo, reso: Option<&'static ParamInfo>, highpass: bool },
}

struct Group {
    name: &'static str,
    params: Vec<&'static ParamInfo>,
    visual: GroupVisual,
    width: f32,
}

thread_local! {
    static PARAMS: RefCell<HashMap<(bool, String), &'static [ParamInfo]>> = RefCell::new(HashMap::new());
}

/// Fiches de paramètres d'un type d'appareil, mises en cache (créer un appareil
/// peut allouer beaucoup, comme les lignes de retard d'une réverbe).
pub fn params_of(kind: &str, instrument: bool) -> &'static [ParamInfo] {
    PARAMS.with(|cache| {
        *cache.borrow_mut().entry((instrument, kind.to_string())).or_insert_with(|| {
            if kind.starts_with(ponzik_clap::KIND_PREFIX) {
                // Plugin CLAP : ses fiches sont créées une fois par type de plugin.
                if instrument {
                    ponzik_clap::instrument_from_kind(kind).map_or(&[], |i| i.params())
                } else {
                    ponzik_clap::effect_from_kind(kind).map_or(&[], |e| e.params())
                }
            } else if kind.starts_with(ponzik_vst3::KIND_PREFIX) {
                // Plugin VST 3 : idem, fiches partagées entre instances.
                if instrument {
                    ponzik_vst3::instrument_from_kind(kind).map_or(&[], |i| i.params())
                } else {
                    ponzik_vst3::effect_from_kind(kind).map_or(&[], |e| e.params())
                }
            } else if instrument {
                registry::create_instrument(kind).map_or(&[], |i| i.params())
            } else {
                registry::create_effect(kind).map_or(&[], |e| e.params())
            }
        })
    })
}

fn find<'a>(list: &[&'a ParamInfo], name: &str) -> Option<&'a ParamInfo> {
    list.iter().copied().find(|p| p.name == name)
}

/// Groupes de paramètres dans l'ordre d'apparition, avec leur visuel.
fn groups(params: &'static [ParamInfo]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for p in params {
        match out.iter_mut().find(|g| g.name == p.group) {
            Some(g) => g.params.push(p),
            None => out.push(Group { name: p.group, params: vec![p], visual: GroupVisual::None, width: 0.0 }),
        }
    }
    for g in &mut out {
        let l = &g.params;
        g.visual = match (find(l, "Attack"), find(l, "Decay"), find(l, "Sustain"), find(l, "Release")) {
            (Some(a), Some(d), Some(s), Some(r)) => GroupVisual::Envelope([a, d, s, r]),
            _ => match find(l, "Cutoff") {
                Some(cutoff) => GroupVisual::Filter {
                    cutoff,
                    reso: find(l, "Resonance"),
                    highpass: find(l, "Type").is_some_and(|t| t.choices.iter().any(|c| c.contains("High"))),
                },
                None => GroupVisual::None,
            },
        };
        g.width = match g.visual {
            // Visuel sur la première rangée, tous les réglages sur la seconde.
            GroupVisual::None => g.params.len().div_ceil(2) as f32 * CELL_W,
            _ => (g.params.len() as f32 * CELL_W).max(220.0),
        };
    }
    out
}

/// Instruments qui jouent un échantillon : ils montrent sa forme d'onde.
pub fn sample_kind(kind: &str) -> bool {
    matches!(kind, "sampler" | "slicer" | "granular")
}
const SAMPLE_W: f32 = 320.0;
const RESPONSE_W: f32 = 260.0;

fn transfer_kind(kind: &str) -> bool {
    matches!(kind, "compressor" | "limiter" | "gate" | "saturator")
}

pub(super) fn card_width(kind: &str, params: &'static [ParamInfo]) -> f32 {
    if kind == "eq" {
        return EQ_W;
    }
    if kind == "drum_machine" {
        return super::notefx::DRUM_W;
    }
    if kind == "drum_pads" {
        return super::pads::WIDTH;
    }
    if kind == "mix_assistant" {
        return super::assistant::WIDTH;
    }
    let g = groups(params);
    if g.is_empty() {
        return 220.0;
    }
    let body: f32 = g.iter().map(|g| g.width).sum::<f32>() + (g.len() - 1) as f32 * GROUP_GAP;
    let extra = if transfer_kind(kind) {
        TRANSFER + GROUP_GAP
    } else if sample_kind(kind) {
        SAMPLE_W + GROUP_GAP
    } else if super::visuals::response_kind(kind).is_some() {
        RESPONSE_W + GROUP_GAP
    } else {
        0.0
    };
    (body + extra + 28.0).max(200.0)
}

/// Cible d'automation d'un paramètre d'appareil.
pub fn device_target(device: Device, id: u32) -> ponzik_engine::AutoTarget {
    match device {
        Device::Instrument(channel) => ponzik_engine::AutoTarget::InstrumentParam { channel, id },
        Device::Effect { track, slot } => ponzik_engine::AutoTarget::EffectParam { track, slot, id },
        Device::RackUnit { track, slot, unit } => ponzik_engine::AutoTarget::EffectParam { track, slot, id: unit_param(unit, id) },
    }
}

impl App {
    pub(super) fn device_kind(&self, d: Device) -> (String, bool) {
        match d {
            Device::Instrument(ch) => (self.project.channels[ch].instrument.kind.clone(), true),
            Device::Effect { track, slot } => {
                (self.project.tracks[track].inserts[slot].as_ref().map_or(String::new(), |e| e.kind.clone()), false)
            }
            Device::RackUnit { track, slot, unit } => (
                self.project.tracks[track].inserts[slot]
                    .as_ref()
                    .and_then(|e| e.rack.as_deref()?.device(unit))
                    .map_or(String::new(), |d| d.kind.clone()),
                false,
            ),
        }
    }

    /// Width of a device's card in the panel.
    pub(super) fn device_width(&self, d: Device) -> f32 {
        let (kind, inst) = self.device_kind(d);
        match d {
            Device::Effect { track, slot } if kind == ponzik_project::rack::RACK => self.rack_width(track, slot),
            _ => card_width(&kind, params_of(&kind, inst)),
        }
    }

    pub(super) fn device_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let track = self.focused_track();
        let header = Rect::new(r.x + PAD, r.y + 10.0, r.w - 2.0 * PAD, 28.0);

        // Titre : ce qu'on édite (double-clic : renommer) et, discrètement, où ça sort.
        let (title, out, rename) = match self.focus {
            Focus::Channel(ch) if ch < self.project.channels.len() => {
                let c = &self.project.channels[ch];
                let dest = &self.project.tracks[c.track].name;
                (c.name.clone(), (dest != &c.name).then(|| dest.clone()), Rename::Channel(ch))
            }
            _ => {
                let tr = &self.project.tracks[track];
                let out = (track != MASTER && tr.output != MASTER).then(|| self.project.tracks[tr.output].name.clone());
                (tr.name.clone(), out, Rename::Track(track))
            }
        };
        let tw = self.fonts.measure(self.f.sans_med, 15.0, &title);
        let title_r = Rect::new(header.x - 6.0, header.y, tw + 12.0, 28.0);
        let tid = wid("dev-title", 0, 0);
        let thov = self.hovered(input, tid, title_r);
        let thv = self.hover_anim(tid, thov);
        if thv > 0.01 {
            c.fill_rrect(title_r, 8.0, t.raised.alpha(thv));
        }
        if thov {
            self.set_tip(tr("Double-click: rename"), "");
            if input.double {
                self.open_popup(Popup::Rename { target: rename, text: title.clone() }, title_r);
            }
        }
        self.text(c, self.f.sans_med, 15.0, header.x, header.y + 14.0, &title, t.text);
        if let Some(out) = out {
            let x = header.x + tw + 14.0;
            draw_icon(c, Icon::Route, x + 7.0, header.y + 14.0, 13.0, t.muted);
            self.text(c, self.f.sans, 12.0, x + 20.0, header.y + 14.0, &out, t.muted);
        }

        let devices = self.panel_devices();

        let chain = Rect::new(r.x + PAD, header.bottom() + 8.0, r.w - 2.0 * PAD, r.bottom() - header.bottom() - 8.0 - 12.0);
        let widths: Vec<f32> = devices
            .iter()
            .map(|&d| {
                if self.layout.floating(d) {
                    return super::float::PLACEHOLDER_W;
                }
                self.device_width(d)
            })
            .collect();
        let mods = self.panel_mod_count() as f32 * 350.0 + 80.0;
        let total: f32 = widths.iter().sum::<f32>() + devices.len() as f32 * CARD_GAP + 120.0 + mods + self.note_fx_lead();
        let over_header = input.inside && header.contains(input.x, input.y);
        let over_chain = input.inside && chain.contains(input.x, input.y);
        if (over_header || (over_chain && input.mods.shift)) && input.wheel != 0.0 {
            self.chain_scroll -= input.wheel * 80.0;
        }
        // Bouton du milieu : faire glisser la chaîne.
        let pan_id = wid("chain-pan", 0, 0);
        if over_chain && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.chain_scroll, 0.0));
        }
        if let Some((_, x0, _, s0, _)) = self.pan.filter(|p| p.0 == pan_id) {
            self.chain_scroll = s0 - (input.x - x0);
            self.chain_scroll = self.chain_scroll.clamp(0.0, (total - chain.w).max(0.0));
            self.anim.set(wid("chain-sx", 0, 0), self.chain_scroll);
        }
        self.chain_scroll = self.chain_scroll.clamp(0.0, (total - chain.w).max(0.0));
        let sx = self.anim.to(wid("chain-sx", 0, 0), self.chain_scroll, T_LAYOUT);

        self.drop_zone(input, chain, DropTarget::Track(track));
        let prev = c.push_clip(chain);
        // Effets de notes du canal, avant l'instrument.
        let mut x = self.note_fx_card(c, input, chain.x - sx, chain);
        for (d, w) in devices.iter().zip(widths) {
            let card = Rect::new(x, chain.y, w, chain.h);
            if card.right() > chain.x && card.x < chain.right() {
                if self.layout.floating(*d) {
                    self.float_placeholder(c, input, card, *d);
                } else {
                    self.device_card(c, input, card, *d, false);
                }
            }
            x += w + CARD_GAP;
        }
        // Emplacement d'ajout.
        let add = Rect::new(x, chain.y, 64.0, chain.h);
        let id = wid("chain-add", track, 0);
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(add, 14.0, t.panel.alpha(hv * 0.6));
        c.stroke_rrect(add, 14.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Plus, ax, ay, 20.0, t.muted.mix(t.accent, hv));
        if hov {
            self.set_tip(tr("Add an effect\nor drag one from the browser"), "");
        }
        if hov && input.pressed {
            self.open_popup(Popup::AddEffect { track }, add);
        }
        // Modulators of the instrument and the track.
        self.modulator_cards(c, input, add.right() + CARD_GAP * 2.0, chain);
        c.set_clip(Some(prev));
        self.mod_drag_layer(c, input);
    }

    /// Modulator cards shown in the panel.
    fn panel_mod_count(&self) -> usize {
        let track = self.focused_track();
        let channel = match self.focus {
            Focus::Channel(ch) => self.project.channels.get(ch).map_or(0, |c| c.modulators.len()),
            _ => 0,
        };
        channel + self.project.tracks[track].modulators.len()
    }

    /// Appareils du panneau : l'instrument du canal sélectionné, puis les effets de sa piste.
    pub(super) fn panel_devices(&self) -> Vec<Device> {
        let track = self.focused_track();
        let mut devices = Vec::new();
        if let Focus::Channel(ch) = self.focus
            && ch < self.project.channels.len()
        {
            devices.push(Device::Instrument(ch));
        }
        for slot in 0..INSERT_SLOTS {
            if self.project.tracks[track].inserts[slot].is_some() {
                devices.push(Device::Effect { track, slot });
            }
        }
        devices
    }

    /// Carte d'un appareil dans `card`, au panneau ou dans une fenêtre (`floating`).
    pub(super) fn device_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, device: Device, floating: bool) {
        let t = self.theme;
        c.fill_rrect(card, 14.0, t.panel);
        c.stroke_rrect(card, 14.0, 1.0, t.hairline);
        if let Device::Instrument(ch) = device {
            self.drop_zone(input, card, DropTarget::Channel(ch));
        }
        let head = Rect::new(card.x, card.y, card.w, 34.0);
        let (kind, inst) = self.device_kind(device);
        let params = params_of(&kind, inst);

        let (name, bypass, color) = match device {
            Device::Instrument(_) => {
                let name = registry::instrument_kind(&kind)
                    .map(|k| tr(k.name).to_string())
                    .or_else(|| self.plugin_name(&kind))
                    .unwrap_or_else(|| super::app::display_kind(&kind));
                (name, false, t.accent)
            }
            Device::Effect { track, slot } => {
                let def = self.project.tracks[track].inserts[slot].as_ref().expect("effect in its slot");
                let name = registry::effect_kind(&def.kind)
                    .map(|k| tr(k.name).to_string())
                    .or_else(|| self.plugin_name(&def.kind))
                    .unwrap_or_else(|| super::app::display_kind(&def.kind));
                (name, def.bypass, pattern_color(1))
            }
            Device::RackUnit { .. } => (super::app::display_kind(&kind), false, pattern_color(1)),
        };
        c.fill_rrect(Rect::new(head.x + 12.0, head.y + 12.0, 3.0, 12.0), 1.5, if bypass { t.muted } else { color });
        // CPU load of the device, share of one core.
        let cpu = match device {
            Device::Instrument(ch) => self.engine.channel_cpu(ch),
            Device::Effect { track, slot } | Device::RackUnit { track, slot, .. } => self.engine.insert_cpu(track, slot),
        };
        let cpu = (cpu >= 0.0005).then(|| ponzik_i18n::tr_fmt!("{:.1}%", cpu * 100.0));
        let title_w = if matches!(device, Device::Effect { .. }) { head.w - 158.0 } else { head.w - 98.0 };
        let title_w = title_w - if cpu.is_some() { 36.0 } else { 0.0 };
        // CLAP or VST 3 plugin: a button right after its name (visible even on
        // a very wide card) opens its own interface in a window.
        let plugin = super::app::plugin_badge(&kind).is_some() && self.editors.editor(device).is_some_and(|e| e.has_gui());
        let title_w = title_w - if plugin { 32.0 } else { 0.0 };
        self.text_fit(c, self.f.sans_med, 13.0, head.x + 22.0, head.y + 18.0, &name, title_w, if bypass { t.muted } else { t.text });
        let name_w = self.fonts.measure(self.f.sans_med, 13.0, &name).min(title_w);
        let editor = Rect::new(head.x + 22.0 + name_w + 6.0, head.y + 5.0, 26.0, 26.0);
        if plugin {
            let open = self.editors.is_open(device);
            let tip = if open { tr("Close the plugin window") } else { tr("Open the plugin window") };
            if self.icon_button(c, input, wid("fx-editor", device_key(device), 0), editor, Icon::PluginWindow, open, (tip, "")) {
                self.toggle_editor(device);
            }
        }

        // Presets de l'appareil.
        let preset_x = if matches!(device, Device::Effect { .. }) { head.right() - 92.0 } else { head.right() - 32.0 };
        let preset = Rect::new(preset_x, head.y + 5.0, 26.0, 26.0);
        if self.icon_button(c, input, wid("fx-preset", device_key(device), 0), preset, Icon::Preset, false, (tr("Presets"), "")) {
            self.open_popup(Popup::Presets { device, kind: kind.clone() }, preset);
        }
        // Fenêtre : détacher ou réancrer ; l'en-tête se tire comme une barre de titre.
        let float = Rect::new(preset.x - 28.0, preset.y, 26.0, 26.0);
        if let Some(cpu) = cpu {
            self.text_right(c, self.f.mono, 9.0, float.x - 4.0, head.y + 18.0, &cpu, t.muted);
        }
        let (icon, tip) = if floating { (Icon::Dock, tr("Dock")) } else { (Icon::Detach, tr("Detach into a window")) };
        if self.icon_button(c, input, wid("fx-float", device_key(device), 0), float, icon, false, (tip, "")) {
            if floating { self.dock(device) } else { self.detach(device, Some(card)) }
        }
        // The header drags like a title bar, around the plugin button; a
        // double-click on a plugin's name opens its window (elsewhere, it
        // detaches the card as for any device).
        let grip = Rect::new(head.x, head.y, float.x - head.x - 2.0, head.h);
        let on_name = plugin && input.inside && Rect::new(head.x, head.y, editor.x - head.x, head.h).contains(input.x, input.y);
        if on_name && input.pressed && input.double {
            self.layout.cancel_gesture();
            self.drag = None;
            self.toggle_editor(device);
        } else if plugin {
            self.float_grip(input, Rect::new(head.x, head.y, editor.x - head.x - 2.0, head.h), card, device, floating);
            let rest = Rect::new(editor.right() + 2.0, head.y, (float.x - editor.right() - 4.0).max(0.0), head.h);
            self.float_grip(input, rest, card, device, floating);
            if on_name && self.drag.is_none() {
                self.set_tip(tr("Double-click: open the plugin window\nDrag upward: detach"), "");
            }
        } else {
            self.float_grip(input, grip, card, device, floating);
        }

        if let Device::Effect { track, slot } = device {
            let power = Rect::new(head.right() - 60.0, head.y + 5.0, 26.0, 26.0);
            let tip = if bypass { (tr("Enable"), "") } else { (tr("Bypass"), "") };
            if self.icon_button(c, input, wid("fx-power", track, slot), power, Icon::Power, !bypass, tip) {
                if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                    fx.bypass = !fx.bypass;
                }
                self.commit(tr("Bypass an effect"));
            }
            let close = Rect::new(head.right() - 32.0, head.y + 5.0, 26.0, 26.0);
            if self.icon_button(c, input, wid("fx-close", track, slot), close, Icon::Close, false, (tr("Remove"), "")) {
                self.project.tracks[track].inserts[slot] = None;
                self.commit(tr("Remove an effect"));
                return;
            }
        }

        let body = Rect::new(card.x + 14.0, head.bottom() + 2.0, card.w - 28.0, card.h - head.h - 8.0);
        if let Device::Effect { track, slot } = device
            && kind == ponzik_project::rack::RACK
        {
            return self.rack_device(c, input, body, track, slot);
        }
        self.device_body(c, input, body, device, &kind, params);
    }

    /// Settings of a device: its visual and its knobs, in groups.
    pub(super) fn device_body(&mut self, c: &mut Canvas, input: &Input, body: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        let kind = kind.to_string();
        if kind == "eq" {
            self.eq_device(c, input, body, device, params);
            return;
        }
        if kind == "drum_machine" {
            self.drum_device(c, input, body, device, params);
            return;
        }
        if kind == "mix_assistant" {
            return self.mix_assistant_device(c, input, body, device);
        }
        if kind == "drum_pads" {
            return self.pads_device(c, input, body, device, params);
        }
        if params.is_empty() {
            self.text(c, self.f.sans, 12.0, body.x, body.y + 20.0, tr("No settings"), t.muted);
            return;
        }
        let mut x = body.x;
        if transfer_kind(&kind) {
            let side = TRANSFER.min(body.h - 6.0);
            self.transfer_graph(c, input, Rect::new(x, body.y + 4.0, side, side), device, &kind, params);
            x += TRANSFER + GROUP_GAP;
        } else if sample_kind(&kind) {
            self.sample_view(c, input, Rect::new(x, body.y + 4.0, SAMPLE_W, body.h - 10.0), device, params);
            x += SAMPLE_W + GROUP_GAP;
        } else if super::visuals::response_kind(&kind).is_some() {
            self.response_graph(c, input, Rect::new(x, body.y + 4.0, RESPONSE_W, body.h - 10.0), device, &kind, params);
            x += RESPONSE_W + GROUP_GAP;
        }
        let key = device_key(device);
        for (gi, g) in groups(params).into_iter().enumerate() {
            if gi > 0 {
                c.fill_rect(Rect::new(x - GROUP_GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
            }
            if !g.name.is_empty() {
                self.text(c, self.f.sans_med, 11.0, x + 4.0, body.y + 6.0, tr(g.name), t.muted);
            }
            let visual = Rect::new(x + 2.0, body.y + 18.0, g.width - 4.0, CELL_H - 8.0);
            let single_row = !matches!(g.visual, GroupVisual::None);
            match g.visual {
                GroupVisual::Envelope(adsr) => self.envelope_graph(c, input, visual, device, adsr),
                GroupVisual::Filter { cutoff, reso, highpass } => {
                    let highpass = highpass && find(&g.params, "Type").is_some_and(|tp| self.param_value(device, tp) > 0.5);
                    self.filter_graph(c, input, visual, device, cutoff, reso, highpass)
                }
                GroupVisual::None => {}
            }
            for (k, p) in g.params.iter().enumerate() {
                let (col, row) = if single_row { (k, 1) } else { (k / 2, k % 2) };
                let cell = Rect::new(x + col as f32 * CELL_W, body.y + 16.0 + row as f32 * CELL_H, CELL_W, CELL_H);
                let value = self.param_value(device, p);
                let id = wid("prm", key, p.id as usize);
                let mut changed = if p.unit == Unit::Choice {
                    self.choice(c, input, id, cell, p, value)
                } else {
                    self.knob(c, input, id, cell, p, value)
                };
                if let Device::RackUnit { track, slot, unit } = device {
                    self.macro_range_knob(c, cell, id, track, slot, unit, p, &mut changed, input.mods.alt);
                }
                self.mod_knob(c, input, id, cell, device, p, value, &mut changed);
                if let Some(v) = changed {
                    self.set_param_value(device, p, v);
                }
                // Clic droit : automatiser, valeur par défaut ; point sarcelle si automatisé.
                let target = device_target(device, p.id);
                if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                    let popup = Popup::Param { target, value, default: p.default };
                    self.open_popup(popup, Rect::new(input.x, input.y, 0.0, 0.0));
                }
                if self.is_automated(target) {
                    c.fill_circle(cell.right() - 10.0, cell.y + 6.0, 3.0, t.select);
                }
            }
            x += g.width + GROUP_GAP;
        }
    }

    /// Réglages d'un appareil qui diffèrent des défauts.
    fn device_params(&self, device: Device) -> Vec<(u32, f32)> {
        match device {
            Device::Instrument(ch) => self.project.channels.get(ch).map_or(Vec::new(), |c| c.instrument.params.clone()),
            Device::Effect { track, slot } => {
                self.project.tracks[track].inserts[slot].as_ref().map_or(Vec::new(), |e| e.params.clone())
            }
            Device::RackUnit { track, slot, unit } => self.project.tracks[track].inserts[slot].as_ref().map_or(Vec::new(), |e| {
                e.params
                    .iter()
                    .filter_map(|&(id, v)| split_param(id).filter(|&(u, _)| u == unit).map(|(_, local)| (local, v)))
                    .collect()
            }),
        }
    }

    /// Chains and mappings of a rack device, saved with its preset.
    fn device_rack(&self, device: Device) -> Option<ponzik_project::RackDef> {
        let Device::Effect { track, slot } = device else { return None };
        self.project.tracks[track].inserts[slot].as_ref()?.rack.as_deref().cloned()
    }

    pub fn save_preset(&mut self, device: Device, name: &str) {
        let (kind, _) = self.device_kind(device);
        let params = self.device_params(device);
        match super::compat::save_preset(&kind, name, &params, self.device_rack(device).as_ref()) {
            Ok(_) => self.toast(&tr_fmt!("Preset saved: {}", name)),
            Err(e) => self.toast(&e),
        }
    }

    /// Remplace tous les réglages de l'appareil par ceux du preset.
    pub fn apply_preset(&mut self, device: Device, params: &[(u32, f32)], rack: Option<ponzik_project::RackDef>) {
        match device {
            Device::Instrument(ch) => {
                if let Some(c) = self.project.channels.get_mut(ch) {
                    c.instrument.params = params.to_vec();
                }
            }
            Device::Effect { track, slot } => {
                if let Some(e) = self.project.tracks[track].inserts[slot].as_mut() {
                    e.params = params.to_vec();
                    if e.kind == ponzik_project::rack::RACK
                        && let Some(rack) = rack
                    {
                        e.rack = Some(Box::new(rack));
                    }
                }
                // Modulations of the old rack devices no longer apply.
                self.project.prune_modulators();
            }
            Device::RackUnit { track, slot, unit } => {
                if let Some(e) = self.project.tracks[track].inserts[slot].as_mut() {
                    e.params.retain(|(id, _)| split_param(*id).is_none_or(|(u, _)| u != unit));
                    e.params.extend(params.iter().map(|&(id, v)| (unit_param(unit, id), v)));
                }
            }
        }
        self.commit(tr("Load a preset"));
    }

    pub(super) fn param_value(&self, device: Device, p: &ParamInfo) -> f32 {
        match device {
            Device::Instrument(ch) => self.project.channels.get(ch).and_then(|c| c.instrument.get(p.id)),
            Device::Effect { track, slot } => self.project.tracks[track].inserts[slot].as_ref().and_then(|e| e.get(p.id)),
            Device::RackUnit { track, slot, unit } => {
                return self.project.tracks[track].inserts[slot].as_ref().map_or(p.default, |e| e.effective(unit_param(unit, p.id), p));
            }
        }
        .unwrap_or(p.default)
    }

    pub(super) fn set_param_value(&mut self, device: Device, p: &ParamInfo, v: f32) {
        let v = p.clamp(v);
        if (self.param_value(device, p) - v).abs() < f32::EPSILON {
            return;
        }
        match device {
            Device::Instrument(ch) => self.project.channels[ch].instrument.set(p.id, v),
            Device::Effect { track, slot } => {
                if let Some(e) = self.project.tracks[track].inserts[slot].as_mut() {
                    e.set(p.id, v);
                }
            }
            Device::RackUnit { track, slot, unit } => {
                if let Some(e) = self.project.tracks[track].inserts[slot].as_mut() {
                    e.set(unit_param(unit, p.id), v);
                }
            }
        }
        self.commit(&tr_fmt!("Setting: {}", tr(p.name)));
    }
}
