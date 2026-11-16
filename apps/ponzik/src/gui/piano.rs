//! Piano roll : notes du canal sélectionné dans le pattern courant.

use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_project::{ChordKind, ScaleKind};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{App, Focus, Popup, Rename, View};
use super::rack::note_name;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const KEY_H: f32 = 16.0;
const KEYBOARD: f32 = 64.0;
const RULER: f32 = 28.0;
const VELOCITY: f32 = 76.0;
const EDGE: f32 = 7.0;
const SNAPS: [u32; 4] = [96, 48, 24, 12];
const PIANO_HELP: &str = "Piano roll
Clic : poser · bord : durée · clic droit : effacer
Ctrl + glisser : sélectionner · règle : boucle
Ctrl+B : dupliquer · Ctrl+Q : quantifier
Molette : défiler · Maj : horizontal · Ctrl : zoom
Bouton du milieu : glisser la vue";

#[derive(Clone, Debug)]
pub enum PianoDrag {
    /// Déplacement : notes d'origine (indice, note), point de départ en ticks et en note.
    Move { orig: Vec<(usize, Note)>, tick0: f32, key0: f32 },
    Resize { orig: Vec<(usize, Note)>, tick0: f32 },
    Select { x0: f32, y0: f32 },
    Velocity,
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

pub fn chord_name(kind: ChordKind) -> &'static str {
    match kind {
        ChordKind::Major => "Majeur",
        ChordKind::Minor => "Mineur",
        ChordKind::Diminished => "Diminué",
        ChordKind::Augmented => "Augmenté",
        ChordKind::Sus2 => "Sus 2",
        ChordKind::Sus4 => "Sus 4",
        ChordKind::Power => "Quinte",
        ChordKind::Major7 => "Majeur 7",
        ChordKind::Minor7 => "Mineur 7",
        ChordKind::Dominant7 => "Septième",
        ChordKind::HalfDiminished7 => "Demi-diminué",
        ChordKind::Diminished7 => "Diminué 7",
        ChordKind::MinorMajor7 => "Mineur maj. 7",
        ChordKind::Add9 => "Add 9",
        ChordKind::MinorAdd9 => "Mineur add 9",
    }
}

/// Symbole court ; « M » pour l'accord majeur, « b5 » à la place de « ♭5 »
/// (les polices embarquées n'ont pas ♭).
pub fn chord_symbol(kind: ChordKind) -> &'static str {
    match kind {
        ChordKind::Major => "M",
        ChordKind::HalfDiminished7 => "m7b5",
        k => k.symbol(),
    }
}

impl App {
    fn piano_channel(&self) -> Option<usize> {
        self.selected_channel().or_else(|| (!self.project.channels.is_empty()).then_some(0))
    }

    fn notes_mut(&mut self, ch: usize) -> &mut Vec<Note> {
        let n = self.project.channels.len();
        let pat = self.project.current_pattern;
        let notes = &mut self.project.patterns[pat].notes;
        if notes.len() < n {
            notes.resize(n, Vec::new());
        }
        &mut notes[ch]
    }

    /// Notes que le tampon d'accord pose sur `root` (au moins `root`).
    fn chord_at(&self, root: u8) -> Vec<u8> {
        let notes = match self.piano.chord {
            Some(kind) => ponzik_project::chord(root, kind),
            None if self.project.scale == ScaleKind::Chromatic => ponzik_project::chord(root, ChordKind::Major),
            None => {
                let root = self.project.snap(root);
                let rel = root as i32 - (self.project.key % 12) as i32;
                let degree = self.project.scale.intervals().iter().position(|&i| i as i32 == rel.rem_euclid(12)).unwrap_or(0);
                self.project.diatonic_chord(degree, 3, rel.div_euclid(12) - 1)
            }
        };
        if notes.is_empty() { vec![root] } else { notes }
    }

    /// Hauteurs posées par un clic sur `key` : collée à la gamme si demandé, en accord si le tampon est actif.
    fn stamp_keys(&self, key: u8) -> Vec<u8> {
        let root = if self.piano.scale_snap { self.project.snap(key) } else { key };
        if self.piano.chord_stamp { self.chord_at(root) } else { vec![root] }
    }

    fn notes(&self, ch: usize) -> &[Note] {
        self.project.patterns[self.project.current_pattern].notes.get(ch).map_or(&[], |v| v.as_slice())
    }

    /// Trie les notes et retrouve la sélection après le tri.
    fn finish_note_edit(&mut self, ch: usize, label: &str) {
        let selected: Vec<Note> = self.piano.selected.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
        let notes = self.notes_mut(ch);
        notes.sort_by_key(|n| (n.start, n.key));
        let mut taken = vec![false; notes.len()];
        let mut sel = Vec::new();
        for s in selected {
            if let Some(i) = notes.iter().enumerate().position(|(i, n)| !taken[i] && *n == s) {
                taken[i] = true;
                sel.push(i);
            }
        }
        self.piano.selected = sel;
        self.commit(label);
    }

    pub(super) fn piano_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let Some(ch) = self.piano_channel() else {
            c.fill_rrect(body, 16.0, t.panel);
            self.empty_state(c, body, Icon::Piano, "Ajoute un instrument pour écrire des notes");
            return;
        };
        let pat = self.project.current_pattern;

        // En-tête : canal édité et grille.
        let chan_box = Rect::new(header.x, header.y, 220.0, header.h);
        c.fill_rrect(chan_box, 10.0, t.raised);
        c.stroke_rrect(chan_box, 10.0, 1.0, t.hairline);
        let n = self.project.channels.len();
        if self.icon_button(c, input, wid("pr-prev", 0, 0), Rect::new(chan_box.x + 2.0, chan_box.y + 2.0, 28.0, chan_box.h - 4.0), Icon::ChevronLeft, false, ("Canal précédent", "")) {
            self.focus = Focus::Channel((ch + n - 1) % n);
            self.piano.selected.clear();
        }
        if self.icon_button(c, input, wid("pr-next", 0, 0), Rect::new(chan_box.right() - 30.0, chan_box.y + 2.0, 28.0, chan_box.h - 4.0), Icon::ChevronRight, false, ("Canal suivant", "")) {
            self.focus = Focus::Channel((ch + 1) % n);
            self.piano.selected.clear();
        }
        let name = self.project.channels[ch].name.clone();
        let (bx, by) = chan_box.center();
        let label = self.fit(self.f.sans_med, 13.0, &name, chan_box.w - 70.0);
        self.text_center(c, self.f.sans_med, 13.0, bx, by, &label, t.text);
        let name_r = Rect::new(chan_box.x + 32.0, chan_box.y, chan_box.w - 64.0, chan_box.h);
        if input.inside && name_r.contains(input.x, input.y) && self.drag.is_none() {
            self.set_tip("Canal\nDouble-clic : renommer", "");
            if input.double {
                self.open_popup(Popup::Rename { target: Rename::Channel(ch), text: name.clone() }, chan_box);
            }
        }

        // Gamme et accords : coller à la gamme, tampon d'accord et son type.
        let scaled = self.project.scale != ScaleKind::Chromatic;
        let mut hx = chan_box.right() + 12.0;
        let tip = if scaled { ("Coller à la gamme", "") } else { ("Coller à la gamme\nChoisis d'abord une gamme, en haut", "") };
        if self.icon_button(c, input, wid("pr-scale", 0, 0), Rect::new(hx, header.y, 36.0, header.h), Icon::Scale, self.piano.scale_snap, tip) {
            self.piano.scale_snap = !self.piano.scale_snap;
        }
        hx += 40.0;
        let stamp = self.piano.chord_stamp;
        if self.icon_button(c, input, wid("pr-chord", 0, 0), Rect::new(hx, header.y, 36.0, header.h), Icon::Chord, stamp, ("Poser des accords\nUn clic : un accord entier", "")) {
            self.piano.chord_stamp = !stamp;
        }
        hx += 38.0;
        let chip = Rect::new(hx, header.y + 4.0, 64.0, header.h - 8.0);
        let cid = wid("pr-chord-kind", 0, 0);
        let chov = self.hovered(input, cid, chip);
        let chv = self.hover_anim(cid, chov);
        let on = self.anim.to(cid ^ 0x4f4e, stamp as u8 as f32, T_HOVER);
        c.fill_rrect(chip, 8.0, t.raised.mix(t.raised_hi, chv).alpha(0.5 + 0.5 * on.max(chv)));
        let label = self.piano.chord.map_or("Gamme", chord_symbol);
        let (kx, ky) = chip.center();
        self.text_center(c, self.f.sans_med, 12.0, kx - 5.0, ky, label, t.muted.mix(t.text, on.max(chv * 0.6)));
        draw_icon(c, Icon::ChevronDown, chip.right() - 10.0, ky, 9.0, t.muted);
        if chov {
            let name = self.piano.chord.map_or("Accord de la gamme", chord_name);
            self.set_tip(&format!("Type d'accord\n{name}"), "");
        }
        if chov && input.pressed {
            self.open_popup(Popup::Chord, chip);
        }

        let snap_box = Rect::new(header.right() - 236.0, header.y, 236.0, header.h);
        let snap_sel = SNAPS.iter().position(|&s| s == self.piano.snap).unwrap_or(2);
        let tips = [("Aimanter à la noire", ""), ("Aimanter à la croche", ""), ("Aimanter à la double", ""), ("Aimanter à la triple", "")];
        if let Some(i) = self.segmented_tips(c, input, wid("pr-snap", 0, 0), snap_box, &["1/4", "1/8", "1/16", "1/32"], &tips, snap_sel) {
            self.piano.snap = SNAPS[i];
        }
        draw_icon(c, Icon::Magnet, snap_box.x - 18.0, header.y + header.h * 0.5, 15.0, t.muted);
        let help = Rect::new(snap_box.x - 74.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("pr-help", 0, 0), help, Icon::Info, false, (PIANO_HELP, ""));

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);
        let keys = Rect::new(inner.x, inner.y + RULER, KEYBOARD, inner.h - RULER - VELOCITY - 8.0);
        let grid = Rect::new(keys.right(), keys.y, inner.w - KEYBOARD, keys.h);
        let ruler = Rect::new(grid.x, inner.y, grid.w, RULER);
        let vel_lane = Rect::new(grid.x, grid.bottom() + 8.0, grid.w, VELOCITY);
        let pat_len = self.project.patterns[pat].len.max(TICKS_PER_BAR) as f32;

        // Cadrage initial sur le pattern et les notes du canal.
        if self.piano.fitted_for != Some((pat, ch)) {
            self.piano.fitted_for = Some((pat, ch));
            self.piano.zoom = grid.w / pat_len;
            self.piano.scroll_x = 0.0;
            let notes = self.notes(ch);
            let center = if notes.is_empty() {
                self.project.channels[ch].root_note as f32
            } else {
                notes.iter().map(|n| n.key as f32).sum::<f32>() / notes.len() as f32
            };
            self.piano.scroll_y = (center + grid.h / KEY_H * 0.5).round().clamp(grid.h / KEY_H, 127.0);
        }

        // Bouton du milieu : faire glisser la vue dans les deux sens.
        let pan_id = wid("pr-pan", 0, 0);
        if input.inside && grid.contains(input.x, input.y) && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.piano.scroll_x, self.piano.scroll_y));
        }
        if let Some((_, x0, y0, sx0, sy0)) = self.pan.filter(|p| p.0 == pan_id) {
            self.piano.scroll_x = (sx0 - (input.x - x0) / self.piano.zoom).max(0.0);
            self.piano.scroll_y = (sy0 + (input.y - y0) / KEY_H).clamp(grid.h / KEY_H, 127.0);
            self.anim.set(wid("pr-sx", 0, 0), self.piano.scroll_x);
            self.anim.set(wid("pr-sy", 0, 0), self.piano.scroll_y);
        }
        // Molette : défilement vertical, Maj horizontal, Ctrl zoom.
        if input.inside && (grid.contains(input.x, input.y) || keys.contains(input.x, input.y)) && input.wheel != 0.0 {
            if input.mods.ctrl {
                let at = self.piano.scroll_x + (input.x - grid.x) / self.piano.zoom;
                self.piano.zoom = (self.piano.zoom * 1.15f32.powf(input.wheel)).clamp(0.05, 8.0);
                self.piano.scroll_x = (at - (input.x - grid.x) / self.piano.zoom).max(0.0);
            } else if input.mods.shift {
                self.piano.scroll_x = (self.piano.scroll_x - input.wheel * 60.0 / self.piano.zoom).max(0.0);
            } else {
                self.piano.scroll_y = (self.piano.scroll_y + input.wheel * 3.0).clamp(grid.h / KEY_H, 127.0);
            }
        }
        let sx = self.anim.to(wid("pr-sx", 0, 0), self.piano.scroll_x, T_LAYOUT);
        let sy = self.anim.to(wid("pr-sy", 0, 0), self.piano.scroll_y, T_LAYOUT);
        let zoom = self.piano.zoom;
        let x_of = |tick: f32| grid.x + (tick - sx) * zoom;
        let y_of = |key: f32| grid.y + (sy - key) * KEY_H;
        let tick_at = |x: f32| sx + (x - grid.x) / zoom;
        let key_at = |y: f32| (sy - ((y - grid.y) / KEY_H).floor()).clamp(0.0, 127.0);

        // Fond : rangées, puis lignes de pas, temps, mesures.
        let prev = c.push_clip(grid);
        let top_key = sy.floor() as i32;
        let rows = (grid.h / KEY_H).ceil() as i32 + 2;
        let scaled = self.project.scale != ScaleKind::Chromatic;
        for k in (top_key - rows..=top_key + 1).filter(|k| (0..128).contains(k)) {
            let y = y_of(k as f32);
            let col = if is_black(k as u8) { t.well } else { t.panel.mix(t.raised, 0.35) };
            c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), col);
            // Gamme du projet : rangées hors gamme assombries, tonique teintée.
            if scaled {
                if !self.project.in_scale(k as u8) {
                    c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), t.bg_deep.alpha(0.55));
                } else if (k as u8 + 12 - self.project.key % 12) % 12 == 0 {
                    c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), t.accent.alpha(0.05));
                }
            }
            if k % 12 == 0 {
                c.fill_rect(Rect::new(grid.x, y + KEY_H - 1.0, grid.w, 1.0), t.hairline.alpha(2.0));
            }
        }
        let first = (sx / TICKS_PER_STEP as f32).floor() as u32;
        let last = ((sx + grid.w / zoom) / TICKS_PER_STEP as f32).ceil() as u32;
        let step_px = TICKS_PER_STEP as f32 * zoom;
        for s in first..=last {
            let tick = s * TICKS_PER_STEP;
            let x = x_of(tick as f32).round();
            let (w, a) = if tick % TICKS_PER_BAR == 0 {
                (1.0, 3.5)
            } else if tick % 96 == 0 {
                (1.0, 1.8)
            } else if step_px >= 8.0 {
                (1.0, 0.7)
            } else {
                continue;
            };
            c.fill_rect(Rect::new(x, grid.y, w, grid.h), t.hairline.alpha(a));
        }
        let end_x = x_of(pat_len);
        if end_x < grid.right() {
            c.fill_rect(Rect::new(end_x, grid.y, grid.right() - end_x, grid.h), t.bg_deep.alpha(0.55));
        }

        // Notes fantômes des autres canaux.
        for (other, notes) in self.project.patterns[pat].notes.iter().enumerate() {
            if other == ch || self.project.channels.get(other).is_none() {
                continue;
            }
            for n in notes {
                let r = Rect::new(x_of(n.start as f32), y_of(n.key as f32) + 2.0, (n.len as f32 * zoom).max(3.0), KEY_H - 4.0);
                if r.right() > grid.x && r.x < grid.right() {
                    c.stroke_rrect(r, 3.0, 1.0, t.text.alpha(0.1));
                }
            }
        }

        // Interaction dans la grille.
        let grid_id = wid("pr-grid", 0, 0);
        let over_grid = input.inside && grid.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(grid_id));
        let hit = |app: &App, x: f32, y: f32| -> Option<(usize, bool)> {
            let key = key_at(y) as u8;
            let tick = tick_at(x);
            app.notes(ch).iter().enumerate().rev().find_map(|(i, n)| {
                let x1 = x_of((n.start + n.len) as f32);
                let inside = n.key == key && tick >= n.start as f32 && x <= x1.max(x_of(n.start as f32) + 4.0);
                inside.then_some((i, x1 - x <= EDGE))
            })
        };
        let hover_note = if over_grid { hit(self, input.x, input.y) } else { None };

        if over_grid && input.pressed && self.drag.is_none() {
            self.begin_drag(grid_id, input, 0.0);
            let tick = tick_at(input.x);
            let key = key_at(input.y);
            match hover_note {
                Some((i, edge)) => {
                    if !self.piano.selected.contains(&i) {
                        if !input.mods.shift {
                            self.piano.selected.clear();
                        }
                        self.piano.selected.push(i);
                    }
                    let orig: Vec<(usize, Note)> = self.piano.selected.iter().filter_map(|&j| self.notes(ch).get(j).map(|n| (j, *n))).collect();
                    let note = self.notes(ch)[i];
                    self.piano.last_len = note.len;
                    if edge {
                        self.piano_drag = Some(PianoDrag::Resize { orig, tick0: tick });
                    } else {
                        self.preview_note(ch, note.key);
                        self.piano_drag = Some(PianoDrag::Move { orig, tick0: tick, key0: key });
                    }
                }
                None if input.mods.ctrl => {
                    self.piano_drag = Some(PianoDrag::Select { x0: input.x, y0: input.y });
                }
                None => {
                    let snap = self.piano.snap as f32;
                    let start = ((tick / snap).floor() * snap).max(0.0) as u32;
                    let len = self.piano.last_len.max(1);
                    let keys = self.stamp_keys(key as u8);
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    notes.extend(keys.iter().map(|&k| Note { start, len, key: k, vel: 0.8 }));
                    let orig: Vec<(usize, Note)> = (base..notes.len()).map(|i| (i, notes[i])).collect();
                    self.piano.selected = (base..base + keys.len()).collect();
                    self.preview_note(ch, keys[0]);
                    self.piano_drag = Some(PianoDrag::Move { orig, tick0: tick, key0: key });
                    self.commit(if keys.len() > 1 { "Poser un accord" } else { "Poser une note" });
                }
            }
        }
        // Clic droit maintenu : efface les notes balayées.
        if over_grid && input.right_down && let Some((i, _)) = hover_note {
            self.notes_mut(ch).remove(i);
            self.piano.selected.clear();
            self.commit("Effacer des notes");
        }

        // Geste en cours.
        if self.is_dragging(grid_id) {
            let snap = self.piano.snap as f32;
            match self.piano_drag.clone() {
                Some(PianoDrag::Move { orig, tick0, key0 }) => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    let dk = key_at(input.y) - key0;
                    let min_start = orig.iter().map(|(_, n)| n.start).min().unwrap_or(0) as f32;
                    let dt = dt.max(-min_start);
                    let mut moved_key = None;
                    let scale_snap = self.piano.scale_snap;
                    for &(i, n) in &orig {
                        let key = (n.key as f32 + dk).clamp(0.0, 127.0) as u8;
                        let key = if scale_snap && dk != 0.0 { self.project.snap(key) } else { key };
                        if let Some(dst) = self.notes_mut(ch).get_mut(i) {
                            *dst = Note { start: (n.start as f32 + dt) as u32, key, ..n };
                        }
                        moved_key = Some(key);
                    }
                    if let (Some(k), Some((pc, pk))) = (moved_key, self.preview)
                        && orig.len() == 1
                        && pc == ch
                        && pk != k
                    {
                        self.preview_note(ch, k);
                    }
                    self.commit("Déplacer des notes");
                }
                Some(PianoDrag::Resize { orig, tick0 }) => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    for &(i, n) in &orig {
                        let len = (n.len as f32 + dt).max(snap.min(n.len as f32)).max(1.0) as u32;
                        if let Some(dst) = self.notes_mut(ch).get_mut(i) {
                            dst.len = len;
                        }
                        self.piano.last_len = len;
                    }
                    self.commit("Durée des notes");
                }
                Some(PianoDrag::Select { x0, y0 }) => {
                    let sel = Rect::new(x0.min(input.x), y0.min(input.y), (x0 - input.x).abs(), (y0 - input.y).abs());
                    c.fill_rect(sel, t.select.alpha(0.12));
                    c.stroke_rrect(sel, 2.0, 1.0, t.select.alpha(0.8));
                    let (t0, t1) = (tick_at(sel.x), tick_at(sel.right()));
                    let (k0, k1) = (key_at(sel.bottom()), key_at(sel.y));
                    self.piano.selected = self
                        .notes(ch)
                        .iter()
                        .enumerate()
                        .filter(|(_, n)| {
                            (n.start + n.len) as f32 > t0 && (n.start as f32) < t1 && (n.key as f32) >= k0 && (n.key as f32) <= k1
                        })
                        .map(|(i, _)| i)
                        .collect();
                }
                _ => {}
            }
            if input.released {
                if matches!(self.piano_drag, Some(PianoDrag::Move { .. } | PianoDrag::Resize { .. })) {
                    self.finish_note_edit(ch, "Modifier des notes");
                }
                self.piano_drag = None;
            }
        }

        // Notes du canal.
        let melodic_labels = KEY_H >= 14.0;
        for (i, n) in self.notes(ch).to_vec().into_iter().enumerate() {
            let r = Rect::new(x_of(n.start as f32), y_of(n.key as f32) + 1.0, (n.len as f32 * zoom).max(4.0), KEY_H - 2.0);
            if r.right() < grid.x || r.x > grid.right() || r.bottom() < grid.y || r.y > grid.bottom() {
                continue;
            }
            let selected = self.piano.selected.contains(&i);
            let hov = hover_note.is_some_and(|(h, _)| h == i);
            let base = t.accent_deep.mix(t.accent, 0.4 + 0.6 * n.vel);
            let fill = if selected { base.mix(t.accent_hot, 0.35) } else { base.mix(t.accent_hot, hov as u8 as f32 * 0.15) };
            c.fill_rrect(r, 4.0, fill);
            if selected {
                c.stroke_rrect(r, 4.0, 1.5, t.select);
            }
            if hover_note.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 4.0, r.y + 3.0, 2.0, r.h - 6.0), 1.0, t.on_accent.alpha(0.6));
            }
            if melodic_labels && r.w > 30.0 {
                self.text(c, self.f.mono_med, 10.0, r.x + 5.0, r.y + r.h * 0.5, &note_name(n.key), t.on_accent);
            }
        }

        // Fantôme de ce qu'un clic poserait, quand la gamme ou le tampon d'accord s'en mêlent.
        if over_grid && hover_note.is_none() && self.drag.is_none() && (self.piano.chord_stamp || self.piano.scale_snap) && !input.mods.ctrl {
            let snap = self.piano.snap as f32;
            let start = ((tick_at(input.x) / snap).floor() * snap).max(0.0);
            let w = (self.piano.last_len.max(1) as f32 * zoom).max(4.0);
            for k in self.stamp_keys(key_at(input.y) as u8) {
                let r = Rect::new(x_of(start), y_of(k as f32) + 1.0, w, KEY_H - 2.0);
                c.fill_rrect(r, 4.0, t.accent.alpha(0.12));
                c.stroke_rrect(r, 4.0, 1.0, t.accent.alpha(0.6));
            }
        }

        // Tête de lecture.
        if self.playing && self.project.mode == PlayMode::Pattern(pat) {
            let x = x_of(self.cycle_position() as f32);
            c.fill_rect(Rect::new(x.round() - 1.0, grid.y, 2.0, grid.h), t.select);
        }
        c.set_clip(Some(prev));

        // Raccourcis d'édition.
        if self.popup.is_none() && !self.sidebar_focus {
            self.piano_keys(input, ch);
        }

        // Clavier : touches blanches pleine largeur, noires par-dessus ; clic pour écouter.
        let prev = c.push_clip(keys);
        let ivory = t.text.mix(t.raised_hi, 0.12);
        c.fill_rect(keys, ivory);
        let visible: Vec<i32> = (top_key - rows..=top_key + 1).filter(|k| (0..128).contains(k)).collect();
        let hover_key = (input.inside && keys.contains(input.x, input.y) && self.drag.is_none()).then(|| key_at(input.y) as u8);
        for &k in &visible {
            let key = k as u8;
            let y = y_of(k as f32);
            let r = Rect::new(keys.x, y, keys.w, KEY_H);
            let playing = self.preview == Some((ch, key));
            let hov = hover_key == Some(key);
            if is_black(key) {
                let fill = if playing { t.accent } else { t.bg_deep.mix(t.raised_hi, hov as u8 as f32 * 0.6) };
                c.fill_rrect(Rect::new(r.x, r.y + 1.0, r.w * 0.6, r.h - 2.0), 3.0, fill);
            } else {
                if playing || hov {
                    c.fill_rect(Rect::new(r.x, r.y, r.w, r.h), if playing { t.accent } else { ivory.mix(t.muted, 0.25) });
                }
                // Séparation entre deux blanches voisines (Mi-Fa, Si-Do).
                if matches!(key % 12, 0 | 5) {
                    c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.bg_deep.alpha(0.35));
                }
            }
            if key % 12 == 0 {
                self.text_right(c, self.f.mono_med, 10.0, r.right() - 6.0, r.y + r.h * 0.5, &note_name(key), t.bg_deep.alpha(0.7));
            }
        }
        let kid = wid("pr-keys", 0, 0);
        if self.hovered(input, kid, keys) && input.pressed {
            self.preview_note(ch, key_at(input.y) as u8);
            self.begin_drag(kid, input, 0.0);
        }
        c.fill_rect(Rect::new(keys.right() - 1.0, keys.y, 1.0, keys.h), t.bg_deep);
        c.set_clip(Some(prev));

        // Règle : mesures ; clic = marqueur de départ, glisser = sélection de temps.
        let prev = c.push_clip(Rect::new(ruler.x, ruler.y, ruler.w, grid.bottom() - ruler.y));
        let bar_first = (sx / TICKS_PER_BAR as f32).floor() as u32;
        let bar_last = ((sx + grid.w / zoom) / TICKS_PER_BAR as f32).ceil() as u32;
        for b in bar_first..=bar_last {
            let x = x_of((b * TICKS_PER_BAR) as f32);
            c.fill_rect(Rect::new(x.round(), ruler.y + 14.0, 1.0, 14.0), t.hairline.alpha(3.0));
            self.text(c, self.f.mono, 11.0, x + 6.0, ruler.y + 18.0, &(b + 1).to_string(), t.text2);
            for beat in 1..4 {
                let bx = x_of((b * TICKS_PER_BAR + beat * 96) as f32).round();
                c.fill_rect(Rect::new(bx, ruler.y + 22.0, 1.0, 6.0), t.hairline.alpha(2.0));
            }
        }
        let snap = self.piano.snap as f32;
        if let Some((a, b)) = self.ruler(c, input, View::Piano, ruler, grid, snap, &x_of, &tick_at) {
            // La sélection de temps sélectionne aussi les notes qui y commencent.
            self.piano.selected = self
                .notes(ch)
                .iter()
                .enumerate()
                .filter(|(_, n)| n.start >= a && n.start < b)
                .map(|(i, _)| i)
                .collect();
        }
        c.set_clip(Some(prev));

        // Vélocités : glisser dans le couloir pour les dessiner.
        c.fill_rrect(vel_lane, 10.0, t.well);
        let prev = c.push_clip(vel_lane);
        let vid = wid("pr-vel", 0, 0);
        if self.hovered(input, vid, vel_lane) && input.pressed {
            self.begin_drag(vid, input, 0.0);
            self.piano_drag = Some(PianoDrag::Velocity);
        }
        if self.is_dragging(vid) {
            let tick = tick_at(input.x);
            let v = ((vel_lane.bottom() - 6.0 - input.y) / (vel_lane.h - 12.0)).clamp(0.05, 1.0);
            let only_selected = !self.piano.selected.is_empty();
            let sel = self.piano.selected.clone();
            let notes = self.notes_mut(ch);
            let mut changed = false;
            for (i, n) in notes.iter_mut().enumerate() {
                let near = (n.start as f32 - tick).abs() * zoom < 6.0;
                if near && (!only_selected || sel.contains(&i)) && n.vel != v {
                    n.vel = v;
                    changed = true;
                }
            }
            if changed {
                self.commit("Vélocité");
            }
        }
        for (i, n) in self.notes(ch).to_vec().into_iter().enumerate() {
            let x = x_of(n.start as f32).round();
            if x < vel_lane.x - 4.0 || x > vel_lane.right() {
                continue;
            }
            let h = (vel_lane.h - 12.0) * n.vel;
            let col = if self.piano.selected.contains(&i) { t.select } else { t.accent.mix(t.accent_deep, 1.0 - n.vel) };
            c.fill_rrect(Rect::new(x, vel_lane.bottom() - 6.0 - h, 3.0, h), 1.5, col);
            c.fill_circle(x + 1.5, vel_lane.bottom() - 6.0 - h, 3.0, col);
        }
        c.set_clip(Some(prev));
        draw_icon(c, Icon::Mixer, inner.x + KEYBOARD * 0.5, vel_lane.y + vel_lane.h * 0.5, 15.0, t.muted.alpha(0.7));
        self.tip_zone(input, Rect::new(inner.x, vel_lane.y, KEYBOARD, vel_lane.h), "Vélocité\nGlisser dans le couloir pour la dessiner", "");
    }

    /// Raccourcis d'édition du piano roll, ceux de FL Studio.
    fn piano_keys(&mut self, input: &Input, ch: usize) {
        let snap = self.piano.snap;
        for &(key, mods) in &input.keys {
            // Sélection explicite, sinon tout le canal.
            let targets: Vec<usize> =
                if self.piano.selected.is_empty() { (0..self.notes(ch).len()).collect() } else { self.piano.selected.clone() };
            match key {
                Key::Delete | Key::Backspace if !self.piano.selected.is_empty() => {
                    let mut sel = std::mem::take(&mut self.piano.selected);
                    sel.sort_unstable();
                    let notes = self.notes_mut(ch);
                    for i in sel.into_iter().rev() {
                        if i < notes.len() {
                            notes.remove(i);
                        }
                    }
                    self.commit("Effacer des notes");
                }
                Key::Char('a') if mods.ctrl => self.piano.selected = (0..self.notes(ch).len()).collect(),
                Key::Char('d') if mods.ctrl => self.piano.selected.clear(),
                Key::Char('c') if mods.ctrl => {
                    let notes: Vec<Note> = targets.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
                    let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
                    self.clipboard_notes = notes.into_iter().map(|n| Note { start: n.start - first, ..n }).collect();
                    let n = self.clipboard_notes.len();
                    self.toast(&format!("{n} note(s) copiée(s)"));
                }
                Key::Char('v') if mods.ctrl && !self.clipboard_notes.is_empty() => {
                    let at = ((self.start_marker as u32) / snap) * snap;
                    let clip = self.clipboard_notes.clone();
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    for n in &clip {
                        notes.push(Note { start: n.start + at, ..*n });
                    }
                    self.piano.selected = (base..base + clip.len()).collect();
                    self.finish_note_edit(ch, "Coller des notes");
                }
                Key::Char('b') if mods.ctrl && !targets.is_empty() => {
                    let sel: Vec<Note> = targets.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
                    // Durée dupliquée : la sélection de temps si elle existe, sinon l'étendue des notes.
                    let span = match self.time_selection {
                        Some((View::Piano, a, b)) => b - a,
                        _ => {
                            let lo = sel.iter().map(|n| n.start).min().unwrap_or(0);
                            let hi = sel.iter().map(|n| n.start + n.len).max().unwrap_or(0);
                            (hi - lo).div_ceil(snap) * snap
                        }
                    };
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    for n in &sel {
                        notes.push(Note { start: n.start + span, ..*n });
                    }
                    self.piano.selected = (base..base + sel.len()).collect();
                    if let Some((View::Piano, a, b)) = self.time_selection {
                        self.time_selection = Some((View::Piano, a + span, b + span));
                    }
                    self.finish_note_edit(ch, "Dupliquer des notes");
                }
                Key::Char('q') if mods.ctrl && !targets.is_empty() => {
                    for &i in &targets {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.start = ((n.start + snap / 2) / snap) * snap;
                        }
                    }
                    self.finish_note_edit(ch, "Quantifier");
                    self.toast("Notes quantifiées");
                }
                Key::Up | Key::Down if !self.piano.selected.is_empty() => {
                    let d: i32 = if mods.ctrl { 12 } else { 1 } * if key == Key::Up { 1 } else { -1 };
                    let sel = self.piano.selected.clone();
                    for i in sel {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.key = (n.key as i32 + d).clamp(0, 127) as u8;
                        }
                    }
                    self.finish_note_edit(ch, "Transposer");
                }
                Key::Left | Key::Right if !self.piano.selected.is_empty() => {
                    let d = if key == Key::Right { snap as i64 } else { -(snap as i64) };
                    let sel = self.piano.selected.clone();
                    for i in sel {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.start = (n.start as i64 + d).max(0) as u32;
                        }
                    }
                    self.finish_note_edit(ch, "Décaler des notes");
                }
                _ => {}
            }
        }
    }
}
