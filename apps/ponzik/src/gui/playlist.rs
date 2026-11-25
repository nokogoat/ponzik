//! Playlist : les patterns posés dans le temps pour construire le morceau.

use ponzik_engine::{PlayMode, TICKS_PER_BAR};
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n, tr_noop};
use ponzik_project::ClipDef;
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::arrange::{ARRANGE_H, TEMPO_H};
use super::app::{App, Popup, Rename, View};
use super::lessons::Spot;
use super::split::{Axis, PATTERNS_SPAN, Split, TRACK_SPAN, splitter_id};
use super::clip::ClipSel;
use super::dnd::DropTarget;
use super::theme::*;
use super::widgets::{Icon, draw_icon, hatch, wid};

const RULER: f32 = 30.0;
const EDGE: f32 = 8.0;
const PLAYLIST_HELP: &str = tr_noop("Playlist
Click: place the chosen pattern · edge: length, a loop repeats
Right-click: erase · on the title: clip menu (loop to the end, ×4…)
Double-click: edit · Ctrl+B: duplicate · B: paint brush
Ruler: start, loop and time selection
Top lane: sections · under the ruler: tempo and meter
Wheel: scroll · Ctrl: zoom · middle: pan");
/// Hauteur de la barre de titre d'un clip (clic droit : menu).
const CLIP_TITLE: f32 = 19.0;

#[derive(Clone, Copy, Debug)]
pub enum ClipDrag {
    Move { clip: usize, orig: ClipDef, tick0: f32, lane0: f32 },
    Resize { clip: usize, orig: ClipDef, tick0: f32 },
    /// Clip audio : (indice, début, ligne ou durée d'origine, point de départ).
    AudioMove { clip: usize, start: u32, lane: usize, tick0: f32, lane0: f32 },
    AudioResize { clip: usize, len: u32, tick0: f32 },
    /// Paint brush: copies of `src` every `period` ticks from `start`.
    Paint { src: ClipSel, start: u32, period: u32, count: usize },
}

/// Couleur d'un pattern : une famille vert-sarcelle, une teinte par pattern.
/// Theme tokens `--pattern-1` to `--pattern-8`.
pub fn pattern_color(i: usize) -> Color {
    super::theme::pattern_palette(i)
}

/// Couleurs proposées pour un pattern : la famille de Ponzik, puis des teintes
/// chaudes pour distinguer les sections. Pas de rouge : il dit l'enregistrement.
pub const PATTERN_COLORS: [(&str, u32); 12] = [
    (tr_noop("Green"), 0x3fd45a),
    (tr_noop("Teal"), 0x3ccfc4),
    (tr_noop("Lime"), 0x9be15d),
    (tr_noop("Sky"), 0x2fb6e0),
    (tr_noop("Mint"), 0x67e8a4),
    (tr_noop("Lemon"), 0xc7e85a),
    (tr_noop("Amber"), 0xf0b44a),
    (tr_noop("Coral"), 0xf08a5d),
    (tr_noop("Pink"), 0xe878b4),
    (tr_noop("Lilac"), 0xa98cf0),
    (tr_noop("Indigo"), 0x6b7cf0),
    (tr_noop("Slate"), 0x8a9aa6),
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
            self.commit(tr("Make unique"));
            self.toast(&tr_fmt!("Clip made unique: {}", name));
        }
    }

    /// Ctrl+B sur une sélection de temps : tout son contenu est recopié juste
    /// après, et la sélection (et la boucle) avance d'autant pour enchaîner.
    pub fn duplicate_time(&mut self) {
        let Some((View::Playlist, a, b)) = self.time_selection else {
            self.toast(tr("Drag a time selection in the ruler first"));
            return;
        };
        let n = self.project.duplicate_time(a, b);
        // Les clips ont pu être coupés : les indices ne tiennent plus.
        self.playlist.selected = None;
        let span = b - a;
        self.time_selection = Some((View::Playlist, b, b + span));
        self.set_loop(View::Playlist, Some((b, b + span)));
        self.start_marker = b as f64;
        self.commit(tr("Duplicate time selection"));
        let map = self.project.tempo_map();
        let (ba, bb) = (map.bar_beat(a as f64), map.bar_beat(b as f64));
        let whole = ba.beat == 0 && bb.beat == 0 && ba.tick_in_beat == 0.0 && bb.tick_in_beat == 0.0;
        let bars = if whole { (bb.bar - ba.bar).to_string() } else { ponzik_i18n::num(span as f64 / TICKS_PER_BAR as f64, 2) };
        self.toast(&tr_fmt_n!("{} clip duplicated · {} bars", "{} clips duplicated · {} bars", n, n, bars));
    }

    pub(super) fn playlist_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let lane_h = self.layout.sizes.track_h;
        self.track_height_buttons(c, input, Rect::new(header.x, header.y, 62.0, header.h));
        let snap_box = Rect::new(header.right() - 120.0, header.y, 120.0, header.h);
        let snap_key = wid("pl-snap-v", 0, 0);
        let snap_sel = self.anim.get(snap_key).unwrap_or(0.0) as usize;
        let tips = [(tr("Snap to bar"), ""), (tr("Snap to beat"), "")];
        if let Some(i) = self.segmented_tips(c, input, wid("pl-snap", 0, 0), snap_box, &["1", "1/4"], &tips, snap_sel) {
            self.anim.set(snap_key, i as f32);
        }
        draw_icon(c, Icon::Magnet, snap_box.x - 18.0, header.y + header.h * 0.5, 15.0, t.muted);
        // Snapping follows the time signature map: to bars, or to beats.
        let bar_snap = self.anim.get(snap_key).unwrap_or(0.0) as usize == 0;
        let map = self.project.tempo_map();
        let snap_round = |t: f32| -> f32 {
            let t = t.max(0.0) as f64;
            (if bar_snap { map.snap_bar(t) } else { map.snap_beat(t) }) as f32
        };
        let snap_floor = |t: f32| -> f32 {
            let bb = map.bar_beat(t.max(0.0) as f64);
            if bar_snap { map.bar_start(bb.bar) as f32 } else { (t.max(0.0) as f64 - bb.tick_in_beat) as f32 }
        };
        // Grid unit at a tick (shortest clip length).
        let unit = |t: f32| -> f32 {
            let sig = map.sig_at(t as f64);
            (if bar_snap { sig.bar_len() } else { sig.beat_len() }) as f32
        };
        let help = Rect::new(snap_box.x - 74.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("pl-help", 0, 0), help, Icon::Info, false, (tr(PLAYLIST_HELP), ""));
        // Paint brush: drag along a lane to lay copies of the clip.
        let paint = Rect::new(help.x - 44.0, header.y, 36.0, header.h);
        let tip = (tr("Paint brush\nDrag along a lane: copies of the pattern, or of the audio clip selected"), "B");
        if self.icon_button(c, input, wid("pl-paint", 0, 0), paint, Icon::Brush, self.playlist.paint, tip) {
            self.toggle_paint();
        }
        // Performance mode: its switch, then its controls when on.
        let left = self.perf_header(c, input, header, paint.x - 8.0);
        // Hors mode morceau, la playlist ne joue pas : un bouton pour y passer.
        if self.project.mode != PlayMode::Song && !self.perf_on() {
            let song = Rect::new(left - 36.0, header.y, 36.0, header.h);
            let glow = 0.5 + 0.5 * (self.time * 3.0).sin();
            c.fill_rrect(song.inset(-2.0), 10.0, t.accent.alpha(0.08 + 0.06 * glow));
            if self.icon_button(c, input, wid("pl-song", 0, 0), song, Icon::Playlist, false, (tr("Play song"), "L")) {
                self.set_mode(PlayMode::Song);
            }
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);

        // Palette de patterns : le pattern choisi est celui qu'on pose ; sa
        // largeur se tire, et la palette se replie au bord.
        let sizes = &self.layout.sizes;
        let pw_want = if sizes.patterns_open { PATTERNS_SPAN.fit(sizes.patterns_w).min(inner.w * 0.4) } else { 0.0 };
        let pw = self.anim.to(wid("pl-palette-w", 0, 0), pw_want, T_LAYOUT).round();
        let palette_w = if pw > 1.0 { pw } else { 14.0 };
        let palette = Rect::new(inner.x, inner.y, (pw - 12.0).max(0.0), inner.h);
        let palette_clip = c.push_clip(Rect::new(palette.x, palette.y, palette.w, palette.h));
        let mut y = palette.y + 4.0;
        for p in 0..self.project.patterns.len() {
            let row = Rect::new(palette.x, y, palette.w, 40.0);
            if row.bottom() > palette.bottom() - 44.0 {
                break;
            }
            let id = wid("pl-pat", p, 0);
            self.drop_zone(input, row, super::dnd::DropTarget::Pattern(p));
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
                self.set_tip(tr("Click: brush · double-click: rename\nRight-click: color, duplicate, delete"), "");
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
            if hv > 0.01 && self.icon_button(c, input, wid("pl-pat-edit", p, 0), edit, Icon::Rack, false, (tr("Edit"), &self.key_label(super::keymap::Shortcut::ViewRack))) {
                self.select_pattern(p);
                self.set_view(View::Rack);
            }
            y += 44.0;
        }
        let add = Rect::new(palette.x, (y + 4.0).min(palette.bottom() - 40.0), palette.w, 40.0);
        self.spot(Spot::PlaylistPatterns, Rect::new(palette.x, palette.y, palette.w, (y - 4.0).min(palette.bottom()) - palette.y));
        let id = wid("pl-add-pat", 0, 0);
        let hov = self.hovered(input, id, add);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(add, 10.0, t.raised.alpha(hv * 0.6));
        c.stroke_rrect(add, 10.0, 1.0, t.hairline.alpha(1.0 + hv * 1.5));
        let (ax, ay) = add.center();
        draw_icon(c, Icon::Plus, ax, ay, 16.0, t.muted.mix(t.accent, hv));
        if hov {
            let key = self.key_label(super::keymap::Shortcut::NewPattern);
            self.set_tip(tr("New pattern"), &key);
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                let p = self.project.add_pattern();
                self.playlist.brush = p;
                self.select_pattern(p);
            }
        }

        c.set_clip(Some(palette_clip));
        self.palette_splitter(c, input, inner, pw);

        // Ligne de temps.
        let area = Rect::new(inner.x + palette_w, inner.y, inner.w - palette_w, inner.h);
        let arrange_r = Rect::new(area.x, area.y, area.w, ARRANGE_H);
        let ruler = Rect::new(area.x, arrange_r.bottom(), area.w, RULER);
        let tempo_r = Rect::new(area.x, ruler.bottom(), area.w, TEMPO_H);
        let top = ARRANGE_H + RULER + TEMPO_H + 4.0;
        let lanes_r = Rect::new(area.x, area.y + top, area.w, area.h - top);
        self.spot(Spot::Arrange, arrange_r);
        self.spot(Spot::PlaylistGrid, lanes_r);
        c.fill_rrect(Rect::new(area.x - 1.0, area.y, area.w + 1.0, area.h), 12.0, t.lane_bg);
        let lanes_used = self
            .project
            .clips
            .iter()
            .map(|c| c.lane + 1)
            .chain(self.project.audio_clips.iter().map(|c| c.lane + 1))
            .chain(self.project.take_groups.iter().map(|g| g.lane + 1))
            .max()
            .unwrap_or(0);
        let autos = self.song_lane_targets().len();
        let fit = ((lanes_r.h - autos as f32 * super::automation::AUTO_LANE_H) / lane_h).floor().max(1.0) as usize;
        let lanes = (lanes_used + 1).max(fit).min(ponzik_project::PLAYLIST_LANES);

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
            if input.mods.alt {
                // Alt + wheel: taller or shorter tracks, the one under the pointer staying.
                let k = 1.12f32.powf(input.wheel);
                let raw = (lane_h * k).round();
                let new = TRACK_SPAN.fit(if k > 1.0 { raw.max(lane_h + 1.0) } else { raw.min(lane_h - 1.0) });
                let at = (input.y - lanes_r.y + self.playlist.scroll_y) / lane_h;
                self.layout.sizes.track_h = new;
                self.playlist.scroll_y = (at * new - (input.y - lanes_r.y)).max(0.0);
                self.anim.set(wid("pl-sy", 0, 0), self.playlist.scroll_y);
            } else if input.mods.shift && !input.mods.ctrl {
                self.playlist.scroll_y -= input.wheel * lane_h;
            } else if input.mods.ctrl {
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
        // Lanes: unfolded take groups add their take lanes under theirs.
        // Taller than the view (many lanes, automation): Shift + wheel scrolls them.
        let content_h = self.lane_layout(0.0, lanes).bottom() + autos as f32 * super::automation::AUTO_LANE_H + 8.0;
        let max_sy = (content_h - lanes_r.h).max(0.0);
        self.playlist.scroll_y = self.playlist.scroll_y.clamp(0.0, max_sy);
        let sy = self.anim.to(wid("pl-sy", 0, 0), self.playlist.scroll_y, T_LAYOUT);
        let layout = self.lane_layout(lanes_r.y - sy, lanes);
        // Lanes scrolled under the ruler take no clicks.
        let masked;
        let lane_input = if sy > 0.0 && input.y < lanes_r.y {
            masked = Input { pressed: false, right_pressed: false, double: false, wheel: 0.0, ..input.clone() };
            &masked
        } else {
            input
        };
        let lane_at = |y: f32| layout.lane_at(y);
        let lane_y = |l: usize| layout.y(l);

        let prev = c.push_clip(area);
        // Fond des pistes et grille.
        let stripes = c.push_clip(lanes_r);
        for l in 0..lanes {
            let ly = lane_y(l);
            if l % 2 == 1 {
                c.fill_rect(Rect::new(area.x, ly, area.w, lane_h), t.lane_alt);
            }
            let takes = lane_y(l + 1) - ly - lane_h;
            if takes > 0.0 {
                c.fill_rect(Rect::new(area.x, ly + lane_h, area.w, takes), t.bg_deep.alpha(0.35));
            }
        }
        c.set_clip(Some(stripes));
        // Bars and beats of the time signature map.
        let song_end = self.project.song_len() as f32;
        let (view_a, view_b) = (sx.max(0.0) as f64, (sx + area.w / ppt) as f64);
        if ppt * 48.0 >= 9.0 {
            map.for_each_beat(view_a, view_b, |tick, down| {
                if !down {
                    let x = x_of(tick as f32).round();
                    c.fill_rect(Rect::new(x, lanes_r.y, 1.0, lanes_r.h), t.playlist_grid.alpha(0.45));
                }
            });
        }
        let mut bars = Vec::new();
        map.for_each_bar(view_a, view_b, |b, tick| bars.push((b, tick)));
        for (b, tick) in bars {
            let x = x_of(tick as f32).round();
            let strong = b % 4 == 0;
            c.fill_rect(Rect::new(x, lanes_r.y, 1.0, lanes_r.h), t.playlist_grid.alpha(if strong { 2.5 } else { 1.0 }));
            if self.playlist.zoom >= 28.0 || strong {
                self.text(c, self.f.mono, 11.0, x + 6.0, ruler.y + 15.0, &(b + 1).to_string(), if strong { t.text2 } else { t.muted });
            }
        }
        // Section starts, through the lanes.
        for i in 0..self.project.markers.len() {
            let x = x_of(self.project.markers[i].tick as f32).round();
            c.fill_rect(Rect::new(x, lanes_r.y, 1.0, lanes_r.h), self.marker_color(i).alpha(0.45));
        }
        let end_x = x_of(song_end);
        c.fill_rect(Rect::new(end_x.round(), ruler.y + 6.0, 2.0, area.h - 6.0), t.text2.alpha(0.5));

        // Règle : clic = marqueur de départ, glisser = sélection de temps.
        let beat_snap = |t: f32| map.snap_beat(t.max(0.0) as f64) as f32;
        if let Some((a, b)) = self.ruler(c, input, View::Playlist, ruler, lanes_r, &beat_snap, &x_of, &tick_at) {
            self.playlist.selected = self.project.clips.iter().position(|cl| cl.start >= a && cl.start < b);
        }
        self.arrangement_lane(c, input, arrange_r, lanes_r, &map, &x_of, &tick_at);
        self.tempo_lane(c, input, tempo_r, ppt, &x_of, &tick_at);
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
            self.tip_zone(input, Rect::new(xa, ruler.y, xb - xa, ruler.h), tr("Time selection\nCtrl+B: copy it right after"), "Ctrl B");
        }

        // Clips.
        let grid_id = wid("pl-grid", 0, 0);
        let clip_zone = Rect::new(lanes_r.x, lanes_r.y, lanes_r.w, (layout.bottom() - lanes_r.y).clamp(0.0, lanes_r.h));
        let lanes_clip = c.push_clip(lanes_r);
        // Performance mode: the pads take their clicks first (Shift: edit).
        let perf_taken = self.perf_input(input, lanes_r, lane_h, &lane_at, &lane_y, &x_of, &tick_at);
        let perf_masked;
        let input = if perf_taken {
            perf_masked = Input { pressed: false, double: false, ..input.clone() };
            &perf_masked
        } else {
            input
        };
        let lane_input = if perf_taken { input } else { lane_input };
        // Take groups first: their lanes keep the pointer from the clips.
        let comp_hot = self.comp_input(lane_input, &layout, area, &x_of, &tick_at, &snap_round);
        let over = !comp_hot && input.inside && clip_zone.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(grid_id));
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
            let tick = snap_floor(tick_at(input.x)) as u32;
            let x = x_of(tick as f32);
            // La zone part du point aimanté et contient toujours le curseur.
            let zone = Rect::new(x, lane_y(lane) + 2.0, (ppt * 96.0 * 4.0).max(input.x - x + 2.0), lane_h - 4.0);
            self.drop_zone(input, zone, DropTarget::Lane { lane, tick });
        }

        // Clips audio : prioritaires sur les clips de pattern.
        let audio_id = wid("pl-audio", 0, 0);
        let over_audio = !comp_hot && input.inside && clip_zone.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(audio_id));
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
            if input.pressed && input.mods.alt && self.drag.is_none() {
                self.choose_take(i);
            } else if input.pressed && self.drag.is_none() {
                self.comp.selected = None;
                self.begin_drag(audio_id, input, 0.0);
                let cl = &self.project.audio_clips[i];
                let tick0 = tick_at(input.x);
                self.clip_drag = Some(if edge {
                    ClipDrag::AudioResize { clip: i, len: cl.len, tick0 }
                } else {
                    ClipDrag::AudioMove { clip: i, start: cl.start, lane: cl.lane, tick0, lane0: lane_at(input.y) }
                });
                self.focus = super::app::Focus::Track(cl.track);
                let path = cl.path.clone();
                self.select_clip(ClipSel::Audio(i));
                if input.double {
                    self.clip_drag = None;
                    self.open_in_lab(&path, super::samplelab::Source::Clip(i));
                }
            }
            // Right-click on the title: its menu; elsewhere (or sweeping): erase.
            let cl = &self.project.audio_clips[i];
            let title_y = lane_y(cl.lane) + 3.0;
            if input.right_pressed && self.drag.is_none() && input.y - title_y < CLIP_TITLE {
                self.select_clip(ClipSel::Audio(i));
                self.open_popup(Popup::AudioClipMenu(i), Rect::new(input.x, title_y, 0.0, CLIP_TITLE));
            } else if input.right_down && self.drag.is_none() && self.popup.is_none() {
                self.project.audio_clips.remove(i);
                self.audio_clip_removed(i);
                self.commit(tr("Delete audio clip"));
            }
        }
        if self.is_dragging(audio_id) {
            match self.clip_drag {
                Some(ClipDrag::AudioMove { clip, start, lane, tick0, lane0 }) if clip < self.project.audio_clips.len() => {
                    let new_start = snap_round(start as f32 + tick_at(input.x) - tick0) as u32;
                    let cl = &mut self.project.audio_clips[clip];
                    let new_lane = (lane as f32 + lane_at(input.y) - lane0).clamp(0.0, (ponzik_project::PLAYLIST_LANES - 1) as f32) as usize;
                    if (cl.start, cl.lane) != (new_start, new_lane) {
                        cl.start = new_start;
                        cl.lane = new_lane;
                        self.commit(tr("Move audio clip"));
                    }
                }
                Some(ClipDrag::AudioResize { clip, len, tick0 }) if clip < self.project.audio_clips.len() => {
                    let cl = &self.project.audio_clips[clip];
                    let new_len = if cl.looped {
                        // Repetitions land on the grid, like pattern clips.
                        let end = snap_round((cl.start + len) as f32 + tick_at(input.x) - tick0);
                        (end - cl.start as f32).max(unit(cl.start as f32)) as u32
                    } else {
                        let dt = ((tick_at(input.x) - tick0) / 24.0).round() * 24.0;
                        (len as f32 + dt).max(24.0) as u32
                    };
                    // Past the end of the file: the clip loops instead of going silent.
                    let duration = self.samples.get(&cl.path).map_or(0.0, |s| s.duration() as f64);
                    let past_end = !cl.looped && duration > 0.0 && new_len > cl.natural_len(duration, &map) + 12;
                    if past_end {
                        self.project.audio_clips[clip].looped = true;
                        self.toast(tr("Loop on: the clip repeats as you drag"));
                    }
                    if self.project.audio_clips[clip].len != new_len || past_end {
                        self.hint_seen(super::hints::Hint::DragToRepeat);
                        self.project.audio_clips[clip].len = new_len;
                        self.commit(tr("Audio clip length"));
                    }
                }
                _ => {}
            }
            if input.released {
                self.clip_drag = None;
            }
        }

        if over && input.pressed && self.drag.is_none() {
            self.comp.selected = None;
            self.begin_drag(grid_id, input, 0.0);
            let tick = tick_at(input.x);
            let lane = lane_at(input.y);
            match hover_clip {
                Some((i, edge)) => {
                    self.playlist.selected = Some(i);
                    self.select_clip(ClipSel::Pattern(i));
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
                None if self.playlist.paint => {
                    self.clip_drag = self.paint_begin(lane as usize, snap_floor(tick) as u32);
                }
                None => {
                    let p = self.playlist.brush.min(self.project.patterns.len() - 1);
                    let start = snap_floor(tick) as u32;
                    let len = self.project.patterns[p].len.max(96);
                    let clip = ClipDef { pattern: p, lane: lane as usize, start, len, offset: 0, muted: false };
                    self.project.clips.push(clip);
                    let i = self.project.clips.len() - 1;
                    self.playlist.selected = Some(i);
                    self.select_clip(ClipSel::Pattern(i));
                    self.clip_drag = Some(ClipDrag::Move { clip: i, orig: clip, tick0: tick, lane0: lane });
                    self.commit(tr("Place clip"));
                }
            }
        }
        // Clic droit sur la barre de titre : menu du clip ; ailleurs (ou en
        // balayant, bouton maintenu) : effacer, comme dans FL Studio.
        let on_title = |cl: &ClipDef| input.y - (lane_y(cl.lane) + 3.0) < CLIP_TITLE;
        if over && input.right_pressed && let Some((i, _)) = hover_clip && on_title(&self.project.clips[i]) {
            self.playlist.selected = Some(i);
            let cl = self.project.clips[i];
            let title = Rect::new(x_of(cl.start as f32), lane_y(cl.lane) + 3.0, 0.0, CLIP_TITLE);
            self.open_popup(Popup::ClipMenu(i), Rect::new(input.x, title.y, 0.0, title.h));
        } else if over && input.right_down && self.popup.is_none() && let Some((i, _)) = hover_clip {
            self.project.clips.remove(i);
            self.playlist.selected = None;
            self.clip_panel = None;
            self.commit(tr("Delete clips"));
        }
        if let Some((i, _)) = hover_clip
            && self.drag.is_none()
            && self.project.clips.get(i).is_some_and(on_title)
        {
            self.set_tip(tr("Right-click: make unique, rename, color"), "");
        }
        if self.is_dragging(grid_id) {
            match self.clip_drag {
                Some(ClipDrag::Move { clip, orig, tick0, lane0 }) if clip < self.project.clips.len() => {
                    let start = snap_round(orig.start as f32 + tick_at(input.x) - tick0) as u32;
                    let lane = (orig.lane as f32 + lane_at(input.y) - lane0).clamp(0.0, (ponzik_project::PLAYLIST_LANES - 1) as f32) as usize;
                    let cl = &mut self.project.clips[clip];
                    if (cl.start, cl.lane) != (start, lane) {
                        cl.start = start;
                        cl.lane = lane;
                        self.commit(tr("Move clip"));
                    }
                }
                Some(ClipDrag::Paint { src, start, period, count }) => {
                    let count = self.paint_to(src, start, period, count, tick_at(input.x));
                    self.clip_drag = Some(ClipDrag::Paint { src, start, period, count });
                }
                Some(ClipDrag::Resize { clip, orig, tick0 }) if clip < self.project.clips.len() => {
                    let end = snap_round((orig.start + orig.len) as f32 + tick_at(input.x) - tick0);
                    let len = (end - orig.start as f32).max(unit(orig.start as f32)) as u32;
                    if self.project.clips[clip].len != len {
                        self.hint_seen(super::hints::Hint::DragToRepeat);
                        self.project.clips[clip].len = len;
                        self.commit(tr("Clip length"));
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
            (Some(ClipDrag::Paint { .. }), _) => self.set_cursor(Cursor::Crosshair),
            (Some(_), _) => self.set_cursor(Cursor::Grab),
            (None, Some(_)) => self.set_cursor(Cursor::Move),
            _ if over && self.playlist.paint => self.set_cursor(Cursor::Crosshair),
            _ => {}
        }
        if self.popup.is_none() && !self.sidebar_focus {
            self.comp_keys(input);
            for &(key, mods) in &input.keys {
                let sel = self.playlist.selected.filter(|&i| i < self.project.clips.len());
                let audio_sel = match self.clip_panel_shown() {
                    Some(ClipSel::Audio(i)) => Some(i),
                    _ => None,
                };
                match key {
                    Key::Delete | Key::Backspace if audio_sel.is_some() => {
                        let i = audio_sel.unwrap_or(0);
                        self.project.audio_clips.remove(i);
                        self.audio_clip_removed(i);
                        self.commit(tr("Delete audio clip"));
                    }
                    Key::Delete | Key::Backspace if sel.is_some() => {
                        self.project.clips.remove(sel.unwrap_or(0));
                        self.playlist.selected = None;
                        self.clip_panel = None;
                        self.commit(tr("Delete clip"));
                    }
                    Key::Char('c') if mods.ctrl && !mods.shift => {
                        if let Some(i) = sel {
                            self.clipboard_clip = Some(self.project.clips[i]);
                            self.toast(tr("Clip copied"));
                        }
                    }
                    Key::Char('v') if mods.ctrl => {
                        if let Some(mut clip) = self.clipboard_clip {
                            clip.start = snap_round(self.start_marker as f32) as u32;
                            self.project.clips.push(clip);
                            self.playlist.selected = Some(self.project.clips.len() - 1);
                            self.commit(tr("Paste clip"));
                        }
                    }
                    Key::Char('b') if mods.ctrl && matches!(self.time_selection, Some((View::Playlist, _, _))) => {
                        self.duplicate_time();
                    }
                    Key::Char('b') if mods.ctrl => {
                        if let Some(i) = audio_sel {
                            self.duplicate_clip(ClipSel::Audio(i));
                        } else if let Some(i) = sel {
                            self.duplicate_clip(ClipSel::Pattern(i));
                        }
                    }
                    Key::Char('b') if !mods.ctrl && !mods.alt && !mods.shift && !self.typing_keyboard => self.toggle_paint(),
                    Key::Char('d') if mods.ctrl => self.playlist.selected = None,
                    // 1–9: jump to that marker.
                    Key::Char(d @ '1'..='9') if !mods.ctrl && !mods.alt && !mods.shift && !self.typing_keyboard && !self.perf_on() => {
                        self.jump_to_marker(d as usize - '1' as usize);
                    }
                    _ => {}
                }
            }
        }

        for (i, cl) in self.project.clips.clone().iter().enumerate() {
            let r = Rect::new(
                x_of(cl.start as f32) + 1.0,
                lane_y(cl.lane) + 3.0,
                (cl.len as f32 * ppt - 2.0).max(4.0),
                lane_h - 6.0,
            );
            if r.right() < area.x || r.x > area.right() || r.y > area.bottom() {
                continue;
            }
            let col = self.pattern_tint(cl.pattern);
            let selected = self.playlist.selected == Some(i);
            let hov = hover_clip.is_some_and(|(h, _)| h == i);
            let hv = self.hover_anim(wid("pl-clip", i, 0), hov);
            // Muted (a bounced source, for instance): faded, its name marked.
            let dim = if cl.muted { 0.4 } else { 1.0 };
            c.fill_rrect(r, 8.0, col.mix(t.panel, 0.72 - hv * 0.08).alpha(dim));
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 18.0), col.mix(t.panel, 0.45).alpha(0.9 * dim));
            if selected {
                c.stroke_rrect(r, 8.0, 1.5, t.select);
            }
            let prev_clip = c.push_clip(r);
            let name = self.project.patterns.get(cl.pattern).map_or(String::new(), |p| p.name.clone());
            let name = if cl.muted { tr_fmt!("{} · muted", name) } else { name };
            self.text_fit(c, self.f.sans_med, 11.0, r.x + 8.0, r.y + 10.0, &name, r.w - 12.0, t.text.alpha(dim));
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
                            c.fill_rect(Rect::new(x, y - 1.0, (n.len as f32 * ppt).max(1.5), 2.0), t.text.alpha(0.55 * dim));
                        }
                        rep += 1;
                    }
                }
            }
            // Repeat marks: where the pattern starts over.
            if let Some(p) = self.project.patterns.get(cl.pattern) {
                let plen = p.len.max(1);
                let mut k = cl.offset / plen + 1;
                while k * plen < cl.offset + cl.len && k < cl.offset / plen + 512 {
                    let x = (r.x - 1.0 + (k * plen - cl.offset) as f32 * ppt).round();
                    if x > area.right() {
                        break;
                    }
                    repeat_mark(c, x, r, col, dim * 0.6, &t, false);
                    k += 1;
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
        for (i, cl) in self.project.audio_clips.clone().iter().enumerate() {
            let r = Rect::new(
                x_of(cl.start as f32) + 1.0,
                lane_y(cl.lane) + 3.0,
                (cl.len as f32 * ppt - 2.0).max(4.0),
                lane_h - 6.0,
            );
            if r.right() < area.x || r.x > area.right() || r.y > area.bottom() {
                continue;
            }
            let hov = hover_audio.is_some_and(|(h, _)| h == i);
            let hv = self.hover_anim(wid("pl-aclip", i, 0), hov);
            let selected = self.clip_panel_shown() == Some(ClipSel::Audio(i));
            // Offline (file missing or not readable yet): red and hatched, silent.
            let offline = self.is_offline(&cl.path);
            let col = if offline { t.negative } else { super::clip::audio_color(cl, &t) };
            // Prise de côté (muette) : estompée, le nom barré d'un « muted ».
            let dim = if cl.muted { 0.45 } else { 1.0 };
            c.fill_rrect(r, 8.0, col.mix(t.panel, 0.78 - hv * 0.08).alpha(dim));
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 18.0), col.mix(t.panel, 0.5).alpha(0.9 * dim));
            if offline {
                hatch(c, Rect::new(r.x + 1.0, r.y + 19.0, r.w - 2.0, r.h - 20.0), 9.0, col.alpha(0.22 * dim));
                c.stroke_rrect(r, 8.0, 1.0, col.alpha(0.55 + 0.3 * hv));
            }
            if selected {
                c.stroke_rrect(r, 8.0, 1.5, t.text.mix(col, 0.4));
            }
            let prev_clip = c.push_clip(r);
            if offline {
                draw_icon(c, Icon::Missing, r.x + 14.0, r.y + 10.0, 12.0, col.mix(t.text, 0.35).alpha(dim));
                let file = ponzik_project::flp::file_name_of(&cl.path);
                self.text_fit(c, self.f.sans_med, 11.0, r.x + 26.0, r.y + 10.0, file, r.right() - r.x - 30.0, t.text.alpha(dim));
                let why = if ponzik_engine::wav::is_audio_file(std::path::Path::new(file)) {
                    tr("Missing")
                } else {
                    tr("Format not yet supported")
                };
                let body_y = r.y + 19.0 + (r.h - 19.0) * 0.5;
                self.text_fit(c, self.f.sans_med, 10.5, r.x + 8.0, body_y, why, r.w - 12.0, col.mix(t.text, 0.5).alpha(dim));
                if hov {
                    self.set_tip(&tr_fmt!("Offline, plays silence: {}\nRight-click its title: locate the missing files", cl.path), "");
                }
                c.set_clip(Some(prev_clip));
                continue;
            }
            // Warped: a stretch badge (pulsing while its render is under way).
            let mut name_x = r.x + 8.0;
            if cl.warp {
                let pending = self.warping.is_pending(i);
                let a = if pending { 0.45 + 0.55 * (0.5 + 0.5 * (self.time * 6.0).sin()) } else { 0.9 };
                draw_icon(c, Icon::Stretch, r.x + 14.0, r.y + 10.0, 11.0, t.text.alpha(a * dim));
                name_x += 14.0;
                if hov {
                    let tip = if pending { tr("Warped · stretching in the background") } else { tr("Warped: follows the song tempo") };
                    self.set_tip(tip, "");
                }
            }
            // Looped: a loop badge; the edge repeats it.
            if cl.looped {
                draw_icon(c, Icon::Loop, name_x + 6.0, r.y + 10.0, 11.0, t.text.alpha(0.9 * dim));
                name_x += 14.0;
            }
            let name = std::path::Path::new(&cl.path).file_stem().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let name = if cl.muted { tr_fmt!("{} · muted", name) } else { name };
            self.text_fit(c, self.f.sans_med, 11.0, name_x, r.y + 10.0, &name, r.right() - name_x - 4.0, t.text.alpha(dim));
            if hov && cl.take > 0 {
                self.set_tip(tr("Alt+click: play this take"), "");
            }
            let (sr, duration) = self.samples.get(&cl.path).map_or((48_000.0, 0.0), |s| (s.sample_rate as f64, s.duration() as f64));
            if let Some(peaks) = self.waveform(&cl.path) {
                let mid = r.y + 22.0 + (r.h - 24.0) * 0.5;
                let amp = (r.h - 26.0) * 0.5;
                let x0 = r.x.max(area.x) as i32;
                let x1 = r.right().min(area.right()) as i32;
                let bucket = super::app::WAVE_BUCKET as f64;
                for px in x0..x1 {
                    let tick_a = tick_at(px as f32) as f64;
                    let tick_b = tick_a + 1.0 / ppt as f64;
                    // Unwarped audio plays at its own speed, warped audio by beats.
                    let (Some(a), Some(b)) = (self.clip_file_secs(cl, tick_a, &map, duration), self.clip_file_secs(cl, tick_b, &map, duration)) else {
                        continue;
                    };
                    let (ba, bb) = ((a.min(b) * sr / bucket) as usize, (a.max(b) * sr / bucket) as usize);
                    let (mut lo, mut hi) = (0.0f32, 0.0f32);
                    for b in ba..=bb.min(ba + 256) {
                        if let Some(&(l, h)) = peaks.get(b) {
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if hi > lo {
                        c.fill_rect(Rect::new(px as f32, mid - hi * amp, 1.0, ((hi - lo) * amp).max(1.0)), t.wave.alpha(0.6 * dim));
                    }
                }
            }
            // Repeat marks: where the file starts over.
            for tick in cl.loop_starts(duration, &map, 512) {
                let x = x_of(tick as f32).round();
                if x > area.right() {
                    break;
                }
                repeat_mark(c, x, r, col, dim, &t, true);
            }
            c.set_clip(Some(prev_clip));
            // A loop on screen: the first time, how to make it last.
            if cl.looped && r.w > 24.0 && r.right() > area.x + 40.0 && r.right() < area.right() - 8.0 {
                let edge = Rect::new(r.right() - EDGE, r.y, EDGE + 2.0, r.h);
                self.want_hint(super::hints::Hint::DragToRepeat, edge);
            }
            if hover_audio.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 5.0, r.y + 8.0, 2.0, r.h - 16.0), 1.0, t.text.alpha(0.7));
            }
        }

        // Take groups: comp lanes, take lanes.
        self.draw_comps(c, lane_input, &layout, area, &x_of, &tick_at);

        self.perf_overlay(c, input, lanes_r, lane_h, &lane_y, &x_of);

        // Couloirs d'automation, sous les clips.
        let auto_y = layout.bottom() + 4.0;
        if autos > 0 {
            c.fill_rect(Rect::new(area.x, auto_y - 3.0, area.w, 1.0), t.hairline.alpha(2.0));
            self.automation_lanes(c, lane_input, area, auto_y, &x_of, &tick_at, 96.0);
        }
        c.set_clip(Some(lanes_clip));
        // Scroll bar of the lanes, when they do not fit.
        if max_sy > 0.0 {
            let h = (lanes_r.h * lanes_r.h / content_h).max(24.0);
            let y = lanes_r.y + (lanes_r.h - h) * sy / max_sy;
            c.fill_rrect(Rect::new(area.right() - 6.0, y, 4.0, h), 2.0, t.text2.alpha(0.35));
        }

        // Tête de lecture.
        if self.project.mode == PlayMode::Song || (self.perf_on() && self.playing) {
            let x = x_of(self.position as f32);
            c.fill_rect(Rect::new(x.round() - 1.0, ruler.y, 2.0, area.h), t.playhead);
        }
        c.set_clip(Some(prev));
    }
}

/// A repeat mark at `x` in clip `r`: a notch in the title bar, a faint line
/// through the body, and for loops a small arrow under the notch.
fn repeat_mark(c: &mut Canvas, x: f32, r: Rect, col: Color, dim: f32, t: &Theme, arrow: bool) {
    if x <= r.x + 2.0 || x >= r.right() - 1.0 {
        return;
    }
    c.fill_rect(Rect::new(x, r.y + 19.0, 1.0, r.h - 20.0), t.text.alpha(0.22 * dim));
    c.fill_rect(Rect::new(x - 1.0, r.y + 1.0, 2.0, 18.0), col.mix(t.text, 0.5).alpha(0.9 * dim));
    if !arrow {
        return;
    }
    let mut tri = ponzik_ui::Path::new();
    tri.polygon(&[(x - 4.0, r.y + 19.0), (x + 4.0, r.y + 19.0), (x, r.y + 24.0)]);
    c.fill_path(&tri, col.mix(t.text, 0.5).alpha(0.8 * dim));
}

impl App {
    /// Shorter and taller playlist tracks.
    fn track_height_buttons(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let tip = tr("Track height\nAlt + wheel on the tracks");
        let h = self.layout.sizes.track_h;
        let less = Rect::new(r.x, r.y, 30.0, r.h);
        if self.icon_button(c, input, wid("pl-track-less", 0, 0), less, Icon::Minus, false, (tip, "")) {
            self.layout.sizes.track_h = TRACK_SPAN.fit((h / 1.25).round());
        }
        let more = Rect::new(less.right() + 2.0, r.y, 30.0, r.h);
        if self.icon_button(c, input, wid("pl-track-more", 0, 0), more, Icon::Plus, false, (tip, "")) {
            self.layout.sizes.track_h = TRACK_SPAN.fit((h * 1.25).round());
        }
    }

    /// The splitter between the patterns and the tracks, or the tab that
    /// brings the folded patterns back.
    fn palette_splitter(&mut self, c: &mut Canvas, input: &Input, inner: Rect, pw: f32) {
        if !self.layout.sizes.patterns_open && pw <= 1.0 && !self.is_dragging(splitter_id("pl-palette")) {
            let tab = Rect::new(inner.x, inner.y + inner.h * 0.5 - 32.0, 10.0, 64.0);
            if self.fold_tab(c, input, "pl-palette", tab, Icon::ChevronRight, tr("Show the patterns")) {
                self.layout.sizes.patterns_open = true;
                self.save_settings();
            }
            return;
        }
        let zone = Rect::new(inner.x + pw - 12.0, inner.y, 10.0, inner.h);
        let span = PATTERNS_SPAN.up_to(PATTERNS_SPAN.max.min(inner.w * 0.4));
        match self.splitter(c, input, "pl-palette", zone, Axis::Columns, pw, 1.0, span, tr("Drag: width of the patterns")) {
            Some(Split::Size(w)) => {
                let s = &mut self.layout.sizes;
                (s.patterns_w, s.patterns_open) = (w, true);
                self.anim.set(wid("pl-palette-w", 0, 0), w);
            }
            Some(Split::Fold) => self.layout.sizes.patterns_open = false,
            Some(Split::Reset) => {
                let s = &mut self.layout.sizes;
                (s.patterns_w, s.patterns_open) = (PATTERNS_SPAN.default, true);
                self.save_settings();
            }
            None => {}
        }
        if self.splitter_released(input, "pl-palette") {
            self.save_settings();
        }
    }
}
