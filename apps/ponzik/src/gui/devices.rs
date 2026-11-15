//! Panneau d'appareils : l'instrument du canal et les effets de sa piste, en chaîne.

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER};
use ponzik_engine::{ParamInfo, Unit, registry};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{App, Focus, Popup};
use super::playlist::pattern_color;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GROUP_GAP: f32 = 18.0;
const CARD_GAP: f32 = 10.0;

/// Ce que montre une carte d'appareil.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Device {
    Instrument(usize),
    Effect { track: usize, slot: usize },
}

/// Groupes de paramètres dans l'ordre d'apparition.
fn groups(params: &'static [ParamInfo]) -> Vec<(&'static str, Vec<&'static ParamInfo>)> {
    let mut out: Vec<(&'static str, Vec<&'static ParamInfo>)> = Vec::new();
    for p in params {
        match out.iter_mut().find(|(g, _)| *g == p.group) {
            Some((_, v)) => v.push(p),
            None => out.push((p.group, vec![p])),
        }
    }
    out
}

fn card_width(params: &'static [ParamInfo]) -> f32 {
    let g = groups(params);
    if g.is_empty() {
        return 220.0;
    }
    let cols: f32 = g.iter().map(|(_, v)| v.len().div_ceil(2) as f32).sum();
    (cols * CELL_W + (g.len() - 1) as f32 * GROUP_GAP + 28.0).max(200.0)
}

impl App {
    fn instrument_params(&self, kind: &str) -> &'static [ParamInfo] {
        registry::create_instrument(kind).map_or(&[], |i| i.params())
    }

    fn effect_params(&self, kind: &str) -> &'static [ParamInfo] {
        registry::create_effect(kind).map_or(&[], |e| e.params())
    }

    pub(super) fn device_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let track = self.focused_track();
        let header = Rect::new(r.x + PAD, r.y + 10.0, r.w - 2.0 * PAD, 28.0);

        // Titre : ce qu'on édite et où ça sort.
        let (title, detail) = match self.focus {
            Focus::Channel(ch) if ch < self.project.channels.len() => {
                let c = &self.project.channels[ch];
                (c.name.clone(), format!("Canal {} → {}", ch + 1, self.project.tracks[c.track].name))
            }
            _ => {
                let name = self.project.tracks[track].name.clone();
                let out = if track == MASTER { "Sortie audio".to_string() } else { format!("→ {}", self.project.tracks[self.project.tracks[track].output].name) };
                (name, out)
            }
        };
        let w = self.text(c, self.f.sans_med, 15.0, header.x, header.y + 14.0, &title, t.text);
        self.text(c, self.f.sans, 12.0, header.x + w + 12.0, header.y + 14.0, &detail, t.muted);

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
            .map(|d| match *d {
                Device::Instrument(ch) => card_width(self.instrument_params(&self.project.channels[ch].instrument.kind.clone())),
                Device::Effect { track, slot } => {
                    let kind = self.project.tracks[track].inserts[slot].as_ref().map_or(String::new(), |e| e.kind.clone());
                    card_width(self.effect_params(&kind))
                }
            })
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + devices.len() as f32 * CARD_GAP + 120.0;
        let over_header = input.inside && header.contains(input.x, input.y);
        let over_chain = input.inside && chain.contains(input.x, input.y);
        if (over_header || (over_chain && input.mods.shift)) && input.wheel != 0.0 {
            self.chain_scroll -= input.wheel * 80.0;
        }
        self.chain_scroll = self.chain_scroll.clamp(0.0, (total - chain.w).max(0.0));
        let sx = self.anim.to(wid("chain-sx", 0, 0), self.chain_scroll, T_LAYOUT);

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
        let add = Rect::new(x, chain.y, 110.0, chain.h);
        let id = wid("chain-add", track, 0);
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.stroke_rrect(add, 14.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Plus, ax, ay - 10.0, 18.0, t.muted.mix(t.accent, hv));
        self.text_center(c, self.f.sans, 12.0, ax, ay + 16.0, "Effet", t.muted.mix(t.text2, hv));
        if hov && input.pressed {
            self.open_popup(Popup::AddEffect { track }, add);
        }
        c.set_clip(Some(prev));
    }

    fn device_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, device: Device) {
        let t = self.theme;
        c.fill_rrect(card, 14.0, t.panel);
        c.stroke_rrect(card, 14.0, 1.0, t.hairline);
        let head = Rect::new(card.x, card.y, card.w, 34.0);

        let (name, params, bypass, color) = match device {
            Device::Instrument(ch) => {
                let kind = self.project.channels[ch].instrument.kind.clone();
                let name = registry::instrument_kind(&kind).map_or(kind.clone(), |k| k.name.to_string());
                (name, self.instrument_params(&kind), false, t.accent)
            }
            Device::Effect { track, slot } => {
                let def = self.project.tracks[track].inserts[slot].clone().expect("effet présent");
                let name = registry::effect_kind(&def.kind).map_or(def.kind.clone(), |k| k.name.to_string());
                (name, self.effect_params(&def.kind), def.bypass, pattern_color(1))
            }
        };
        c.fill_rrect(Rect::new(head.x + 12.0, head.y + 12.0, 3.0, 12.0), 1.5, if bypass { t.muted } else { color });
        self.text_fit(c, self.f.sans_med, 13.0, head.x + 22.0, head.y + 18.0, &name, head.w - 90.0, if bypass { t.muted } else { t.text });

        if let Device::Effect { track, slot } = device {
            let power = Rect::new(head.right() - 60.0, head.y + 5.0, 26.0, 26.0);
            if self.icon_button(c, input, wid("fx-power", track, slot), power, Icon::Power, !bypass) {
                if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                    fx.bypass = !fx.bypass;
                }
                self.commit("Contourner un effet");
            }
            let close = Rect::new(head.right() - 32.0, head.y + 5.0, 26.0, 26.0);
            if self.icon_button(c, input, wid("fx-close", track, slot), close, Icon::Close, false) {
                self.project.tracks[track].inserts[slot] = None;
                self.commit("Retirer un effet");
                return;
            }
        }

        let body = Rect::new(card.x + 14.0, head.bottom() + 2.0, card.w - 28.0, card.h - head.h - 8.0);
        if params.is_empty() {
            self.text(c, self.f.sans, 12.0, body.x, body.y + 20.0, "Aucun réglage", t.muted);
            return;
        }
        let mut x = body.x;
        for (gi, (group, list)) in groups(params).into_iter().enumerate() {
            let cols = list.len().div_ceil(2);
            if !group.is_empty() {
                self.text(c, self.f.sans_med, 11.0, x + 4.0, body.y + 6.0, group, t.muted);
            }
            for (k, p) in list.iter().enumerate() {
                let (col, row) = (k / 2, k % 2);
                let cell = Rect::new(x + col as f32 * CELL_W, body.y + 16.0 + row as f32 * CELL_H, CELL_W, CELL_H);
                let value = self.param_value(device, p);
                let id = match device {
                    Device::Instrument(ch) => wid("prm-i", ch, p.id as usize),
                    Device::Effect { track, slot } => wid("prm-e", track * 16 + slot, p.id as usize),
                };
                let changed = if p.unit == Unit::Choice {
                    self.choice(c, input, id, cell, p, value)
                } else {
                    self.knob(c, input, id, cell, p, value)
                };
                if let Some(v) = changed {
                    self.set_param_value(device, p, v);
                }
            }
            x += cols as f32 * CELL_W + GROUP_GAP;
            if gi > 0 || !group.is_empty() {
                c.fill_rect(Rect::new(x - GROUP_GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
            }
        }
    }

    fn param_value(&self, device: Device, p: &ParamInfo) -> f32 {
        match device {
            Device::Instrument(ch) => self.project.channels[ch].instrument.get(p.id),
            Device::Effect { track, slot } => self.project.tracks[track].inserts[slot].as_ref().and_then(|e| e.get(p.id)),
        }
        .unwrap_or(p.default)
    }

    fn set_param_value(&mut self, device: Device, p: &ParamInfo, v: f32) {
        let v = p.clamp(v);
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
