//! Playlist : les patterns posés dans le temps pour construire le morceau.

use ponzik_engine::{PlayMode, TICKS_PER_BAR};
use ponzik_project::ClipDef;
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::{App, Popup, Rename, View};
use super::dnd::DropTarget;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const PALETTE_W: f32 = 196.0;
const RULER: f32 = 30.0;
const LANE_H: f32 = 52.0;
const EDGE: f32 = 8.0;
const PLAYLIST_HELP: &str = "Playlist
Clic : poser le pattern choisi · bord : durée
Clic droit : effacer · sur le titre : menu du clip
Double-clic : éditer · Ctrl+B : dupliquer
Règle : départ, boucle et sélection de temps
Molette : défiler · Ctrl : zoom · milieu : glisser";
/// Hauteur de la barre de titre d'un clip (clic droit : menu).
const CLIP_TITLE: f32 = 19.0;

#[derive(Clone, Copy, Debug)]
pub enum ClipDrag {
    Move { clip: usize, orig: ClipDef, tick0: f32, lane0: f32 },
    Resize { clip: usize, orig: ClipDef, tick0: f32 },
    /// Clip audio : (indice, début, ligne ou durée d'origine, point de départ).
    AudioMove { clip: usize, start: u32, lane: usize, tick0: f32, lane0: f32 },
    AudioResize { clip: usize, len: u32, tick0: f32 },
}

/// Couleur d'un pattern : une famille vert-sarcelle, une teinte par pattern.
pub fn pattern_color(i: usize) -> Color {
    const P: [u32; 8] = [0x3fd45a, 0x3ccfc4, 0x9be15d, 0x2fb6e0, 0x67e8a4, 0xc7e85a, 0x5ad1e8, 0x8ce07a];
    Color::hex(P[i % P.len()])
}

/// Couleurs proposées pour un pattern : la famille de Ponzik, puis des teintes
/// chaudes pour distinguer les sections. Pas de rouge : il dit l'enregistrement.
pub const PATTERN_COLORS: [(&str, u32); 12] = [
    ("Vert", 0x3fd45a),
    ("Sarcelle", 0x3ccfc4),
    ("Lime", 0x9be15d),
    ("Ciel", 0x2fb6e0),
    ("Menthe", 0x67e8a4),
    ("Citron", 0xc7e85a),
    ("Ambre", 0xf0b44a),
    ("Corail", 0xf08a5d),
    ("Rose", 0xe878b4),
    ("Lilas", 0xa98cf0),
    ("Indigo", 0x6b7cf0),
    ("Ardoise", 0x8a9aa6),
];

impl App {
    /// Couleur affichée d'un pattern : celle choisie, sinon celle de sa place.
    pub fn pattern_tint(&self, p: usize) -> Color {
        match self.project.patterns.get(p).map_or(0, |p| p.color) {
            0 => pattern_color(p),
            hex => Color::hex(hex),
        }
    }

    /// « Make unique » : le clip joue désormais sa propre copie du pattern.
    pub fn make_unique(&mut self, clip: usize) {
        if let Some(p) = self.project.make_unique(clip) {
            self.playlist.brush = p;
            self.playlist.selected = Some(clip);
            let name = self.project.patterns[p].name.clone();
            self.commit("Rendre unique");
            self.toast(&format!("Clip rendu unique : {name}"));
        }
    }

    /// Ctrl+B sur une sélection de temps : tout son contenu est recopié juste
    /// après, et la sélection (et la boucle) avance d'autant pour enchaîner.
    pub fn duplicate_time(&mut self) {
        let Some((View::Playlist, a, b)) = self.time_selection else {
            self.toast("Tire d'abord une sélection de temps dans la règle");
            return;
        };
        let n = self.project.duplicate_time(a, b);
        // Les clips ont pu être coupés : les indices ne tiennent plus.
        self.playlist.selected = None;
        let span = b - a;
        self.time_selection = Some((View::Playlist, b, b + span));
        self.set_loop(View::Playlist, Some((b, b + span)));
        self.start_marker = b as f64;
        self.commit("Dupliquer la sélection de temps");
        let bars = span as f32 / TICKS_PER_BAR as f32;
        let bars = if bars.fract() == 0.0 { format!("{bars:.0}") } else { format!("{bars:.2}").replace('.', ",") };
        let s = if n > 1 { "s" } else { "" };
        self.toast(&format!("{n} clip{s} dupliqué{s} · {bars} mes."));
    }

    pub(super) fn playlist_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let snap_box = Rect::new(header.right() - 120.0, header.y, 120.0, header.h);
        let snap_key = wid("pl-snap-v", 0, 0);
        let snap_sel = self.anim.get(snap_key).unwrap_or(0.0) as usize;
        let tips = [("Aimanter à la mesure", ""), ("Aimanter au temps", "")];
        if let Some(i) = self.segmented_tips(c, input, wid("pl-snap", 0, 0), snap_box, &["1", "1/4"], &tips, snap_sel) {
            self.anim.set(snap_key, i as f32);
        }
        draw_icon(c, Icon::Magnet, snap_box.x - 18.0, header.y + header.h * 0.5, 15.0, t.muted);
        let snap = if self.anim.get(snap_key).unwrap_or(0.0) as usize == 0 { TICKS_PER_BAR } else { 96 } as f32;
        let help = Rect::new(snap_box.x - 74.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("pl-help", 0, 0), help, Icon::Info, false, (PLAYLIST_HELP, ""));
        // Hors mode morceau, la playlist ne joue pas : un bouton pour y passer.
        if self.project.mode != PlayMode::Song {
            let song = Rect::new(help.x - 44.0, header.y, 36.0, header.h);
            let glow = 0.5 + 0.5 * (self.time * 3.0).sin();
            c.fill_rrect(song.inset(-2.0), 10.0, t.accent.alpha(0.08 + 0.06 * glow));
            if self.icon_button(c, input, wid("pl-song", 0, 0), song, Icon::Playlist, false, ("Jouer le morceau", "L")) {
                self.set_mode(PlayMode::Song);
            }
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);

        // Palette de patterns : le pattern choisi est celui qu'on pose.
        let palette = Rect::new(inner.x, inner.y, PALETTE_W - 12.0, inner.h);
        let mut y = palette.y + 4.0;
        for p in 0..self.project.patterns.len() {
            let row = Rect::new(palette.x, y, palette.w, 40.0);
            if row.bottom() > palette.bottom() - 44.0 {
                break;
            }
            let id = wid("pl-pat", p, 0);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            let current = self.playlist.brush == p;
            let sel = self.anim.to(wid("pl-pat-sel", p, 0), current as u8 as f32, T_HOVER);
            c.fill_rrect(row, 10.0, t.raised.alpha((hv * 0.6).max(sel)));
            c.fill_rrect(Rect::new(row.x + 10.0, row.y + 12.0, 4.0, 16.0), 2.0, self.pattern_tint(p));
            let name = self.project.patterns[p].name.clone();
            let edit = Rect::new(row.right() - 32.0, row.y + 6.0, 28.0, 28.0);
            let name_w = if hov { row.w - 66.0 } else { row.w - 36.0 };
            self.text_fit(c, self.f.sans_med, 13.0, row.x + 24.0, row.y + 20.0, &name, name_w, if current { t.text } else { t.text2 });
            if hov && edit.contains(input.x, input.y) {
                // Bouton d'édition, au-dessus de la ligne.
            } else if hov {
                self.set_tip("Clic : pinceau · double-clic : renommer\nClic droit : couleur, dupliquer, supprimer", "");
                self.set_cursor(Cursor::Hand);
                if input.right_pressed {
                    self.select_pattern(p);
                    self.open_popup(Popup::PatternMenu(p), Rect::new(input.x, input.y, 0.0, 0.0));
                }
                if input.pressed {
                    self.playlist.brush = p;
                    if input.double {
                        self.open_popup(Popup::Rename { target: Rename::Pattern(p), text: name.clone() }, row);
                    }
                }
            }
            if hv > 0.01 && self.icon_button(c, input, wid("pl-pat-edit", p, 0), edit, Icon::Rack, false, ("Éditer", &self.key_label(super::keymap::Shortcut::ViewRack))) {
                self.select_pattern(p);
                self.set_view(View::Rack);
            }
            y += 44.0;
        }
        let add = Rect::new(palette.x, (y + 4.0).min(palette.bottom() - 40.0), palette.w, 40.0);
        let id = wid("pl-add-pat", 0, 0);
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(add, 10.0, t.raised.alpha(hv * 0.6));
        c.stroke_rrect(add, 10.0, 1.0, t.hairline.alpha(1.0 + hv * 1.5));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Plus, ax, ay, 16.0, t.muted.mix(t.accent, hv));
        if hov {
            let key = self.key_label(super::keymap::Shortcut::NewPattern);
            self.set_tip("Nouveau pattern", &key);
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                let p = self.project.add_pattern();
                self.playlist.brush = p;
                self.select_pattern(p);
            }
        }

        // Ligne de temps.
        let area = Rect::new(inner.x + PALETTE_W, inner.y, inner.w - PALETTE_W, inner.h);
        let ruler = Rect::new(area.x, area.y, area.w, RULER);
        let lanes_r = Rect::new(area.x, area.y + RULER + 4.0, area.w, area.h - RULER - 4.0);
        c.fill_rrect(Rect::new(area.x - 1.0, area.y, area.w + 1.0, area.h), 12.0, t.well);
        let lanes_used = self
            .project
            .clips
            .iter()
            .map(|c| c.lane + 1)
            .chain(self.project.audio_clips.iter().map(|c| c.lane + 1))
            .max()
            .unwrap_or(0);
        let autos = self.project.automation.iter().filter(|a| a.scope == ponzik_engine::AutoScope::Song).count();
        let fit = ((lanes_r.h - autos as f32 * super::automation::AUTO_LANE_H) / LANE_H).floor().max(1.0) as usize;
        let lanes = (lanes_used + 1).max(fit).min(64);

        // Bouton du milieu : faire glisser le temps.
        let pan_id = wid("pl-pan", 0, 0);
        if input.inside && area.contains(input.x, input.y) && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.playlist.scroll_x, 0.0));
        }
        if let Some((_, x0, _, sx0, _)) = self.pan.filter(|p| p.0 == pan_id) {
            self.playlist.scroll_x = (sx0 - (input.x - x0) / self.playlist.zoom * TICKS_PER_BAR as f32).max(0.0);
            self.anim.set(wid("pl-sx", 0, 0), self.playlist.scroll_x);
        }
        if input.inside && area.contains(input.x, input.y) && input.wheel != 0.0 {
            if input.mods.ctrl {
                let at = self.playlist.scroll_x + (input.x - area.x) / self.playlist.zoom * TICKS_PER_BAR as f32;
                self.playlist.zoom = (self.playlist.zoom * 1.15f32.powf(input.wheel)).clamp(8.0, 400.0);
                self.playlist.scroll_x = (at - (input.x - area.x) / self.playlist.zoom * TICKS_PER_BAR as f32).max(0.0);
            } else {
                self.playlist.scroll_x = (self.playlist.scroll_x - input.wheel * 2.0 * TICKS_PER_BAR as f32 / 4.0).max(0.0);
            }
        }
        let sx = self.anim.to(wid("pl-sx", 0, 0), self.playlist.scroll_x, T_LAYOUT);
        let ppt = self.playlist.zoom / TICKS_PER_BAR as f32;
        let x_of = |tick: f32| area.x + (tick - sx) * ppt;
        let tick_at = |x: f32| sx + (x - area.x) / ppt;
        let lane_at = |y: f32| ((y - lanes_r.y) / LANE_H).floor().max(0.0);

        let prev = c.push_clip(area);
        // Fond des pistes et grille.
        for l in 0..lanes {
            let ly = lanes_r.y + l as f32 * LANE_H;
            if l % 2 == 1 {
                c.fill_rect(Rect::new(area.x, ly, area.w, LANE_H), t.panel.alpha(0.35));
            }
        }
        let bar_first = (sx / TICKS_PER_BAR as f32).floor() as u32;
        let bar_last = ((sx + area.w / ppt) / TICKS_PER_BAR as f32).ceil() as u32;
        let song_end = self.project.song_len() as f32;
        for b in bar_first..=bar_last {
            let x = x_of((b * TICKS_PER_BAR) as f32).round();
            let strong = b % 4 == 0;
            c.fill_rect(Rect::new(x, lanes_r.y, 1.0, lanes_r.h), t.hairline.alpha(if strong { 2.5 } else { 1.0 }));
            if self.playlist.zoom >= 28.0 || strong {
                self.text(c, self.f.mono, 11.0, x + 6.0, ruler.y + 15.0, &(b + 1).to_string(), if strong { t.text2 } else { t.muted });
            }
        }
        let end_x = x_of(song_end);
        c.fill_rect(Rect::new(end_x.round(), ruler.y + 6.0, 2.0, area.h - 6.0), t.text2.alpha(0.5));

        // Règle : clic = marqueur de départ, glisser = sélection de temps.
        if let Some((a, b)) = self.ruler(c, input, View::Playlist, ruler, lanes_r, snap.min(96.0), &x_of, &tick_at) {
            self.playlist.selected = self.project.clips.iter().position(|cl| cl.start >= a && cl.start < b);
        }
        // Sélection de temps : son raccourci, affiché au bout dans la règle.
        let sel = match self.time_selection {
            Some((View::Playlist, a, b)) => Some((a, b)),
            _ => None,
        };
        let shown = self.anim.to(wid("pl-tsel-key", 0, 0), sel.is_some() as u8 as f32, T_HOVER);
        if let Some((a, b)) = sel {
            let (xa, xb) = (x_of(a as f32), x_of(b as f32));
            if xb - xa > 64.0 && shown > 0.01 {
                self.keycap(c, xb - 6.0, ruler.y + ruler.h * 0.5, "Ctrl B", 0.8 * shown);
            }
            self.tip_zone(input, Rect::new(xa, ruler.y, xb - xa, ruler.h), "Sélection de temps\nCtrl+B : la recopier juste après", "Ctrl B");
        }

        // Clips.
        let grid_id = wid("pl-grid", 0, 0);
        let clip_zone = Rect::new(lanes_r.x, lanes_r.y, lanes_r.w, lanes as f32 * LANE_H);
        let over = input.inside && clip_zone.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(grid_id));
        let hit = |app: &App, x: f32, y: f32| -> Option<(usize, bool)> {
            let lane = lane_at(y) as usize;
            app.project.clips.iter().enumerate().rev().find_map(|(i, cl)| {
                let (x0, x1) = (x_of(cl.start as f32), x_of((cl.start + cl.len) as f32));
                (cl.lane == lane && x >= x0 && x <= x1).then_some((i, x1 - x <= EDGE))
            })
        };
        let hover_clip = if over { hit(self, input.x, input.y) } else { None };

        // Dépôt d'un échantillon : sur la ligne visée, à la position aimantée.
        if self.dnd_active() && input.inside && clip_zone.contains(input.x, input.y) {
            let lane = lane_at(input.y) as usize;
            let tick = ((tick_at(input.x) / snap).floor() * snap).max(0.0) as u32;
            let x = x_of(tick as f32);
            // La zone part du point aimanté et contient toujours le curseur.
            let zone = Rect::new(x, lanes_r.y + lane as f32 * LANE_H + 2.0, (ppt * 96.0 * 4.0).max(input.x - x + 2.0), LANE_H - 4.0);
            self.drop_zone(input, zone, DropTarget::Lane { lane, tick });
        }

        // Clips audio : prioritaires sur les clips de pattern.
        let audio_id = wid("pl-audio", 0, 0);
        let over_audio = input.inside && clip_zone.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(audio_id));
        let hover_audio = if over_audio {
            let lane = lane_at(input.y) as usize;
            self.project.audio_clips.iter().enumerate().rev().find_map(|(i, cl)| {
                let (x0, x1) = (x_of(cl.start as f32), x_of((cl.start + cl.len) as f32));
                (cl.lane == lane && input.x >= x0 && input.x <= x1).then_some((i, x1 - input.x <= EDGE))
            })
        } else {
            None
        };
        if let Some((i, edge)) = hover_audio {
            if input.pressed && self.drag.is_none() {
                self.begin_drag(audio_id, input, 0.0);
                let cl = &self.project.audio_clips[i];
                let tick0 = tick_at(input.x);
                self.clip_drag = Some(if edge {
                    ClipDrag::AudioResize { clip: i, len: cl.len, tick0 }
                } else {
                    ClipDrag::AudioMove { clip: i, start: cl.start, lane: cl.lane, tick0, lane0: lane_at(input.y) }
                });
                self.focus = super::app::Focus::Track(cl.track);
                if input.double {
                    let path = cl.path.clone();
                    self.clip_drag = None;
                    self.open_in_lab(&path, super::samplelab::Source::Clip(i));
                }
            }
            if input.right_down && self.drag.is_none() {
                self.project.audio_clips.remove(i);
                self.commit("Effacer un clip audio");
            }
        }
        if self.is_dragging(audio_id) {
            match self.clip_drag {
                Some(ClipDrag::AudioMove { clip, start, lane, tick0, lane0 }) if clip < self.project.audio_clips.len() => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    let cl = &mut self.project.audio_clips[clip];
                    let new_start = (start as f32 + dt).max(0.0) as u32;
                    let new_lane = (lane as f32 + lane_at(input.y) - lane0).clamp(0.0, 63.0) as usize;
                    if (cl.start, cl.lane) != (new_start, new_lane) {
                        cl.start = new_start;
                        cl.lane = new_lane;
                        self.commit("Déplacer un clip audio");
                    }
                }
                Some(ClipDrag::AudioResize { clip, len, tick0 }) if clip < self.project.audio_clips.len() => {
                    let dt = ((tick_at(input.x) - tick0) / 24.0).round() * 24.0;
                    let new_len = (len as f32 + dt).max(24.0) as u32;
                    if self.project.audio_clips[clip].len != new_len {
                        self.project.audio_clips[clip].len = new_len;
                        self.commit("Longueur du clip audio");
                    }
                }
                _ => {}
            }
            if input.released {
                self.clip_drag = None;
            }
        }

        if over && input.pressed && self.drag.is_none() {
            self.begin_drag(grid_id, input, 0.0);
            let tick = tick_at(input.x);
            let lane = lane_at(input.y);
            match hover_clip {
                Some((i, edge)) => {
                    self.playlist.selected = Some(i);
                    let orig = self.project.clips[i];
                    if input.double {
                        self.select_pattern(orig.pattern);
                        self.set_view(View::Rack);
                    } else if edge {
                        self.clip_drag = Some(ClipDrag::Resize { clip: i, orig, tick0: tick });
                    } else {
                        self.playlist.brush = orig.pattern;
                        self.clip_drag = Some(ClipDrag::Move { clip: i, orig, tick0: tick, lane0: lane });
                    }
                }
                None => {
                    let p = self.playlist.brush.min(self.project.patterns.len() - 1);
                    let start = ((tick / snap).floor() * snap).max(0.0) as u32;
                    let len = self.project.patterns[p].len.max(96);
                    let clip = ClipDef { pattern: p, lane: lane as usize, start, len, offset: 0 };
                    self.project.clips.push(clip);
                    let i = self.project.clips.len() - 1;
                    self.playlist.selected = Some(i);
                    self.clip_drag = Some(ClipDrag::Move { clip: i, orig: clip, tick0: tick, lane0: lane });
                    self.commit("Poser un clip");
                }
            }
        }
        // Clic droit sur la barre de titre : menu du clip ; ailleurs (ou en
        // balayant, bouton maintenu) : effacer, comme dans FL Studio.
        let on_title = |cl: &ClipDef| input.y - (lanes_r.y + cl.lane as f32 * LANE_H + 3.0) < CLIP_TITLE;
        if over && input.right_pressed && let Some((i, _)) = hover_clip && on_title(&self.project.clips[i]) {
            self.playlist.selected = Some(i);
            let cl = self.project.clips[i];
            let title = Rect::new(x_of(cl.start as f32), lanes_r.y + cl.lane as f32 * LANE_H + 3.0, 0.0, CLIP_TITLE);
            self.open_popup(Popup::ClipMenu(i), Rect::new(input.x, title.y, 0.0, title.h));
        } else if over && input.right_down && self.popup.is_none() && let Some((i, _)) = hover_clip {
            self.project.clips.remove(i);
            self.playlist.selected = None;
            self.commit("Effacer des clips");
        }
        if let Some((i, _)) = hover_clip
            && self.drag.is_none()
            && self.project.clips.get(i).is_some_and(on_title)
        {
            self.set_tip("Clic droit : rendre unique, renommer, couleur", "");
        }
        if self.is_dragging(grid_id) {
            match self.clip_drag {
                Some(ClipDrag::Move { clip, orig, tick0, lane0 }) if clip < self.project.clips.len() => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    let start = (orig.start as f32 + dt).max(0.0) as u32;
                    let lane = (orig.lane as f32 + lane_at(input.y) - lane0).clamp(0.0, 63.0) as usize;
                    let cl = &mut self.project.clips[clip];
                    if (cl.start, cl.lane) != (start, lane) {
                        cl.start = start;
                        cl.lane = lane;
                        self.commit("Déplacer un clip");
                    }
                }
                Some(ClipDrag::Resize { clip, orig, tick0 }) if clip < self.project.clips.len() => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    let len = (orig.len as f32 + dt).max(snap) as u32;
                    if self.project.clips[clip].len != len {
                        self.project.clips[clip].len = len;
                        self.commit("Longueur du clip");
                    }
                }
                _ => {}
            }
            if input.released {
                self.clip_drag = None;
            }
        }
        // Curseur : bord à étirer, clip à saisir (saisi pendant le geste).
        let gesture = if input.down && (self.is_dragging(grid_id) || self.is_dragging(audio_id)) { self.clip_drag } else { None };
        match (gesture, hover_audio.or(hover_clip)) {
            (Some(ClipDrag::Resize { .. } | ClipDrag::AudioResize { .. }), _) | (None, Some((_, true))) => self.set_cursor(Cursor::ResizeH),
            (Some(_), _) => self.set_cursor(Cursor::Grab),
            (None, Some(_)) => self.set_cursor(Cursor::Move),
            _ => {}
        }
        if self.popup.is_none() && !self.sidebar_focus {
            for &(key, mods) in &input.keys {
                let sel = self.playlist.selected.filter(|&i| i < self.project.clips.len());
                match key {
                    Key::Delete | Key::Backspace if sel.is_some() => {
                        self.project.clips.remove(sel.unwrap_or(0));
                        self.playlist.selected = None;
                        self.commit("Effacer un clip");
                    }
                    Key::Char('c') if mods.ctrl => {
                        if let Some(i) = sel {
                            self.clipboard_clip = Some(self.project.clips[i]);
                            self.toast("Clip copié");
                        }
                    }
                    Key::Char('v') if mods.ctrl => {
                        if let Some(mut clip) = self.clipboard_clip {
                            clip.start = ((self.start_marker as f32 / snap).round() * snap) as u32;
                            self.project.clips.push(clip);
                            self.playlist.selected = Some(self.project.clips.len() - 1);
                            self.commit("Coller un clip");
                        }
                    }
                    Key::Char('b') if mods.ctrl && matches!(self.time_selection, Some((View::Playlist, _, _))) => {
                        self.duplicate_time();
                    }
                    Key::Char('b') if mods.ctrl => {
                        if let Some(i) = sel {
                            let mut clip = self.project.clips[i];
                            clip.start += clip.len;
                            self.project.clips.push(clip);
                            self.playlist.selected = Some(self.project.clips.len() - 1);
                            self.commit("Dupliquer un clip");
                        }
                    }
                    Key::Char('d') if mods.ctrl => self.playlist.selected = None,
                    _ => {}
                }
            }
        }

        for (i, cl) in self.project.clips.clone().iter().enumerate() {
            let r = Rect::new(
                x_of(cl.start as f32) + 1.0,
                lanes_r.y + cl.lane as f32 * LANE_H + 3.0,
                (cl.len as f32 * ppt - 2.0).max(4.0),
                LANE_H - 6.0,
            );
            if r.right() < area.x || r.x > area.right() || r.y > area.bottom() {
                continue;
            }
            let col = self.pattern_tint(cl.pattern);
            let selected = self.playlist.selected == Some(i);
            let hov = hover_clip.is_some_and(|(h, _)| h == i);
            let hv = self.hover_anim(wid("pl-clip", i, 0), hov);
            c.fill_rrect(r, 8.0, col.mix(t.panel, 0.72 - hv * 0.08));
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 18.0), col.mix(t.panel, 0.45).alpha(0.9));
            if selected {
                c.stroke_rrect(r, 8.0, 1.5, t.select);
            }
            let prev_clip = c.push_clip(r);
            let name = self.project.patterns.get(cl.pattern).map_or(String::new(), |p| p.name.clone());
            self.text_fit(c, self.f.sans_med, 11.0, r.x + 8.0, r.y + 10.0, &name, r.w - 12.0, t.text);
            // Aperçu des notes, répété sur la longueur du clip.
            if let Some(p) = self.project.patterns.get(cl.pattern) {
                let all: Vec<_> = p.notes.iter().flatten().collect();
                if !all.is_empty() {
                    let (lo, hi) = all.iter().fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.key), hi.max(n.key)));
                    let span = (hi - lo).max(6) as f32;
                    let area_y = r.y + 22.0;
                    let area_h = r.h - 26.0;
                    let plen = p.len.max(1);
                    let mut rep = 0;
                    while rep * plen < cl.len {
                        for n in &all {
                            let start = rep * plen + n.start;
                            if start < cl.offset || start - cl.offset >= cl.len {
                                continue;
                            }
                            let x = r.x + (start - cl.offset) as f32 * ppt;
                            let y = area_y + area_h - ((n.key - lo) as f32 / span) * area_h;
                            c.fill_rect(Rect::new(x, y - 1.0, (n.len as f32 * ppt).max(1.5), 2.0), t.text.alpha(0.55));
                        }
                        rep += 1;
                    }
                }
            }
            // Chevron du menu, au survol de la barre de titre.
            let title_hov = hov && input.y - r.y < CLIP_TITLE;
            let ch = self.anim.to(wid("pl-clip-menu", i, 0), title_hov as u8 as f32, T_HOVER);
            if ch > 0.01 && r.w > 40.0 {
                c.fill_rect(Rect::new(r.right() - 22.0, r.y + 1.0, 21.0, 17.0), col.mix(t.panel, 0.45));
                draw_icon(c, Icon::ChevronDown, r.right() - 11.0, r.y + 10.0, 11.0, t.text.alpha(ch));
            }
            c.set_clip(Some(prev_clip));
            if hover_clip.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 5.0, r.y + 8.0, 2.0, r.h - 16.0), 1.0, t.text.alpha(0.7));
            }
        }

        // Clips audio, avec leur forme d'onde.
        let bpm = self.project.bpm as f64;
        for (i, cl) in self.project.audio_clips.clone().iter().enumerate() {
            let r = Rect::new(
                x_of(cl.start as f32) + 1.0,
                lanes_r.y + cl.lane as f32 * LANE_H + 3.0,
                (cl.len as f32 * ppt - 2.0).max(4.0),
                LANE_H - 6.0,
            );
            if r.right() < area.x || r.x > area.right() || r.y > area.bottom() {
                continue;
            }
            let hov = hover_audio.is_some_and(|(h, _)| h == i);
            let hv = self.hover_anim(wid("pl-aclip", i, 0), hov);
            c.fill_rrect(r, 8.0, t.select.mix(t.panel, 0.78 - hv * 0.08));
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 18.0), t.select.mix(t.panel, 0.5).alpha(0.9));
            let prev_clip = c.push_clip(r);
            let name = std::path::Path::new(&cl.path).file_stem().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            self.text_fit(c, self.f.sans_med, 11.0, r.x + 8.0, r.y + 10.0, &name, r.w - 12.0, t.text);
            let sr = self.samples.get(&cl.path).map_or(48_000.0, |s| s.sample_rate as f64);
            if let Some(peaks) = self.waveform(&cl.path) {
                let mid = r.y + 22.0 + (r.h - 24.0) * 0.5;
                let amp = (r.h - 26.0) * 0.5;
                let x0 = r.x.max(area.x) as i32;
                let x1 = r.right().min(area.right()) as i32;
                for px in x0..x1 {
                    let tick_a = tick_at(px as f32) as f64 - cl.start as f64;
                    let tick_b = tick_a + 1.0 / ppt as f64;
                    let secs = |tk: f64| tk / ponzik_engine::PPQ as f64 * 60.0 / bpm + cl.offset;
                    let (ba, bb) = ((secs(tick_a) * sr) as usize / super::app::WAVE_BUCKET, (secs(tick_b) * sr) as usize / super::app::WAVE_BUCKET);
                    let (mut lo, mut hi) = (0.0f32, 0.0f32);
                    for b in ba..=bb.max(ba) {
                        if let Some(&(l, h)) = peaks.get(b) {
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if hi > lo {
                        c.fill_rect(Rect::new(px as f32, mid - hi * amp, 1.0, ((hi - lo) * amp).max(1.0)), t.text.alpha(0.6));
                    }
                }
            }
            c.set_clip(Some(prev_clip));
            if hover_audio.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 5.0, r.y + 8.0, 2.0, r.h - 16.0), 1.0, t.text.alpha(0.7));
            }
        }

        // Couloirs d'automation, sous les clips.
        let auto_y = lanes_r.y + lanes as f32 * LANE_H + 4.0;
        if autos > 0 {
            c.fill_rect(Rect::new(area.x, auto_y - 3.0, area.w, 1.0), t.hairline.alpha(2.0));
            self.automation_lanes(c, input, area, auto_y, &x_of, &tick_at, snap.min(96.0));
        }

        // Tête de lecture.
        if self.project.mode == PlayMode::Song {
            let x = x_of(self.position as f32);
            c.fill_rect(Rect::new(x.round() - 1.0, ruler.y, 2.0, area.h), t.select);
        }
        c.set_clip(Some(prev));
    }
}
