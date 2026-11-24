//! Pitch Editor (Melodyne, FL Newtone): the notes of a recorded take as
//! blobs on a pitch grid, their shape following the level, the sung pitch
//! curve drawn through them, the project's scale as lanes.
//!
//! Drag a note up or down (snaps to semitones or to the scale, Alt: free),
//! drag its edges in time (the rest stays in place, a touching neighbour
//! follows), double-click to cut it, box-select, then correct the pitch
//! (centres pulled to the scale, drift flattened, by an amount), the
//! vibrato, formants and level of the selection. Every edit is one undo
//! step; the take renders in the background and plays in the playlist.
//! A/B plays the take as recorded. The editor takes the place of the view
//! until it is closed (or another view is chosen).

use std::sync::Arc;

use ponzik_engine::pitchedit::{self, Analysis, HOP, NoteEdit, TimeMap};
use ponzik_engine::{PPQ, ParamInfo, PlayMode, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::{AudioClipDef, ScaleKind, tune};
use ponzik_ui::{Canvas, Color, Path, Rect};
use ponzik_window::{Cursor, Key, Modifiers};

use super::Input;
use super::app::App;
use super::rack::note_name;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const KEYS_W: f32 = 50.0;
const RULER: f32 = 26.0;
const LANE_H: f32 = 52.0;
const SIDE_W: f32 = 252.0;
const EDGE: f32 = 6.0;
/// Shortest note a time edit leaves, seconds.
const MIN_NOTE: f64 = 0.03;

const CENTRE: ParamInfo = ParamInfo::new(0, tr_noop("Pitch centre"), 0.0, 1.0, 1.0, Unit::Percent);
const DRIFT: ParamInfo = ParamInfo::new(1, tr_noop("Pitch drift"), 0.0, 1.0, 0.5, Unit::Percent);
const VIBRATO: ParamInfo = ParamInfo::new(2, tr_noop("Vibrato"), 0.0, 2.0, 1.0, Unit::Percent);
const FORMANT: ParamInfo = ParamInfo::new(3, tr_noop("Formant"), -12.0, 12.0, 0.0, Unit::Semitones);
const LEVEL: ParamInfo = ParamInfo::new(4, tr_noop("Level"), -24.0, 12.0, 0.0, Unit::Db);

const HELP: &str = tr_noop("Pitch Editor
Drag a note: pitch (snaps · Alt: free)
Drag its edges: timing · double-click: cut
Drag the background: select · Ctrl+A: all
Up / Down: a semitone · Delete: reset
Wheel: scroll · Shift: time · Ctrl: zoom");

/// How a note dragged up or down lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Snap {
    #[default]
    Semitone,
    Scale,
    Free,
}

#[derive(Clone, Debug)]
enum Gesture {
    Pitch { orig: Vec<NoteEdit>, anchor: usize, y0: f32 },
    Edge { orig: Vec<NoteEdit>, note: usize, start: bool },
    Select { x0: f32, y0: f32, base: Vec<usize> },
}

pub struct PitchEditor {
    /// The file edited and the clip it was opened from.
    pub open: Option<(String, usize)>,
    pub analysis: Option<Arc<Analysis>>,
    pub selected: Vec<usize>,
    gesture: Option<Gesture>,
    /// Pixels per second, first second shown, highest pitch shown, pixels per semitone.
    zoom: f32,
    scroll_x: f32,
    scroll_y: f32,
    row_h: f32,
    fitted: bool,
    /// Last frame's grid, first second, top pitch, zoom and row height (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    shown: Option<(Rect, f32, f32, f32, f32)>,
    pub centre: f32,
    pub drift: f32,
    pub snap: Snap,
    /// A/B: the take as recorded.
    pub original: bool,
}

impl Default for PitchEditor {
    fn default() -> Self {
        Self {
            open: None,
            analysis: None,
            selected: Vec::new(),
            gesture: None,
            zoom: 100.0,
            scroll_x: 0.0,
            scroll_y: 72.0,
            row_h: 18.0,
            fitted: false,
            centre: CENTRE.default,
            drift: DRIFT.default,
            snap: Snap::Semitone,
            original: false,
            shown: None,
        }
    }
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Moves a note's start or end to `t` (seconds of the output): a neighbour
/// it touches in the file follows, the others bound it; a note keeps 30 ms
/// and is stretched four times at most.
pub fn move_edge(notes: &mut [NoteEdit], i: usize, start: bool, t: f64, duration: f64) {
    let Some(e) = notes.get(i).copied() else { return };
    let len = e.end - e.start;
    if start {
        let tied = i > 0 && (notes[i - 1].end - e.start).abs() < 1e-6;
        let mut lo = if i == 0 {
            0.0
        } else if tied {
            notes[i - 1].new_start + MIN_NOTE.max(0.25 * (notes[i - 1].end - notes[i - 1].start))
        } else {
            notes[i - 1].new_end
        };
        lo = lo.max(e.new_end - 4.0 * len);
        let hi = e.new_end - MIN_NOTE.max(0.25 * len);
        let t = t.clamp(lo, hi.max(lo));
        notes[i].new_start = t;
        if tied {
            notes[i - 1].new_end = t;
        }
    } else {
        let tied = i + 1 < notes.len() && (notes[i + 1].start - e.end).abs() < 1e-6;
        let mut hi = if i + 1 == notes.len() {
            duration
        } else if tied {
            notes[i + 1].new_end - MIN_NOTE.max(0.25 * (notes[i + 1].end - notes[i + 1].start))
        } else {
            notes[i + 1].new_start
        };
        hi = hi.min(e.new_start + 4.0 * len);
        let lo = e.new_start + MIN_NOTE.max(0.25 * len);
        let t = t.clamp(lo.min(hi), hi);
        notes[i].new_end = t;
        if tied {
            notes[i + 1].new_start = t;
        }
    }
}

/// Cuts note `i` at output time `t`; both halves keep its edits. False when too close to an edge.
pub fn split_note(a: &Analysis, notes: &mut Vec<NoteEdit>, i: usize, t: f64) -> bool {
    let Some(e) = notes.get(i).copied() else { return false };
    let s = TimeMap::new(notes, a.duration()).src(t);
    if s - e.start < MIN_NOTE || e.end - s < MIN_NOTE || t <= e.new_start || t >= e.new_end {
        return false;
    }
    let shift = e.shift();
    let (mut l, mut r) = (e, e);
    (l.end, l.new_end, r.start, r.new_start) = (s, t, s, t);
    for n in [&mut l, &mut r] {
        if let Some(st) = a.stats(n.start, n.end) {
            n.centre = st.centre;
            n.pitch = st.centre + shift;
        }
    }
    notes[i] = l;
    notes.insert(i + 1, r);
    true
}

/// Joins notes `first..=last` into one, with the first one's edits.
pub fn join_notes(a: &Analysis, notes: &mut Vec<NoteEdit>, first: usize, last: usize) -> bool {
    if last <= first || last >= notes.len() {
        return false;
    }
    let (f, l) = (notes[first], notes[last]);
    let mut j = NoteEdit { end: l.end, new_end: l.new_end, ..f };
    if let Some(st) = a.stats(j.start, j.end) {
        j.centre = st.centre;
        j.pitch = st.centre + f.shift();
    }
    notes.splice(first..=last, [j]);
    true
}

/// The note back as found.
fn reset(e: &mut NoteEdit) {
    *e = NoteEdit { new_start: e.start, new_end: e.end, pitch: e.centre, drift: 0.0, vibrato: 1.0, formant: 0.0, gain: 0.0, ..*e };
}

impl App {
    /// Opens the Pitch Editor on an audio clip's file.
    pub fn open_pitch_editor(&mut self, clip: usize) {
        let Some(path) = self.project.audio_clips.get(clip).map(|c| c.path.clone()) else { return };
        if !self.samples.contains_key(&path) {
            self.toast(tr("The clip's file is missing: nothing to edit"));
            return;
        }
        let same = self.pitch.open.as_ref().is_some_and(|(p, _)| *p == path);
        self.pitch.open = Some((path.clone(), clip));
        if !same {
            self.pitch.analysis = None;
            self.pitch.selected.clear();
            self.pitch.fitted = false;
        }
        self.popup = None;
        self.anim.set(wid("view-enter", 0, 0), 0.0);
        self.request_analysis(&path);
    }

    pub fn close_pitch_editor(&mut self) {
        if self.pitch.open.take().is_none() {
            return;
        }
        self.tuning.cancel_analysis();
        self.pitch.gesture = None;
        if self.pitch.original {
            self.pitch.original = false;
            tune::set_bypass(false);
            self.resend_sequence();
        }
        self.anim.set(wid("view-enter", 0, 0), 0.0);
    }

    /// An analysis landed (the editor's take, or another one).
    pub(super) fn pitch_analysed(&mut self, path: &str, a: Arc<Analysis>) {
        if self.pitch.open.as_ref().is_some_and(|(p, _)| p == path) {
            self.pitch.analysis = Some(a);
            self.pitch.fitted = false;
        }
    }

    /// The notes of the take: the project's edits, else the analysis as found.
    fn pitch_notes(&self) -> Vec<NoteEdit> {
        let Some((path, _)) = &self.pitch.open else { return Vec::new() };
        match self.project.pitch_edit(path) {
            Some(d) => d.notes.clone(),
            None => self.pitch.analysis.as_ref().map_or_else(Vec::new, |a| a.notes.iter().map(NoteEdit::from_note).collect()),
        }
    }

    fn set_pitch_notes(&mut self, label: &str, notes: Vec<NoteEdit>) {
        let Some((path, _)) = self.pitch.open.clone() else { return };
        self.project.set_pitch_edit(&path, notes);
        self.commit(label);
    }

    /// The selection, or every note when nothing is selected.
    fn pitch_targets(&self, n: usize) -> Vec<usize> {
        if self.pitch.selected.is_empty() { (0..n).collect() } else { self.pitch.selected.iter().copied().filter(|&i| i < n).collect() }
    }

    /// Notes of the scale (bit 0: C), all twelve in chromatic.
    fn pitch_mask(&self) -> u16 {
        if self.project.scale == ScaleKind::Chromatic { 0xFFF } else { self.project.scale_mask() }
    }

    /// "Correct pitch" on the selection (or all): centres to the scale, drift flattened.
    pub fn pitch_correct(&mut self) {
        let mut notes = self.pitch_notes();
        let mask = self.pitch_mask();
        let (centre, drift) = (self.pitch.centre, self.pitch.drift);
        for i in self.pitch_targets(notes.len()) {
            pitchedit::correct_pitch(&mut notes[i], mask, centre);
            if drift > 0.0 {
                notes[i].drift = drift;
            }
        }
        self.set_pitch_notes(tr("Correct pitch"), notes);
    }

    fn pitch_split_selected(&mut self, at: Option<(usize, f64)>) {
        let Some(a) = self.pitch.analysis.clone() else { return };
        let mut notes = self.pitch_notes();
        let (i, t) = match at {
            Some(x) => x,
            // The selected note, in its middle.
            None => match self.pitch.selected.first().and_then(|&i| notes.get(i).map(|n| (i, 0.5 * (n.new_start + n.new_end)))) {
                Some(x) => x,
                None => return,
            },
        };
        if split_note(&a, &mut notes, i, t) {
            self.pitch.selected = vec![i + 1];
            self.set_pitch_notes(tr("Cut note"), notes);
        }
    }

    fn pitch_join_selected(&mut self) {
        let Some(a) = self.pitch.analysis.clone() else { return };
        let mut notes = self.pitch_notes();
        let mut sel = self.pitch.selected.clone();
        sel.sort_unstable();
        let (Some(&first), Some(&last)) = (sel.first(), sel.last()) else { return };
        if join_notes(&a, &mut notes, first, last) {
            self.pitch.selected = vec![first];
            self.set_pitch_notes(tr("Join notes"), notes);
        } else {
            self.toast(tr("Select two notes or more to join them"));
        }
    }

    fn pitch_reset_selected(&mut self) {
        let mut notes = self.pitch_notes();
        for i in self.pitch_targets(notes.len()) {
            reset(&mut notes[i]);
        }
        self.set_pitch_notes(tr("Reset notes"), notes);
    }

    fn pitch_nudge(&mut self, semitones: f32) {
        let mut notes = self.pitch_notes();
        let sel: Vec<usize> = self.pitch.selected.iter().copied().filter(|&i| i < notes.len()).collect();
        if sel.is_empty() {
            return;
        }
        for i in sel {
            notes[i].pitch = (notes[i].pitch + semitones).clamp(0.0, 127.0);
        }
        self.set_pitch_notes(tr("Move pitch"), notes);
    }

    /// Keys of the editor while it is open; true when used.
    pub fn pitch_key(&mut self, key: Key, mods: Modifiers) -> bool {
        if self.pitch.open.is_none() {
            return false;
        }
        match key {
            Key::Up | Key::Down => {
                let dir = if key == Key::Up { 1.0 } else { -1.0 };
                self.pitch_nudge(dir * if mods.shift { 12.0 } else if mods.alt { 0.1 } else { 1.0 });
            }
            Key::Char('a') if mods.ctrl => self.pitch.selected = (0..self.pitch_notes().len()).collect(),
            Key::Delete | Key::Backspace if !self.pitch.selected.is_empty() => self.pitch_reset_selected(),
            Key::Escape if !self.pitch.selected.is_empty() => self.pitch.selected.clear(),
            Key::Escape => self.close_pitch_editor(),
            _ => return false,
        }
        true
    }

    /// Tick of the song where a clip plays second `secs` of its file.
    fn pitch_tick_of(&self, c: &AudioClipDef, secs: f64, duration: f64) -> f64 {
        let s = if c.reverse { duration - secs } else { secs };
        let map = self.project.tempo_map();
        let tick = if c.warp {
            let w = c.warp_map();
            c.start as f64 + (w.beat_at(s) - w.beat_at(c.offset)) * PPQ as f64
        } else {
            map.tick_at_secs(map.secs_at(c.start as f64) + s - c.offset)
        };
        tick.max(0.0)
    }

    /// Plays the song from where the clip reaches `secs` of its file.
    fn pitch_play_from(&mut self, secs: f64) {
        let Some(c) = self.pitch.open.as_ref().and_then(|(_, i)| self.project.audio_clips.get(*i)).cloned() else { return };
        let duration = self.pitch.analysis.as_ref().map_or(0.0, |a| a.duration());
        if self.playing {
            self.stop();
            return;
        }
        if self.project.mode != PlayMode::Song {
            self.set_mode(PlayMode::Song);
        }
        self.start_marker = self.pitch_tick_of(&c, secs, duration).max(c.start as f64);
        self.play_stop();
    }

    fn pitch_set_original(&mut self, on: bool) {
        if self.pitch.original != on {
            self.pitch.original = on;
            tune::set_bypass(on);
            self.resend_sequence();
        }
    }

    /// The editor, in place of the view's body.
    pub(super) fn pitch_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let Some((path, clip_i)) = self.pitch.open.clone() else { return };
        let clip = self.project.audio_clips.get(clip_i).filter(|c| c.path == path).cloned();
        self.pitch_header(c, input, header, &path);
        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let Some(a) = self.pitch.analysis.clone() else {
            let progress = self.tuning.analysing().filter(|(p, _)| *p == path).map(|(_, f)| f);
            self.pitch_waiting(c, body, progress);
            return;
        };
        let inner = body.inset(12.0);
        let side = Rect::new(inner.right() - SIDE_W, inner.y, SIDE_W, inner.h);
        let area = Rect::new(inner.x, inner.y, inner.w - SIDE_W - 14.0, inner.h);
        let ruler = Rect::new(area.x + KEYS_W, area.y, area.w - KEYS_W, RULER);
        let keys = Rect::new(area.x, area.y + RULER, KEYS_W, area.h - RULER - LANE_H - 8.0);
        let grid = Rect::new(keys.right(), keys.y, area.w - KEYS_W, keys.h);
        let lane = Rect::new(grid.x, grid.bottom() + 8.0, grid.w, LANE_H);
        if grid.w < 80.0 || grid.h < 60.0 {
            return;
        }
        let duration = a.duration();
        let mut notes = self.pitch_notes();
        let original = self.pitch.original;
        let map = if original { TimeMap::new(&[], duration) } else { TimeMap::new(&notes, duration) };
        // The span of the file the clip plays.
        let span = clip.as_ref().map(|c| {
            let tm = self.project.tempo_map();
            let end = c.offset_at((c.start + c.len) as f64, &tm);
            let (a0, b0) = (c.offset.clamp(0.0, duration), end.clamp(0.0, duration));
            if c.reverse { (duration - b0, duration - a0) } else { (a0, b0) }
        });

        // Fit to the clip's span and the notes' pitches when opened.
        if !self.pitch.fitted {
            self.pitch.fitted = true;
            let (s0, s1) = span.filter(|s| s.1 - s.0 > 0.2).unwrap_or((0.0, duration.max(0.5)));
            // A long take opens on its first phrase: about eight seconds, from the first note.
            let first = notes.iter().map(|n| n.new_start).find(|&s| s >= s0).unwrap_or(s0);
            let (s0, s1) = if s1 - s0 > 10.0 { ((first - 0.4).max(s0), (first - 0.4).max(s0) + 8.0) } else { (s0, s1) };
            let pad = 0.03 * (s1 - s0);
            self.pitch.zoom = (grid.w as f64 / (s1 - s0 + 2.0 * pad)) as f32;
            self.pitch.scroll_x = (s0 - pad).max(0.0) as f32;
            // Rows for most of the notes shown (a stray one does not shrink them all).
            let mut shown: Vec<f32> = notes.iter().filter(|n| n.new_end > s0 && n.new_start < s1).map(|n| n.pitch).collect();
            shown.sort_by(f32::total_cmp);
            let (lo, hi) = match shown.len() {
                0 => (55.0, 67.0),
                n => (shown[n / 20], shown[n - 1 - n / 20]),
            };
            self.pitch.row_h = (grid.h / (hi - lo + 7.0)).clamp(12.0, 28.0);
            self.pitch.scroll_y = 0.5 * (lo + hi) + grid.h / self.pitch.row_h * 0.5;
            self.anim.set(wid("pe-sx", 0, 0), self.pitch.scroll_x);
            self.anim.set(wid("pe-sy", 0, 0), self.pitch.scroll_y);
        }

        // Scrolling and zoom: wheel, Shift (time), Ctrl (zoom), middle button.
        let row_h = self.pitch.row_h;
        let rows_shown = grid.h / row_h;
        let over_area = input.inside && (grid.contains(input.x, input.y) || keys.contains(input.x, input.y) || lane.contains(input.x, input.y));
        if over_area && input.wheel != 0.0 {
            self.claim_wheel();
            if input.mods.ctrl {
                let at = self.pitch.scroll_x + (input.x - grid.x) / self.pitch.zoom;
                let min = grid.w / (duration as f32).max(0.5) * 0.8;
                self.pitch.zoom = (self.pitch.zoom * 1.15f32.powf(input.wheel)).clamp(min, 3_000.0);
                self.pitch.scroll_x = (at - (input.x - grid.x) / self.pitch.zoom).max(0.0);
            } else if input.mods.shift {
                self.pitch.scroll_x = (self.pitch.scroll_x - input.wheel * 80.0 / self.pitch.zoom).max(0.0);
            } else {
                self.pitch.scroll_y = (self.pitch.scroll_y + input.wheel * 2.0).clamp(rows_shown + 12.0, 120.0);
            }
        }
        let pan_id = wid("pe-pan", 0, 0);
        if input.inside && grid.contains(input.x, input.y) && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.pitch.scroll_x, self.pitch.scroll_y));
        }
        if let Some((_, x0, y0, sx0, sy0)) = self.pan.filter(|p| p.0 == pan_id) {
            self.pitch.scroll_x = (sx0 - (input.x - x0) / self.pitch.zoom).max(0.0);
            self.pitch.scroll_y = (sy0 + (input.y - y0) / row_h).clamp(rows_shown + 12.0, 120.0);
            self.anim.set(wid("pe-sx", 0, 0), self.pitch.scroll_x);
            self.anim.set(wid("pe-sy", 0, 0), self.pitch.scroll_y);
        }
        let sx = self.anim.to(wid("pe-sx", 0, 0), self.pitch.scroll_x, T_LAYOUT);
        let sy = self.anim.to(wid("pe-sy", 0, 0), self.pitch.scroll_y, T_LAYOUT);
        let zoom = self.pitch.zoom;
        let x_of = |s: f64| grid.x + (s as f32 - sx) * zoom;
        let secs_at = |x: f32| f64::from(sx + (x - grid.x) / zoom);
        let y_of = |m: f32| grid.y + (sy - m) * row_h;
        let midi_at = |y: f32| sy - (y - grid.y) / row_h;
        self.pitch.shown = Some((grid, sx, sy, zoom, row_h));

        // Rows: semitones, the scale's lit, the tonic tinted; a line at the centre of each scale note.
        let prev = c.push_clip(grid);
        let scaled = self.project.scale != ScaleKind::Chromatic;
        let top = sy.ceil() as i32 + 1;
        let bottom = (sy - rows_shown).floor() as i32 - 1;
        for k in (bottom..=top).filter(|k| (0..128).contains(k)) {
            let (y0, y1) = (y_of(k as f32 + 0.5), y_of(k as f32 - 0.5));
            let black = matches!(k % 12, 1 | 3 | 6 | 8 | 10);
            c.fill_rect(Rect::new(grid.x, y0, grid.w, y1 - y0), if black { t.piano_row_black } else { t.piano_row });
            let in_scale = self.project.in_scale(k as u8);
            if scaled && !in_scale {
                c.fill_rect(Rect::new(grid.x, y0, grid.w, y1 - y0), t.bg_deep.alpha(0.45));
            } else if scaled && (k as u8 + 12 - self.project.key % 12) % 12 == 0 {
                c.fill_rect(Rect::new(grid.x, y0, grid.w, y1 - y0), t.accent.alpha(0.06));
            }
            if in_scale || !scaled {
                c.fill_rect(Rect::new(grid.x, (0.5 * (y0 + y1)).round(), grid.w, 1.0), t.piano_grid.alpha(if scaled { 1.6 } else { 0.8 }));
            }
            if k % 12 == 0 {
                c.fill_rect(Rect::new(grid.x, y1.round() - 1.0, grid.w, 1.0), t.piano_grid.alpha(2.6));
            }
        }
        // Seconds.
        let step = [0.05f64, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0].into_iter().find(|s| *s as f32 * zoom >= 70.0).unwrap_or(60.0);
        let first = (f64::from(sx) / step).floor() as i64;
        let last = (secs_at(grid.right()) / step).ceil() as i64;
        for k in first..=last {
            let x = x_of(k as f64 * step).round();
            c.fill_rect(Rect::new(x, grid.y, 1.0, grid.h), t.piano_grid.alpha(if k % 4 == 0 { 2.2 } else { 1.0 }));
        }
        // Outside the clip: dimmed.
        if let Some((s0, s1)) = span {
            let (x0, x1) = (x_of(s0), x_of(s1));
            if x0 > grid.x {
                c.fill_rect(Rect::new(grid.x, grid.y, x0 - grid.x, grid.h), t.bg_deep.alpha(0.5));
            }
            if x1 < grid.right() {
                c.fill_rect(Rect::new(x1, grid.y, grid.right() - x1, grid.h), t.bg_deep.alpha(0.5));
            }
        }

        // Hit test: the note under the pointer and whether on an edge.
        let gid = wid("pe-grid", 0, 0);
        let over_grid = input.inside && grid.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(gid));
        let hit = |x: f32, y: f32| -> Option<(usize, Option<bool>)> {
            notes.iter().enumerate().rev().find_map(|(i, n)| {
                let (x0, x1) = if original { (x_of(n.start), x_of(n.end)) } else { (x_of(n.new_start), x_of(n.new_end)) };
                let yc = y_of(if original { n.centre } else { n.pitch });
                if x < x0 - 2.0 || x > x1 + 2.0 || (y - yc).abs() > 0.75 * row_h {
                    return None;
                }
                let edge = if x1 - x0 < 3.0 * EDGE || original {
                    None
                } else if x - x0 <= EDGE {
                    Some(true)
                } else if x1 - x <= EDGE {
                    Some(false)
                } else {
                    None
                };
                Some((i, edge))
            })
        };
        let hover = if over_grid && self.drag.is_none() { hit(input.x, input.y) } else { None };
        if over_grid && self.drag.is_none() {
            match hover {
                Some((_, Some(_))) => {
                    self.set_cursor(Cursor::ResizeH);
                    self.set_tip(tr("Drag: the note's start or end, the rest stays in place"), "");
                }
                Some(_) => {
                    self.set_cursor(Cursor::ResizeV);
                    self.set_tip(tr("Drag: pitch (Alt: free) · double-click: cut"), "");
                }
                None => {}
            }
        }
        if over_grid && input.pressed && self.drag.is_none() && !original {
            self.begin_drag(gid, input, 0.0);
            match hover {
                Some((i, None)) if input.double => {
                    self.pitch.gesture = None;
                    self.pitch_split_selected(Some((i, secs_at(input.x))));
                    notes = self.pitch_notes();
                }
                Some((i, edge)) => {
                    if !self.pitch.selected.contains(&i) {
                        if !input.mods.shift {
                            self.pitch.selected.clear();
                        }
                        self.pitch.selected.push(i);
                    } else if input.mods.shift {
                        self.pitch.selected.retain(|&j| j != i);
                    }
                    self.pitch.gesture = Some(match edge {
                        Some(start) => Gesture::Edge { orig: notes.clone(), note: i, start },
                        None => Gesture::Pitch { orig: notes.clone(), anchor: i, y0: input.y },
                    });
                }
                None => {
                    let base = if input.mods.shift { self.pitch.selected.clone() } else { Vec::new() };
                    self.pitch.selected = base.clone();
                    self.pitch.gesture = Some(Gesture::Select { x0: input.x, y0: input.y, base });
                }
            }
        }
        if self.is_dragging(gid) {
            match self.pitch.gesture.clone() {
                Some(Gesture::Pitch { orig, anchor, y0 }) => {
                    let p0 = orig[anchor].pitch;
                    let want = p0 + (y0 - input.y) / row_h;
                    let free = input.mods.alt || self.pitch.snap == Snap::Free;
                    let target = if free {
                        want
                    } else if self.pitch.snap == Snap::Scale {
                        ponzik_engine::effects::autotune::nearest(want, self.pitch_mask()).map_or(want.round(), |n| n as f32)
                    } else {
                        want.round()
                    };
                    let moved = (input.y - y0).abs() > 3.0;
                    let delta = if moved { target - p0 } else { 0.0 };
                    let mut next = orig.clone();
                    let count = next.len();
                    for &i in self.pitch.selected.iter().filter(|&&i| i < count) {
                        next[i].pitch = (orig[i].pitch + delta).clamp(0.0, 127.0);
                    }
                    if next != notes {
                        self.set_pitch_notes(tr("Move pitch"), next);
                        notes = self.pitch_notes();
                    }
                    self.set_cursor(Cursor::ResizeV);
                }
                Some(Gesture::Edge { orig, note, start }) => {
                    let mut next = orig.clone();
                    move_edge(&mut next, note, start, secs_at(input.x).clamp(0.0, duration), duration);
                    if next != notes {
                        self.set_pitch_notes(tr("Note timing"), next);
                        notes = self.pitch_notes();
                    }
                    self.set_cursor(Cursor::ResizeH);
                }
                Some(Gesture::Select { x0, y0, base }) => {
                    let sel = Rect::new(x0.min(input.x), y0.min(input.y), (x0 - input.x).abs(), (y0 - input.y).abs());
                    let (t0, t1) = (secs_at(sel.x), secs_at(sel.right()));
                    let (m0, m1) = (midi_at(sel.bottom()), midi_at(sel.y));
                    let mut picked = base.clone();
                    for (i, n) in notes.iter().enumerate() {
                        if n.new_end > t0 && n.new_start < t1 && n.pitch + 0.5 >= m0 && n.pitch - 0.5 <= m1 && !picked.contains(&i) {
                            picked.push(i);
                        }
                    }
                    self.pitch.selected = picked;
                }
                None => {}
            }
            if input.released {
                self.pitch.gesture = None;
            }
        }
        let map = if original { map } else { TimeMap::new(&notes, duration) };

        // The voice outside notes (glides, short bits): a faint line.
        let covered = |k: usize| notes.iter().any(|n| (k as f64 * HOP) >= n.start && (k as f64 * HOP) < n.end);
        let (k0, k1) = (a.frame_at(map.src(secs_at(grid.x)).max(0.0)), a.frame_at(map.src(secs_at(grid.right()))) + 1);
        let stride = ((1.5 / (zoom * HOP as f32)).floor() as usize).max(1);
        let mut line: Vec<(f32, f32)> = Vec::new();
        let flush = |c: &mut Canvas, line: &mut Vec<(f32, f32)>, w: f32, col: Color| {
            if line.len() > 1 {
                c.stroke_polyline(line, w, col);
            }
            line.clear();
        };
        for k in (k0..k1.min(a.pitch.len())).step_by(stride) {
            let p = a.pitch[k];
            if !p.is_finite() || covered(k) {
                flush(c, &mut line, 1.2, t.text.alpha(0.22));
                continue;
            }
            line.push((x_of(map.dst(k as f64 * HOP)), y_of(p)));
        }
        flush(c, &mut line, 1.2, t.text.alpha(0.22));

        // The notes: a blob whose thickness is the level, the pitch curve through it.
        let level_at = |s: f64| pitchedit::interp(&a.level, s / HOP);
        for (i, n) in notes.iter().enumerate() {
            let (t0, t1) = if original { (n.start, n.end) } else { (n.new_start, n.new_end) };
            let (x0, x1) = (x_of(t0), x_of(t1));
            if x1 < grid.x || x0 > grid.right() {
                continue;
            }
            let centre = if original { n.centre } else { n.pitch };
            let yc = y_of(centre);
            if yc < grid.y - 2.0 * row_h || yc > grid.bottom() + 2.0 * row_h {
                continue;
            }
            let selected = self.pitch.selected.contains(&i);
            let hov = hover.is_some_and(|(h, _)| h == i);
            let hv = self.hover_anim(wid("pe-note", i, 0), hov);
            let edited = !original && !n.is_identity();
            // Where it was: a faint outline, once moved or stretched.
            if edited && ((n.pitch - n.centre).abs() > 0.05 || n.time_edited()) {
                let (gx0, gx1) = (x_of(n.start), x_of(n.end));
                let gy = y_of(n.centre);
                c.stroke_rrect(Rect::new(gx0, gy - 0.32 * row_h, (gx1 - gx0).max(4.0), 0.64 * row_h), 0.32 * row_h, 1.0, t.text.alpha(0.2));
            }
            // Very short notes keep a visible width.
            let (x0, x1) = if x1 - x0 < 5.0 { (0.5 * (x0 + x1) - 2.5, 0.5 * (x0 + x1) + 2.5) } else { (x0, x1) };
            let w = x1 - x0;
            let ramp = (0.3 * w).min(8.0);
            let steps = ((w / 3.0).ceil() as usize).clamp(2, 600);
            let mut upper = Vec::with_capacity(steps + 1);
            let mut lower = Vec::with_capacity(steps + 1);
            for j in 0..=steps {
                let f = j as f64 / steps as f64;
                let tt = t0 + (t1 - t0) * f;
                let s = if original { tt } else { map.src(tt) };
                let lv = level_at(s);
                let amp = ((lv + 54.0) / 46.0).clamp(0.12, 1.0);
                let px = w * f as f32;
                let taper = (0.35 + 0.65 * smoothstep(px.min(w - px) / ramp.max(1.0))).sqrt();
                let h = ((0.2 + 0.5 * amp) * row_h * taper).max(2.0);
                let x = x0 + px;
                upper.push((x, yc - h));
                lower.push((x, yc + h));
            }
            let mut outline = upper.clone();
            outline.extend(lower.iter().rev());
            let mut path = Path::new();
            path.polygon(&outline);
            let base = t.note_low.mix(t.note, 0.6);
            let fill = if selected { base.mix(t.note_hot, 0.45) } else { base.mix(t.note_hot, 0.25 * hv) };
            let fill = if edited { fill.mix(t.accent_hot, 0.3) } else { fill };
            if hv > 0.01 || selected {
                c.fill_rrect(Rect::new(x0 - 3.0, yc - 0.72 * row_h, x1 - x0 + 6.0, 1.44 * row_h), 6.0, t.accent.alpha(0.07 + 0.08 * hv));
            }
            c.fill_path(&path, fill.alpha(if original { 0.45 } else { 0.78 }));
            outline.push(outline[0]);
            let edge_col = if selected { t.select } else { t.note_hot.alpha(0.35 + 0.4 * hv) };
            c.stroke_polyline(&outline, if selected { 1.6 } else { 1.0 }, edge_col);
            if hover.is_some_and(|(h, e)| h == i && e.is_some()) {
                let at_start = hover.is_some_and(|(_, e)| e == Some(true));
                let hx = if at_start { x0 + 1.5 } else { x1 - 4.5 };
                c.fill_rrect(Rect::new(hx, yc - 0.45 * row_h, 3.0, 0.9 * row_h), 1.5, t.on_accent.alpha(0.9));
            }

            // Pitch curve, as edited (and the sung one, faint, where it moved).
            let curves = a.note_curves(n.start, n.end, n.centre);
            let taper_frames = 0.02 / HOP;
            let len = curves.pitch.len();
            let mut sung: Vec<(f32, f32)> = Vec::new();
            let mut ghost: Vec<(f32, f32)> = Vec::new();
            for j in (0..len).step_by(stride) {
                let p = curves.pitch[j];
                let s = (curves.first + j) as f64 * HOP;
                if !p.is_finite() {
                    flush(c, &mut sung, 1.4, t.text.alpha(0.8));
                    flush(c, &mut ghost, 1.0, t.text.alpha(0.22));
                    continue;
                }
                let w = smoothstep(j.min(len - 1 - j) as f32 / taper_frames as f32);
                let shift = if original { 0.0 } else { n.shift() - n.drift * curves.drift[j] * w + (n.vibrato - 1.0) * curves.vibrato[j] * w };
                let x = if original { x_of(s) } else { x_of(map.dst(s)) };
                sung.push((x, y_of(p + shift)));
                if edited {
                    ghost.push((x_of(s), y_of(p)));
                }
            }
            flush(c, &mut ghost, 1.0, t.text.alpha(0.22));
            flush(c, &mut sung, 1.4, t.text.alpha(0.8));

            // Name and cents of the selected or hovered note.
            if (selected || hov) && x1 - x0 > 18.0 {
                let r = centre.round();
                let cents = ((centre - r) * 100.0).round() as i32;
                let label = if cents == 0 { note_name(r.clamp(0.0, 127.0) as u8) } else { format!("{} {:+}", note_name(r.clamp(0.0, 127.0) as u8), cents) };
                let w = self.fonts.measure(self.f.mono, 11.0, &label) + 10.0;
                let tag = Rect::new(x0, yc - 0.75 * row_h - 18.0, w, 16.0);
                c.fill_rrect(tag, 8.0, t.bg_deep.alpha(0.85));
                self.text(c, self.f.mono, 11.0, tag.x + 5.0, tag.y + 8.0, &label, if selected { t.select } else { t.text2 });
            }
        }

        // Selection box.
        if let (true, Some(Gesture::Select { x0, y0, .. })) = (self.is_dragging(gid), &self.pitch.gesture) {
            let sel = Rect::new(x0.min(input.x), y0.min(input.y), (x0 - input.x).abs(), (y0 - input.y).abs());
            c.fill_rect(sel, t.select.alpha(0.12));
            c.stroke_rrect(sel, 2.0, 1.0, t.select.alpha(0.8));
        }

        // Playhead: where the clip is in its file.
        let head = clip.as_ref().and_then(|cl| {
            let tm = self.project.tempo_map();
            self.clip_file_secs(cl, self.position, &tm, duration)
        });
        let start_marker = clip.as_ref().and_then(|cl| {
            let tm = self.project.tempo_map();
            self.clip_file_secs(cl, self.start_marker, &tm, duration)
        });
        if let Some(s) = head.filter(|_| self.playing) {
            let x = x_of(s).round();
            c.fill_rect(Rect::new(x, grid.y, 2.0, grid.h), t.playhead);
        }
        c.set_clip(Some(prev));

        self.pitch_keys(c, keys, sy, row_h);
        self.pitch_ruler(c, input, ruler, (sx, zoom), step, start_marker, head.filter(|_| self.playing));
        self.pitch_lane(c, lane, &a, &map, (sx, zoom), span);
        self.pitch_side(c, input, side, &a, &notes);
    }

    /// Header: the take, A/B, snap, the work under way, play and close.
    fn pitch_header(&mut self, c: &mut Canvas, input: &Input, header: Rect, path: &str) {
        let t = self.theme;
        let name = std::path::Path::new(path).file_stem().map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned());
        let title = tr("Pitch Editor");
        // A narrow window: shorter controls (the buttons on the right need 130 px).
        let narrow = header.w < 800.0;
        let name = self.fit(self.f.sans, 12.0, &name, if narrow { 110.0 } else { 220.0 });
        let w = 44.0 + self.fonts.measure(self.f.sans_med, 13.0, title).max(self.fonts.measure(self.f.sans, 12.0, &name)) + 14.0;
        let chip = Rect::new(header.x, header.y, w, header.h);
        c.fill_rrect(chip, 10.0, t.raised);
        c.stroke_rrect(chip, 10.0, 1.0, t.hairline);
        c.fill_circle(chip.x + 20.0, chip.y + chip.h * 0.5, 13.0, t.accent.alpha(0.16));
        draw_icon(c, Icon::PitchBlobs, chip.x + 20.0, chip.y + chip.h * 0.5, 16.0, t.accent);
        self.text(c, self.f.sans_med, 13.0, chip.x + 40.0, chip.y + 11.0, title, t.text);
        self.text(c, self.f.sans, 12.0, chip.x + 40.0, chip.y + 26.0, &name, t.muted);
        self.tip_zone(input, chip, path, "");

        let mut x = chip.right() + 12.0;
        let ab = Rect::new(x, header.y, if narrow { 158.0 } else { 196.0 }, header.h);
        let tips = [(tr("Edited: the take as you corrected it"), ""), (tr("Original: the take as recorded, to compare"), "")];
        if let Some(i) = self.segmented_tips(c, input, wid("pe-ab", 0, 0), ab, &[tr("Edited"), tr("Original")], &tips, usize::from(self.pitch.original)) {
            self.pitch_set_original(i == 1);
        }
        x = ab.right() + 10.0;
        let snap = Rect::new(x, header.y, if narrow { 198.0 } else { 228.0 }, header.h);
        let opts = [tr("Semitone"), tr("Scale"), tr("Free")];
        let tips = [
            (tr("Dragged notes land on semitones"), ""),
            (tr("Dragged notes land on the project's scale"), ""),
            (tr("Dragged notes move freely (Alt does it too)"), ""),
        ];
        let sel = match self.pitch.snap {
            Snap::Semitone => 0,
            Snap::Scale => 1,
            Snap::Free => 2,
        };
        // The magnet says what the choice is about, when there is room for it.
        let glyph = if narrow { 0.0 } else { 20.0 };
        if !narrow {
            draw_icon(c, Icon::Magnet, snap.x + 7.0, header.y + header.h * 0.5, 14.0, t.muted);
        }
        let snap = Rect::new(snap.x + glyph, snap.y, snap.w - glyph, snap.h);
        if let Some(i) = self.segmented_tips(c, input, wid("pe-snap", 0, 0), snap, &opts, &tips, sel) {
            self.pitch.snap = [Snap::Semitone, Snap::Scale, Snap::Free][i];
        }
        x = snap.right() + 14.0;

        // Work under way: a ring filling up and a word.
        let work = self
            .tuning
            .analysing()
            .map(|(_, f)| (tr("Analysing"), f))
            .or_else(|| self.tuning.rendering().map(|(_, f)| (tr("Rendering"), f)));
        let close = Rect::new(header.right() - 36.0, header.y, 36.0, header.h);
        let play = Rect::new(close.x - 42.0, header.y, 36.0, header.h);
        let help = Rect::new(play.x - 42.0, header.y, 36.0, header.h);
        if let Some((word, f)) = work.filter(|_| x + 120.0 < help.x) {
            let cy = header.y + header.h * 0.5;
            let label = tr_fmt!("{} {} %", word, (f * 100.0).round() as i32);
            let w = self.fonts.measure(self.f.sans, 12.0, &label) + 40.0;
            let pill = Rect::new(x, header.y + 5.0, w, header.h - 10.0);
            c.fill_rrect(pill, pill.h * 0.5, t.accent.alpha(0.1));
            c.stroke_circle(pill.x + 14.0, cy, 6.0, 2.0, t.knob_track);
            let a0 = -std::f32::consts::FRAC_PI_2;
            c.stroke_arc(pill.x + 14.0, cy, 6.0, a0, a0 + std::f32::consts::TAU * f.max(0.04), 2.0, t.accent);
            self.text(c, self.f.sans, 12.0, pill.x + 27.0, cy, &label, t.text2);
            self.tip_zone(input, pill, tr("Working in the background · the playlist plays the take meanwhile"), "");
        }
        self.icon_button(c, input, wid("pe-help", 0, 0), help, Icon::Info, false, (tr(HELP), ""));
        let from = self.pitch_play_point();
        let tip = if self.playing { tr("Stop") } else { tr("Play the song from the selection (or the clip's start)") };
        if self.icon_button(c, input, wid("pe-play", 0, 0), play, if self.playing { Icon::Stop } else { Icon::Play }, self.playing, (tip, "")) {
            self.pitch_play_from(from);
        }
        if self.icon_button(c, input, wid("pe-close", 0, 0), close, Icon::Close, false, (tr("Close the Pitch Editor"), "Esc")) {
            self.close_pitch_editor();
        }
    }

    /// Where Play starts: the first selected note, else the clip's start.
    fn pitch_play_point(&self) -> f64 {
        let notes = self.pitch_notes();
        let first = self.pitch.selected.iter().filter_map(|&i| notes.get(i)).map(|n| n.new_start).fold(f64::MAX, f64::min);
        if first < f64::MAX {
            return (first - 0.25).max(0.0);
        }
        self.pitch.open.as_ref().and_then(|(_, i)| self.project.audio_clips.get(*i)).map_or(0.0, |c| c.offset)
    }

    /// Before the analysis: a big icon and a progress bar.
    fn pitch_waiting(&mut self, c: &mut Canvas, body: Rect, progress: Option<f32>) {
        let t = self.theme;
        let (cx, cy) = body.center();
        c.fill_circle(cx, cy - 34.0, 30.0, t.accent.alpha(0.12));
        let pulse = 0.75 + 0.25 * (self.time * 3.0).sin();
        draw_icon(c, Icon::PitchBlobs, cx, cy - 34.0, 30.0, t.accent.alpha(pulse));
        self.text_center(c, self.f.sans_med, 15.0, cx, cy + 12.0, tr("Finding the notes of the take…"), t.text);
        let bar = Rect::new(cx - 140.0, cy + 34.0, 280.0, 6.0);
        c.fill_rrect(bar, 3.0, t.well);
        let f = progress.unwrap_or(0.0);
        c.fill_rrect(Rect::new(bar.x, bar.y, (bar.w * f).max(6.0), bar.h), 3.0, t.accent);
        self.text_center(c, self.f.sans, 12.0, cx, cy + 58.0, tr("Pitch every 2 ms, notes, vibrato and level · once per file"), t.muted);
    }

    /// Note names beside the rows: every C, and the scale's notes when there is room.
    fn pitch_keys(&mut self, c: &mut Canvas, r: Rect, sy: f32, row_h: f32) {
        let t = self.theme;
        c.fill_rrect(r, 8.0, t.strip);
        let prev = c.push_clip(r);
        let y_of = |m: f32| r.y + (sy - m) * row_h;
        let scaled = self.project.scale != ScaleKind::Chromatic;
        let top = sy.ceil() as i32 + 1;
        let bottom = (sy - r.h / row_h).floor() as i32 - 1;
        for k in (bottom..=top).filter(|k| (0..128).contains(k)) {
            let y = y_of(k as f32);
            let black = matches!(k % 12, 1 | 3 | 6 | 8 | 10);
            if black {
                c.fill_rect(Rect::new(r.x, y - 0.5 * row_h, r.w * 0.55, row_h), t.key_black.alpha(0.6));
            }
            let tonic = scaled && (k as u8 + 12 - self.project.key % 12) % 12 == 0;
            let in_scale = self.project.in_scale(k as u8);
            let show = k % 12 == 0 || tonic || (row_h >= 15.0 && (in_scale || !scaled) && !black) || (row_h >= 22.0 && in_scale);
            if show {
                let col = if tonic { t.accent } else if k % 12 == 0 { t.text2 } else { t.muted };
                self.text_right(c, self.f.mono, 10.5, r.right() - 6.0, y, &note_name(k as u8), col);
            }
            if scaled && in_scale {
                c.fill_rect(Rect::new(r.right() - 3.0, y - 0.5 * row_h + 2.0, 2.0, row_h - 4.0), if tonic { t.accent } else { t.accent.alpha(0.35) });
            }
        }
        c.set_clip(Some(prev));
    }

    /// Seconds of the take, the play start (a click places it) and the playhead.
    #[allow(clippy::too_many_arguments)]
    fn pitch_ruler(&mut self, c: &mut Canvas, input: &Input, r: Rect, (sx, zoom): (f32, f32), step: f64, start: Option<f64>, head: Option<f64>) {
        let t = self.theme;
        c.fill_rrect(r, 8.0, t.strip);
        let prev = c.push_clip(r);
        let x_of = |s: f64| r.x + (s as f32 - sx) * zoom;
        let first = (f64::from(sx) / step).floor() as i64;
        let last = (f64::from(sx + r.w / zoom) / step).ceil() as i64;
        for k in first..=last {
            let s = k as f64 * step;
            let x = x_of(s).round();
            c.fill_rect(Rect::new(x, r.bottom() - 7.0, 1.0, 7.0), t.muted.alpha(0.6));
            let label = if step < 1.0 { format!("{s:.2} s") } else if s >= 60.0 { format!("{}:{:02}", (s / 60.0) as i64, (s % 60.0) as i64) } else { format!("{s:.0} s") };
            self.text(c, self.f.mono, 10.5, x + 4.0, r.y + r.h * 0.45, &label, t.muted);
        }
        if let Some(s) = start {
            let x = x_of(s);
            let mut tri = Path::new();
            tri.polygon(&[(x - 5.0, r.y + 4.0), (x + 5.0, r.y + 4.0), (x, r.y + 11.0)]);
            c.fill_path(&tri, t.select);
        }
        if let Some(s) = head {
            c.fill_rect(Rect::new(x_of(s).round(), r.y, 2.0, r.h), t.playhead);
        }
        c.set_clip(Some(prev));
        let id = wid("pe-ruler", 0, 0);
        if self.hovered(input, id, r) {
            self.set_tip(tr("Click: play from here"), "");
            if input.pressed {
                let s = f64::from(sx + (input.x - r.x) / zoom).max(0.0);
                if let Some(cl) = self.pitch.open.as_ref().and_then(|(_, i)| self.project.audio_clips.get(*i)).cloned() {
                    let duration = self.pitch.analysis.as_ref().map_or(0.0, |a| a.duration());
                    self.start_marker = self.pitch_tick_of(&cl, s, duration);
                    if self.playing {
                        self.send(ponzik_engine::Command::Locate(self.start_marker));
                    }
                }
            }
        }
    }

    /// The level of the take under the grid: voiced parts in the note color, consonants and breaths grey.
    fn pitch_lane(&mut self, c: &mut Canvas, r: Rect, a: &Analysis, map: &TimeMap, (sx, zoom): (f32, f32), span: Option<(f64, f64)>) {
        let t = self.theme;
        c.fill_rrect(r, 8.0, t.well);
        let prev = c.push_clip(r);
        let mid = r.y + r.h * 0.5;
        let mut x = r.x;
        while x < r.right() {
            let s = map.src(f64::from(sx + (x - r.x) / zoom));
            if (0.0..a.duration()).contains(&s) {
                let lv = pitchedit::interp(&a.level, s / HOP);
                let v = ((lv + 60.0) / 60.0).clamp(0.0, 1.0);
                let h = (v * (r.h * 0.5 - 3.0)).max(0.5);
                let voiced = a.pitch.get(a.frame_at(s)).is_some_and(|p| p.is_finite());
                let col = if voiced { t.note.alpha(0.7) } else { t.wave.alpha(0.45) };
                c.fill_rect(Rect::new(x, mid - h, 2.0, 2.0 * h), col);
            }
            x += 3.0;
        }
        if let Some((s0, s1)) = span {
            let (x0, x1) = (r.x + (s0 as f32 - sx) * zoom, r.x + (s1 as f32 - sx) * zoom);
            if x0 > r.x {
                c.fill_rect(Rect::new(r.x, r.y, x0 - r.x, r.h), t.bg_deep.alpha(0.5));
            }
            if x1 < r.right() {
                c.fill_rect(Rect::new(x1, r.y, r.right() - x1, r.h), t.bg_deep.alpha(0.5));
            }
        }
        c.set_clip(Some(prev));
    }

    /// The inspector: the selection, the correction macro, the note's sound, cut and join.
    fn pitch_side(&mut self, c: &mut Canvas, input: &Input, r: Rect, a: &Analysis, notes: &[NoteEdit]) {
        let t = self.theme;
        let sel: Vec<usize> = self.pitch.selected.iter().copied().filter(|&i| i < notes.len()).collect();
        let first = sel.first().map(|&i| notes[i]);
        // The selection.
        // A short window: tighter cards, the same controls.
        let compact = r.h < 410.0;
        let gap = if compact { 8.0 } else { 10.0 };
        let card = Rect::new(r.x, r.y, r.w, if compact { 56.0 } else { 74.0 });
        c.fill_rrect(card, 12.0, t.raised);
        match (sel.len(), first) {
            (1, Some(n)) => {
                let p = if self.pitch.original { n.centre } else { n.pitch };
                let k = p.round();
                let cents = ((p - k) * 100.0).round() as i32;
                self.text(c, self.f.mono_med, 22.0, card.x + 14.0, card.y + 24.0, &note_name(k.clamp(0.0, 127.0) as u8), t.text);
                let col = if cents.abs() <= 10 { t.accent } else if cents.abs() <= 25 { t.warning } else { t.negative };
                let cents_label = tr_fmt!("{:+} cents", cents);
                self.text(c, self.f.mono, 12.0, card.x + 84.0, card.y + 18.0, &cents_label, col);
                let vib = a.stats(n.start, n.end).map_or(0.0, |s| s.vibrato) * n.vibrato;
                let info = tr_fmt!("{} s · vibrato {} c", ponzik_i18n::num(n.new_end - n.new_start, 2), vib.round() as i32);
                self.text(c, self.f.sans, 11.5, card.x + 84.0, card.y + 34.0, &info, t.muted);
                // Where it is against the nearest semitone: a small bar.
                let bar = Rect::new(card.x + 14.0, card.y + if compact { 47.0 } else { 54.0 }, card.w - 28.0, 4.0);
                c.fill_rrect(bar, 2.0, t.well);
                let cx = bar.x + bar.w * 0.5;
                c.fill_rect(Rect::new(cx, bar.y - 3.0, 1.0, bar.h + 6.0), t.muted);
                let off = (cents as f32 / 50.0).clamp(-1.0, 1.0) * bar.w * 0.5;
                c.fill_circle(cx + off, bar.y + 2.0, if compact { 4.0 } else { 5.0 }, col);
            }
            (0, _) => {
                self.text(c, self.f.sans_med, 14.0, card.x + 14.0, card.y + 24.0, tr("No note selected"), t.text2);
                self.text(c, self.f.sans, 11.5, card.x + 14.0, card.y + 46.0, tr("Click a note or drag a box"), t.muted);
                if !compact {
                    self.text(c, self.f.sans, 11.5, card.x + 14.0, card.y + 61.0, &tr_fmt!("{} notes in the take", notes.len()), t.muted);
                }
            }
            (count, _) => {
                self.text(c, self.f.sans_med, 16.0, card.x + 14.0, card.y + 26.0, &tr_fmt!("{} notes selected", count), t.text);
                self.text(c, self.f.sans, 11.5, card.x + 14.0, card.y + 50.0, tr("Edits apply to all of them"), t.muted);
            }
        }

        // Correct pitch: the macro, with the scale it aims at.
        let mac = Rect::new(r.x, card.bottom() + gap, r.w, if compact { 130.0 } else { 140.0 });
        c.fill_rrect(mac, 12.0, t.raised.alpha(0.6));
        draw_icon(c, Icon::Magnet, mac.x + 18.0, mac.y + 17.0, 13.0, t.accent);
        self.text(c, self.f.sans_med, 13.0, mac.x + 32.0, mac.y + 17.0, tr("Correct pitch"), t.text);
        let scale_name = if self.project.scale == ScaleKind::Chromatic {
            tr("Chromatic").to_string()
        } else {
            format!("{} {}", ponzik_project::scale::key_name(self.project.key), tr(self.project.scale.name()).to_lowercase())
        };
        let sname = self.fit(self.f.sans, 11.0, &scale_name, 90.0);
        let sw = self.fonts.measure(self.f.sans, 11.0, &sname) + 28.0;
        let chip = Rect::new(mac.right() - sw - 10.0, mac.y + 7.0, sw, 20.0);
        c.fill_rrect(chip, 10.0, t.well);
        draw_icon(c, Icon::Scale, chip.x + 11.0, chip.y + 10.0, 11.0, t.accent);
        self.text(c, self.f.sans, 11.0, chip.x + 21.0, chip.y + 10.0, &sname, t.text2);
        self.tip_zone(input, chip, tr("The project's scale: notes are pulled to it (the top bar changes it)"), "");
        let mid = mac.x + mac.w * 0.5;
        let cell = |k: usize| Rect::new(mid - 84.0 + k as f32 * 92.0, mac.y + if compact { 24.0 } else { 30.0 }, 76.0, 80.0);
        if let Some(v) = self.knob(c, input, wid("pe-centre", 0, 0), cell(0), &CENTRE, self.pitch.centre) {
            self.pitch.centre = v;
        }
        self.tip_zone(input, cell(0), tr("How far each centre goes to the nearest note of the scale"), "");
        if let Some(v) = self.knob(c, input, wid("pe-drift", 0, 0), cell(1), &DRIFT, self.pitch.drift) {
            self.pitch.drift = v;
        }
        self.tip_zone(input, cell(1), tr("How much of the slow wandering around the centre is flattened"), "");
        let label = if sel.is_empty() { tr("Correct all") } else { tr("Correct") };
        let apply = Rect::new(mac.x + 12.0, mac.bottom() - 38.0, mac.w - 24.0, 28.0);
        if self.pill_button(c, input, wid("pe-apply", 0, 0), apply, Icon::Check, label, true) {
            self.pitch_correct();
        }
        self.tip_zone(input, apply, tr("Pulls the notes to the scale and flattens their drift, by the amounts above"), "");

        // The selection's sound: vibrato, formant, level.
        let snd = Rect::new(r.x, mac.bottom() + gap, r.w, if compact { 106.0 } else { 116.0 });
        c.fill_rrect(snd, 12.0, t.raised.alpha(0.6));
        draw_icon(c, Icon::PitchBlobs, snd.x + 18.0, snd.y + 17.0, 13.0, t.accent);
        self.text(c, self.f.sans_med, 13.0, snd.x + 32.0, snd.y + 17.0, tr("Note"), t.text);
        let cell = |k: usize| Rect::new(snd.x + 8.0 + k as f32 * 80.0, snd.y + if compact { 24.0 } else { 30.0 }, 76.0, 80.0);
        let n0 = first.unwrap_or_else(|| NoteEdit::from_note(&pitchedit::Note { start: 0.0, end: 0.0, centre: 60.0, vibrato: 0.0, level: 0.0 }));
        let params: [(&ParamInfo, f32, &str); 3] = [(&VIBRATO, n0.vibrato, "pe-vib"), (&FORMANT, n0.formant, "pe-formant"), (&LEVEL, n0.gain, "pe-level")];
        for (k, (info, value, id)) in params.into_iter().enumerate() {
            if let Some(v) = self.knob(c, input, wid(id, 0, 0), cell(k), info, value)
                && !sel.is_empty()
            {
                let mut next = notes.to_vec();
                for &i in &sel {
                    match k {
                        0 => next[i].vibrato = v,
                        1 => next[i].formant = v,
                        _ => next[i].gain = v,
                    }
                }
                let label = [tr("Vibrato"), tr("Formant"), tr("Note level")][k];
                self.set_pitch_notes(label, next);
            }
        }
        if sel.is_empty() {
            c.fill_rrect(Rect::new(snd.x, snd.y + 28.0, snd.w, snd.h - 28.0), 12.0, t.panel.alpha(0.55));
            self.tip_zone(input, snd, tr("Select notes to change their vibrato, formants and level"), "");
        }

        // Cut, join, reset, select all.
        let row = Rect::new(r.x, snd.bottom() + gap, r.w, 40.0);
        c.fill_rrect(row, 12.0, t.raised.alpha(0.6));
        let bw = (row.w - 8.0) / 4.0;
        let b = |k: usize| Rect::new(row.x + 4.0 + k as f32 * bw, row.y + 2.0, bw, 36.0);
        if self.icon_button(c, input, wid("pe-split", 0, 0), b(0), Icon::Slice, false, (tr("Cut the selected note in two\nOr double-click a note where to cut"), "")) {
            self.pitch_split_selected(None);
        }
        if self.icon_button(c, input, wid("pe-join", 0, 0), b(1), Icon::Glue, false, (tr("Join the selected notes into one"), "")) {
            self.pitch_join_selected();
        }
        if self.icon_button(c, input, wid("pe-reset", 0, 0), b(2), Icon::Reset, false, (tr("Back as recorded: the selection, or every note"), "Delete")) {
            self.pitch_reset_selected();
        }
        if self.icon_button(c, input, wid("pe-all", 0, 0), b(3), Icon::Columns, false, (tr("Select all the notes"), "Ctrl+A")) {
            self.pitch.selected = (0..notes.len()).collect();
        }

        // Tuning: how far the notes sit from the scale, as recorded and as edited.
        let tune_card = Rect::new(r.x, row.bottom() + 10.0, r.w, (r.bottom() - 30.0 - row.bottom() - 10.0).min(150.0));
        if tune_card.h >= 84.0 && !notes.is_empty() {
            self.pitch_tuning(c, input, tune_card, notes);
        }

        // What plays.
        let edited = notes.iter().filter(|n| !n.is_identity()).count();
        let status = if self.pitch.original {
            tr("Playing the take as recorded").to_string()
        } else if edited == 0 {
            tr("No edit yet · the take plays as recorded").to_string()
        } else {
            tr_fmt!("{} notes edited · they play in the playlist", edited)
        };
        let status = self.fit(self.f.sans, 11.5, &status, r.w - 8.0);
        if row.bottom() + 24.0 < r.bottom() {
            let dot = if self.pitch.original { t.warning } else if edited > 0 { t.accent } else { t.muted };
            c.fill_circle(r.x + 8.0, r.bottom() - 10.0, 3.5, dot);
            self.text(c, self.f.sans, 11.5, r.x + 18.0, r.bottom() - 10.0, &status, t.muted);
        }
    }

    /// Histogram of the notes' distance to the scale (cents, weighted by length):
    /// bars as edited, outlines as recorded, and the share within ±10 cents.
    fn pitch_tuning(&mut self, c: &mut Canvas, input: &Input, r: Rect, notes: &[NoteEdit]) {
        let t = self.theme;
        c.fill_rrect(r, 12.0, t.raised.alpha(0.6));
        let mask = self.pitch_mask();
        let bins = |pick: &dyn Fn(&NoteEdit) -> f32| {
            let mut h = [0.0f32; 11];
            let mut good = 0.0;
            let mut total = 0.0;
            for n in notes {
                let p = pick(n);
                let Some(target) = ponzik_engine::effects::autotune::nearest(p, mask) else { continue };
                let cents = ((p - target as f32) * 100.0).clamp(-55.0, 55.0);
                let w = (n.end - n.start) as f32;
                h[((cents + 55.0) / 10.0).floor().clamp(0.0, 10.0) as usize] += w;
                total += w;
                if cents.abs() <= 10.0 {
                    good += w;
                }
            }
            (h, if total > 0.0 { good / total } else { 0.0 })
        };
        let (now, now_good) = bins(&|n| if self.pitch.original { n.centre } else { n.pitch });
        let (was, was_good) = bins(&|n| n.centre);
        let changed = !self.pitch.original && notes.iter().any(|n| (n.pitch - n.centre).abs() > 1e-3);
        draw_icon(c, Icon::Detect, r.x + 18.0, r.y + 17.0, 13.0, t.accent);
        self.text(c, self.f.sans_med, 13.0, r.x + 32.0, r.y + 17.0, tr("Intonation"), t.text);
        let pct = format!("{} %", (now_good * 100.0).round() as i32);
        let right = self.text_right(c, self.f.mono_med, 13.0, r.right() - 12.0, r.y + 17.0, &pct, t.accent);
        if changed {
            let was_pct = format!("{} % →", (was_good * 100.0).round() as i32);
            self.text_right(c, self.f.mono, 11.0, right - 6.0, r.y + 17.0, &was_pct, t.muted);
        }
        let plot = Rect::new(r.x + 14.0, r.y + 34.0, r.w - 28.0, r.h - 62.0);
        let max = now.iter().chain(was.iter()).copied().fold(1e-6f32, f32::max);
        let bw = plot.w / 11.0;
        for k in 0..11 {
            let x = plot.x + k as f32 * bw;
            let centre = k as f32 * 10.0 - 50.0;
            let col = if centre.abs() <= 10.0 {
                t.accent
            } else if centre.abs() <= 25.0 {
                t.warning
            } else {
                t.negative
            };
            let h = (now[k] / max * plot.h).max(if now[k] > 0.0 { 2.0 } else { 0.0 });
            if h > 0.0 {
                c.fill_rrect(Rect::new(x + 2.0, plot.bottom() - h, bw - 4.0, h), 2.0, col.alpha(0.75));
            }
            if changed && was[k] > 0.0 {
                let h = (was[k] / max * plot.h).max(2.0);
                c.stroke_rrect(Rect::new(x + 2.0, plot.bottom() - h, bw - 4.0, h), 2.0, 1.0, t.text.alpha(0.35));
            }
        }
        c.fill_rect(Rect::new(plot.x, plot.bottom(), plot.w, 1.0), t.hairline);
        let ly = plot.bottom() + 11.0;
        self.text(c, self.f.mono, 10.0, plot.x, ly, "-50", t.muted);
        self.text_center(c, self.f.mono, 10.0, plot.x + plot.w * 0.5, ly, "0", t.muted);
        self.text_right(c, self.f.mono, 10.0, plot.right(), ly, "+50", t.muted);
        self.tip_zone(input, r, tr("How far the notes sit from the scale, in cents, by length\nBars: as edited · outlines: as recorded · figure: within 10 cents"), "");
    }

    /// `PONZIK_PITCH=[take:<file>|analysing][,edited][,select:<n>]`: the Pitch
    /// Editor on a sung take (synthetic unless a file is given).
    pub fn screenshot_pitch(&mut self, spec: &str) {
        let parts: Vec<&str> = spec.split(',').collect();
        let file = parts.iter().find_map(|p| p.strip_prefix("take:"));
        let (path, sample) = match file.and_then(|f| ponzik_engine::wav::load_wav(f).ok().map(|s| (f.to_string(), s))) {
            Some((f, s)) => (f, s),
            None => ("Lead vocal take 3.wav".to_string(), demo_take()),
        };
        let sample = Arc::new(sample);
        let secs = sample.duration() as f64;
        self.samples.insert(path.clone(), sample.clone());
        let lane = self.project.clips.iter().map(|c| c.lane + 1).chain(self.project.audio_clips.iter().map(|c| c.lane + 1)).max().unwrap_or(0);
        let map = self.project.tempo_map();
        let len = map.tick_at_secs(secs).ceil() as u32;
        self.project.audio_clips.push(AudioClipDef { path: path.clone(), lane, track: 2, start: 0, len, color: 0x3ccfc4, ..Default::default() });
        // A minor, as the take is sung.
        if file.is_none() {
            self.project.key = 9;
            self.project.scale = ScaleKind::Minor;
        }
        self.commit(tr("Place an audio clip"));
        let clip = self.project.audio_clips.len() - 1;
        self.set_view(super::app::View::Playlist);
        // The editor gets the height of the window (`panel` keeps the devices).
        self.panel = parts.contains(&"panel");
        if parts.contains(&"analysing") {
            self.pitch.open = Some((path, clip));
            return;
        }
        let Some(a) = tune::analyze(&sample, None, None) else { return };
        self.pitch.open = Some((path.clone(), clip));
        self.pitch.analysis = Some(a.clone());
        let mut notes: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
        if parts.contains(&"edited") && notes.len() > 6 {
            // A note moved up a third, two corrected, one stretched.
            let mask = self.pitch_mask();
            for i in [1usize, 4] {
                pitchedit::correct_pitch(&mut notes[i], mask, 1.0);
                notes[i].drift = 0.8;
            }
            notes[3].pitch = notes[3].centre.round() + 3.0;
            let d = a.duration();
            let end = notes[6].new_end + 0.12;
            move_edge(&mut notes, 6, false, end, d);
            self.project.set_pitch_edit(&path, notes.clone());
            self.commit(tr("Correct pitch"));
            self.project.render_tunes(&|p: &str| self.samples.get(p).cloned());
            self.pitch.selected = vec![3];
        }
        if let Some(n) = parts.iter().find_map(|p| p.strip_prefix("select:")).and_then(|v| v.parse::<usize>().ok()) {
            self.pitch.selected = vec![n.min(notes.len().saturating_sub(1))];
        }
        self.pitch.centre = 0.85;
    }
}

/// A sung phrase in A minor, slightly off here and there, with vibrato,
/// drift, glides, breaths and sibilants (the Pitch Correction's singer).
fn demo_take() -> ponzik_engine::Sample {
    use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
    let sr = 48_000.0;
    let n = |start: f32, dur: f32, midi: f32, vowel: usize| Sung { start, dur, midi, vowel };
    let s = f32::NAN;
    let notes = [
        n(0.30, 0.42, 57.22, 0),
        n(0.72, 0.30, 59.86, 1),
        n(1.02, 0.55, 62.31, 3),
        n(1.57, 0.12, s, 0),
        n(1.69, 0.35, 63.78, 0),
        n(2.04, 0.30, 62.12, 2),
        n(2.34, 0.85, 59.83, 0),
        n(3.40, 0.10, s, 0),
        n(3.50, 0.40, 59.27, 4),
        n(3.90, 0.35, 56.81, 1),
        n(4.25, 0.30, 55.18, 3),
        n(4.55, 0.95, 56.92, 0),
        n(5.75, 0.12, s, 0),
        n(5.87, 0.40, 64.24, 1),
        n(6.27, 0.45, 66.71, 0),
        n(6.72, 0.35, 65.12, 3),
        n(7.07, 1.00, 63.86, 0),
    ];
    let style = Style { vibrato_cents: 38.0, vibrato_hz: 5.3, drift_cents: 28.0, glide: 0.07, level: 0.32 };
    let x = sing(sr, &notes, &style, 8.6);
    ponzik_engine::Sample { name: "Lead vocal take 3".into(), sample_rate: sr, right: x.clone(), left: x, markers: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64, end: f64, centre: f32) -> NoteEdit {
        NoteEdit::from_note(&pitchedit::Note { start, end, centre, vibrato: 0.0, level: -10.0 })
    }

    #[test]
    fn edges_move_with_their_neighbour_and_stay_bounded() {
        let mut v = vec![note(0.5, 1.0, 60.0), note(1.0, 1.5, 62.0), note(2.0, 2.5, 64.0)];
        // Touching notes: the boundary moves for both.
        move_edge(&mut v, 1, true, 0.8, 3.0);
        assert_eq!((v[0].new_end, v[1].new_start), (0.8, 0.8));
        // The end of the second stops at the third's start.
        move_edge(&mut v, 1, false, 2.4, 3.0);
        assert_eq!(v[1].new_end, 2.0);
        // At most four times longer, at least a quarter.
        move_edge(&mut v, 2, false, 9.0, 3.0);
        assert!((v[2].new_end - 3.0).abs() < 1e-9);
        move_edge(&mut v, 2, true, 2.95, 3.0);
        assert!((v[2].new_start - (v[2].new_end - 0.125)).abs() < 1e-9);
    }

    #[test]
    fn cut_and_join_give_back_the_note() {
        use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
        let x = sing(48_000.0, &[Sung { start: 0.1, dur: 0.8, midi: 60.0, vowel: 0 }], &Style::default(), 1.0);
        let a = pitchedit::analyze(&x, &x, 48_000.0);
        let mut v: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
        assert_eq!(v.len(), 1);
        v[0].pitch += 2.0;
        let whole = v[0];
        assert!(split_note(&a, &mut v, 0, 0.5));
        assert_eq!(v.len(), 2);
        assert!((v[0].shift() - 2.0).abs() < 1e-4 && (v[1].shift() - 2.0).abs() < 1e-4);
        assert_eq!((v[0].end, v[1].start), (v[1].start, v[0].end));
        let near_start = v[0].start + 0.01;
        assert!(!split_note(&a, &mut v, 0, near_start));
        assert!(join_notes(&a, &mut v, 0, 1));
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].start, v[0].end), (whole.start, whole.end));
        assert!((v[0].shift() - 2.0).abs() < 1e-4);
    }

    #[test]
    fn the_editor_edits_and_undoes() {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.screenshot_pitch("");
        assert!(app.pitch.analysis.as_ref().is_some_and(|a| a.notes.len() >= 12), "{:?}", app.pitch.analysis.as_ref().map(|a| a.notes.len()));
        app.pitch.selected = vec![2];
        app.pitch_nudge(1.0);
        let path = app.pitch.open.clone().unwrap().0;
        let d = app.project.pitch_edit(&path).expect("edited").notes.clone();
        assert!((d[2].shift() - 1.0).abs() < 1e-4);
        app.pitch.selected.clear();
        app.pitch_correct();
        let mask = app.pitch_mask();
        for n in &app.project.pitch_edit(&path).unwrap().notes {
            let target = ponzik_engine::effects::autotune::nearest(n.pitch, mask).unwrap() as f32;
            assert!((n.pitch - target).abs() <= 0.16, "{} for {target}", n.pitch);
        }
        app.undo();
        app.undo();
        assert!(app.project.pitch_edit(&path).is_none());
        app.close_pitch_editor();
        assert!(app.pitch.open.is_none());
    }
}

#[cfg(test)]
mod gestures {
    use super::*;
    use crate::gui::app::View;

    fn step(app: &mut App, c: &mut Canvas, input: &mut Input, x: f32, y: f32, down: bool) {
        input.pressed = down && !input.down;
        input.released = !down && input.down;
        (input.x, input.y, input.inside, input.down) = (x, y, true, down);
        app.frame(c, input, 1.0 / 60.0);
        input.double = false;
    }

    fn drag(app: &mut App, c: &mut Canvas, i: &mut Input, from: (f32, f32), to: (f32, f32)) {
        step(app, c, i, from.0, from.1, false);
        step(app, c, i, from.0, from.1, true);
        for k in 1..=6 {
            let f = k as f32 / 6.0;
            step(app, c, i, from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f, true);
        }
        step(app, c, i, to.0, to.1, false);
    }

    /// Screen point of note `i`: (x at its middle, its pitch), and of its end.
    fn at(app: &App, i: usize) -> ((f32, f32), (f32, f32)) {
        let (grid, sx, sy, zoom, row_h) = app.pitch.shown.expect("drawn");
        let n = app.pitch_notes()[i];
        let x = |s: f64| grid.x + (s as f32 - sx) * zoom;
        let y = grid.y + (sy - n.pitch) * row_h;
        ((x(0.5 * (n.new_start + n.new_end)), y), (x(n.new_end) - 2.0, y))
    }

    #[test]
    fn notes_move_stretch_cut_and_select_with_the_pointer() {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        let mut c = Canvas::new(1600, 960);
        let mut i = Input::default();
        app.screenshot_pitch("");
        for _ in 0..40 {
            step(&mut app, &mut c, &mut i, 5.0, 5.0, false);
        }
        let path = app.pitch.open.clone().unwrap().0;
        let row_h = app.pitch.shown.unwrap().4;
        // Up three rows: the centre lands three semitones up, on a semitone.
        let before = app.pitch_notes()[3];
        let (mid, _) = at(&app, 3);
        drag(&mut app, &mut c, &mut i, mid, (mid.0, mid.1 - 3.0 * row_h));
        let n = app.pitch_notes()[3];
        assert_eq!(n.pitch, before.pitch.round() + 3.0, "{} from {}", n.pitch, before.pitch);
        assert_eq!(app.pitch.selected, vec![3]);
        app.undo();
        assert!(app.project.pitch_edit(&path).is_none());
        // Alt: free, a row and a half.
        i.mods.alt = true;
        let (mid, _) = at(&app, 3);
        drag(&mut app, &mut c, &mut i, mid, (mid.0, mid.1 - 1.5 * row_h));
        i.mods.alt = false;
        let n = app.pitch_notes()[3];
        assert!((n.pitch - before.pitch - 1.5).abs() < 0.05, "{}", n.pitch - before.pitch);
        app.undo();
        // The last note's end, 0.2 s later.
        let last = app.pitch_notes().len() - 1;
        let zoom = app.pitch.shown.unwrap().3;
        let (_, end) = at(&app, last);
        let was = app.pitch_notes()[last];
        drag(&mut app, &mut c, &mut i, end, (end.0 + 0.2 * zoom, end.1));
        let n = app.pitch_notes()[last];
        assert!((n.new_end - was.new_end - 0.2).abs() < 0.02, "{} -> {}", was.new_end, n.new_end);
        assert_eq!((n.start, n.end, n.new_start), (was.start, was.end, was.new_start));
        // Double-click: a note in two.
        let count = app.pitch_notes().len();
        let (mid, _) = at(&app, 5);
        step(&mut app, &mut c, &mut i, mid.0, mid.1, false);
        i.double = true;
        step(&mut app, &mut c, &mut i, mid.0, mid.1, true);
        step(&mut app, &mut c, &mut i, mid.0, mid.1, false);
        assert_eq!(app.pitch_notes().len(), count + 1);
        // A box over the whole grid selects every note.
        let (grid, ..) = app.pitch.shown.unwrap();
        drag(&mut app, &mut c, &mut i, (grid.x + 3.0, grid.y + 3.0), (grid.right() - 3.0, grid.bottom() - 3.0));
        assert_eq!(app.pitch.selected.len(), count + 1);
        // Up arrow: everything a semitone up.
        i.keys.push((Key::Up, Modifiers::default()));
        step(&mut app, &mut c, &mut i, 5.0, 5.0, false);
        i.keys.clear();
        assert!(app.pitch_notes().iter().all(|n| (n.shift() - 1.0).abs() < 1e-4));
        // Another view closes the editor.
        app.set_view(View::Mixer);
        assert!(app.pitch.open.is_none());
    }
}
