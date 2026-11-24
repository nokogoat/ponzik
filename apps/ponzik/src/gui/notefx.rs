//! Effets de notes du canal (arpège, accord, gamme, hasard, répétition) : une
//! carte avant l'instrument, quatre emplacements et, pour l'effet choisi, un
//! visuel calculé par l'effet lui-même. Et la boîte à rythmes : huit pads qui
//! s'allument quand ils jouent.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::{
    NOTE_FX_SLOTS, Note, NoteBuffer, NoteEvent, NoteFx, PPQ, ParamInfo, PlayMode, TICKS_PER_BAR, Unit, registry,
};
use ponzik_i18n::{tr, tr_ctx, tr_fmt, tr_noop};
use ponzik_project::{NoteFxDef, Project};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{App, Popup};
use super::devices::{Device, device_target};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const CHIP_W: f32 = 44.0;
const CHIP_H: f32 = 34.0;
const CHIP_GAP: f32 = 6.0;
const CELL_W: f32 = 72.0;
/// Les choix (modes, gammes) ont besoin de place pour se lire en entier.
const CHOICE_W: f32 = 116.0;
const CELL_H: f32 = 76.0;
const GAP: f32 = 18.0;
const INSET: f32 = 14.0;
/// Écart entre deux cartes de la chaîne (celui du panneau d'appareils).
const CARD_GAP: f32 = 10.0;

thread_local! {
    /// Emplacement d'effet de notes affiché, par canal.
    static SELECTED: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    /// Pad de boîte à rythmes affiché, par canal.
    static PAD: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
}

/// Pictogramme d'un type d'effet de notes.
pub fn icon(kind: &str) -> Icon {
    match kind {
        "arpeggiator" => Icon::Arpeggio,
        "chord" => Icon::Chord,
        "scale" => Icon::Scale,
        "random" => Icon::Dice,
        "repeat" => Icon::Repeat,
        _ => Icon::Effect,
    }
}

thread_local! {
    /// Parameters of the plugin note effects (LV2 MIDI effects), read once per kind.
    static PLUGIN_PARAMS: RefCell<HashMap<String, &'static [ParamInfo]>> = RefCell::new(HashMap::new());
}

pub(super) fn fx_params(kind: &str) -> &'static [ParamInfo] {
    if let Some(f) = registry::create_note_fx(kind) {
        return f.params();
    }
    PLUGIN_PARAMS.with(|c| {
        *c.borrow_mut().entry(kind.to_string()).or_insert_with(|| {
            ponzik_project::plugins::note_fx_from_kind(kind).and_then(Result::ok).map_or(&[], |f| f.params())
        })
    })
}

/// A note effect kind Ponzik can add: native, or an LV2 MIDI effect.
fn is_note_fx(kind: &str) -> bool {
    use ponzik_project::plugins::Format;
    registry::note_fx_kind(kind).is_some() || Format::of_kind(kind) == Some(Format::Lv2)
}

fn value(def: &NoteFxDef, p: &ParamInfo) -> f32 {
    def.get(p.id).unwrap_or(p.default)
}

/// Colonnes de réglages (deux par colonne) : abscisse relative et largeur de chacun.
fn columns(params: &[&ParamInfo]) -> (Vec<(f32, f32)>, f32) {
    let width = |p: &ParamInfo| {
        if p.unit == Unit::Choice { CHOICE_W } else { CELL_W }
    };
    let mut out = Vec::new();
    let mut x = 0.0;
    for pair in params.chunks(2) {
        let w = pair.iter().map(|p| width(p)).fold(0.0, f32::max);
        out.extend(pair.iter().map(|_| (x, w)));
        x += w;
    }
    (out, x)
}

/// Réglages montrés : les intervalles libres de l'accord n'apparaissent qu'en « Libre ».
fn shown(def: &NoteFxDef) -> Vec<&'static ParamInfo> {
    let params = fx_params(&def.kind);
    let free = def.kind == "chord" && params.first().is_some_and(|p| value(def, p) < 0.5);
    params.iter().filter(|p| p.group != "Free" || free).collect()
}

fn visual_w(kind: &str) -> f32 {
    match kind {
        "chord" => 132.0,
        "scale" => 236.0,
        _ => 216.0,
    }
}

/// Fréquence d'échantillonnage où un échantillon vaut un tick : les simulations
/// se lisent directement sur la grille.
fn tick_rate(bpm: f32) -> f32 {
    bpm * PPQ as f32 / 60.0
}

/// Joue `input` (instant, événement), triés, dans l'effet comme le fait le
/// moteur et renvoie ce qu'il émet avant `until`.
fn simulate(fx: &mut dyn NoteFx, input: &[(u64, NoteEvent)], until: u64) -> Vec<(u64, NoteEvent)> {
    let mut out = NoteBuffer::default();
    let mut log = Vec::new();
    let (mut now, mut elapsed, mut next_in) = (0, 0, 0);
    for _ in 0..4_096 {
        out.clear();
        fx.advance(elapsed);
        while let Some(&(_, ev)) = input.get(next_in).filter(|(at, _)| *at <= now) {
            if ev.on {
                fx.note_on(ev.key, ev.vel, &mut out);
            } else {
                fx.note_off(ev.key, &mut out);
            }
            next_in += 1;
        }
        fx.tick(&mut out);
        if now >= until {
            break;
        }
        log.extend(out.as_slice().iter().map(|&e| (now, e)));
        let mut next = until;
        if let Some(&(at, _)) = input.get(next_in) {
            next = next.min(at);
        }
        if let Some(w) = fx.next_event() {
            next = next.min(now + w.max(1) as u64);
        }
        elapsed = (next - now) as u32;
        now = next;
    }
    log
}

/// Notes émises (début, fin, note, vélocité) ; une note jamais arrêtée finit à `until`.
fn spans(log: &[(u64, NoteEvent)], until: u64) -> Vec<(u64, u64, u8, f32)> {
    let mut out: Vec<(u64, u64, u8, f32)> = Vec::new();
    for &(at, e) in log {
        if e.on {
            out.push((at, until, e.key, e.vel));
        } else if let Some(n) = out.iter_mut().find(|n| n.2 == e.key && n.1 == until && n.0 <= at) {
            n.1 = at;
        }
    }
    out
}

const NOTE_NAMES: [&str; 12] = [
    tr_noop("C"),
    tr_noop("C♯"),
    tr_noop("D"),
    tr_noop("E♭"),
    tr_noop("E"),
    tr_noop("F"),
    tr_noop("F♯"),
    tr_noop("G"),
    tr_noop("A♭"),
    tr_noop("A"),
    tr_noop("B♭"),
    tr_noop("B"),
];

impl App {
    fn note_fx_def(&self, ch: usize, slot: usize) -> Option<&NoteFxDef> {
        self.project.channels.get(ch)?.note_fx.get(slot)?.as_ref()
    }

    /// Emplacement affiché : le dernier choisi s'il est occupé, sinon le premier occupé.
    fn note_fx_selected(&self, ch: usize) -> Option<usize> {
        let picked = SELECTED.with(|s| s.borrow().get(&ch).copied());
        picked
            .filter(|&s| self.note_fx_def(ch, s).is_some())
            .or_else(|| (0..NOTE_FX_SLOTS).find(|&s| self.note_fx_def(ch, s).is_some()))
    }

    /// Shown name of a note effect kind (a plugin's own name).
    pub(super) fn note_fx_name(&self, kind: &str) -> String {
        match registry::note_fx_kind(kind) {
            Some(k) => tr(k.name).to_string(),
            None => self.plugin_name(kind).unwrap_or_else(|| super::app::display_kind(kind)),
        }
    }

    pub fn add_note_fx(&mut self, ch: usize, slot: usize, kind: &str) {
        if !is_note_fx(kind) || ch >= self.project.channels.len() || slot >= NOTE_FX_SLOTS {
            return;
        }
        let slots = &mut self.project.channels[ch].note_fx;
        if slots.len() <= slot {
            slots.resize(slot + 1, None);
        }
        slots[slot] = Some(NoteFxDef::new(kind));
        SELECTED.with(|s| s.borrow_mut().insert(ch, slot));
        self.commit(tr("Add a note effect"));
    }

    fn remove_note_fx(&mut self, ch: usize, slot: usize) {
        let slots = &mut self.project.channels[ch].note_fx;
        if let Some(s) = slots.get_mut(slot) {
            *s = None;
        }
        while slots.last().is_some_and(Option::is_none) {
            slots.pop();
        }
        self.commit(tr("Remove a note effect"));
    }

    /// Largeur cible de la carte d'effets de notes.
    fn note_fx_target_w(&self, ch: usize) -> f32 {
        let column = INSET + CHIP_W + INSET;
        match self.note_fx_selected(ch).and_then(|s| self.note_fx_def(ch, s)) {
            Some(def) => column + visual_w(&def.kind) + GAP * 0.5 + columns(&shown(def)).1 + INSET,
            None => column + 4.0,
        }
    }

    /// Place prise par la carte d'effets de notes en tête de chaîne (animée), 0 hors canal.
    pub(super) fn note_fx_lead(&mut self) -> f32 {
        let Some(ch) = self.selected_channel() else {
            return 0.0;
        };
        let target = self.note_fx_target_w(ch);
        self.anim.to(wid("nfx-w", ch, 0), target, T_LAYOUT) + CARD_GAP
    }

    /// Dessine la carte d'effets de notes à `x` dans la chaîne ; renvoie l'abscisse suivante.
    pub(super) fn note_fx_card(&mut self, c: &mut Canvas, input: &Input, x: f32, chain: Rect) -> f32 {
        let Some(ch) = self.selected_channel() else {
            return x;
        };
        let w = self.anim.get(wid("nfx-w", ch, 0)).unwrap_or_else(|| self.note_fx_target_w(ch));
        let card = Rect::new(x, chain.y, w, chain.h);
        if card.right() > chain.x && card.x < chain.right() {
            let prev = c.push_clip(card);
            self.note_fx_body(c, input, card, ch);
            c.set_clip(Some(prev));
        }
        x + w + CARD_GAP
    }

    fn note_fx_body(&mut self, c: &mut Canvas, input: &Input, card: Rect, ch: usize) {
        let t = self.theme;
        c.fill_rrect(card, 14.0, t.panel);
        c.stroke_rrect(card, 14.0, 1.0, t.hairline);
        let selected = self.note_fx_selected(ch);
        let def = selected.and_then(|s| self.note_fx_def(ch, s)).cloned();

        // Titre : l'effet affiché.
        let head_y = card.y + 18.0;
        c.fill_rrect(
            Rect::new(card.x + 12.0, card.y + 12.0, 3.0, 12.0),
            1.5,
            if def.is_some() { t.select } else { t.muted },
        );
        let title = match &def {
            Some(d) => self.note_fx_name(&d.kind),
            None => tr("Notes").to_string(),
        };
        self.text_fit(c, self.f.sans_med, 13.0, card.x + 22.0, head_y, &title, card.w - 34.0, t.text);

        // Emplacements, de haut en bas dans l'ordre de la chaîne.
        let col_x = card.x + INSET;
        let col_y = card.y + 36.0;
        let sel_y = self.anim.to(wid("nfx-sel", ch, 0), selected.unwrap_or(0) as f32, T_LAYOUT);
        if selected.is_some() {
            let r = Rect::new(col_x, col_y + sel_y * (CHIP_H + CHIP_GAP), CHIP_W, CHIP_H);
            c.fill_rrect(r, 9.0, t.select.alpha(0.14));
            c.stroke_rrect(r, 9.0, 1.0, t.select.alpha(0.7));
        }
        let first_empty = (0..NOTE_FX_SLOTS).find(|&s| self.note_fx_def(ch, s).is_none());
        for slot in 0..NOTE_FX_SLOTS {
            let r = Rect::new(col_x, col_y + slot as f32 * (CHIP_H + CHIP_GAP), CHIP_W, CHIP_H);
            let id = wid("nfx-chip", ch, slot);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let kind = self.note_fx_def(ch, slot).map(|d| d.kind.clone());
            let (cx, cy) = r.center();
            match kind {
                Some(kind) => {
                    let on = selected == Some(slot);
                    if !on {
                        c.fill_rrect(r, 9.0, t.raised.mix(t.raised_hi, hv));
                    }
                    let col = if on { t.select } else { t.text2.mix(t.text, hv) };
                    draw_icon(c, icon(&kind), cx, cy, 17.0, col);
                    if hov {
                        let name = self.note_fx_name(&kind);
                        self.set_tip(&tr_fmt!("{}\nRight-click: remove", name.as_str()), "");
                        if input.pressed {
                            SELECTED.with(|s| s.borrow_mut().insert(ch, slot));
                        }
                    }
                    // Clic droit maintenu : retire tout ce que l'on balaie, comme dans FL.
                    if hov && input.right_down && self.drag.is_none() {
                        self.remove_note_fx(ch, slot);
                    }
                }
                None if first_empty == Some(slot) => {
                    c.fill_rrect(r, 9.0, t.raised.alpha(hv));
                    c.stroke_rrect(r, 9.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
                    draw_icon(c, Icon::Plus, cx, cy, 15.0, t.muted.mix(t.accent, hv));
                    if hov {
                        self.set_tip(tr("Add a note effect\nArpeggio, chord, scale… before the instrument"), "");
                        if input.pressed {
                            self.open_popup(Popup::AddNoteFx { channel: ch, slot }, r);
                        }
                    }
                }
                None => c.fill_circle(cx, cy, 2.0, t.hairline.alpha(2.0)),
            }
        }

        let (Some(slot), Some(def)) = (selected, def) else {
            return;
        };
        let body_x = col_x + CHIP_W + INSET;
        c.fill_rect(Rect::new(body_x - INSET * 0.5, card.y + 44.0, 1.0, card.h - 58.0), t.hairline);
        let vis = Rect::new(body_x + 4.0, card.y + 40.0, visual_w(&def.kind) - 8.0, card.h - 52.0);
        let key = ch * NOTE_FX_SLOTS + slot;
        match def.kind.as_str() {
            "arpeggiator" => self.arp_visual(c, vis, ch, &def, key),
            "chord" => self.chord_visual(c, vis, ch, &def, key),
            "scale" => self.scale_visual(c, input, vis, &def, key),
            "random" => self.random_visual(c, vis, &def, key),
            "repeat" => self.repeat_visual(c, vis, &def, key),
            _ => {}
        }

        // Réglages, sur deux rangées.
        let px = body_x + visual_w(&def.kind) + GAP * 0.5;
        let params = shown(&def);
        let (cols, _) = columns(&params);
        for (k, (p, (cx, cw))) in params.into_iter().zip(cols).enumerate() {
            let cell = Rect::new(px + cx, card.y + 40.0 + (k % 2) as f32 * CELL_H, cw, CELL_H);
            let v = value(&def, p);
            let id = wid("nfx-prm", key, p.id as usize);
            let changed = if p.unit == Unit::Choice {
                self.choice(c, input, id, cell, p, v)
            } else {
                self.knob(c, input, id, cell, p, v)
            };
            if let Some(nv) = changed {
                let nv = p.clamp(nv);
                if (nv - v).abs() > f32::EPSILON
                    && let Some(Some(d)) = self.project.channels[ch].note_fx.get_mut(slot)
                {
                    d.set(p.id, nv);
                    self.commit(&tr_fmt!("Setting: {}", tr(p.name)));
                }
            }
        }
    }

    /// Fond commun des visuels : un puits sombre.
    fn well(&self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
    }

    /// Effet tout neuf, réglé comme dans le projet, cadencé au tick.
    fn sim_fx(&self, def: &NoteFxDef) -> Option<Box<dyn NoteFx>> {
        Project::make_note_fx(def, tick_rate(self.project.bpm), self.project.bpm)
    }

    /// Position de lecture rapportée à une boucle de `len` ticks, si on joue.
    fn loop_phase(&self, len: u64) -> Option<f64> {
        (self.playing && len > 0).then(|| self.position.rem_euclid(len as f64))
    }

    /// Arpège : grille de notes calculée par l'arpégiateur sur l'accord tenu
    /// (les touches enfoncées sur ce canal, sinon un accord parfait).
    fn arp_visual(&mut self, c: &mut Canvas, r: Rect, ch: usize, def: &NoteFxDef, key: usize) {
        let t = self.theme;
        self.well(c, r);
        let Some(mut fx) = self.sim_fx(def) else {
            return;
        };
        let root = self.project.channels[ch].root_note.clamp(24, 96);
        let mut held: Vec<u8> = self.held.iter().filter(|h| h.channel == ch).map(|h| h.key).collect();
        if held.is_empty() {
            held = vec![root, root + 4, root + 7];
        }
        let until = TICKS_PER_BAR as u64;
        let input: Vec<(u64, NoteEvent)> = held.iter().map(|&k| (0, NoteEvent::on(k, 0.8))).collect();
        let notes = spans(&simulate(fx.as_mut(), &input, until), until);
        let mut keys: Vec<u8> = notes.iter().map(|n| n.2).collect();
        keys.sort_unstable();
        keys.dedup();
        let g = r.inset_xy(10.0, 10.0);
        let rows = keys.len().max(1) as f32;
        let row_h = (g.h / rows).min(14.0);
        let top = g.y + (g.h - row_h * rows) * 0.5;
        // Temps de la mesure.
        for beat in 1..4 {
            let x = (g.x + g.w * beat as f32 / 4.0).round();
            c.fill_rect(Rect::new(x, g.y, 1.0, g.h), t.hairline);
        }
        let phase = self.loop_phase(until);
        if let Some(p) = phase {
            let x = g.x + g.w * (p / until as f64) as f32;
            c.fill_rect(Rect::new(x, g.y, 1.5, g.h), t.select.alpha(0.6));
        }
        for (i, &(start, end, k, vel)) in notes.iter().enumerate().take(64) {
            let row = keys.iter().position(|&x| x == k).unwrap_or(0);
            let y = self.anim.to(wid("arp-y", key, i), top + (rows - 1.0 - row as f32) * row_h, T_LAYOUT);
            let x0 = g.x + g.w * start as f32 / until as f32;
            let x1 = g.x + g.w * end.min(until) as f32 / until as f32;
            let w = self.anim.to(wid("arp-w", key, i), (x1 - x0 - 1.0).max(2.0), T_LAYOUT);
            let lit = phase.is_some_and(|p| p >= start as f64 && p < end as f64);
            let col = if lit { t.accent_hot } else { t.accent.alpha(0.45 + 0.55 * vel) };
            c.fill_rrect(Rect::new(x0, y + 1.0, w, row_h - 2.0), 2.5, col);
        }
    }

    /// Accord : les notes empilées au-dessus (ou au-dessous) de la note jouée.
    fn chord_visual(&mut self, c: &mut Canvas, r: Rect, ch: usize, def: &NoteFxDef, key: usize) {
        let t = self.theme;
        self.well(c, r);
        let Some(mut fx) = self.sim_fx(def) else {
            return;
        };
        let root = self.project.channels[ch].root_note.clamp(24, 96);
        let mut out = NoteBuffer::default();
        fx.note_on(root, 1.0, &mut out);
        let mut notes: Vec<(i32, f32)> =
            out.as_slice().iter().filter(|e| e.on).map(|e| (e.key as i32 - root as i32, e.vel)).collect();
        notes.sort_by_key(|n| n.0);
        notes.dedup_by_key(|n| n.0);
        let lo = notes.iter().map(|n| n.0).min().unwrap_or(0).min(0);
        let hi = notes.iter().map(|n| n.0).max().unwrap_or(0).max(12);
        let g = r.inset_xy(8.0, 10.0);
        let span = (hi - lo + 1) as f32;
        let step = g.h / span;
        let y_of = |semi: i32| g.bottom() - (semi - lo + 1) as f32 * step;
        // Clavier vertical : touches noires sur fond ivoire, touches qui sonnent en vert.
        let strip = Rect::new(g.x, y_of(hi), 16.0, (hi - lo + 1) as f32 * step);
        c.fill_rrect(strip, 3.0, t.text2.alpha(0.55));
        for s in lo..=hi {
            let y = y_of(s);
            let pc = (root as i32 + s).rem_euclid(12);
            if matches!(pc, 1 | 3 | 6 | 8 | 10) {
                c.fill_rect(Rect::new(strip.x, y + step * 0.15, strip.w * 0.6, step * 0.7), t.well);
            } else if matches!(pc, 0 | 5) && s > lo {
                c.fill_rect(Rect::new(strip.x, (y + step).round(), strip.w, 1.0), t.well.alpha(0.6));
            }
            if notes.iter().any(|n| n.0 == s) {
                c.fill_rect(Rect::new(strip.x + strip.w * 0.6, y + 1.0, strip.w * 0.4, step - 2.0), t.accent);
            }
        }
        let bx = strip.right() + 8.0;
        let bw = g.w - strip.w - 8.0 - 30.0;
        for (i, &(semi, vel)) in notes.iter().enumerate() {
            let y = self.anim.to(wid("chord-y", key, i), y_of(semi), T_LAYOUT);
            let h = step.clamp(4.0, 12.0);
            let yc = y + step * 0.5;
            let col = if semi == 0 { t.text } else { t.accent.alpha(0.4 + 0.6 * vel) };
            c.fill_rrect(Rect::new(bx, yc - h * 0.5, bw, h), 3.0, col);
            let label = if semi == 0 { tr(NOTE_NAMES[root as usize % 12]).to_string() } else { format!("{semi:+}") };
            self.text(c, self.f.mono, 10.0, bx + bw + 6.0, yc, &label, if semi == 0 { t.text2 } else { t.accent });
        }
    }

    /// Gamme : un clavier d'une octave, notes gardées en vert ; les autres
    /// montrent où elles partent (flèche) ou qu'elles sont écartées (croix).
    fn scale_visual(&mut self, c: &mut Canvas, input: &Input, r: Rect, def: &NoteFxDef, key: usize) {
        let t = self.theme;
        self.well(c, r);
        let Some(mut fx) = self.sim_fx(def) else {
            return;
        };
        let base = 60u8;
        let mut dest = [None; 12];
        for (pc, d) in dest.iter_mut().enumerate() {
            let mut out = NoteBuffer::default();
            fx.note_on(base + pc as u8, 1.0, &mut out);
            *d = out.as_slice().iter().find(|e| e.on).map(|e| e.key as i32 - base as i32);
            fx.note_off(base + pc as u8, &mut out);
        }
        let root = fx_params("scale").first().map_or(0, |p| value(def, p).round() as i32);
        let kb = r.inset_xy(10.0, 12.0);
        let ww = kb.w / 7.0;
        const WHITE_PC: [usize; 7] = [0, 2, 4, 5, 7, 9, 11];
        const BLACK: [(usize, f32); 5] = [(1, 1.0), (3, 2.0), (6, 4.0), (8, 5.0), (10, 6.0)];
        let mut rects = [Rect::new(0.0, 0.0, 0.0, 0.0); 12];
        for (i, &pc) in WHITE_PC.iter().enumerate() {
            rects[pc] = Rect::new(kb.x + i as f32 * ww + 1.0, kb.y, ww - 2.0, kb.h);
        }
        for &(pc, at) in &BLACK {
            rects[pc] = Rect::new(kb.x + at * ww - ww * 0.32, kb.y, ww * 0.64, kb.h * 0.58);
        }
        let draw_key = |app: &mut App, c: &mut Canvas, pc: usize, black: bool| {
            let kept = dest[pc] == Some(pc as i32);
            let lit = app.anim.to(wid("scale-k", key, pc), kept as u8 as f32, T_LAYOUT);
            let rk = rects[pc];
            let off = if black { t.bg_deep } else { t.raised_hi };
            let on = if black { t.accent_deep } else { t.accent.mix(t.raised_hi, 0.2) };
            c.fill_rrect(rk, 4.0, off.mix(on, lit));
            if black {
                c.stroke_rrect(rk, 4.0, 1.0, t.well);
            }
            let (cx, _) = rk.center();
            let cy = rk.bottom() - 12.0;
            let mark = if lit > 0.5 { t.on_accent } else { t.text2 };
            if pc as i32 == root.rem_euclid(12) {
                c.fill_circle(cx, rk.y + 9.0, 2.5, mark);
            }
            match dest[pc] {
                Some(d) if d != pc as i32 => {
                    let icon = if d > pc as i32 { Icon::ChevronRight } else { Icon::ChevronLeft };
                    draw_icon(c, icon, cx, cy, 11.0, mark.alpha(0.8));
                }
                None => {
                    c.stroke_polyline(&[(cx - 3.0, cy - 3.0), (cx + 3.0, cy + 3.0)], 1.4, t.negative.alpha(0.8));
                    c.stroke_polyline(&[(cx - 3.0, cy + 3.0), (cx + 3.0, cy - 3.0)], 1.4, t.negative.alpha(0.8));
                }
                _ => {}
            }
        };
        for &pc in &WHITE_PC {
            draw_key(self, c, pc, false);
        }
        for &(pc, _) in &BLACK {
            draw_key(self, c, pc, true);
        }
        // Survol d'une touche : ce qu'elle devient.
        let hovered =
            BLACK.iter().map(|b| b.0).chain(WHITE_PC).find(|&pc| input.inside && rects[pc].contains(input.x, input.y));
        if let Some(pc) = hovered {
            let tip = match dest[pc] {
                Some(d) if d == pc as i32 => tr_fmt!("{}: in the scale", tr(NOTE_NAMES[pc])),
                Some(d) => format!("{} → {}", tr(NOTE_NAMES[pc]), tr(NOTE_NAMES[d.rem_euclid(12) as usize])),
                None => tr_fmt!("{}: dropped", tr(NOTE_NAMES[pc])),
            };
            self.set_tip(&tip, "");
        }
    }

    /// Aléatoire : seize doubles-croches passées dans l'effet ; hauteur =
    /// vélocité, décalage = retard, cercle vide = note écartée.
    fn random_visual(&mut self, c: &mut Canvas, r: Rect, def: &NoteFxDef, key: usize) {
        let t = self.theme;
        self.well(c, r);
        let Some(mut fx) = self.sim_fx(def) else {
            return;
        };
        let step = (TICKS_PER_BAR / 16) as u64;
        let until = TICKS_PER_BAR as u64;
        let input: Vec<(u64, NoteEvent)> = (0..16u64)
            .flat_map(|i| [(i * step, NoteEvent::on(60, 0.7)), (i * step + step / 2, NoteEvent::off(60))])
            .collect();
        let notes = spans(&simulate(fx.as_mut(), &input, until + step), until + step);
        let g = r.inset_xy(12.0, 12.0);
        let col_w = g.w / 16.0;
        let base = g.bottom();
        c.fill_rect(Rect::new(g.x, base, g.w, 1.0), t.hairline);
        let phase = self.loop_phase(until);
        for i in 0..16u64 {
            let x = g.x + (i as f32 + 0.5) * col_w;
            let hit = notes.iter().find(|n| n.0 >= i * step && n.0 < (i + 1) * step);
            match hit {
                Some(&(start, _, _, vel)) => {
                    let dx = (start - i * step) as f32 / until as f32 * g.w;
                    let x = self.anim.to(wid("rnd-x", key, i as usize), x + dx, T_LAYOUT);
                    let h = self.anim.to(wid("rnd-h", key, i as usize), vel * (g.h - 6.0), T_LAYOUT);
                    let lit = phase.is_some_and(|p| p >= start as f64 && p < (start + step) as f64);
                    let col = if lit { t.accent_hot } else { t.accent };
                    c.fill_rect(Rect::new(x - 0.5, base - h, 1.5, h), col.alpha(0.6));
                    c.fill_circle(x, base - h, 3.5, col);
                }
                None => c.stroke_circle(x, base - 6.0, 3.0, 1.2, t.muted),
            }
        }
    }

    /// Répétition : une note tenue un temps, et les coups que l'effet en tire.
    fn repeat_visual(&mut self, c: &mut Canvas, r: Rect, def: &NoteFxDef, key: usize) {
        let t = self.theme;
        self.well(c, r);
        let Some(mut fx) = self.sim_fx(def) else {
            return;
        };
        let until = TICKS_PER_BAR as u64 / 2;
        let held = until / 2;
        let input = [(0, NoteEvent::on(60, 1.0)), (held, NoteEvent::off(60))];
        let notes = spans(&simulate(fx.as_mut(), &input, until), until);
        let g = r.inset_xy(12.0, 12.0);
        let x_of = |tick: u64| g.x + g.w * tick as f32 / until as f32;
        // La note jouée, en haut.
        c.fill_rrect(Rect::new(g.x, g.y, x_of(held) - g.x, 6.0), 3.0, t.text2.alpha(0.5));
        let bars = Rect::new(g.x, g.y + 14.0, g.w, g.h - 14.0);
        let phase = self.loop_phase(until);
        for (i, &(start, end, _, vel)) in notes.iter().enumerate().take(64) {
            let h = self.anim.to(wid("rep-h", key, i), vel * bars.h, T_LAYOUT);
            let (x0, x1) = (x_of(start), x_of(end));
            let lit = phase.is_some_and(|p| p >= start as f64 && p < end as f64);
            let col = if lit { t.accent_hot } else { t.accent.alpha(0.5 + 0.5 * vel) };
            c.fill_rrect(Rect::new(x0, bars.bottom() - h, (x1 - x0 - 1.0).max(2.0), h), 2.5, col);
        }
    }
}

// --- Boîte à rythmes ---

const PADS: usize = 8;
/// Note du premier pad (General MIDI).
const FIRST_PAD_NOTE: u8 = 36;
/// Nom court d'un pad, sur le pad lui-même.
fn pad_name(pad: usize) -> &'static str {
    match pad {
        0 => tr_ctx("drum pad", "Kick"),
        1 => tr_ctx("drum pad", "Rim"),
        2 => tr_ctx("drum pad", "Snare"),
        3 => tr_ctx("drum pad", "Clap"),
        4 => tr_ctx("drum pad", "Open hat"),
        5 => tr_ctx("drum pad", "Low tom"),
        6 => tr_ctx("drum pad", "Closed hat"),
        _ => tr_ctx("drum pad", "High tom"),
    }
}
const PAD_W: f32 = 66.0;
const PAD_H: f32 = 62.0;
const PAD_GAP: f32 = 8.0;
const PADS_W: f32 = 4.0 * PAD_W + 3.0 * PAD_GAP;
/// Largeur de la carte de la boîte à rythmes.
pub const DRUM_W: f32 = 28.0 + PADS_W + GAP + 3.0 * CELL_W;

/// Groupe (nom du pad) d'un paramètre.
fn group(params: &[ParamInfo], id: u32) -> &'static str {
    params.iter().find(|p| p.id == id).map_or("", |p| p.group)
}

impl App {
    /// Éclat de chaque pad (notes 36 et suivantes) : la vélocité à la frappe,
    /// puis décroissance rapide.
    pub(super) fn pad_hits<const N: usize>(&self, ch: usize) -> [f32; N] {
        let mut out = [0.0f32; N];
        let pad_of = |key: u8| key.checked_sub(FIRST_PAD_NOTE).map(usize::from).filter(|&p| p < N);
        for h in self.held.iter().filter(|h| h.channel == ch) {
            if let Some(p) = pad_of(h.key) {
                out[p] = h.vel.max(0.3);
            }
        }
        if let Some((c, k)) = self.preview
            && c == ch
            && let Some(p) = pad_of(k)
        {
            out[p] = 1.0;
        }
        if !self.playing {
            return out;
        }
        let secs_per_tick = 60.0 / (self.project.bpm.max(1.0) as f64 * PPQ as f64);
        let mut scan = |notes: &[Note], local: f64| {
            for n in notes {
                let dt = local - n.start as f64;
                if let Some(p) = pad_of(n.key)
                    && (0.0..PPQ as f64).contains(&dt)
                {
                    out[p] = out[p].max((-(dt * secs_per_tick) / 0.12).exp() as f32 * n.vel.max(0.3));
                }
            }
        };
        let pattern_notes = |p: usize| self.project.patterns.get(p).and_then(|p| p.notes.get(ch));
        match self.project.mode {
            PlayMode::Pattern(p) => {
                if let Some(notes) = pattern_notes(p) {
                    scan(notes, self.cycle_position());
                }
            }
            PlayMode::Song => {
                for clip in &self.project.clips {
                    let local = self.position - clip.start as f64;
                    if (0.0..clip.len as f64).contains(&local)
                        && let Some(notes) = pattern_notes(clip.pattern)
                    {
                        scan(notes, local + clip.offset as f64);
                    }
                }
            }
            PlayMode::Session => {}
        }
        out
    }

    /// Carte de la boîte à rythmes : huit pads (clic : choisir et écouter) et
    /// les réglages du pad choisi.
    pub(super) fn drum_device(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        body: Rect,
        device: Device,
        params: &'static [ParamInfo],
    ) {
        let t = self.theme;
        let Device::Instrument(ch) = device else {
            return;
        };
        let sel = PAD.with(|p| p.borrow().get(&ch).copied()).unwrap_or(0);
        let hits = self.pad_hits::<PADS>(ch);
        let find = |id: u32| params.iter().find(|p| p.id == id);
        let grid_y = body.y + 16.0;
        let sel_x = self.anim.to(wid("dm-sel-x", ch, 0), (sel % 4) as f32, T_LAYOUT);
        let sel_y = self.anim.to(wid("dm-sel-y", ch, 0), (sel / 4) as f32, T_LAYOUT);
        for (pad, &hit) in hits.iter().enumerate() {
            let r = Rect::new(
                body.x + (pad % 4) as f32 * (PAD_W + PAD_GAP),
                grid_y + (pad / 4) as f32 * (PAD_H + PAD_GAP),
                PAD_W,
                PAD_H,
            );
            let id = wid("dm-pad", ch, pad);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let flash_id = wid("dm-flash", ch, pad);
            let cur = self.anim.get(flash_id).unwrap_or(0.0);
            let glow = if hit > cur { hit } else { self.anim.to(flash_id, hit, T_FLASH) };
            self.anim.set(flash_id, glow);
            let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.94 } else { 1.0 }, T_PRESS);
            let base = t.pad_a.mix(t.raised_hi, hv * 0.6);
            c.fill_rrect(r.scale(press), 10.0, base.mix(t.accent, glow * 0.85));
            // Niveau du pad : un filet en bas.
            if let Some(level) = find(pad as u32 * 3 + 2) {
                let n = level.to_norm(self.param_value(device, level));
                let lr = Rect::new(r.x + 8.0, r.bottom() - 9.0, (r.w - 16.0) * n, 3.0);
                c.fill_rrect(lr, 1.5, t.text2.mix(t.on_accent, glow).alpha(0.55));
            }
            let label = self.short_label(self.f.sans_med, 11.0, pad_name(pad), r.w - 12.0);
            let ink = t.text2.mix(t.text, glow).mix(t.on_accent, (glow - 0.5) * 2.0);
            self.text(c, self.f.sans_med, 11.0, r.x + 8.0, r.y + 14.0, &label, ink);
            if hov {
                self.set_tip(&tr_fmt!("{}\nClick: select and listen", tr(group(params, pad as u32 * 3))), "");
                if input.pressed {
                    PAD.with(|p| p.borrow_mut().insert(ch, pad));
                    self.preview_note(ch, FIRST_PAD_NOTE + pad as u8);
                }
            }
        }
        let ring = Rect::new(body.x + sel_x * (PAD_W + PAD_GAP), grid_y + sel_y * (PAD_H + PAD_GAP), PAD_W, PAD_H);
        c.stroke_rrect(ring.inset(-2.0), 12.0, 1.5, t.select);

        // Réglages : le pad choisi en haut, le général en bas.
        let px = body.x + PADS_W + GAP;
        c.fill_rect(Rect::new(px - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        self.text(c, self.f.sans_med, 11.0, px + 4.0, body.y + 6.0, tr(group(params, sel as u32 * 3)), t.muted);
        let cells: Vec<(usize, usize, u32)> = (0..3)
            .map(|k| (k, 0, sel as u32 * 3 + k as u32))
            .chain([(0, 1, PADS as u32 * 3), (1, 1, PADS as u32 * 3 + 1)])
            .collect();
        for (col, row, pid) in cells {
            let Some(p) = find(pid) else { continue };
            let cell = Rect::new(px + col as f32 * CELL_W, body.y + 16.0 + row as f32 * CELL_H, CELL_W, CELL_H);
            let v = self.param_value(device, p);
            if let Some(nv) = self.knob(c, input, wid("prm", ch, p.id as usize), cell, p, v) {
                self.set_param_value(device, p, nv);
            }
            let target = device_target(device, p.id);
            if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                self.open_popup(
                    Popup::Param { target, value: v, default: p.default },
                    Rect::new(input.x, input.y, 0.0, 0.0),
                );
            }
            self.param_mark(c, cell, target);
            self.auto_mark(c, input, (cell.right() - 10.0, cell.y + 6.0), Rect::new(cell.x, cell.y + 8.0, cell.w, 54.0), target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arp(def: NoteFxDef) -> Vec<(u64, u64, u8, f32)> {
        let mut fx = Project::make_note_fx(&def, tick_rate(120.0), 120.0).unwrap();
        let input = [(0, NoteEvent::on(60, 0.8)), (0, NoteEvent::on(64, 0.8)), (0, NoteEvent::on(67, 0.8))];
        let until = TICKS_PER_BAR as u64;
        spans(&simulate(fx.as_mut(), &input, until), until)
    }

    #[test]
    fn arpeggio_preview_lands_on_the_tick_grid() {
        // Défaut : doubles-croches montantes, durée 50 %.
        let notes = arp(NoteFxDef::new("arpeggiator"));
        assert_eq!(notes.len(), 16);
        let step = (TICKS_PER_BAR / 16) as u64;
        assert_eq!(notes[0], (0, step / 2, 60, 0.8));
        assert_eq!((notes[1].0, notes[1].2), (step, 64));
        assert_eq!((notes[3].0, notes[3].2), (3 * step, 60));
    }

    #[test]
    fn chord_free_intervals_only_shown_in_free_mode() {
        let def = NoteFxDef::new("chord");
        assert!(shown(&def).iter().all(|p| p.group != "Free"));
        assert_eq!(shown(&def.with(0, 0.0)).len(), 6);
    }

    #[test]
    fn every_note_fx_has_its_own_icon() {
        for k in registry::note_fx() {
            assert!(icon(k.id) != Icon::Effect, "{}", k.id);
        }
    }
}
