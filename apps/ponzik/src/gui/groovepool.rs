//! Groove pool: the browser's Grooves tab (the project's pool, the built-in
//! library, extraction by drop), the groove popup of the piano roll and the
//! Channel Rack, the grooves' timing pictures, drag and drop of grooves onto
//! patterns, channels and clips, and the ghost of where grooved notes land.

use ponzik_engine::{ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::groove::{Groove, GrooveBase, GrooveId, factory, groove_from_loop, groove_from_midi, groove_warp_markers};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Popup, View};
use super::dnd::Payload;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Index of the Grooves tab in the browser.
pub const GROOVE_TAB: usize = 4;

static QUANTIZE: ParamInfo = ParamInfo::new(0, tr_noop("Quantize"), 0.0, 1.0, 0.0, Unit::Percent);
static TIMING: ParamInfo = ParamInfo::new(1, tr_noop("Timing"), 0.0, 1.0, 1.0, Unit::Percent);
static RANDOM: ParamInfo = ParamInfo::new(2, tr_noop("Random"), 0.0, 1.0, 0.0, Unit::Percent);
static VELOCITY: ParamInfo = ParamInfo::new(3, tr_noop("Velocity"), 0.0, 1.0, 1.0, Unit::Percent);

/// A groove offered somewhere: one of the pool, or one of the library
/// (copied into the pool when used).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroovePick {
    Pool(GrooveId),
    Factory(usize),
}

/// Who gets a groove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrooveTarget {
    Pattern(usize),
    Channel { pattern: usize, channel: usize },
}

/// State of the groove interface.
#[derive(Default)]
pub struct GrooveUi {
    /// Groove of the pool shown with its controls.
    pub sel: Option<GrooveId>,
    pub scroll: f32,
    /// The popup sets the piano roll's channel's own groove, not the pattern's.
    pub channel_scope: bool,
    /// Step being dragged in a timing picture, with its starting offset and velocity.
    edit: Option<(usize, f32, f32)>,
    /// The library in the interface's language.
    library: Option<(&'static str, Vec<Groove>)>,
}

impl GrooveUi {
    /// The built-in grooves, rebuilt when the language changes.
    pub fn library(&mut self) -> &[Groove] {
        let lang = ponzik_i18n::lang().code();
        if self.library.as_ref().is_none_or(|(l, _)| *l != lang) {
            self.library = Some((lang, factory::library()));
        }
        self.library.as_ref().map_or(&[], |(_, v)| v.as_slice())
    }
}

/// "2 bars · 1/16": the shape of a groove.
fn shape_label(g: &Groove) -> String {
    let bars = if g.bars() == 1 { tr("1 bar").to_string() } else { tr_fmt!("{} bars", g.bars()) };
    format!("{bars} · {}", if g.steps == 32 { "1/32" } else { "1/16" })
}

impl App {
    /// Name of a pick, to show while it is dragged.
    pub(super) fn groove_pick_name(&mut self, pick: GroovePick) -> String {
        match pick {
            GroovePick::Pool(id) => self.project.groove(id).map_or_else(String::new, |g| g.name.clone()),
            GroovePick::Factory(i) => self.groove_ui.library().get(i).map_or_else(String::new, |g| g.name.clone()),
        }
    }

    /// The pool's groove for `pick`: a library groove joins the pool (once).
    pub(super) fn pool_groove(&mut self, pick: GroovePick) -> Option<GrooveId> {
        match pick {
            GroovePick::Pool(id) => self.project.groove(id).map(|g| g.id),
            GroovePick::Factory(i) => {
                let g = self.groove_ui.library().get(i)?.clone();
                Some(self.project.find_or_add_groove(g))
            }
        }
    }

    /// Plays `pick` on `target` (`None`: straight again).
    pub(super) fn apply_groove(&mut self, target: GrooveTarget, pick: Option<GroovePick>) {
        let id = match pick {
            Some(p) => match self.pool_groove(p) {
                Some(id) => Some(id),
                None => return,
            },
            None => None,
        };
        match target {
            GrooveTarget::Pattern(p) => self.project.set_pattern_groove(p, id),
            GrooveTarget::Channel { pattern, channel } => self.project.set_channel_groove(pattern, channel, id),
        }
        if id.is_some() {
            self.groove_ui.sel = id;
        }
        self.commit(tr("Groove"));
        if let Some(g) = id.and_then(|id| self.project.groove(id)) {
            let name = g.name.clone();
            let to = match target {
                GrooveTarget::Pattern(p) => self.project.patterns.get(p).map_or_else(String::new, |p| p.name.clone()),
                GrooveTarget::Channel { channel, .. } => self.project.channels.get(channel).map_or_else(String::new, |c| c.name.clone()),
            };
            self.toast(&tr_fmt!("\"{}\" plays with {}", to, name));
        }
    }

    /// A groove dropped on a playlist lane: the pattern clip or the warped
    /// audio clip under the pointer takes it.
    pub(super) fn drop_groove_on_lane(&mut self, pick: GroovePick, lane: usize, tick: u32) {
        let under = |s: u32, l: u32| s <= tick && tick < s + l;
        if let Some(i) = self.project.audio_clips.iter().rposition(|c| c.lane == lane && under(c.start, c.len)) {
            self.groove_audio_clip(i, pick);
        } else if let Some(c) = self.project.clips.iter().rev().find(|c| c.lane == lane && under(c.start, c.len)) {
            let p = c.pattern;
            self.apply_groove(GrooveTarget::Pattern(p), Some(pick));
        } else {
            self.toast(tr("Drop the groove on a clip"));
        }
    }

    /// Moves the attacks of a warped audio clip where `pick` plays them (as
    /// warp markers: undo gives the clip back).
    pub(super) fn groove_audio_clip(&mut self, i: usize, pick: GroovePick) {
        let Some(clip) = self.project.audio_clips.get(i).cloned() else { return };
        if !clip.warp {
            self.toast(tr("Turn Warp on for the clip to follow a groove"));
            return;
        }
        let Some(sample) = self.samples.get(&clip.path).cloned() else {
            self.toast(tr("The clip's file is not loaded"));
            return;
        };
        let Some(id) = self.pool_groove(pick) else { return };
        let Some(g) = self.project.groove(id).cloned() else { return };
        match groove_warp_markers(&clip, &sample, &g) {
            Some(markers) => {
                self.project.audio_clips[i].warp_markers = markers;
                self.commit(tr("Groove the clip"));
                self.toast(&tr_fmt!("The clip's attacks follow \"{}\"", g.name));
            }
            None => self.toast(tr("No attacks found in the clip")),
        }
    }

    /// Extracts a groove from a sound file (a loop, its tempo guessed) or a
    /// MIDI file, into the pool.
    pub(super) fn extract_groove_from_file(&mut self, path: &std::path::Path) {
        let name = path.file_stem().map_or_else(|| tr("Groove").to_string(), |s| s.to_string_lossy().into_owned());
        let found = match super::dnd::file_kind(path) {
            super::dnd::FileKind::Midi => match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| groove_from_midi(&b, name.clone())) {
                Ok(g) => Some((g, None)),
                Err(e) => {
                    self.toast(&e);
                    return;
                }
            },
            super::dnd::FileKind::Audio => {
                let p = path.to_string_lossy().into_owned();
                if !self.ensure_sample(&p) {
                    return;
                }
                self.samples.get(&p).cloned().and_then(|s| groove_from_loop(&s, name.clone())).map(|(g, bpm)| (g, Some(bpm)))
            }
            _ => {
                self.toast(tr("Drop a loop or a MIDI file to take its groove"));
                return;
            }
        };
        match found {
            Some((g, bpm)) => {
                let id = self.project.add_groove(g);
                self.groove_ui.sel = Some(id);
                self.commit(tr("Extract a groove"));
                match bpm {
                    Some(bpm) => self.toast(&tr_fmt!("Groove \"{}\" saved in the pool (read at {:.1} BPM)", name, bpm)),
                    None => self.toast(&tr_fmt!("Groove \"{}\" saved in the pool", name)),
                }
            }
            None => self.toast(tr("No attacks found in the sound")),
        }
    }

    /// Extracts the groove of pattern `pat` into the pool.
    fn extract_pattern_groove(&mut self, pat: usize) {
        match self.project.extract_groove(pat) {
            Some(id) => {
                self.groove_ui.sel = Some(id);
                self.commit(tr("Extract a groove"));
                self.toast(tr("Groove saved in the pool"));
            }
            None => self.toast(tr("No notes to take a groove from")),
        }
    }

    /// Bakes the grooves of pattern `pat` into its notes.
    fn commit_pattern_groove(&mut self, pat: usize) {
        if self.project.commit_groove(pat) {
            self.piano.selected.clear();
            self.commit(tr("Commit the groove"));
            self.toast(tr("The notes now sit where the groove played them"));
        } else {
            self.toast(tr("Pick a groove first"));
        }
    }

    /// Picture of a groove over its bars: the grid, a bar per step as high
    /// as its accent, moved by its timing offset, a dot on top. Late steps in
    /// the hot accent, early ones in the selection color. With `edit`, steps
    /// can be dragged (sideways: timing, up and down: accent); returns the
    /// edited groove.
    fn groove_picture(&mut self, c: &mut Canvas, input: Option<&Input>, r: Rect, g: &Groove, edit: u64) -> Option<Groove> {
        let t = self.theme;
        c.fill_rrect(r, 8.0, t.well);
        let n = g.timing.len().max(1);
        let pad = 6.0;
        let inner = Rect::new(r.x + pad, r.y + 4.0, r.w - 2.0 * pad, r.h - 8.0);
        let slot_w = inner.w / n as f32;
        let px_per_tick = slot_w / g.step_ticks() as f32;
        let per_beat = (g.steps / 4).max(1) as usize;
        let big = r.h > 50.0;
        let base_y = inner.bottom() - 2.0;
        let max_h = inner.h - 8.0;
        let mut changed: Option<Groove> = None;
        let mut hot: Option<usize> = None;
        for k in 0..n {
            let gx = (inner.x + (k as f32 + 0.5) * slot_w).round();
            let bar = k % g.steps as usize == 0;
            let beat = k % per_beat == 0;
            let line = if bar && k > 0 { 3.2 } else if beat { 1.9 } else { 1.0 };
            if slot_w >= 2.5 || beat {
                c.fill_rect(Rect::new(gx, inner.y + 2.0, 1.0, inner.h - 4.0), t.hairline.alpha(line));
            }
            let dt = g.timing[k];
            let vel = g.velocity.get(k).copied().unwrap_or(1.0);
            let bx = gx + dt * px_per_tick;
            let h = (max_h * (vel / 1.6).clamp(0.05, 1.0)).max(3.0);
            let col = if dt > 0.5 {
                t.accent_hot
            } else if dt < -0.5 {
                t.select
            } else {
                t.accent
            };
            let bw = (slot_w * 0.32).clamp(1.0, if big { 5.0 } else { 3.0 });
            if dt.abs() > 0.5 && slot_w >= 3.0 {
                // The way from the grid to where the step lands.
                c.fill_rect(Rect::new(gx.min(bx), base_y, (bx - gx).abs().max(1.0), 1.5), col.alpha(0.6));
            }
            c.fill_rrect(Rect::new(bx - bw * 0.5, base_y - h, bw, h), bw * 0.4, col.alpha(0.35 + 0.45 * (vel / 1.6).min(1.0)));
            if slot_w >= 3.0 {
                c.fill_circle(bx, base_y - h, if big { 3.5 } else { 1.8 }, col);
            }
            if let Some(input) = input
                && input.inside
                && (input.x - bx).abs() <= (slot_w * 0.5).max(4.0)
                && r.contains(input.x, input.y)
                && self.groove_ui.edit.is_none()
            {
                hot = Some(k);
            }
        }
        let input = input?;
        // Editing: grab the nearest step.
        let id = wid("groove-pic", edit as usize, 0);
        if let Some(k) = hot.filter(|_| self.drag.is_none()) {
            self.set_cursor(Cursor::Move);
            let ms = g.timing[k] / ponzik_engine::PPQ as f32 * 60_000.0 / self.project.bpm.max(1.0);
            self.set_tip(
                &tr_fmt!(
                    "Step {} · {:+.1} ticks ({:+.0} ms) · accent {:.0}%\nDrag sideways: timing · up and down: accent · double-click: reset",
                    k + 1,
                    g.timing[k],
                    ms,
                    g.velocity[k] * 100.0
                ),
                "",
            );
            if input.pressed {
                if input.double {
                    let mut e = g.clone();
                    e.timing[k] = 0.0;
                    e.velocity[k] = 1.0;
                    changed = Some(e);
                } else {
                    self.groove_ui.edit = Some((k, g.timing[k], g.velocity[k]));
                    self.begin_drag(id, input, 0.0);
                }
            }
        }
        if let (Some((k, t0, v0)), Some(d)) = (self.groove_ui.edit, self.drag.filter(|d| d.id == id)) {
            if k < n {
                let half = g.step_ticks() as f32 * 0.5;
                let mut e = g.clone();
                e.timing[k] = ((t0 + (input.x - d.x0) / px_per_tick) * 2.0).round().clamp(-2.0 * half, 2.0 * half) / 2.0;
                e.velocity[k] = ((v0 + (d.y0 - input.y) / max_h * 1.6) * 100.0).round().clamp(5.0, 160.0) / 100.0;
                if e != *g {
                    changed = Some(e);
                }
                let x = inner.x + (k as f32 + 0.5) * slot_w;
                c.fill_rect(Rect::new(x.round(), inner.y, 1.0, inner.h), t.text.alpha(0.5));
            }
        } else if self.groove_ui.edit.is_some() && !self.is_dragging(id) {
            self.groove_ui.edit = None;
        }
        changed
    }

    /// Base, picture and amounts of pool groove `id`, from `y`; returns the
    /// height used.
    fn groove_editor(&mut self, c: &mut Canvas, input: &Input, x: f32, y: f32, w: f32, id: GrooveId) -> f32 {
        let Some(g) = self.project.groove(id).cloned() else { return 0.0 };
        let pic = Rect::new(x, y, w, 76.0);
        if let Some(e) = self.groove_picture(c, Some(input), pic, &g, id as u64) {
            if let Some(m) = self.project.groove_mut(id) {
                *m = e;
            }
            self.commit(tr("Edit the groove"));
        }
        let mut yy = pic.bottom() + 8.0;
        // Base: one row of six, or two of three when narrow.
        let tip = tr("Base\nThe grid the notes are matched against: each note takes the groove's offset at its grid point");
        let sel = GrooveBase::ALL.iter().position(|b| *b == g.base).unwrap_or(3);
        let per = if w >= 300.0 { 6 } else { 3 };
        let cw = (w - (per - 1) as f32 * 4.0) / per as f32;
        for (i, b) in GrooveBase::ALL.iter().enumerate() {
            let cell = Rect::new(x + (i % per) as f32 * (cw + 4.0), yy + (i / per) as f32 * 30.0, cw, 26.0);
            let hov = input.inside && cell.contains(input.x, input.y);
            self.choice_cell(c, cell, wid("groove-base", i, id as usize), hov, i == sel, b.label());
            if hov {
                self.set_tip(tip, "");
                if input.pressed
                    && i != sel
                    && let Some(m) = self.project.groove_mut(id)
                {
                    m.base = *b;
                    self.commit(tr("Groove base"));
                }
            }
        }
        yy += (6 / per) as f32 * 30.0 + 4.0;
        let knobs = [(&QUANTIZE, g.quantize), (&TIMING, g.amount), (&RANDOM, g.random), (&VELOCITY, g.velocity_amount)];
        let per_row = if w >= 260.0 { 4 } else { 2 };
        let kw = w / per_row as f32;
        for (i, (info, v)) in knobs.iter().enumerate() {
            let cell = Rect::new(x + (i % per_row) as f32 * kw, yy + (i / per_row) as f32 * 74.0, kw, 72.0);
            if let Some(nv) = self.knob(c, input, wid("groove-amt", i, id as usize), cell, info, *v)
                && let Some(m) = self.project.groove_mut(id)
            {
                let nv = nv.clamp(0.0, 1.0);
                match i {
                    0 => m.quantize = nv,
                    1 => m.amount = nv,
                    2 => m.random = nv,
                    _ => m.velocity_amount = nv,
                }
                self.commit(tr("Groove amount"));
            }
        }
        yy += (4 / per_row) as f32 * 74.0;
        yy - y
    }

    /// One groove in a list: name, shape and uses, its picture; drags away
    /// as a groove. Returns (clicked, double-clicked).
    #[allow(clippy::too_many_arguments)]
    fn groove_row(&mut self, c: &mut Canvas, input: &Input, row: Rect, list: Rect, g: &Groove, pick: GroovePick, selected: bool, detail: &str) -> (bool, bool) {
        let t = self.theme;
        let key = match pick {
            GroovePick::Pool(id) => wid("gp-row", id as usize, 0),
            GroovePick::Factory(i) => wid("gp-lib", i, 0),
        };
        let visible = row.bottom() > list.y && row.y < list.bottom();
        let hov = visible && self.hovered(input, key, row) && list.contains(input.x, input.y);
        let hv = self.hover_anim(key, hov);
        let on = self.anim.to(key ^ 0x5e1, selected as u8 as f32, T_HOVER);
        if !visible {
            return (false, false);
        }
        c.fill_rrect(row, 10.0, t.raised.alpha((0.35 + 0.5 * hv).max(on)));
        if on > 0.01 {
            c.stroke_rrect(row, 10.0, 1.0, t.accent.alpha(0.7 * on));
        }
        let thumb_w = (row.w * 0.34).clamp(60.0, 130.0);
        let thumb = Rect::new(row.right() - thumb_w - 8.0, row.y + 7.0, thumb_w, row.h - 14.0);
        let text_w = thumb.x - row.x - 18.0;
        self.text_fit(c, self.f.sans_med, 13.0, row.x + 12.0, row.y + 16.0, &g.name, text_w, if selected { t.text } else { t.text2.mix(t.text, hv) });
        self.text_fit(c, self.f.sans, 11.0, row.x + 12.0, row.y + 33.0, detail, text_w, t.muted);
        self.groove_picture(c, None, thumb, g, 0);
        let (mut clicked, mut double) = (false, false);
        if hov {
            self.set_cursor(Cursor::Grab);
            self.set_tip(
                &tr_fmt!("{}\nClick: settings · double-click: play it in the current pattern\nDrag onto a pattern, a channel or a clip", g.name),
                "",
            );
            if input.pressed {
                if input.double {
                    double = true;
                } else {
                    clicked = true;
                    self.begin_dnd(input, Payload::Groove(pick));
                }
            }
        }
        (clicked, double)
    }

    /// The browser's Grooves tab, in `r` (the list area under the tabs).
    pub(super) fn groove_pool_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect, x: f32, w: f32) {
        let t = self.theme;
        let dropping = self.drop_zone(input, r, super::dnd::DropTarget::GroovePool);
        let hov_list = input.inside && r.contains(input.x, input.y);
        if hov_list && input.wheel != 0.0 {
            self.groove_ui.scroll = (self.groove_ui.scroll - input.wheel * 60.0).max(0.0);
        }
        let scroll = self.anim.to(wid("gp-scroll", 0, 0), self.groove_ui.scroll, T_LAYOUT);
        let prev = c.push_clip(r);
        let top = r.y + 4.0 - scroll;
        let mut y = top;
        let pat = self.project.current_pattern;

        // Header: extract from the pattern, from the Sample Lab's sound.
        self.text(c, self.f.sans_med, 12.0, x + 2.0, y + 15.0, tr("Groove pool"), t.muted);
        let b1 = Rect::new(x + w - 32.0, y, 32.0, 30.0);
        let b0 = Rect::new(b1.x - 36.0, y, 32.0, 30.0);
        let tip = tr_fmt!("Extract from \"{}\"\nThe timing and accents of the pattern's notes", self.project.patterns[pat].name);
        if self.icon_button(c, input, wid("gp-extract", 0, 0), b0, Icon::Piano, false, (&tip, "")) {
            self.extract_pattern_groove(pat);
        }
        let lab = self.lab.path.clone();
        let tip = match &lab {
            Some(p) => tr_fmt!(
                "Extract from the Sample Lab's sound\n{} · its tempo is read from its length and beat",
                std::path::Path::new(p).file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned())
            ),
            None => tr("Extract from the Sample Lab's sound\nOpen a loop in the Sample Lab first, or drop one here").to_string(),
        };
        if self.icon_button(c, input, wid("gp-extract", 1, 0), b1, Icon::Lab, false, (&tip, "")) {
            match lab {
                Some(p) => self.extract_groove_from_file(std::path::Path::new(&p)),
                None => self.toast(tr("Open a loop in the Sample Lab first, or drop one here")),
            }
        }
        y += 38.0;

        // The pool.
        let pool = self.project.grooves.clone();
        if self.groove_ui.sel.is_some_and(|id| self.project.groove(id).is_none()) {
            self.groove_ui.sel = None;
        }
        if pool.is_empty() {
            let lines = self.wrap(self.f.sans, 12.0, tr("Drag a groove from the library onto a pattern, or drop a loop or a MIDI file here to take its groove."), w - 8.0);
            let box_h = 20.0 + lines.len() as f32 * 16.0;
            let hint = Rect::new(x, y, w, box_h);
            c.fill_rrect(hint, 10.0, t.raised.alpha(0.35));
            if dropping || self.dnd_active() {
                c.stroke_rrect(hint, 10.0, 1.0, t.accent.alpha(0.6));
            }
            for (i, l) in lines.iter().enumerate() {
                self.text(c, self.f.sans, 12.0, x + 10.0, y + 18.0 + i as f32 * 16.0, l, t.text2);
            }
            y += box_h + 10.0;
        }
        let row_h = 46.0;
        for g in &pool {
            let users = self.project.groove_users(g.id);
            let here = users.iter().any(|u| u.pattern == pat);
            let uses = match users.len() {
                0 => tr("unused").to_string(),
                1 => tr("1 use").to_string(),
                n => tr_fmt!("{} uses", n),
            };
            let detail = format!("{} · {uses}", shape_label(g));
            let row = Rect::new(x, y, w, row_h);
            let sel = self.groove_ui.sel == Some(g.id);
            let (click, double) = self.groove_row(c, input, row, r, g, GroovePick::Pool(g.id), sel, &detail);
            if here {
                c.fill_rrect(Rect::new(row.x + 3.0, row.y + 12.0, 3.0, row.h - 24.0), 1.5, t.accent);
            }
            if click {
                self.groove_ui.sel = if sel { None } else { Some(g.id) };
            }
            if double {
                self.apply_groove(GrooveTarget::Pattern(pat), Some(GroovePick::Pool(g.id)));
            }
            y += row_h + 4.0;
            if sel {
                y += 4.0;
                y += self.groove_editor(c, input, x, y, w, g.id) + 4.0;
                y += self.groove_uses(c, input, x, y, w, g.id) + 10.0;
            }
        }

        // The library.
        y += 6.0;
        c.fill_rect(Rect::new(x, y, w, 1.0), t.hairline);
        y += 10.0;
        self.text(c, self.f.sans_med, 12.0, x + 2.0, y + 8.0, tr("Library"), t.muted);
        y += 22.0;
        let lib: Vec<Groove> = self.groove_ui.library().to_vec();
        for (i, g) in lib.iter().enumerate() {
            let row = Rect::new(x, y, w, row_h);
            let detail = shape_label(g);
            let (click, double) = self.groove_row(c, input, row, r, g, GroovePick::Factory(i), false, &detail);
            if click {
                // Chosen in the library: it joins the pool, shown open.
                if let Some(id) = self.pool_groove(GroovePick::Factory(i)) {
                    self.groove_ui.sel = Some(id);
                    self.commit(tr("Add a groove to the pool"));
                }
            }
            if double {
                self.apply_groove(GrooveTarget::Pattern(pat), Some(GroovePick::Factory(i)));
            }
            y += row_h + 4.0;
        }
        c.set_clip(Some(prev));
        let content = y + scroll - r.y + 12.0;
        self.groove_ui.scroll = self.groove_ui.scroll.min((content - r.h).max(0.0));
        if dropping {
            c.fill_rrect(r.inset(4.0), 10.0, t.accent.alpha(0.06));
        }
    }

    /// Buttons for the selected groove and who plays it; returns the height.
    fn groove_uses(&mut self, c: &mut Canvas, input: &Input, x: f32, y: f32, w: f32, id: GrooveId) -> f32 {
        let t = self.theme;
        let pat = self.project.current_pattern;
        let users = self.project.groove_users(id);
        let names: Vec<String> = users
            .iter()
            .map(|u| {
                let p = self.project.patterns.get(u.pattern).map_or("", |p| p.name.as_str());
                match u.channel.and_then(|ch| self.project.channels.get(ch)) {
                    Some(ch) => format!("{p} ({})", ch.name),
                    None => p.to_string(),
                }
            })
            .collect();
        let text = if names.is_empty() { tr("Not used yet: drag it onto a pattern").to_string() } else { tr_fmt!("Played by {}", names.join(", ")) };
        let lines = self.wrap(self.f.sans, 11.0, &text, w - 4.0);
        let mut yy = y;
        for l in lines.iter().take(3) {
            self.text(c, self.f.sans, 11.0, x + 2.0, yy + 7.0, l, t.muted);
            yy += 15.0;
        }
        yy += 4.0;
        let plays = self.project.patterns.get(pat).and_then(|p| p.groove.as_ref()).is_some_and(|g| g.all == Some(id));
        let trash = Rect::new(x + w - 32.0, yy, 32.0, 32.0);
        let narrow = w < 300.0;
        let bw = if narrow { trash.x - x - 6.0 } else { (trash.x - x - 12.0) / 2.0 };
        let apply = Rect::new(x, yy, bw, 32.0);
        let commit = if narrow { Rect::new(x, yy + 38.0, w, 32.0) } else { Rect::new(apply.right() + 6.0, yy, bw, 32.0) };
        let (icon, label, tip) = if plays {
            (Icon::Close, tr("Remove"), tr("The current pattern plays straight again"))
        } else {
            (Icon::Groove, tr("Apply"), tr("The current pattern plays this groove"))
        };
        if self.lab_button(c, input, wid("gp-apply", id as usize, 0), apply, Some(icon), label, if plays { 0 } else { 1 }, tip) {
            let pick = (!plays).then_some(GroovePick::Pool(id));
            self.apply_groove(GrooveTarget::Pattern(pat), pick);
        }
        let commit_tip = tr("Commit\nMoves the current pattern's notes where its grooves play them");
        if self.lab_button(c, input, wid("gp-commit", id as usize, 0), commit, Some(Icon::Check), tr("Commit"), 0, commit_tip) {
            self.commit_pattern_groove(pat);
        }
        if self.icon_button(c, input, wid("gp-trash", id as usize, 0), trash, Icon::Trash, false, (tr("Remove from the pool\nIts patterns play straight again"), "")) {
            self.project.remove_groove(id);
            self.groove_ui.sel = None;
            self.commit(tr("Remove a groove"));
        }
        commit.bottom() - y
    }

    /// The groove popup (piano roll, Channel Rack): no groove, the pool and
    /// the library; the chosen groove's settings; extract and commit. False
    /// when it closes.
    pub(super) fn groove_popup(&mut self, c: &mut Canvas, input: &Input, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        const COLS: usize = 4;
        const GAP: f32 = 4.0;
        let (w, cell_h) = (520.0, 28.0);
        let cell_w = (w - 24.0 - (COLS - 1) as f32 * GAP) / COLS as f32;
        let pat = self.project.current_pattern;
        let piano_ch = if self.view == View::Piano { self.piano_channel() } else { None };
        let scope_ch = piano_ch.filter(|_| self.groove_ui.channel_scope);
        let target = match scope_ch {
            Some(channel) => GrooveTarget::Channel { pattern: pat, channel },
            None => GrooveTarget::Pattern(pat),
        };
        let current: Option<GrooveId> = {
            let pg = self.project.patterns[pat].groove.as_ref();
            match scope_ch {
                Some(ch) => pg.and_then(|g| g.channels.iter().find(|(c, _)| *c == ch).map(|&(_, id)| id)),
                None => pg.and_then(|g| g.all),
            }
        };
        let pool: Vec<Groove> = self.project.grooves.clone();
        let lib: Vec<Groove> = self.groove_ui.library().to_vec();
        let pool_rows = (pool.len() + 1).div_ceil(COLS) as f32;
        let lib_rows = lib.len().div_ceil(COLS) as f32;
        let editor_h = if current.is_some() { 76.0 + 8.0 + 28.0 + 6.0 + 74.0 + 10.0 } else { 0.0 };
        let scope_h = if piano_ch.is_some() { 36.0 } else { 0.0 };
        let h = 40.0 + scope_h + pool_rows * (cell_h + GAP) + 26.0 + lib_rows * (cell_h + GAP) + 10.0 + editor_h + 32.0 + 14.0;
        let x = anchor.x.min(screen.right() - 12.0 - w).max(12.0);
        let mut y0 = anchor.bottom() + 8.0;
        if y0 + h > screen.bottom() - 8.0 {
            y0 = (screen.bottom() - 8.0 - h).max(8.0);
        }
        let r = Rect::new(x, y0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, t.shade(90.0 * open));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        let title = tr_fmt!("Groove · {}", self.project.patterns[pat].name);
        let title = self.fit(self.f.sans_med, 12.0, &title, w - 80.0);
        self.text(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, &title, t.muted);
        let pool_btn = Rect::new(r.right() - 44.0, r.y + 6.0, 32.0, 28.0);
        if self.icon_button(c, input, wid("groove-open-pool", 0, 0), pool_btn, Icon::Sidebar, false, (tr("Groove pool\nEvery groove of the project, in the browser"), "")) {
            self.sidebar = true;
            self.sidebar_tab = GROOVE_TAB;
            self.groove_ui.sel = current;
        }
        let over = |b: Rect| input.inside && b.contains(input.x, input.y);
        let mut y = r.y + 34.0;
        if let Some(ch) = piano_ch {
            let seg = Rect::new(r.x + 12.0, y, w - 24.0, 28.0);
            let name = self.project.channels.get(ch).map_or_else(String::new, |c| c.name.clone());
            let ch_label = tr_fmt!("Only {}", name);
            let tips = [(tr("The whole pattern plays the groove"), ""), (tr("Only this channel plays it, in this pattern; it wins over the pattern's"), "")];
            if let Some(i) = self.segmented_tips(c, input, wid("groove-scope", 0, 0), seg, &[tr("Whole pattern"), &ch_label], &tips, scope_ch.is_some() as usize) {
                self.groove_ui.channel_scope = i == 1;
            }
            y += scope_h;
        }

        // Cells: none, the pool, then the library.
        let mut picked: Option<Option<GroovePick>> = None;
        let cell_at = |i: usize, y: f32| Rect::new(r.x + 12.0 + (i % COLS) as f32 * (cell_w + GAP), y + (i / COLS) as f32 * (cell_h + GAP), cell_w, cell_h);
        for i in 0..=pool.len() {
            let cell = cell_at(i, y);
            let (label, pick) = if i == 0 { (tr("None").to_string(), None) } else { (pool[i - 1].name.clone(), Some(GroovePick::Pool(pool[i - 1].id))) };
            let cur = match pick {
                None => current.is_none(),
                Some(GroovePick::Pool(id)) => current == Some(id),
                _ => false,
            };
            let hov = over(cell);
            self.choice_cell(c, cell, wid("groove-cell", i, 0), hov, cur, &label);
            if hov {
                if let Some(g) = pick.and_then(|_| pool.get(i - 1)) {
                    self.set_tip(&tr_fmt!("{}\n{} · up to {:.0} ticks off the grid", g.name, shape_label(g), g.depth()), "");
                }
                if input.pressed {
                    picked = Some(pick);
                }
            }
        }
        y += pool_rows * (cell_h + GAP) + 4.0;
        self.text(c, self.f.sans_med, 11.0, r.x + 16.0, y + 8.0, tr("Library"), t.muted);
        y += 20.0;
        for (i, g) in lib.iter().enumerate() {
            let cell = cell_at(i, y);
            let hov = over(cell);
            self.choice_cell(c, cell, wid("groove-lib", i, 0), hov, false, &g.name);
            if hov {
                self.set_tip(&tr_fmt!("{}\n{} · joins the project's pool when chosen", g.name, shape_label(g)), "");
                if input.pressed {
                    picked = Some(Some(GroovePick::Factory(i)));
                }
            }
        }
        y += lib_rows * (cell_h + GAP) + 8.0;
        if let Some(pick) = picked {
            self.apply_groove(target, pick);
        }
        if let Some(id) = current {
            y += self.groove_editor(c, input, r.x + 12.0, y, w - 24.0, id) + 6.0;
        }

        // Extract and commit.
        let bw = (w - 24.0 - 8.0) / 2.0;
        let extract = Rect::new(r.x + 12.0, y, bw, 32.0);
        if self.lab_button(c, input, wid("groove-extract", 0, 0), extract, Some(Icon::Groove), tr("Extract"), 0, tr("Saves this pattern's groove in the pool")) {
            self.extract_pattern_groove(pat);
        }
        let commit = Rect::new(extract.right() + 8.0, y, bw, 32.0);
        let grooved = self.project.patterns[pat].groove.is_some();
        if self.lab_button(c, input, wid("groove-commit", 0, 0), commit, Some(Icon::Check), tr("Commit"), grooved as u8, tr("Moves the notes where the groove plays them")) {
            self.commit_pattern_groove(pat);
        }
        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }

    /// Ghost of a grooved note in the piano roll: its outline where the
    /// groove plays it, tied to the note by a thin line.
    pub(super) fn groove_ghost(&mut self, c: &mut Canvas, note: Rect, ghost: Rect) {
        let t = self.theme;
        if (ghost.x - note.x).abs() < 0.5 && (ghost.w - note.w).abs() < 0.5 {
            return;
        }
        let col = t.accent_hot;
        c.fill_rrect(ghost, 4.0, col.alpha(0.12));
        c.stroke_rrect(ghost, 4.0, 1.0, col.alpha(0.75));
        let cy = note.y + note.h * 0.5;
        let (a, b) = (note.x.min(ghost.x), note.x.max(ghost.x));
        if b - a >= 1.0 {
            c.fill_rect(Rect::new(a, cy - 0.5, b - a, 1.0), col.alpha(0.8));
        }
        c.fill_rect(Rect::new(ghost.x.round(), ghost.y - 1.0, 2.0, ghost.h + 2.0), col.alpha(0.9));
    }

    /// A small groove mark (rack rows): drawn when channel `ch` plays its own
    /// groove in the current pattern (right-click removes it).
    pub(super) fn channel_groove_mark(&mut self, c: &mut Canvas, input: &Input, cx: f32, cy: f32, ch: usize) {
        let t = self.theme;
        let pat = self.project.current_pattern;
        let own = self.project.patterns[pat].groove.as_ref().and_then(|g| g.channels.iter().find(|(c, _)| *c == ch).map(|&(_, id)| id));
        let Some(name) = own.and_then(|id| self.project.groove(id)).map(|g| g.name.clone()) else { return };
        draw_icon(c, Icon::Groove, cx, cy, 12.0, t.accent_hot);
        let r = Rect::new(cx - 8.0, cy - 8.0, 16.0, 16.0);
        if input.inside && r.contains(input.x, input.y) {
            self.set_tip(&tr_fmt!("Groove: {}\nThis channel's own groove in this pattern · right-click: remove", name), "");
            if input.right_pressed {
                self.project.set_channel_groove(pat, ch, None);
                self.commit(tr("Groove"));
            }
        }
    }

    /// Screenshot setup (`PONZIK_GROOVES=pool|edit|popup|drag`): grooves in
    /// the pool, some patterns using them, the Grooves tab open.
    pub fn screenshot_grooves(&mut self, spec: &str) {
        let lib = factory::library();
        let mut ids = Vec::new();
        for name_part in ["Dilla", "garage", "Trap"] {
            if let Some(g) = lib.iter().find(|g| g.name.contains(name_part)) {
                ids.push(self.project.add_groove(g.clone()));
            }
        }
        ids.push(self.project.add_groove(Groove::swing(62.0)));
        let n = self.project.patterns.len();
        for p in 0..n {
            self.project.set_pattern_groove(p, Some(ids[p % ids.len()]));
        }
        if let Some(g) = self.project.groove_mut(ids[0]) {
            g.random = 0.25;
            g.quantize = 0.4;
        }
        self.sidebar = true;
        self.sidebar_tab = GROOVE_TAB;
        self.groove_ui.sel = Some(ids[0]);
        for part in spec.split(',').map(str::trim) {
            match part {
                "popup" => self.open_popup(Popup::Groove, Rect::new(700.0, 90.0, 36.0, 36.0)),
                "channel" => self.groove_ui.channel_scope = true,
                "closed" => self.groove_ui.sel = None,
                _ => {}
            }
        }
        self.commit(tr("Groove"));
    }
}
