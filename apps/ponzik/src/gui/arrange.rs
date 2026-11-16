//! Arrangement lane (named section markers) and tempo lane (tempo and time
//! signature changes) of the playlist, and the transport helpers that follow
//! the tempo map.

use ponzik_engine::{PlayMode, TempoMap, TimeSig};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::{MARKER_COLORS, Placement};
use ponzik_ui::{Canvas, Color, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, MenuItem, Popup, Rename, View};

use super::widgets::{Icon, wid};

/// Height of the arrangement lane, above the ruler.
pub const ARRANGE_H: f32 = 26.0;
/// Height of the tempo lane, under the ruler (signatures on top).
pub const TEMPO_H: f32 = 46.0;
const SIG_ROW: f32 = 16.0;

/// Meters offered in the time signature menu.
pub const SIGNATURES: [(u8, u8); 10] = [(4, 4), (3, 4), (2, 4), (5, 4), (6, 8), (7, 8), (9, 8), (12, 8), (5, 8), (7, 4)];

const ARRANGE_HELP: &str = tr_noop(
    "Sections\nClick: loop the section · double-click: add a marker\nHeader: drag to move the section (Ctrl: copy, Alt: overwrite)\nDouble-click the header: rename · edge: move the marker\nRight-click: section menu · 1–9: jump to a marker",
);
const ARRANGE_EMPTY_HELP: &str = tr_noop("Arrangement lane\nClick: add a section marker here");
const TEMPO_HELP: &str = tr_noop(
    "Tempo lane\nClick: add a tempo change · drag a point: tempo and place\nDouble-click a point: ramp or instant · right-click: delete",
);
const SIG_HELP: &str = tr_noop("Time signature\nClick: change the meter from this bar");

/// Gesture on the arrangement or tempo lane.
#[derive(Clone, Copy, Debug)]
pub enum ArrangeDrag {
    /// Moves a marker alone.
    Marker { index: usize },
    /// Moves (or copies) a whole section, grabbed `grab` ticks after its start.
    Section { index: usize, grab: f32 },
    /// Tempo lane point (0 = the first tempo): tempo and place.
    Tempo { index: usize, bpm0: f32, tick0: u32, x0: f32, y0: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerOp {
    Jump,
    Loop,
    Duplicate,
    DeleteMarker,
    DeleteSection,
    Color(u32),
}

enum Hit {
    Edge(usize),
    Header(usize),
    Body(usize),
    Empty,
}

impl App {
    /// Position the transport edits refer to: the playhead while playing,
    /// the start marker otherwise.
    pub fn playhead(&self) -> f64 {
        if self.playing { self.position } else { self.start_marker }
    }

    /// Tempo lane point edited by the BPM field and tap tempo: the one in
    /// effect at the playhead in song mode, the first tempo otherwise.
    pub fn tempo_point_here(&self) -> usize {
        if self.project.mode == PlayMode::Song { self.project.tempo_point_at(self.playhead()) } else { 0 }
    }

    pub fn tempo_here(&self) -> f32 {
        self.project.tempo_point_bpm(self.tempo_point_here())
    }

    /// Sets the tempo at the playhead; returns true if it changed.
    pub fn set_tempo_here(&mut self, bpm: f32) -> bool {
        let i = self.tempo_point_here();
        if self.project.tempo_point_bpm(i) == bpm {
            return false;
        }
        self.project.set_tempo_point_bpm(i, bpm);
        true
    }

    /// Signature heard at the start marker (the first one outside song mode).
    pub fn sig_here(&self) -> TimeSig {
        let map = self.project.tempo_map();
        if self.project.mode == PlayMode::Song { map.sig_at(self.start_marker) } else { map.sigs()[0] }
    }

    pub fn marker_color(&self, i: usize) -> Color {
        match self.project.markers.get(i).map_or(0, |m| m.color) {
            0 => Color::hex(MARKER_COLORS[i % MARKER_COLORS.len()]),
            hex => Color::hex(hex),
        }
    }

    pub fn jump_to_marker(&mut self, i: usize) {
        let Some(m) = self.project.markers.get(i) else { return };
        let (tick, name) = (m.tick as f64, m.name.clone());
        if self.project.mode != PlayMode::Song {
            self.set_mode(PlayMode::Song);
        }
        self.set_start_marker(tick);
        self.toast(&tr_fmt!("Marker: {}", name));
    }

    /// Loops the section and starts it there.
    pub fn loop_section(&mut self, i: usize) {
        let Some((a, b)) = self.project.section(i) else { return };
        if self.project.mode != PlayMode::Song {
            self.set_mode(PlayMode::Song);
        }
        self.time_selection = Some((View::Playlist, a, b));
        self.set_loop(View::Playlist, Some((a, b)));
        self.set_start_marker(a as f64);
    }

    /// Adds a marker with the next default name; returns its index.
    pub fn add_marker_at(&mut self, tick: u32) -> usize {
        let name = self.project.next_marker_name();
        let i = self.project.add_marker(tick, name);
        self.commit(tr("Add marker"));
        i
    }

    fn rename_marker(&mut self, i: usize, anchor: Rect) {
        if let Some(m) = self.project.markers.get(i) {
            let text = m.name.clone();
            self.open_popup(Popup::Rename { target: Rename::Marker(i), text }, anchor);
        }
    }

    pub fn marker_op(&mut self, i: usize, op: MarkerOp) {
        if i >= self.project.markers.len() {
            return;
        }
        match op {
            MarkerOp::Jump => self.jump_to_marker(i),
            MarkerOp::Loop => self.loop_section(i),
            MarkerOp::Duplicate => {
                let Some((_, b)) = self.project.section(i) else { return };
                self.project.duplicate_section(i, b, Placement::Insert);
                self.after_section_edit(tr("Duplicate section"));
            }
            MarkerOp::DeleteMarker => {
                self.project.markers.remove(i);
                self.commit(tr("Delete marker"));
            }
            MarkerOp::DeleteSection => {
                self.project.delete_section(i);
                self.after_section_edit(tr("Delete section"));
            }
            MarkerOp::Color(color) => {
                self.project.markers[i].color = color;
                self.commit(tr("Marker color"));
            }
        }
    }

    /// Clip indices and the time selection no longer hold after a section edit.
    fn after_section_edit(&mut self, label: &str) {
        self.playlist.selected = None;
        if matches!(self.time_selection, Some((View::Playlist, _, _))) {
            self.time_selection = None;
            self.set_loop(View::Playlist, None);
        }
        self.commit(label);
    }

    pub(super) fn marker_menu(&self, i: usize) -> (String, Vec<MenuItem>) {
        let Some(m) = self.project.markers.get(i) else { return (String::new(), Vec::new()) };
        let rename = Popup::Rename { target: Rename::Marker(i), text: m.name.clone() };
        let mut color = MenuItem::plain(tr("Color"), Action::Popup(Popup::MarkerColor(i)));
        color.color = Some(self.marker_color(i));
        let key = if i < 9 { (i + 1).to_string() } else { String::new() };
        (
            tr_fmt!("Section · {}", m.name.as_str()),
            vec![
                MenuItem::new(Icon::Play, tr("Go to the marker"), Action::Marker(i, MarkerOp::Jump)).detail(key),
                MenuItem::new(Icon::Reset, tr("Loop the section"), Action::Marker(i, MarkerOp::Loop)).detail(tr("click").into()),
                MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)).detail(tr("double-click").into()),
                color,
                MenuItem::new(Icon::Duplicate, tr("Duplicate the section"), Action::Marker(i, MarkerOp::Duplicate))
                    .tip(tr("Inserts a copy right after it · Ctrl+drag the header to copy elsewhere"))
                    .sep(),
                MenuItem::new(Icon::Export, tr("Export sections"), Action::ExportSections(Default::default()))
                    .tip(tr("One WAV per arrangement section: \"<project> - <section>.wav\"")),
                MenuItem::new(Icon::Close, tr("Delete the marker"), Action::Marker(i, MarkerOp::DeleteMarker))
                    .tip(tr("Keeps the content: the previous section grows"))
                    .sep(),
                MenuItem::new(Icon::Trash, tr("Delete the section"), Action::Marker(i, MarkerOp::DeleteSection))
                    .tip(tr("Removes its clips and automation and closes the gap")),
            ],
        )
    }

    pub(super) fn marker_color_menu(&self, i: usize) -> (String, Vec<MenuItem>) {
        let cur = self.project.markers.get(i).map_or(0, |m| m.color);
        let mut v = Vec::new();
        for &(name, hex) in super::playlist::PATTERN_COLORS.iter() {
            let mut it = MenuItem::plain(tr(name), Action::Marker(i, MarkerOp::Color(hex))).on(cur == hex);
            it.color = Some(Color::hex(hex));
            v.push(it);
        }
        let name = self.project.markers.get(i).map_or("", |m| m.name.as_str());
        (tr_fmt!("Color · {}", name), v)
    }

    pub(super) fn signature_menu(&self, tick: u32) -> (String, Vec<MenuItem>) {
        let map = self.project.tempo_lane_map();
        let cur = map.sig_at(tick as f64);
        let here = self.project.signatures.iter().any(|s| s.tick == tick);
        let mut v: Vec<MenuItem> = SIGNATURES
            .iter()
            .map(|&(num, den)| {
                MenuItem::plain(&format!("{num}/{den}"), Action::SetSignature(tick, num, den)).on((cur.num, cur.den) == (num, den))
            })
            .collect();
        if here && tick > 0 {
            v.push(MenuItem::new(Icon::Trash, tr("Remove this change"), Action::DeleteSignature(tick)).sep());
        }
        (tr_fmt!("Time signature · bar {}", map.bar_at(tick as f64) + 1), v)
    }

    /// Arrangement lane: sections from marker to marker.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn arrangement_lane(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        r: Rect,
        lanes: Rect,
        map: &TempoMap,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) {
        let t = self.theme;
        let id = wid("arr-lane", 0, 0);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        let sections = self.project.sections();
        let geo: Vec<(f32, f32, f32)> = sections
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| {
                let (xa, xb) = (x_of(a as f32), x_of(b as f32));
                let w = self.fonts.measure(self.f.sans_med, 11.0, &self.project.markers[i].name) + 20.0;
                (xa, xb, w.min(xb - xa).max(12.0))
            })
            .collect();
        let over = input.inside && r.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(id));
        let hit = if over {
            geo.iter()
                .enumerate()
                .rev()
                .find_map(|(i, &(xa, xb, hw))| {
                    if (input.x - xa).abs() <= 4.0 {
                        Some(Hit::Edge(i))
                    } else if input.x >= xa && input.x <= xa + hw {
                        Some(Hit::Header(i))
                    } else if input.x >= xa && input.x < xb {
                        Some(Hit::Body(i))
                    } else {
                        None
                    }
                })
                .unwrap_or(Hit::Empty)
        } else {
            Hit::Empty
        };
        let snap_bar = |x: f32| map.snap_bar(tick_at(x).max(0.0) as f64);

        if over && self.drag.is_none() {
            match hit {
                Hit::Edge(_) => self.set_cursor(Cursor::ResizeH),
                Hit::Header(_) => self.set_cursor(Cursor::Move),
                _ => self.set_cursor(Cursor::Hand),
            }
            match hit {
                Hit::Empty => self.set_tip(tr(ARRANGE_EMPTY_HELP), ""),
                _ => self.set_tip(tr(ARRANGE_HELP), ""),
            }
        }
        if over && input.right_pressed && self.drag.is_none() {
            if let Hit::Edge(i) | Hit::Header(i) | Hit::Body(i) = hit {
                self.open_popup(Popup::MarkerMenu(i), Rect::new(input.x, r.y, 0.0, r.h));
            }
        } else if over && input.pressed && self.drag.is_none() {
            match hit {
                Hit::Edge(i) => {
                    self.begin_drag(id, input, 0.0);
                    self.arrange_drag = Some(ArrangeDrag::Marker { index: i });
                }
                Hit::Header(i) if input.double => {
                    let (xa, _, hw) = geo[i];
                    self.rename_marker(i, Rect::new(xa, r.y, hw.max(120.0), r.h));
                }
                Hit::Header(i) => {
                    self.begin_drag(id, input, 0.0);
                    let grab = tick_at(input.x) - sections[i].0 as f32;
                    self.arrange_drag = Some(ArrangeDrag::Section { index: i, grab });
                }
                Hit::Body(_) if input.double => {
                    let tick = snap_bar(input.x);
                    if !self.project.markers.iter().any(|m| m.tick == tick) {
                        let i = self.add_marker_at(tick);
                        self.rename_marker(i, Rect::new(x_of(tick as f32), r.y, 140.0, r.h));
                    }
                }
                Hit::Body(i) => self.loop_section(i),
                Hit::Empty => {
                    let tick = snap_bar(input.x);
                    if !self.project.markers.iter().any(|m| m.tick == tick) {
                        let i = self.add_marker_at(tick);
                        self.begin_drag(id, input, 0.0);
                        self.arrange_drag = Some(ArrangeDrag::Marker { index: i });
                    }
                }
            }
        }

        // Gesture in progress.
        let mut ghost = None;
        if self.is_dragging(id) {
            match self.arrange_drag {
                Some(ArrangeDrag::Marker { index }) if index < self.project.markers.len() => {
                    let tick = snap_bar(input.x);
                    let taken = self.project.markers.iter().enumerate().any(|(j, m)| j != index && m.tick == tick);
                    if self.project.markers[index].tick != tick && !taken {
                        self.project.markers[index].tick = tick;
                        let index = self.project.sort_markers(index);
                        self.arrange_drag = Some(ArrangeDrag::Marker { index });
                        self.commit(tr("Move marker"));
                    }
                    self.set_cursor(Cursor::ResizeH);
                }
                Some(ArrangeDrag::Section { index, grab }) if index < sections.len() => {
                    let (a, b) = sections[index];
                    let dest = map.snap_bar((tick_at(input.x) - grab).max(0.0) as f64);
                    let copy = input.mods.ctrl;
                    let placement = if input.mods.alt { Placement::Overwrite } else { Placement::Insert };
                    self.set_cursor(Cursor::Grab);
                    if input.released {
                        if copy {
                            self.project.duplicate_section(index, dest, placement);
                            self.after_section_edit(tr("Duplicate section"));
                        } else if dest != a {
                            self.project.move_section(index, dest, placement);
                            self.after_section_edit(tr("Move section"));
                        }
                    } else if dest != a || copy {
                        ghost = Some((index, dest, b - a, copy, placement));
                    }
                }
                _ => {}
            }
            if input.released {
                self.arrange_drag = None;
            }
        }

        // Sections.
        let prev = c.push_clip(r);
        let looped = match self.time_selection {
            Some((View::Playlist, a, b)) => Some((a, b)),
            _ => None,
        };
        for (i, &(xa, xb, hw)) in geo.iter().enumerate() {
            if xb < r.x || xa > r.right() {
                continue;
            }
            let col = self.marker_color(i);
            let hov = matches!(hit, Hit::Header(h) | Hit::Body(h) | Hit::Edge(h) if h == i);
            let hv = self.hover_anim(wid("arr-sec", i, 0), hov);
            let body = Rect::new(xa, r.y + 3.0, (xb - xa - 1.0).max(2.0), r.h - 6.0);
            c.fill_rrect(body, 6.0, col.alpha(0.13 + 0.07 * hv));
            if looped == Some(sections[i]) {
                c.stroke_rrect(body, 6.0, 1.5, col.alpha(0.9));
            }
            let head = Rect::new(xa, r.y + 3.0, hw, r.h - 6.0);
            c.fill_rrect(head, 6.0, col.mix(t.panel, 0.42 - 0.12 * hv));
            c.fill_rect(Rect::new(xa, r.y + 2.0, 2.0, r.h - 4.0), col);
            let name = self.project.markers[i].name.clone();
            self.text_fit(c, self.f.sans_med, 11.0, xa + 9.0, r.y + r.h * 0.5, &name, hw - 12.0, t.text);
            if i < 9 && xb - xa > hw + 22.0 {
                self.text(c, self.f.mono, 10.0, xa + hw + 6.0, r.y + r.h * 0.5, &(i + 1).to_string(), col.alpha(0.7));
            }
        }
        if geo.is_empty() {
            self.text(c, self.f.sans, 11.0, r.x + 10.0, r.y + r.h * 0.5, tr("Click to add a section marker (Intro, Verse, Chorus…)"), t.muted);
        }
        // Moved or copied section: where it will land.
        if let Some((index, dest, len, copy, placement)) = ghost {
            let col = self.marker_color(index);
            let (xa, xb) = (x_of(dest as f32), x_of((dest + len) as f32));
            let g = Rect::new(xa, r.y + 2.0, xb - xa, r.h - 4.0);
            c.fill_rrect(g, 6.0, col.alpha(0.35));
            c.stroke_rrect(g, 6.0, 1.5, col);
            let label = match (copy, placement) {
                (true, Placement::Insert) => tr("Copy · insert"),
                (true, Placement::Overwrite) => tr("Copy · overwrite"),
                (false, Placement::Insert) => tr("Move · insert"),
                (false, Placement::Overwrite) => tr("Move · overwrite"),
            };
            self.text_fit(c, self.f.sans_med, 11.0, xa + 8.0, r.y + r.h * 0.5, label, (xb - xa - 12.0).max(10.0), t.text);
        }
        c.set_clip(Some(prev));
        if let Some((index, dest, len, _, placement)) = ghost {
            let col = self.marker_color(index);
            let (xa, xb) = (x_of(dest as f32), x_of((dest + len) as f32));
            let prev = c.push_clip(Rect::new(lanes.x, lanes.y, lanes.w, lanes.h));
            c.fill_rect(Rect::new(xa, lanes.y, xb - xa, lanes.h), col.alpha(0.08));
            c.fill_rect(Rect::new(xa.round() - 1.0, lanes.y, 2.0, lanes.h), col);
            if placement == Placement::Overwrite {
                c.fill_rect(Rect::new(xb.round() - 1.0, lanes.y, 2.0, lanes.h), col.alpha(0.7));
            }
            c.set_clip(Some(prev));
        }
    }

    /// Tempo lane: signature chips on top, the tempo curve under them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tempo_lane(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        r: Rect,
        ppt: f32,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) {
        let t = self.theme;
        let map = self.project.tempo_lane_map();
        let sig_r = Rect::new(r.x, r.y, r.w, SIG_ROW);
        let graph = Rect::new(r.x, r.y + SIG_ROW, r.w, r.h - SIG_ROW);
        c.fill_rect(r, t.panel.alpha(0.3));
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        c.fill_rect(Rect::new(r.x, graph.y, r.w, 1.0), t.hairline.alpha(0.5));

        // Time signatures.
        let sig_id = wid("tempo-sig", 0, 0);
        let sig_hov = self.hovered(input, sig_id, sig_r);
        let mut chip_hit = None;
        let prev = c.push_clip(r);
        for s in map.sigs() {
            let x = x_of(s.tick as f32);
            let label = format!("{}/{}", s.num, s.den);
            let w = self.fonts.measure(self.f.mono_med, 10.0, &label) + 10.0;
            let chip = Rect::new(x + 1.0, sig_r.y + 1.0, w, SIG_ROW - 2.0);
            let hov = sig_hov && chip.contains(input.x, input.y);
            if hov {
                chip_hit = Some(s.tick);
            }
            c.fill_rect(Rect::new(x.round(), r.y, 1.0, r.h), t.text2.alpha(0.35));
            c.fill_rrect(chip, 4.0, if hov { t.raised_hi } else { t.raised });
            self.text(c, self.f.mono_med, 10.0, chip.x + 5.0, chip.y + chip.h * 0.5, &label, if s.tick == 0 { t.text2 } else { t.text });
        }
        if sig_hov {
            self.set_tip(tr(SIG_HELP), "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                let tick = chip_hit.unwrap_or_else(|| map.snap_bar(tick_at(input.x).max(0.0) as f64));
                self.open_popup(Popup::Signature(tick), Rect::new(x_of(tick as f32), sig_r.y, 0.0, sig_r.h));
            }
        }

        // Tempo points: the first tempo, then the changes.
        let points: Vec<(u32, f32, bool)> = std::iter::once((0, self.project.bpm, false))
            .chain(self.project.tempo_changes.iter().map(|p| (p.tick, p.bpm, p.ramp)))
            .collect();
        let (lo, hi) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
        let (mid, span) = ((lo + hi) * 0.5, (hi - lo).max(24.0) * 1.25);
        let (lo, hi) = (mid - span * 0.5, mid + span * 0.5);
        let y_of = |bpm: f32| graph.bottom() - 5.0 - (bpm - lo) / (hi - lo) * (graph.h - 10.0);
        let bpm_at_y = |y: f32| lo + (graph.bottom() - 5.0 - y) / (graph.h - 10.0) * (hi - lo);
        let automated = self.project.tempo_automated();
        let curve_col = if automated { t.muted } else { t.accent };
        let mut pts = Vec::new();
        let mut x = graph.x;
        while x <= graph.right() + 2.0 {
            pts.push((x, y_of(map.bpm_at(tick_at(x).max(0.0) as f64) as f32)));
            x += 2.0;
        }
        let mut area = pts.clone();
        area.push((graph.right() + 2.0, graph.bottom()));
        area.push((graph.x, graph.bottom()));
        let mut p = Path::new();
        p.polygon(&area);
        c.fill_path(&p, curve_col.alpha(0.1));
        c.stroke_polyline(&pts, 1.5, curve_col);

        let id = wid("tempo-lane", 0, 0);
        let over = input.inside && graph.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(id));
        let near = if over {
            points.iter().enumerate().rev().find(|(_, p)| (x_of(p.0 as f32) - input.x).abs() <= 7.0 && (y_of(p.1) - input.y).abs() <= 9.0).map(|(i, _)| i)
        } else {
            None
        };
        for (i, &(tick, bpm, ramp)) in points.iter().enumerate() {
            let (px, py) = (x_of(tick as f32), y_of(bpm));
            if px < r.x - 40.0 || px > r.right() + 4.0 {
                continue;
            }
            let hov = near == Some(i) || matches!(self.arrange_drag, Some(ArrangeDrag::Tempo { index, .. }) if index == i && self.is_dragging(id));
            let rad = if hov { 5.0 } else { 3.5 };
            if ramp {
                c.fill_circle(px, py, rad, t.panel);
                c.stroke_circle(px, py, rad, 1.5, curve_col);
            } else {
                c.fill_circle(px, py, rad, curve_col);
            }
            let next_x = points.get(i + 1).map_or(f32::MAX, |n| x_of(n.0 as f32));
            if next_x - px > 44.0 || hov {
                let label = ponzik_i18n::num(bpm as f64, 1);
                let ly = if py - graph.y > 14.0 { py - 9.0 } else { py + 9.0 };
                self.text(c, self.f.mono, 10.0, px + 6.0, ly, &label, if hov { t.text } else { t.text2 });
            }
        }
        if automated {
            self.text_right(c, self.f.sans, 10.0, r.right() - 8.0, graph.y + 9.0, tr("The tempo automation lane is in use"), t.muted);
        }
        c.set_clip(Some(prev));

        if over && self.drag.is_none() {
            self.set_tip(tr(TEMPO_HELP), "");
            self.set_cursor(if near.is_some() { Cursor::Move } else { Cursor::Hand });
            if input.right_pressed {
                if let Some(i) = near.filter(|&i| i > 0) {
                    self.project.tempo_changes.remove(i - 1);
                    self.commit(tr("Delete tempo change"));
                }
            } else if input.pressed {
                match near {
                    Some(i) if input.double && i > 0 => {
                        let p = &mut self.project.tempo_changes[i - 1];
                        p.ramp = !p.ramp;
                        let ramp = p.ramp;
                        self.commit(if ramp { tr("Tempo ramp") } else { tr("Instant tempo change") });
                    }
                    Some(i) => {
                        self.begin_drag(id, input, 0.0);
                        let (tick0, bpm0, _) = points[i];
                        self.arrange_drag = Some(ArrangeDrag::Tempo { index: i, bpm0, tick0, x0: input.x, y0: input.y });
                    }
                    None => {
                        let tick = map.snap_beat(tick_at(input.x).max(0.0) as f64);
                        if tick > 0 {
                            let bpm = ((bpm_at_y(input.y) * 10.0).round() / 10.0).clamp(20.0, 999.0);
                            let index = self.project.set_tempo_change(tick, bpm, false);
                            self.commit(tr("Add tempo change"));
                            self.begin_drag(id, input, 0.0);
                            self.arrange_drag = Some(ArrangeDrag::Tempo { index, bpm0: bpm, tick0: tick, x0: input.x, y0: input.y });
                        }
                    }
                }
            }
        }
        if self.is_dragging(id) {
            if let Some(ArrangeDrag::Tempo { index, bpm0, tick0, x0, y0 }) = self.arrange_drag {
                self.set_cursor(Cursor::Grab);
                let fine = if input.mods.shift { 0.1 } else { 1.0 };
                let bpm = ((bpm0 + (y0 - input.y) * 0.25 * fine) * 10.0).round() / 10.0;
                let mut index = index;
                if index > 0 && index <= self.project.tempo_changes.len() {
                    let tick = map.snap_beat((tick0 as f32 + (input.x - x0) / ppt).max(1.0) as f64).max(1);
                    let p = self.project.tempo_changes[index - 1];
                    let taken = self.project.tempo_changes.iter().any(|q| q.tick == tick);
                    if tick != p.tick && !taken {
                        self.project.tempo_changes.remove(index - 1);
                        index = self.project.set_tempo_change(tick, p.bpm, p.ramp);
                        self.arrange_drag = Some(ArrangeDrag::Tempo { index, bpm0, tick0, x0, y0 });
                        self.commit(tr("Tempo change"));
                    }
                }
                if index <= self.project.tempo_changes.len() && self.project.tempo_point_bpm(index) != bpm.clamp(20.0, 999.0) {
                    self.project.set_tempo_point_bpm(index, bpm);
                    self.commit(tr("Tempo change"));
                }
                let shown = ponzik_i18n::num(self.project.tempo_point_bpm(index) as f64, 1);
                self.set_tip(&tr_fmt!("{} BPM", shown), "");
            }
            if input.released {
                self.arrange_drag = None;
            }
        }
    }

    /// Screenshot demo (`PONZIK_ARRANGE`): sections, a tempo ramp and a 3/4
    /// section; `menu` opens a section menu, `sig` the signature menu.
    pub fn screenshot_arrange(&mut self, spec: &str) {
        let bar = ponzik_engine::TICKS_PER_BAR;
        let p = &mut self.project;
        p.markers.clear();
        for (b, name) in [(0, tr("Intro")), (4, tr("Build-up")), (8, tr("Drop")), (16, tr("Break"))] {
            p.add_marker(b * bar, name);
        }
        p.set_tempo_change(8 * bar, p.bpm, false);
        p.set_tempo_change(12 * bar, 140.0, true);
        p.set_tempo_change(16 * bar, 128.0, false);
        // Four bars of 3/4, then 4/4 again for the outro.
        p.set_signature(16 * bar, 3, 4);
        let outro = 16 * bar + 4 * 288;
        p.set_signature(outro, 4, 4);
        p.add_marker(outro, tr("Outro"));
        p.normalize_tempo();
        self.commit(tr("Arrangement"));
        self.playlist.zoom = 52.0;
        self.loop_section(2);
        match spec {
            "menu" => self.open_popup(Popup::MarkerMenu(2), Rect::new(760.0, 120.0, 0.0, ARRANGE_H)),
            "sig" => self.open_popup(Popup::Signature(16 * bar), Rect::new(1000.0, 176.0, 0.0, SIG_ROW)),
            _ => {}
        }
    }
}
