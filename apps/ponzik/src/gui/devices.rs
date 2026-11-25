//! Panneau d'appareils : l'instrument du canal et les effets de sa piste, en chaîne.
//! Chaque appareil montre un visuel (courbe d'EQ, transfert, enveloppe, filtre)
//! en plus de ses réglages.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::effects::fx_rack::{split_param, unit_param};
use ponzik_engine::mixer::{INSERT_SLOTS, MASTER};
use ponzik_engine::{ParamInfo, Unit, registry};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::ab::AbDevice;
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
/// Knobs an envelope group shows at most before its delay and hold go to the graph only.
const MAX_ENV_KNOBS: usize = 8;
/// Narrowest envelope editor.
const ENV_MIN_W: f32 = 260.0;
const CARD_GAP: f32 = 10.0;
const TRANSFER: f32 = 168.0;
const EQ_W: f32 = 640.0;
/// A and B letters and the copy button, in a device's header.
const AB_W: f32 = 66.0;
/// Left edge of a device's name in its header (after the color mark or fold button).
const TITLE_X: f32 = 22.0;
/// The plugin window button, after a plugin's name.
const PLUGIN_BUTTON_W: f32 = 32.0;
/// Folding moves the header right by this much.
const FOLD_SHIFT: f32 = 10.0;
/// Most a card widens to show its whole name (a plugin's can be very long).
const TITLE_MAX_W: f32 = 240.0;

/// Width taken by a header's buttons, from its right edge, with a gap before them:
/// A/B, detach and presets, plus power and remove for an effect.
fn header_controls_w(device: Device) -> f32 {
    match device {
        Device::Effect { .. } => 120.0 + AB_W + 4.0 + 8.0,
        Device::Instrument(_) => 60.0 + AB_W + 4.0 + 8.0,
        Device::RackUnit { .. } => 60.0 + 8.0,
    }
}

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
    /// The shared envelope editor; `live` shows the last played voice (the amp envelope).
    Envelope { spec: super::envelope::EnvSpec, live: bool },
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
            use ponzik_project::plugins;
            if plugins::Format::of_kind(kind).is_some() {
                // Plugin (CLAP, VST 3, LV2, LADSPA) : ses fiches sont créées une
                // fois par type de plugin, partagées entre instances.
                if instrument {
                    plugins::instrument_from_kind(kind).and_then(Result::ok).map_or(&[], |i| i.params())
                } else {
                    plugins::effect_from_kind(kind).and_then(Result::ok).map_or(&[], |e| e.params())
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
        if let Some(spec) = super::envelope::EnvSpec::find(g.name, &g.params) {
            // The envelope's knobs in the order it plays, then the others; its
            // curves are bent on the graph. Short of room, delay and hold too.
            let crowded = g.params.iter().filter(|p| !spec.graph_only(p)).count() > MAX_ENV_KNOBS;
            let in_graph = |p: &ParamInfo| spec.graph_only(p) || (crowded && matches!(p.name, "Delay" | "Hold"));
            let mut knobs: Vec<&'static ParamInfo> = spec.ordered().into_iter().filter(|p| !in_graph(p)).collect();
            knobs.extend(g.params.iter().copied().filter(|p| !spec.has(p)));
            g.params = knobs;
            g.visual = GroupVisual::Envelope { spec, live: matches!(g.name, "Amp" | "Operator 1") };
            g.width = (g.params.len() as f32 * CELL_W).max(ENV_MIN_W);
            continue;
        }
        let l = &g.params;
        g.visual = match find(l, "Cutoff") {
            Some(cutoff) => GroupVisual::Filter {
                cutoff,
                reso: find(l, "Resonance"),
                highpass: find(l, "Type").is_some_and(|t| t.choices.iter().any(|c| c.contains("High"))),
            },
            None => GroupVisual::None,
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
    if kind == "autotune" {
        return super::autotune::WIDTH;
    }
    let g = groups(params);
    if g.is_empty() {
        return 280.0;
    }
    let body: f32 = g.iter().map(|g| g.width).sum::<f32>() + (g.len() - 1) as f32 * GROUP_GAP;
    let extra = if transfer_kind(kind) {
        TRANSFER + GROUP_GAP
    } else if kind == "piano" {
        super::grand::WIDTH + GROUP_GAP
    } else if sample_kind(kind) {
        SAMPLE_W + GROUP_GAP
    } else if super::visuals::response_kind(kind).is_some() {
        RESPONSE_W + GROUP_GAP
    } else if super::acoustic::acoustic_kind(kind) {
        super::acoustic::ACOUSTIC_W + GROUP_GAP
    } else if kind == "wavetable" {
        super::wavetable::WAVE_W + GROUP_GAP
    } else if kind == "vocoder" {
        super::vocoder::VIS_W + GROUP_GAP
    } else {
        0.0
    };
    (body + extra + 28.0).max(280.0)
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
            Device::Instrument(ch) if kind == ponzik_project::inst_rack::INST_RACK => self.layer_width(ch),
            _ => card_width(&kind, params_of(&kind, inst)).max(self.title_width(d)),
        }
    }

    /// Width a card needs for its whole name before the header's buttons (a
    /// very long plugin name still ends in an ellipsis, its tooltip shows it).
    fn title_width(&self, d: Device) -> f32 {
        let name = self.fonts.measure(self.f.sans_med, 13.0, &self.device_name(d));
        let (kind, _) = self.device_kind(d);
        let plugin = super::app::plugin_badge(&kind).is_some() && self.editors.editor(d).is_some_and(|e| e.has_gui());
        let plugin = if plugin { PLUGIN_BUTTON_W } else { 0.0 };
        FOLD_SHIFT + TITLE_X + name.min(TITLE_MAX_W) + plugin + header_controls_w(d)
    }

    pub(super) fn device_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        use super::devstrip::{ADD_W, COLLAPSED_W, ChainLayout, StripItem};
        let t = self.theme;
        let track = self.focused_track();
        let header = Rect::new(r.x + PAD, r.y + 10.0, r.w - 2.0 * PAD, 28.0);
        let chain = Rect::new(r.x + PAD, header.bottom() + 8.0, r.w - 2.0 * PAD, r.bottom() - header.bottom() - 8.0 - 12.0);

        // The wheel scrolls the panel sideways anywhere (knobs take it only
        // when it starts on them); the cards then see no wheel.
        let scrolled = self.panel_wheel(input, r, header);
        let no_wheel;
        let input = if scrolled {
            no_wheel = Input { wheel: 0.0, ..input.clone() };
            &no_wheel
        } else {
            input
        };

        // Titre : ce qu'on édite (double-clic : renommer), sa couleur et, discrètement, où ça sort.
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
        let tint = self.panel_tint();
        c.fill_rrect(Rect::new(header.x - 2.0, header.y + 6.0, 4.0, 16.0), 2.0, tint);
        let title_x = header.x + 10.0;
        let tw = self.fonts.measure(self.f.sans_med, 15.0, &title).min(header.w * 0.22);
        let title_r = Rect::new(title_x - 6.0, header.y, tw + 12.0, 28.0);
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
        self.text_fit(c, self.f.sans_med, 15.0, title_x, header.y + 14.0, &title, tw, t.text);
        let mut hx = title_x + tw + 12.0;
        if let Some(out) = out {
            draw_icon(c, Icon::Route, hx + 7.0, header.y + 14.0, 13.0, t.muted);
            let ow = self.fonts.measure(self.f.sans, 12.0, &out).min(120.0);
            self.text_fit(c, self.f.sans, 12.0, hx + 20.0, header.y + 14.0, &out, ow, t.muted);
            hx += 20.0 + ow + 12.0;
        }

        // Layout of the chain: note effects, devices (collapsed ones slim), « Add », modulators.
        let devices = self.panel_devices();
        let lead = self.note_fx_lead();
        let mut items = Vec::new();
        if lead > 0.0 {
            items.push((StripItem::Notes, 0.0, lead - CARD_GAP));
        }
        let mut x = lead;
        let mut widths = Vec::with_capacity(devices.len());
        for &d in &devices {
            let w = if self.layout.floating(d) {
                super::float::PLACEHOLDER_W
            } else if self.is_collapsed(d) {
                COLLAPSED_W
            } else {
                self.device_width(d)
            };
            items.push((StripItem::Dev(d), x, w));
            widths.push(w);
            x += w + CARD_GAP;
        }
        let add_x = x;
        let mods = self.panel_mod_count();
        if mods > 0 {
            items.push((StripItem::Mods, add_x + ADD_W + CARD_GAP * 2.0, mods as f32 * 350.0));
        }
        let total = add_x + ADD_W + CARD_GAP * 2.0 + mods as f32 * 350.0 + 80.0;
        let lay = ChainLayout { items, total };
        if let Some(item) = self.panel_reveal.take() {
            self.reveal(&lay, item);
        }

        // Right of the header: collapse all, expand all, add an effect.
        let add_label = tr("Add effect");
        let aw = self.fonts.measure(self.f.sans_med, 13.0, add_label) + 48.0;
        let add_r = Rect::new(header.right() - aw, header.y + 1.0, aw, 26.0);
        if self.lab_button(c, input, wid("dev-add-top", track, 0), add_r, Some(Icon::Plus), add_label, 0, tr("A new effect at the end of the chain\nor drag one from the browser")) {
            self.open_popup(Popup::AddEffect { track }, add_r);
        }
        let expand_r = Rect::new(add_r.x - 34.0, header.y, 28.0, 28.0);
        let collapse_r = Rect::new(expand_r.x - 30.0, header.y, 28.0, 28.0);
        if self.icon_button(c, input, wid("dev-collapse-all", 0, 0), collapse_r, Icon::Collapse, false, (tr("Collapse all the devices"), "")) {
            self.collapse_all(true);
        }
        if self.icon_button(c, input, wid("dev-expand-all", 0, 0), expand_r, Icon::Expand, false, (tr("Expand all the devices"), "")) {
            self.collapse_all(false);
        }
        // Hide the panel; a tab at the bottom of the screen brings it back.
        let hide_r = Rect::new(collapse_r.x - 34.0, header.y, 28.0, 28.0);
        let k_panel = self.key_label(super::keymap::Shortcut::TogglePanel);
        if self.icon_button(c, input, wid("dev-hide", 0, 0), hide_r, Icon::ChevronDown, false, (tr("Hide the devices"), &k_panel)) {
            self.panel = false;
        }

        // The switch between the instrument and the track's effects, then the strip.
        let sx_end = self.panel_switch(c, input, hx, header.y + 14.0, &lay);
        let strip = Rect::new(sx_end + 10.0, header.y, hide_r.x - 10.0 - sx_end - 10.0, header.h);
        self.device_strip(c, input, strip, &lay, chain.w);
        if devices.len() >= 4 && strip.w > 120.0 {
            self.want_hint(super::hints::Hint::DeviceStrip, strip);
        }
        // Ctrl+arrows (the piano roll keeps them while notes are selected).
        let piano_keys = self.view == super::app::View::Piano && !self.piano.selected.is_empty();
        if self.popup.is_none() && !piano_keys {
            self.panel_keys(input, &lay, chain.w);
        }

        // Bouton du milieu : faire glisser la chaîne.
        let over_chain = input.inside && chain.contains(input.x, input.y);
        let pan_id = wid("chain-pan", 0, 0);
        if over_chain && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.chain_scroll, 0.0));
        }
        if let Some((_, x0, _, s0, _)) = self.pan.filter(|p| p.0 == pan_id) {
            self.chain_scroll = s0 - (input.x - x0);
            self.chain_scroll = self.chain_scroll.clamp(0.0, (lay.total - chain.w).max(0.0));
            self.anim.set(wid("chain-sx", 0, 0), self.chain_scroll);
        }
        self.chain_scroll = self.chain_scroll.clamp(0.0, (lay.total - chain.w).max(0.0));
        let sx = self.anim.to(wid("chain-sx", 0, 0), self.chain_scroll, T_LAYOUT);

        self.drop_zone(input, chain, DropTarget::Track(track));
        let prev = c.push_clip(chain);
        // Frozen devices are read-only: drawn without the pointer, under frost.
        let dead = Input { x: -1.0e6, y: -1.0e6, ..Input::default() };
        let channel_lock = match self.focus {
            Focus::Channel(ch) => self.channel_lock(ch),
            _ => None,
        };
        let track_lock = self.project.is_frozen(ponzik_project::FreezeTarget::Track(track)).then_some(ponzik_project::FreezeTarget::Track(track));
        // Effets de notes du canal, avant l'instrument.
        let fx_x = chain.x - sx;
        let mut x = self.note_fx_card(c, if channel_lock.is_some() { &dead } else { input }, fx_x, chain);
        if let Some(target) = channel_lock
            && x - CARD_GAP > fx_x + 1.0
        {
            self.frost_card(c, input, Rect::new(fx_x, chain.y, x - CARD_GAP - fx_x, chain.h), target, false);
        }
        let mut button = true;
        let mut gaps = Vec::new();
        for (d, w) in devices.iter().zip(widths) {
            let card = Rect::new(x, chain.y, w, chain.h);
            let lock = self.device_lock(*d);
            if card.right() > chain.x && card.x < chain.right() {
                if self.layout.floating(*d) {
                    self.float_placeholder(c, input, card, *d);
                } else if let Some(target) = lock {
                    self.device_card(c, &dead, card, *d, false);
                    // One Unfreeze button per freeze, on its first card.
                    self.frost_card(c, input, card, target, button);
                    button = false;
                } else if self.is_collapsed(*d) {
                    self.collapsed_card(c, input, card, *d);
                } else {
                    self.device_card(c, input, card, *d, false);
                }
                if self.panel_cursor == Some(StripItem::Dev(*d)) {
                    let a = self.anim.to(wid("dev-cursor", device_key(*d), 0), 1.0, T_HOVER);
                    c.stroke_rrect(card.inset(-1.0), 15.0, 1.5, t.accent.alpha(0.7 * a));
                }
            }
            if lock.is_none() {
                button = true;
            }
            // Before each effect: room to insert one there.
            if let Device::Effect { slot, .. } = *d
                && track_lock.is_none()
            {
                gaps.push((x - CARD_GAP * 0.5, slot));
            }
            x += w + CARD_GAP;
        }
        for (gx, slot) in gaps {
            self.insert_gap(c, input, gx, chain, track, slot);
        }
        // Emplacement d'ajout : une carte nommée au bout de la chaîne.
        let add = Rect::new(x, chain.y, ADD_W, chain.h);
        let id = wid("chain-add", track, 0);
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(add, 14.0, t.panel.alpha(hv * 0.6));
        c.stroke_rrect(add, 14.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Plus, ax, ay - 12.0, 22.0, t.muted.mix(t.accent, hv));
        self.text_center(c, self.f.sans_med, 12.0, ax, ay + 16.0, add_label, t.text2.mix(t.accent, hv));
        if hov {
            self.set_tip(tr("Add an effect\nor drag one from the browser"), "");
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        if hov && input.pressed {
            self.open_popup(Popup::AddEffect { track }, add);
        }
        // Modulators of the instrument and the track (read-only when frozen).
        let locked = channel_lock.is_some() || track_lock.is_some();
        self.modulator_cards(c, if locked { &dead } else { input }, add.right() + CARD_GAP * 2.0, chain);
        c.set_clip(Some(prev));
        self.mod_drag_layer(c, input);
        // Scroll bar: where the view is in a chain wider than the panel.
        if lay.total > chain.w + 1.0 && chain.h > 40.0 {
            let w = (chain.w * chain.w / lay.total).max(40.0);
            let bx = chain.x + (chain.w - w) * (sx / (lay.total - chain.w)).clamp(0.0, 1.0);
            c.fill_rrect(Rect::new(bx, chain.bottom() + 4.0, w, 4.0), 2.0, t.text2.alpha(0.3));
        }
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
            Device::Instrument(_) => (self.device_name(device), false, t.accent),
            Device::Effect { track, slot } => {
                let bypass = self.project.tracks[track].inserts[slot].as_ref().is_some_and(|d| d.bypass);
                (self.device_name(device), bypass, pattern_color(1))
            }
            Device::RackUnit { .. } => (super::app::display_kind(&kind), false, pattern_color(1)),
        };
        // Collapse (in the panel): the card folds to its name and on/off.
        // In its color, in place of the color mark.
        let folds = !floating && !matches!(device, Device::RackUnit { .. });
        let mark = if bypass { t.muted } else { color };
        if folds {
            let fold = Rect::new(head.x + 3.0, head.y + 5.0, 26.0, 26.0);
            let id = wid("fx-fold", device_key(device), 0);
            let hov = self.hovered(input, id, fold);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(fold, 7.0, t.raised.alpha(hv));
            draw_icon(c, Icon::Collapse, fold.x + fold.w * 0.5, fold.y + fold.h * 0.5, 13.0, mark.mix(t.text, hv * 0.5));
            if hov {
                self.set_tip(tr("Collapse\nDouble-click its slim card or Expand to open it again"), "");
                self.set_cursor(ponzik_window::Cursor::Hand);
                if input.pressed {
                    self.device_op(device, super::devstrip::DeviceOp::Collapse);
                }
            }
        }
        let head = if folds { Rect::new(head.x + FOLD_SHIFT, head.y, head.w - FOLD_SHIFT, head.h) } else { head };
        if !folds {
            c.fill_rrect(Rect::new(head.x + 12.0, head.y + 12.0, 3.0, 12.0), 1.5, mark);
        }
        // CPU load of the device, share of one core.
        let cpu = match device {
            Device::Instrument(ch) => self.engine.channel_cpu(ch),
            Device::Effect { track, slot } | Device::RackUnit { track, slot, .. } => self.engine.insert_cpu(track, slot),
        };
        let cpu = (cpu >= 0.0005).then(|| ponzik_i18n::tr_fmt!("{:.1}%", cpu * 100.0));
        let has_ab = !matches!(device, Device::RackUnit { .. });
        // CLAP or VST 3 plugin: a button right after its name (visible even on
        // a very wide card) opens its own interface in a window.
        let plugin = super::app::plugin_badge(&kind).is_some() && self.editors.editor(device).is_some_and(|e| e.has_gui());
        // The name takes the room up to the header's buttons; the CPU load only
        // shows where it leaves the name whole.
        let title_w = head.w - TITLE_X - header_controls_w(device) - if plugin { PLUGIN_BUTTON_W } else { 0.0 };
        let full_w = self.fonts.measure(self.f.sans_med, 13.0, &name);
        let cpu = cpu.filter(|s| full_w + 8.0 + self.fonts.measure(self.f.mono, 9.0, s) <= title_w);
        self.text_fit(c, self.f.sans_med, 13.0, head.x + TITLE_X, head.y + 18.0, &name, title_w, if bypass { t.muted } else { t.text });
        let name_w = full_w.min(title_w);
        if full_w > title_w && input.inside && Rect::new(head.x + TITLE_X, head.y, name_w, head.h).contains(input.x, input.y) {
            self.set_tip(&name, "");
        }
        let editor = Rect::new(head.x + TITLE_X + name_w + 6.0, head.y + 5.0, 26.0, 26.0);
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
        let ab = Rect::new(float.x - AB_W - 4.0, head.y + 7.0, AB_W, 22.0);
        if has_ab {
            self.ab_buttons(c, input, ab, device);
        }
        let grip_end = if has_ab { ab.x } else { float.x };
        if let Some(cpu) = cpu {
            self.text_right(c, self.f.mono, 9.0, grip_end - 4.0, head.y + 18.0, &cpu, t.muted);
        }
        let (icon, tip) = if floating { (Icon::Dock, tr("Dock")) } else { (Icon::Detach, tr("Detach into a window")) };
        if self.icon_button(c, input, wid("fx-float", device_key(device), 0), float, icon, false, (tip, "")) {
            if floating { self.dock(device) } else { self.detach(device, Some(card)) }
        }
        // The header drags like a title bar, around the plugin button; a
        // double-click on a plugin's name opens its window (elsewhere, it
        // detaches the card as for any device).
        let grip = Rect::new(head.x, head.y, grip_end - head.x - 2.0, head.h);
        let on_name = plugin && input.inside && Rect::new(head.x, head.y, editor.x - head.x, head.h).contains(input.x, input.y);
        if on_name && input.pressed && input.double {
            self.layout.cancel_gesture();
            self.drag = None;
            self.toggle_editor(device);
        } else if plugin {
            self.float_grip(input, Rect::new(head.x, head.y, editor.x - head.x - 2.0, head.h), card, device, floating);
            let rest = Rect::new(editor.right() + 2.0, head.y, (grip_end - editor.right() - 4.0).max(0.0), head.h);
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
                self.device_op(device, super::devstrip::DeviceOp::Remove);
                return;
            }
        }

        let body = Rect::new(card.x + 14.0, head.bottom() + 2.0, card.w - 28.0, card.h - head.h - 8.0);
        if let Device::Effect { track, slot } = device
            && kind == ponzik_project::rack::RACK
        {
            return self.rack_device(c, input, body, track, slot);
        }
        if let Device::Instrument(ch) = device
            && kind == ponzik_project::inst_rack::INST_RACK
        {
            return self.layer_device(c, input, body, ch);
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
        if kind == "autotune" && matches!(device, Device::Effect { .. }) {
            return self.autotune_device(c, input, body, device);
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
        } else if kind == "piano" {
            self.grand_piano_view(c, input, Rect::new(x, body.y + 4.0, super::grand::WIDTH, body.h - 10.0), device, params);
            x += super::grand::WIDTH + GROUP_GAP;
        } else if sample_kind(&kind) {
            self.sample_view(c, input, Rect::new(x, body.y + 4.0, SAMPLE_W, body.h - 10.0), device, params);
            x += SAMPLE_W + GROUP_GAP;
        } else if super::visuals::response_kind(&kind).is_some() {
            self.response_graph(c, input, Rect::new(x, body.y + 4.0, RESPONSE_W, body.h - 10.0), device, &kind, params);
            x += RESPONSE_W + GROUP_GAP;
        } else if super::acoustic::acoustic_kind(&kind) {
            let w = super::acoustic::ACOUSTIC_W;
            self.acoustic_visual(c, input, Rect::new(x, body.y + 4.0, w, body.h - 10.0), device, &kind, params);
            x += w + GROUP_GAP;
        } else if kind == "wavetable" {
            let w = super::wavetable::WAVE_W;
            self.wavetable_visual(c, input, Rect::new(x, body.y + 4.0, w, body.h - 10.0), device, params);
            x += w + GROUP_GAP;
        } else if kind == "vocoder" {
            let w = super::vocoder::VIS_W;
            self.vocoder_visual(c, input, Rect::new(x, body.y + 4.0, w, body.h - 10.0), device, params);
            x += w + GROUP_GAP;
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
                GroupVisual::Envelope { spec, live } => self.envelope_editor(c, input, visual, device, &spec, live),
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
                // While a curve plays, the knob follows it (grabbing it starts from there).
                let target = device_target(device, p.id);
                let shown = if self.is_dragging(id) { value } else { self.live_auto_value(target).map_or(value, |v| p.clamp(v)) };
                let mut changed = if p.unit == Unit::Choice {
                    self.choice(c, input, id, cell, p, shown)
                } else if kind == "vocoder" && p.id == ponzik_engine::effects::vocoder::P_MIDI_CHANNEL {
                    // The channel whose notes the internal chord plays, by its name.
                    self.at_channel_cell(c, input, cell, device, p, tr("Channel whose notes the Saw and Pulse chords play (none: the Drone note)\nClick: next channel · right-click: none · wheel"));
                    None
                } else {
                    self.knob(c, input, id, cell, p, shown)
                };
                if let Device::RackUnit { track, slot, unit } = device {
                    self.macro_range_knob(c, cell, id, track, slot, unit, p, &mut changed, input.mods.alt);
                }
                if let Device::Instrument(ch) = device {
                    self.layer_range_knob(c, cell, id, ch, p, &mut changed, input.mods.alt);
                }
                self.mod_knob(c, input, id, cell, device, p, value, &mut changed);
                if let Some(v) = changed {
                    self.set_param_value(device, p, v);
                }
                // Clic droit : automatiser, valeur par défaut ; point sarcelle si automatisé.
                // Enter on a focused knob: type its value.
                if p.unit != Unit::Choice && self.kb_enter(id) {
                    self.open_popup(Popup::Rename { target: super::app::Rename::Value(target), text: String::new() }, Rect::new(cell.x - 60.0, cell.y + 20.0, cell.w + 120.0, 36.0));
                }
                if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                    let popup = Popup::Param { target, value, default: p.default };
                    self.open_popup(popup, Rect::new(input.x, input.y, 0.0, 0.0));
                }
                self.param_mark(c, cell, target);
                self.auto_mark(c, input, (cell.right() - 10.0, cell.y + 6.0), Rect::new(cell.x, cell.y + 8.0, cell.w, 54.0), target);
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
        if let Device::Instrument(ch) = device
            && kind == ponzik_project::inst_rack::INST_RACK
        {
            return self.save_layer_preset(ch, name);
        }
        let params = self.device_params(device);
        // A plugin's preset keeps its whole patch.
        let editor = self.editors.editor(device).filter(|e| e.is_alive());
        let state = editor.and_then(|e| e.save_state(ponzik_engine::StateContext::Preset));
        match super::compat::save_preset(&kind, name, &params, self.device_rack(device).as_ref(), state.as_deref()) {
            Ok(_) => self.toast(&tr_fmt!("Preset saved: {}", name)),
            Err(e) => self.toast(&e),
        }
    }

    /// A plugin preset: the plugin loads its state, and the values it reads
    /// back become the device's (one undo step for the values; the patch
    /// itself stays loaded). Without a live instance, the values of the file
    /// apply and the state waits for the next instance.
    pub fn apply_plugin_preset(&mut self, device: Device, params: &[(u32, f32)], state: ponzik_project::PluginState) {
        let editor = self.editors.editor(device).filter(|e| e.is_alive()).cloned();
        let values = match editor.map(|e| e.load_state(&state, ponzik_engine::StateContext::Preset)) {
            Some(Ok(read)) => {
                // Like every definition, only the values off their default.
                let (kind, inst) = self.device_kind(device);
                let infos = params_of(&kind, inst);
                read.into_iter()
                    .filter(|&(id, v)| infos.iter().find(|p| p.id == id).is_some_and(|p| (p.clamp(v) - p.default).abs() > f32::EPSILON))
                    .map(|(id, v)| (id, infos.iter().find(|p| p.id == id).map_or(v, |p| p.clamp(v))))
                    .collect()
            }
            Some(Err(e)) => {
                self.toast(&tr_fmt!("The plugin refused the preset: {}", e));
                params.to_vec()
            }
            None => params.to_vec(),
        };
        if let Some(slot) = super::editors::state_slot(&mut self.project, device) {
            *slot = Some(state);
        }
        self.apply_preset(device, &values, None);
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
            // An Instrument Rack's knob driven by a macro follows it.
            Device::Instrument(ch) => return self.project.channels.get(ch).map_or(p.default, |c| c.instrument.effective(p.id, p)),
            Device::Effect { track, slot } => self.project.tracks[track].inserts[slot].as_ref().and_then(|e| e.get(p.id)),
            Device::RackUnit { track, slot, unit } => {
                return self.project.tracks[track].inserts[slot].as_ref().map_or(p.default, |e| e.effective(unit_param(unit, p.id), p));
            }
        }
        .unwrap_or(p.default)
    }

    pub(super) fn set_param_value(&mut self, device: Device, p: &ParamInfo, v: f32) {
        if self.layer_learn_hit(device, p) {
            return;
        }
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

    /// The A/B of a device in the project (not for devices inside a rack).
    fn ab_device(&mut self, device: Device) -> Option<&mut dyn AbDevice> {
        match device {
            Device::Instrument(ch) => Some(&mut self.project.channels.get_mut(ch)?.instrument),
            Device::Effect { track, slot } => Some(self.project.tracks.get_mut(track)?.inserts.get_mut(slot)?.as_mut()?),
            Device::RackUnit { .. } => None,
        }
    }

    /// Letter in use and whether both letters are the same.
    fn ab_of(&self, device: Device) -> Option<(u8, bool)> {
        let d: &dyn AbDevice = match device {
            Device::Instrument(ch) => &self.project.channels.get(ch)?.instrument,
            Device::Effect { track, slot } => self.project.tracks.get(track)?.inserts.get(slot)?.as_ref()?,
            Device::RackUnit { .. } => return None,
        };
        Some((d.ab_active(), d.ab_same()))
    }

    /// A live plugin's state goes into the device first: it belongs to the letter left.
    fn capture_for_ab(&mut self, device: Device) {
        let state = self.editors.editor(device).filter(|e| e.is_alive()).and_then(|e| e.save_state(ponzik_engine::StateContext::Project));
        if let Some(state) = state {
            self.store_plugin_state(device, state);
        }
    }

    /// Switches a device to its other state (one undo step).
    pub fn ab_switch(&mut self, device: Device) {
        self.capture_for_ab(device);
        let Some(d) = self.ab_device(device) else { return };
        d.ab_switch();
        let letter = if d.ab_active() == 0 { "A" } else { "B" };
        self.project.prune_modulators();
        self.commit(&tr_fmt!("Compare: {}", letter));
    }

    /// Copies the state in use onto the other letter.
    pub fn ab_copy(&mut self, device: Device) {
        self.capture_for_ab(device);
        let Some(d) = self.ab_device(device) else { return };
        d.ab_copy();
        let msg = if d.ab_active() == 0 { tr("A copied to B") } else { tr("B copied to A") };
        self.commit(msg);
        self.toast(msg);
    }

    /// A and B letters (the one in use lit) and the copy button.
    fn ab_buttons(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device) {
        let t = self.theme;
        let Some((active, same)) = self.ab_of(device) else { return };
        let key = device_key(device);
        c.fill_rrect(Rect::new(r.x, r.y, 44.0, r.h), 7.0, t.bg_deep);
        for (i, letter) in ["A", "B"].into_iter().enumerate() {
            let b = Rect::new(r.x + 2.0 + i as f32 * 20.0, r.y + 2.0, 20.0, r.h - 4.0);
            let id = wid("fx-ab", key, i);
            // Small letters, a taller target: the whole height of the header.
            let hov = self.hovered(input, id, Rect::new(b.x, b.y - 7.0, b.w, b.h + 14.0));
            let hv = self.hover_anim(id, hov);
            let lit = active as usize == i;
            let on = self.anim.to(id ^ 0x4f4e, lit as u8 as f32, T_HOVER);
            if on > 0.01 {
                c.fill_rrect(b, 5.0, t.accent.alpha(on));
            }
            let col = t.muted.mix(t.text, hv).mix(t.on_accent, on);
            self.text_center(c, self.f.mono_med, 11.0, b.x + b.w * 0.5, b.y + b.h * 0.5, letter, col);
            if hov {
                let tip = if lit {
                    tr("A/B: the state in use")
                } else if i == 1 {
                    tr("A/B: switch to B\nThe first time, B starts as a copy of A")
                } else {
                    tr("A/B: switch back to A")
                };
                self.set_tip(tip, "");
                if input.pressed && !lit {
                    self.ab_switch(device);
                }
            }
        }
        let copy = Rect::new(r.x + 46.0, r.y, 20.0, r.h);
        let tip = if active == 0 { tr("Copy A to B") } else { tr("Copy B to A") };
        let id = wid("fx-ab-copy", key, 0);
        let hov = self.hovered(input, id, Rect::new(copy.x, copy.y - 6.0, copy.w + 2.0, copy.h + 12.0));
        let hv = self.hover_anim(id, hov);
        let alpha = if same { 0.5 } else { 1.0 };
        draw_icon(c, Icon::Duplicate, copy.x + copy.w * 0.5, copy.y + copy.h * 0.5, 13.0, t.muted.mix(t.text, hv).alpha(alpha));
        if hov {
            self.set_tip(tip, "");
            if input.pressed {
                self.ab_copy(device);
            }
        }
    }

    /// Screenshot setup (`PONZIK_AB=1`): the panel's first device compared,
    /// on B with a knob moved.
    pub fn screenshot_ab(&mut self) {
        let Some(&device) = self.panel_devices().first() else { return };
        self.ab_switch(device);
        let (kind, inst) = self.device_kind(device);
        if let Some(p) = params_of(&kind, inst).first() {
            let v = self.param_value(device, p);
            self.set_param_value(device, p, v + (p.max - p.min) * 0.3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every native device's name fits whole in its card's header, in every
    /// shipped language: the card widens up to `TITLE_MAX_W` for it.
    #[test]
    fn device_names_fit_their_header() {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let app = App::new(handle, project, 48_000);
        let names = registry::effects().iter().map(|k| k.name).chain(registry::instruments().iter().map(|k| k.name));
        for name in names {
            for lang in ponzik_i18n::Lang::all() {
                let w = ponzik_i18n::with_lang(lang, || app.fonts.measure(app.f.sans_med, 13.0, tr(name)));
                assert!(w <= TITLE_MAX_W, "{name} ({}) : {w}", lang.code());
            }
        }
    }
}
