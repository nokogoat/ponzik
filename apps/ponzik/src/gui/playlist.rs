//! Playlist : les patterns posés dans le temps pour construire le morceau.

use ponzik_engine::{PlayMode, TICKS_PER_BAR};
use ponzik_project::ClipDef;
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{App, View};
use super::theme::*;
use super::widgets::{Btn, wid};

const PALETTE_W: f32 = 196.0;
const RULER: f32 = 30.0;
const LANE_H: f32 = 52.0;
const EDGE: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
pub enum ClipDrag {
    Move { clip: usize, orig: ClipDef, tick0: f32, lane0: f32 },
    Resize { clip: usize, orig: ClipDef, tick0: f32 },
}

/// Couleur d'un pattern : une famille vert-sarcelle, une teinte par pattern.
pub fn pattern_color(i: usize) -> Color {
    const P: [u32; 8] = [0x3fd45a, 0x3ccfc4, 0x9be15d, 0x2fb6e0, 0x67e8a4, 0xc7e85a, 0x5ad1e8, 0x8ce07a];
    Color::hex(P[i % P.len()])
}

impl App {
    pub(super) fn playlist_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let snap_box = Rect::new(header.right() - 200.0, header.y, 200.0, header.h);
        let snap_key = wid("pl-snap-v", 0, 0);
        let snap_sel = self.anim.get(snap_key).unwrap_or(0.0) as usize;
        if let Some(i) = self.segmented(c, input, wid("pl-snap", 0, 0), snap_box, &["Mesure", "Temps"], snap_sel) {
            self.anim.set(snap_key, i as f32);
        }
        let snap = if self.anim.get(snap_key).unwrap_or(0.0) as usize == 0 { TICKS_PER_BAR } else { 96 } as f32;
        if self.project.mode != PlayMode::Song {
            let hint_room = snap_box.x - header.x - 16.0;
            self.text_fit(c, self.f.sans, 12.0, header.x, header.y + header.h * 0.5, "Passe en mode Morceau pour écouter la playlist", hint_room, t.muted);
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);

        // Palette de patterns : le pattern choisi est celui qu'on pose.
        let palette = Rect::new(inner.x, inner.y, PALETTE_W - 12.0, inner.h);
        self.text(c, self.f.sans_med, 12.0, palette.x + 4.0, palette.y + 10.0, "Patterns", t.muted);
        let mut y = palette.y + 28.0;
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
            c.fill_rrect(Rect::new(row.x + 10.0, row.y + 12.0, 4.0, 16.0), 2.0, pattern_color(p));
            let name = self.project.patterns[p].name.clone();
            self.text_fit(c, self.f.sans_med, 13.0, row.x + 24.0, row.y + 20.0, &name, row.w - 36.0, if current { t.text } else { t.text2 });
            if hov && input.pressed {
                self.playlist.brush = p;
                if input.double {
                    self.select_pattern(p);
                    self.set_view(View::Rack);
                }
            }
            y += 44.0;
        }
        let add = Rect::new(palette.x, (y + 4.0).min(palette.bottom() - 36.0), palette.w, 36.0);
        if self.button(c, input, wid("pl-add-pat", 0, 0), add, "+ Nouveau pattern", Btn::Ghost) {
            let p = self.project.add_pattern();
            self.playlist.brush = p;
            self.select_pattern(p);
        }

        // Ligne de temps.
        let area = Rect::new(inner.x + PALETTE_W, inner.y, inner.w - PALETTE_W, inner.h);
        let ruler = Rect::new(area.x, area.y, area.w, RULER);
        let lanes_r = Rect::new(area.x, area.y + RULER + 4.0, area.w, area.h - RULER - 4.0);
        c.fill_rrect(Rect::new(area.x - 1.0, area.y, area.w + 1.0, area.h), 12.0, t.well);
        let lanes_used = self.project.clips.iter().map(|c| c.lane + 1).max().unwrap_or(0);
        let lanes = (lanes_used + 2).max((lanes_r.h / LANE_H).floor() as usize).min(64);

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

        // Clips.
        let grid_id = wid("pl-grid", 0, 0);
        let over = input.inside && lanes_r.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(grid_id));
        let hit = |app: &App, x: f32, y: f32| -> Option<(usize, bool)> {
            let lane = lane_at(y) as usize;
            app.project.clips.iter().enumerate().rev().find_map(|(i, cl)| {
                let (x0, x1) = (x_of(cl.start as f32), x_of((cl.start + cl.len) as f32));
                (cl.lane == lane && x >= x0 && x <= x1).then_some((i, x1 - x <= EDGE))
            })
        };
        let hover_clip = if over { hit(self, input.x, input.y) } else { None };

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
        if over && input.right_down && let Some((i, _)) = hover_clip {
            self.project.clips.remove(i);
            self.playlist.selected = None;
            self.commit("Effacer des clips");
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
            let col = pattern_color(cl.pattern);
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
            c.set_clip(Some(prev_clip));
            if hover_clip.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 5.0, r.y + 8.0, 2.0, r.h - 16.0), 1.0, t.text.alpha(0.7));
            }
        }

        // Tête de lecture.
        if self.project.mode == PlayMode::Song {
            let x = x_of(self.position as f32);
            c.fill_rect(Rect::new(x.round() - 1.0, ruler.y, 2.0, area.h), t.select);
        }
        c.set_clip(Some(prev));
    }
}
