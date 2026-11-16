//! Panneau d'appareils : l'instrument du canal et les effets de sa piste, en chaîne.
//! Chaque appareil montre un visuel (courbe d'EQ, transfert, enveloppe, filtre)
//! en plus de ses réglages.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER};
use ponzik_engine::{ParamInfo, Unit, registry};
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    Instrument(usize),
    Effect { track: usize, slot: usize },
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
        g.visual = match (find(l, "Attaque"), find(l, "Déclin"), find(l, "Maintien"), find(l, "Relâche")) {
            (Some(a), Some(d), Some(s), Some(r)) => GroupVisual::Envelope([a, d, s, r]),
            _ => match find(l, "Coupure") {
                Some(cutoff) => GroupVisual::Filter {
                    cutoff,
                    reso: find(l, "Résonance"),
                    highpass: find(l, "Type").is_some_and(|t| t.choices.iter().any(|c| c.contains("haut"))),
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

fn card_width(kind: &str, params: &'static [ParamInfo]) -> f32 {
    if kind == "eq" {
        return EQ_W;
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
    }
}

impl App {
    fn device_kind(&self, d: Device) -> (String, bool) {
        match d {
            Device::Instrument(ch) => (self.project.channels[ch].instrument.kind.clone(), true),
            Device::Effect { track, slot } => {
                (self.project.tracks[track].inserts[slot].as_ref().map_or(String::new(), |e| e.kind.clone()), false)
            }
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
            self.set_tip("Double-clic : renommer", "");
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

        let chain = Rect::new(r.x + PAD, header.bottom() + 8.0, r.w - 2.0 * PAD, r.bottom() - header.bottom() - 8.0 - 12.0);
        let widths: Vec<f32> = devices
            .iter()
            .map(|&d| {
                let (kind, inst) = self.device_kind(d);
                card_width(&kind, params_of(&kind, inst))
            })
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + devices.len() as f32 * CARD_GAP + 120.0;
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
        let mut x = chain.x - sx;
        for (d, w) in devices.iter().zip(widths) {
            let card = Rect::new(x, chain.y, w, chain.h);
            if card.right() > chain.x && card.x < chain.right() {
                self.device_card(c, input, card, *d);
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
            self.set_tip("Ajouter un effet\nou glisser un effet du navigateur", "");
        }
        if hov && input.pressed {
            self.open_popup(Popup::AddEffect { track }, add);
        }
        c.set_clip(Some(prev));
    }

    fn device_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, device: Device) {
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
                let name = registry::instrument_kind(&kind).map_or_else(|| super::app::display_kind(&kind), |k| k.name.to_string());
                (name, false, t.accent)
            }
            Device::Effect { track, slot } => {
                let def = self.project.tracks[track].inserts[slot].as_ref().expect("effet présent");
                let name = registry::effect_kind(&def.kind).map_or_else(|| super::app::display_kind(&def.kind), |k| k.name.to_string());
                (name, def.bypass, pattern_color(1))
            }
        };
        c.fill_rrect(Rect::new(head.x + 12.0, head.y + 12.0, 3.0, 12.0), 1.5, if bypass { t.muted } else { color });
        let title_w = if matches!(device, Device::Effect { .. }) { head.w - 130.0 } else { head.w - 70.0 };
        self.text_fit(c, self.f.sans_med, 13.0, head.x + 22.0, head.y + 18.0, &name, title_w, if bypass { t.muted } else { t.text });

        // Presets de l'appareil.
        let preset_x = if matches!(device, Device::Effect { .. }) { head.right() - 92.0 } else { head.right() - 32.0 };
        let preset = Rect::new(preset_x, head.y + 5.0, 26.0, 26.0);
        if self.icon_button(c, input, wid("fx-preset", device_key(device), 0), preset, Icon::Preset, false, ("Presets", "")) {
            self.open_popup(Popup::Presets { device, kind: kind.clone() }, preset);
        }

        if let Device::Effect { track, slot } = device {
            let power = Rect::new(head.right() - 60.0, head.y + 5.0, 26.0, 26.0);
            let tip = if bypass { ("Activer", "") } else { ("Contourner", "") };
            if self.icon_button(c, input, wid("fx-power", track, slot), power, Icon::Power, !bypass, tip) {
                if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                    fx.bypass = !fx.bypass;
                }
                self.commit("Contourner un effet");
            }
            let close = Rect::new(head.right() - 32.0, head.y + 5.0, 26.0, 26.0);
            if self.icon_button(c, input, wid("fx-close", track, slot), close, Icon::Close, false, ("Retirer", "")) {
                self.project.tracks[track].inserts[slot] = None;
                self.commit("Retirer un effet");
                return;
            }
        }

        let body = Rect::new(card.x + 14.0, head.bottom() + 2.0, card.w - 28.0, card.h - head.h - 8.0);
        if kind == "eq" {
            self.eq_device(c, input, body, device, params);
            return;
        }
        if params.is_empty() {
            self.text(c, self.f.sans, 12.0, body.x, body.y + 20.0, "Aucun réglage", t.muted);
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
            self.response_graph(c, Rect::new(x, body.y + 4.0, RESPONSE_W, body.h - 10.0), device, &kind, params);
            x += RESPONSE_W + GROUP_GAP;
        }
        let key = device_key(device);
        for (gi, g) in groups(params).into_iter().enumerate() {
            if gi > 0 {
                c.fill_rect(Rect::new(x - GROUP_GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
            }
            if !g.name.is_empty() {
                self.text(c, self.f.sans_med, 11.0, x + 4.0, body.y + 6.0, g.name, t.muted);
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
                let changed = if p.unit == Unit::Choice {
                    self.choice(c, input, id, cell, p, value)
                } else {
                    self.knob(c, input, id, cell, p, value)
                };
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
        }
    }

    pub fn save_preset(&mut self, device: Device, name: &str) {
        let (kind, _) = self.device_kind(device);
        let params = self.device_params(device);
        match super::compat::save_preset(&kind, name, &params) {
            Ok(_) => self.toast(&format!("Preset enregistré : {name}")),
            Err(e) => self.toast(&e),
        }
    }

    /// Remplace tous les réglages de l'appareil par ceux du preset.
    pub fn apply_preset(&mut self, device: Device, params: &[(u32, f32)]) {
        match device {
            Device::Instrument(ch) => {
                if let Some(c) = self.project.channels.get_mut(ch) {
                    c.instrument.params = params.to_vec();
                }
            }
            Device::Effect { track, slot } => {
                if let Some(e) = self.project.tracks[track].inserts[slot].as_mut() {
                    e.params = params.to_vec();
                }
            }
        }
        self.commit("Charger un preset");
    }

    pub(super) fn param_value(&self, device: Device, p: &ParamInfo) -> f32 {
        match device {
            Device::Instrument(ch) => self.project.channels.get(ch).and_then(|c| c.instrument.get(p.id)),
            Device::Effect { track, slot } => self.project.tracks[track].inserts[slot].as_ref().and_then(|e| e.get(p.id)),
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
        }
        self.commit(&format!("Réglage : {}", p.name));
    }
}
