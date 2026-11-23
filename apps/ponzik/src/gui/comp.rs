//! Take comping in the playlist (Reaper 7, Logic, Cubase). A take group shows
//! its comp on its lane, each segment in the color of its take; unfolded, one
//! lane per take hangs under it, the newest on top. Swiping a take lane picks
//! that region for the comp; the comp's edges drag; up/down switch the take
//! under the playhead, Q toggles quick swipe, S splits at the playhead.

use std::sync::Arc;

use ponzik_engine::Sample;
use ponzik_i18n::{num, tr, tr_fmt, tr_fmt_n, tr_noop};
use ponzik_project::comp::{CompSegment, Edge, MAX_CROSSFADE};
use ponzik_project::{AudioClipDef, BitDepth, TakeGroup};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::{Action, App, MenuItem, Popup, Rename};
use super::playlist::{LANE_H, PATTERN_COLORS};
use super::widgets::{Icon, draw_icon, wid};

/// Height of a take lane.
pub const TAKE_H: f32 = 34.0;
/// Header of the comp lane (fold chevron, name, crossfade).
const HEADER: f32 = 18.0;
/// Distance at which a segment edge can be grabbed, in pixels.
const EDGE_GRAB: f32 = 5.0;
/// Pointer travel that makes a press on a take lane a swipe, in pixels.
const SWIPE_MIN: f32 = 3.0;

const COMP_HELP: &str = tr_noop(
    "Comp\nSwipe a take lane: that take plays there · click: the region between edges\nDrag an edge: move it · Alt+click: next take here\n↑ ↓: take under the playhead · S: split · Q: quick swipe\nRight-click: flatten, crossfade, unused takes",
);

/// Crossfades offered in the comp menu, in milliseconds.
const CROSSFADES: [u32; 6] = [0, 5, 10, 20, 50, 100];

/// What a comp menu entry does to a take group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CompOp {
    Flatten,
    Split,
    QuickSwipe,
    Fold,
    DeleteUnused,
    Mute,
    Delete,
    Crossfade(f32),
    /// Audition this take alone (again: back to the comp).
    Solo(usize),
    /// The whole take plays.
    Whole(usize),
    DeleteTake(usize),
    TakeColor(usize, u32),
}

#[derive(Clone, Debug)]
enum CompDrag {
    /// Swipe on a take lane, from `anchor`; the comp as it was before.
    Swipe { group: usize, take: usize, anchor: u32, x0: f32, before: Vec<CompSegment> },
    Edge { group: usize, seg: usize, edge: Edge },
    Move { group: usize, start: u32, lane: usize, tick0: f32, lane0: f32 },
    Fade { group: usize, fade0: f32, x0: f32 },
}

/// Where the pointer is on a take group.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Fold,
    Fade,
    Edge(usize, Edge),
    Body,
    Solo(usize),
    Take(usize),
}

#[derive(Debug)]
pub struct CompUi {
    /// Take group the keys act on.
    pub selected: Option<usize>,
    /// Quick swipe: dragging on a take lane comps; off, a click picks the whole take.
    pub quick_swipe: bool,
    drag: Option<CompDrag>,
}

impl Default for CompUi {
    fn default() -> Self {
        Self { selected: None, quick_swipe: true, drag: None }
    }
}

/// Vertical layout of the playlist lanes: unfolded take groups add their
/// take lanes under their lane.
pub struct Lanes {
    /// Top of each lane, and the bottom of the last one.
    top: Vec<f32>,
}

impl Lanes {
    pub fn y(&self, lane: usize) -> f32 {
        let last = self.top.len() - 1;
        self.top[lane.min(last)] + lane.saturating_sub(last) as f32 * LANE_H
    }

    pub fn bottom(&self) -> f32 {
        self.top[self.top.len() - 1]
    }

    /// Lane under `y` (take lanes count as their group's lane).
    pub fn lane_at(&self, y: f32) -> f32 {
        if y < self.top[0] {
            return 0.0;
        }
        match self.top.windows(2).position(|w| y < w[1]) {
            Some(l) => l as f32,
            None => (self.top.len() - 1) as f32 + ((y - self.bottom()) / LANE_H).floor(),
        }
    }

    /// True on the take lanes under a lane.
    pub fn in_takes(&self, y: f32) -> bool {
        self.top.windows(2).any(|w| y >= w[0] + LANE_H && y < w[1])
    }
}

/// Color of a take: chosen, or the automatic one of its place.
pub fn take_color(t: &super::theme::Theme, g: &TakeGroup, take: usize) -> Color {
    match g.takes.get(take).map_or(0, |c| c.clip.color) {
        0 => t.takes[take % 8],
        hex => Color::hex(hex),
    }
}

impl App {
    /// Lane tops for `lanes` lanes from `y0`.
    pub(super) fn lane_layout(&self, y0: f32, lanes: usize) -> Lanes {
        let mut top = Vec::with_capacity(lanes + 1);
        let mut y = y0;
        for l in 0..lanes {
            top.push(y);
            let rows = self.project.take_groups.iter().filter(|g| g.lane == l && !g.folded).map(|g| g.takes.len()).max().unwrap_or(0);
            y += LANE_H + rows as f32 * TAKE_H;
        }
        top.push(y);
        Lanes { top }
    }

    /// Comp lane of a group (its whole take span).
    fn comp_rect(&self, g: &TakeGroup, lay: &Lanes, x_of: &dyn Fn(f32) -> f32) -> Rect {
        let (a, b) = g.span();
        let x = x_of(a as f32);
        Rect::new(x + 1.0, lay.y(g.lane) + 3.0, (x_of(b as f32) - x - 2.0).max(4.0), LANE_H - 6.0)
    }

    /// Lane of take `k` (the newest on top).
    fn take_rect(&self, g: &TakeGroup, k: usize, lay: &Lanes, x_of: &dyn Fn(f32) -> f32) -> Rect {
        let (a, b) = g.take_span(k).unwrap_or((0, 0));
        let row = g.takes.len() - 1 - k;
        let x = x_of(a as f32);
        Rect::new(x + 1.0, lay.y(g.lane) + LANE_H + row as f32 * TAKE_H + 1.0, (x_of(b as f32) - x - 2.0).max(4.0), TAKE_H - 3.0)
    }

    fn comp_hit(&self, x: f32, y: f32, lay: &Lanes, area: Rect, x_of: &dyn Fn(f32) -> f32) -> Option<(usize, Hit)> {
        for (gi, g) in self.project.take_groups.iter().enumerate().rev() {
            let r = self.comp_rect(g, lay, x_of);
            if r.contains(x, y) {
                let left = r.x.max(area.x);
                if x < left + 22.0 && y < r.y + HEADER {
                    return Some((gi, Hit::Fold));
                }
                if r.w > 150.0 && x > r.right().min(area.right()) - 66.0 && y < r.y + HEADER {
                    return Some((gi, Hit::Fade));
                }
                if y >= r.y + HEADER - 2.0 {
                    let near = |tick: u32| (x_of(tick as f32) - x).abs() <= EDGE_GRAB;
                    for (si, s) in g.comp.iter().enumerate() {
                        if near(s.end) {
                            return Some((gi, Hit::Edge(si, Edge::End)));
                        }
                        let touching = si > 0 && g.comp[si - 1].end == s.start;
                        if near(s.start) && !touching {
                            return Some((gi, Hit::Edge(si, Edge::Start)));
                        }
                    }
                }
                return Some((gi, Hit::Body));
            }
            if g.folded {
                continue;
            }
            for k in 0..g.takes.len() {
                let r = self.take_rect(g, k, lay, x_of);
                if r.contains(x, y) {
                    let left = r.x.max(area.x);
                    let solo = Rect::new(left + 4.0, r.y + 3.0, 20.0, 18.0);
                    return Some((gi, if solo.contains(x, y) { Hit::Solo(k) } else { Hit::Take(k) }));
                }
            }
        }
        None
    }

    /// Input on the take groups. Returns true while the pointer (or a drag)
    /// belongs to them: the clips under them don't get it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn comp_input(
        &mut self,
        input: &Input,
        lay: &Lanes,
        area: Rect,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
        snap: &dyn Fn(f32) -> f32,
    ) -> bool {
        let id = wid("pl-comp", 0, 0);
        if self.is_dragging(id) {
            self.comp_drag(input, lay, tick_at, snap);
            if input.released {
                self.comp_release(input, tick_at);
            }
            return true;
        }
        if !input.inside || !area.contains(input.x, input.y) || self.drag.is_some() {
            return false;
        }
        let Some((gi, hit)) = self.comp_hit(input.x, input.y, lay, area, x_of) else {
            // Empty take lanes under a group: nothing to place there.
            return lay.in_takes(input.y);
        };
        let tick = tick_at(input.x).max(0.0).round() as u32;
        let quick = self.comp.quick_swipe;
        match hit {
            Hit::Fold => {
                self.set_cursor(Cursor::Hand);
                let g = &self.project.take_groups[gi];
                self.set_tip(if g.folded { tr("Show the take lanes") } else { tr("Hide the take lanes") }, "");
            }
            Hit::Fade => {
                self.set_cursor(Cursor::ResizeH);
                self.set_tip(tr("Crossfade between comp segments\nDrag or scroll · equal power"), "");
                if input.wheel != 0.0 {
                    let g = &mut self.project.take_groups[gi];
                    g.crossfade = (g.crossfade + input.wheel.signum() * 0.001).clamp(0.0, MAX_CROSSFADE);
                    g.crossfade = (g.crossfade * 1000.0).round() / 1000.0;
                    self.commit(tr("Comp crossfade"));
                }
            }
            Hit::Edge(..) => {
                self.set_cursor(Cursor::ResizeH);
                self.set_tip(tr("Drag: move this comp edge"), "");
            }
            Hit::Body => {
                self.set_cursor(Cursor::Move);
                self.set_tip(tr(COMP_HELP), "");
            }
            Hit::Solo(k) => {
                self.set_cursor(Cursor::Hand);
                let on = self.project.take_groups[gi].solo == Some(k);
                self.set_tip(if on { tr("Back to the comp") } else { tr("Audition this take alone") }, "");
            }
            Hit::Take(_) => {
                self.set_cursor(if quick { Cursor::Crosshair } else { Cursor::Hand });
                self.set_tip(
                    if quick {
                        tr("Swipe: this take plays there · click: between the comp edges\nRight-click: take menu · Q: quick swipe off")
                    } else {
                        tr("Click: this whole take plays · Q: quick swipe on")
                    },
                    "Q",
                );
            }
        }
        if input.pressed {
            self.comp.selected = Some(gi);
            self.playlist.selected = None;
            self.clip_panel = None;
            match hit {
                Hit::Fold => {
                    let g = &mut self.project.take_groups[gi];
                    g.folded = !g.folded;
                    self.commit(tr("Fold takes"));
                }
                Hit::Solo(k) => self.comp_op(gi, CompOp::Solo(k)),
                Hit::Fade => {
                    self.begin_drag(id, input, 0.0);
                    let fade0 = self.project.take_groups[gi].crossfade;
                    self.comp.drag = Some(CompDrag::Fade { group: gi, fade0, x0: input.x });
                }
                Hit::Edge(seg, edge) => {
                    self.begin_drag(id, input, 0.0);
                    self.comp.drag = Some(CompDrag::Edge { group: gi, seg, edge });
                }
                Hit::Body if input.mods.alt => {
                    if self.project.take_groups[gi].switch_take_at(tick, 1).is_some() {
                        self.commit(tr("Switch take"));
                    }
                }
                Hit::Body if input.double => {
                    let g = &mut self.project.take_groups[gi];
                    g.folded = !g.folded;
                    self.commit(tr("Fold takes"));
                }
                Hit::Body => {
                    self.begin_drag(id, input, 0.0);
                    let g = &self.project.take_groups[gi];
                    let lane0 = lay.lane_at(input.y);
                    self.comp.drag = Some(CompDrag::Move { group: gi, start: g.span().0, lane: g.lane, tick0: tick_at(input.x), lane0 });
                }
                Hit::Take(k) if !quick => {
                    if self.project.take_groups[gi].choose_whole(k) {
                        self.commit(tr("Choose take"));
                    }
                }
                Hit::Take(k) => {
                    self.begin_drag(id, input, 0.0);
                    let before = self.project.take_groups[gi].comp.clone();
                    self.comp.drag = Some(CompDrag::Swipe { group: gi, take: k, anchor: tick, x0: input.x, before });
                }
            }
        }
        if input.right_pressed {
            self.comp.selected = Some(gi);
            let at = Rect::new(input.x, input.y, 0.0, 0.0);
            match hit {
                Hit::Take(k) | Hit::Solo(k) => self.open_popup(Popup::TakeMenu(gi, k), at),
                _ => self.open_popup(Popup::CompMenu(gi), at),
            }
        }
        true
    }

    fn comp_drag(&mut self, input: &Input, lay: &Lanes, tick_at: &dyn Fn(f32) -> f32, snap: &dyn Fn(f32) -> f32) {
        let tick = tick_at(input.x).max(0.0).round() as u32;
        match self.comp.drag.clone() {
            Some(CompDrag::Swipe { group, take, anchor, x0, before }) => {
                self.set_cursor(Cursor::Crosshair);
                let Some(g) = self.project.take_groups.get_mut(group) else { return };
                if (input.x - x0).abs() < SWIPE_MIN {
                    return;
                }
                g.comp = before;
                g.swipe(take, anchor, tick);
                self.commit(tr("Swipe comp"));
            }
            Some(CompDrag::Edge { group, seg, edge }) => {
                self.set_cursor(Cursor::ResizeH);
                if let Some(g) = self.project.take_groups.get_mut(group) {
                    g.move_edge(seg, edge, tick);
                    self.commit(tr("Move comp edge"));
                }
            }
            Some(CompDrag::Move { group, start, lane, tick0, lane0 }) => {
                self.set_cursor(Cursor::Grab);
                let new_start = snap(start as f32 + tick_at(input.x) - tick0).max(0.0) as u32;
                let new_lane = (lane as f32 + lay.lane_at(input.y) - lane0).clamp(0.0, (ponzik_project::PLAYLIST_LANES - 1) as f32) as usize;
                if let Some(g) = self.project.take_groups.get_mut(group) {
                    let delta = new_start as i64 - g.span().0 as i64;
                    if delta != 0 || g.lane != new_lane {
                        g.shift(delta);
                        g.lane = new_lane;
                        self.commit(tr("Move take group"));
                    }
                }
            }
            Some(CompDrag::Fade { group, fade0, x0 }) => {
                self.set_cursor(Cursor::ResizeH);
                // Half a millisecond per pixel, whole milliseconds.
                let ms = ((fade0 * 1000.0 + (input.x - x0) * 0.5).round() / 1000.0).clamp(0.0, MAX_CROSSFADE);
                if let Some(g) = self.project.take_groups.get_mut(group)
                    && g.crossfade != ms
                {
                    g.crossfade = ms;
                    self.commit(tr("Comp crossfade"));
                }
            }
            None => {}
        }
    }

    /// End of a comp gesture: a click on a take lane (no swipe) gives it the
    /// region between the comp edges around it.
    fn comp_release(&mut self, input: &Input, tick_at: &dyn Fn(f32) -> f32) {
        if let Some(CompDrag::Swipe { group, take, anchor, x0, .. }) = self.comp.drag.clone()
            && (input.x - x0).abs() < SWIPE_MIN
            && let Some(g) = self.project.take_groups.get_mut(group)
        {
            let _ = tick_at;
            let (lo, hi) = match g.segment_at(anchor) {
                Some(i) => (g.comp[i].start, g.comp[i].end),
                None => {
                    let lo = g.comp.iter().filter(|s| s.end <= anchor).map(|s| s.end).max().unwrap_or(0);
                    let hi = g.comp.iter().filter(|s| s.start > anchor).map(|s| s.start).min().unwrap_or(u32::MAX);
                    (lo, hi)
                }
            };
            if g.swipe(take, lo, hi) {
                self.commit(tr("Swipe comp"));
            }
        }
        self.comp.drag = None;
    }

    /// Keys of the selected take group (playlist view).
    pub(super) fn comp_keys(&mut self, input: &Input) {
        let Some(gi) = self.comp.selected.filter(|&g| g < self.project.take_groups.len()) else {
            self.comp.selected = None;
            return;
        };
        let tick = self.playhead().max(0.0) as u32;
        for &(key, mods) in &input.keys {
            let plain = !mods.ctrl && !mods.alt && !mods.shift;
            match key {
                Key::Up | Key::Down if plain => {
                    let step = if key == Key::Up { 1 } else { -1 };
                    match self.project.take_groups[gi].switch_take_at(tick, step) {
                        Some(t) => {
                            let name = self.project.take_groups[gi].takes[t].name.clone();
                            self.commit(tr("Switch take"));
                            self.toast(&tr_fmt!("Comp under the playhead: {}", name));
                        }
                        None => self.toast(tr("No comp segment under the playhead")),
                    }
                }
                Key::Char('q') if plain && !self.typing_keyboard => self.comp_op(gi, CompOp::QuickSwipe),
                Key::Char('s') if plain && !self.typing_keyboard => self.comp_op(gi, CompOp::Split),
                Key::Delete | Key::Backspace => self.comp_op(gi, CompOp::Delete),
                _ => {}
            }
        }
    }

    pub fn comp_op(&mut self, gi: usize, op: CompOp) {
        let Some(g) = self.project.take_groups.get_mut(gi) else { return };
        match op {
            CompOp::Flatten => self.flatten_comp(gi),
            CompOp::Split => {
                let tick = self.playhead().max(0.0) as u32;
                if self.project.take_groups[gi].split_at(tick) {
                    self.commit(tr("Split comp"));
                } else {
                    self.toast(tr("Put the playhead inside a comp segment to split it"));
                }
            }
            CompOp::QuickSwipe => {
                self.comp.quick_swipe = !self.comp.quick_swipe;
                self.toast(if self.comp.quick_swipe {
                    tr("Quick swipe on: drag across a take lane to comp")
                } else {
                    tr("Quick swipe off: click a take lane to play the whole take")
                });
            }
            CompOp::Fold => {
                g.folded = !g.folded;
                self.commit(tr("Fold takes"));
            }
            CompOp::DeleteUnused => {
                let n = g.delete_unused_takes();
                if n == 0 {
                    self.toast(tr("Every take plays a part of the comp"));
                } else {
                    self.commit(tr("Delete unused takes"));
                    self.toast(&tr_fmt_n!("{} unused take deleted", "{} unused takes deleted", n, n));
                }
            }
            CompOp::Mute => {
                g.muted = !g.muted;
                self.commit(tr("Mute take group"));
            }
            CompOp::Delete => {
                self.project.take_groups.remove(gi);
                self.comp.selected = None;
                self.commit(tr("Delete take group"));
            }
            CompOp::Crossfade(secs) => {
                g.crossfade = secs.clamp(0.0, MAX_CROSSFADE);
                self.commit(tr("Comp crossfade"));
            }
            CompOp::Solo(k) => {
                // Audition: heard at once, no undo step.
                g.solo = if g.solo == Some(k) { None } else { Some(k) };
                let (on, name) = (g.solo.is_some(), g.takes.get(k).map_or(String::new(), |t| t.name.clone()));
                self.sync_engine();
                if on {
                    self.toast(&tr_fmt!("Auditioning {} alone · click again for the comp", name));
                }
            }
            CompOp::Whole(k) => {
                if g.choose_whole(k) {
                    self.commit(tr("Choose take"));
                }
            }
            CompOp::DeleteTake(k) => {
                if g.takes.len() <= 1 {
                    self.project.take_groups.remove(gi);
                    self.comp.selected = None;
                    self.commit(tr("Delete take group"));
                } else {
                    g.delete_take(k);
                    self.commit(tr("Delete take"));
                }
            }
            CompOp::TakeColor(k, hex) => {
                if let Some(t) = g.takes.get_mut(k) {
                    t.clip.color = hex;
                    self.commit(tr("Take color"));
                }
            }
        }
    }

    /// Renders the comp into a new audio clip on the group's lane; the group
    /// stays, muted and folded, on a free lane until its takes are deleted.
    pub fn flatten_comp(&mut self, gi: usize) {
        let Some(g) = self.project.take_groups.get(gi).cloned() else { return };
        let samples = &self.samples;
        let loader = |p: &str| samples.get(p).cloned();
        let sr = self.sample_rate as f32;
        let Some(flat) = g.flatten(&self.project.tempo_map(), sr, &loader) else {
            self.toast(tr("Nothing to flatten: the comp is empty or a take file is missing"));
            return;
        };
        let track_name = self.project.tracks.get(g.track).map_or(String::new(), |t| t.name.clone());
        let dir = if self.rec.demo { std::env::temp_dir().join("ponzik-takes") } else { super::compat::projects_dir().join("Recordings") };
        let _ = std::fs::create_dir_all(&dir);
        let (name, path) = (1..)
            .map(|n| {
                let name = tr_fmt!("{} comp {}", track_name, n);
                let path = dir.join(format!("{}.wav", super::record::file_safe(&name)));
                (name, path)
            })
            .find(|(_, p)| !p.exists() || self.rec.demo)
            .unwrap_or_default();
        let audio: Vec<f32> = flat.sample.left.iter().zip(&flat.sample.right).flat_map(|(&l, &r)| [l, r]).collect();
        if let Err(e) = ponzik_project::write_wav_channels(&path, &audio, 2, sr as u32, BitDepth::Float32) {
            self.toast(&format!("{}: {e}", path.display()));
            return;
        }
        let key = path.to_string_lossy().into_owned();
        self.samples.insert(key.clone(), Arc::new(Sample { name: name.clone(), ..flat.sample }));
        self.waveforms.remove(&key);
        self.project.audio_clips.push(AudioClipDef { path: key, ..flat.clip });
        let free = self.used_lanes();
        let g = &mut self.project.take_groups[gi];
        g.muted = true;
        g.folded = true;
        g.solo = None;
        g.lane = free.min(63);
        self.comp.selected = None;
        self.commit(tr("Flatten comp"));
        self.toast(&tr_fmt!("Comp flattened to {} · the takes stay, muted, on lane {}", name, free + 1));
    }

    /// Lanes in use (pattern clips, audio clips, take groups): the first free one.
    pub fn used_lanes(&self) -> usize {
        let p = &self.project;
        p.clips.iter().map(|c| c.lane + 1).chain(p.audio_clips.iter().map(|c| c.lane + 1)).chain(p.take_groups.iter().map(|g| g.lane + 1)).max().unwrap_or(0)
    }

    pub(super) fn comp_menu(&self, gi: usize) -> (String, Vec<MenuItem>) {
        let Some(g) = self.project.take_groups.get(gi) else { return (String::new(), Vec::new()) };
        let unused = g.unused_takes().len();
        let comp = |op| Action::Comp(gi, op);
        let fade_ms = (g.crossfade * 1000.0).round() as u32;
        let mut v = vec![
            MenuItem::new(Icon::Export, tr("Flatten comp"), comp(CompOp::Flatten)).tip(tr("Renders the comp to one audio clip · the takes stay, muted")),
            MenuItem::new(Icon::Slice, tr("Split at the playhead"), comp(CompOp::Split)).detail("S".into()),
            MenuItem::new(Icon::Wand, tr("Quick swipe"), comp(CompOp::QuickSwipe))
                .detail("Q".into())
                .on(self.comp.quick_swipe)
                .tip(tr("On: drag across a take lane to comp · off: a click picks the whole take")),
            MenuItem::new(Icon::ChevronDown, if g.folded { tr("Show the take lanes") } else { tr("Hide the take lanes") }, comp(CompOp::Fold))
                .detail(tr("double-click").into()),
        ];
        for (i, &ms) in CROSSFADES.iter().enumerate() {
            let label = if ms == 0 { tr("No crossfade").to_string() } else { tr_fmt!("Crossfade {} ms", ms) };
            let it = MenuItem::plain(&label, comp(CompOp::Crossfade(ms as f32 / 1000.0))).on(fade_ms == ms);
            v.push(if i == 0 { it.sep() } else { it });
        }
        let delete_unused = MenuItem::new(Icon::Trash, tr("Delete unused takes"), comp(CompOp::DeleteUnused));
        v.push(if unused > 0 { delete_unused.detail(tr_fmt_n!("{} take", "{} takes", unused, unused)) } else { delete_unused.tip(tr("Every take plays a part of the comp")) }.sep());
        v.push(MenuItem::new(Icon::Mute, if g.muted { tr("Unmute the takes") } else { tr("Mute the takes") }, comp(CompOp::Mute)));
        v.push(MenuItem::new(Icon::Trash, tr("Delete the take group"), comp(CompOp::Delete)).detail(tr("Del").into()));
        let track = self.project.tracks.get(g.track).map_or("", |t| t.name.as_str());
        (tr_fmt!("Comp · {}", track), v)
    }

    pub(super) fn take_menu(&self, gi: usize, k: usize) -> (String, Vec<MenuItem>) {
        let Some(t) = self.project.take_groups.get(gi).and_then(|g| g.takes.get(k)) else { return (String::new(), Vec::new()) };
        let g = &self.project.take_groups[gi];
        let comp = |op| Action::Comp(gi, op);
        let rename = Popup::Rename { target: Rename::Take(gi, k), text: t.name.clone() };
        let mut color = MenuItem::plain(tr("Color"), Action::Popup(Popup::TakeColor(gi, k)));
        color.color = Some(take_color(&self.theme, g, k));
        (
            t.name.clone(),
            vec![
                MenuItem::new(Icon::Solo, tr("Audition alone"), comp(CompOp::Solo(k))).on(g.solo == Some(k)).tip(tr("Plays this take instead of the comp")),
                MenuItem::new(Icon::Check, tr("Use the whole take"), comp(CompOp::Whole(k))),
                MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)),
                color,
                MenuItem::new(Icon::Trash, tr("Delete the take"), comp(CompOp::DeleteTake(k))).sep(),
            ],
        )
    }

    pub(super) fn take_color_menu(&self, gi: usize, k: usize) -> (String, Vec<MenuItem>) {
        let Some(g) = self.project.take_groups.get(gi) else { return (String::new(), Vec::new()) };
        let cur = g.takes.get(k).map_or(0, |t| t.clip.color);
        let mut auto = MenuItem::plain(tr("Automatic"), Action::Comp(gi, CompOp::TakeColor(k, 0))).on(cur == 0);
        auto.color = Some(self.theme.takes[k % 8]);
        let mut v = vec![auto];
        for (i, &(name, hex)) in PATTERN_COLORS.iter().enumerate() {
            let mut it = MenuItem::plain(tr(name), Action::Comp(gi, CompOp::TakeColor(k, hex))).on(cur == hex);
            it.color = Some(Color::hex(hex));
            v.push(if i == 0 { it.sep() } else { it });
        }
        let name = g.takes.get(k).map_or("", |t| t.name.as_str());
        (tr_fmt!("Color · {}", name), v)
    }

    /// Waveform of a take between two screen x, along its file.
    #[allow(clippy::too_many_arguments)]
    fn take_wave(&mut self, c: &mut Canvas, clip: &AudioClipDef, x0: f32, x1: f32, mid: f32, amp: f32, col: Color, tick_at: &dyn Fn(f32) -> f32) {
        let Some(peaks) = self.waveform(&clip.path) else { return };
        let (sr, duration) = self.samples.get(&clip.path).map_or((48_000.0, 0.0), |s| (s.sample_rate as f64, s.duration() as f64));
        let map = self.project.tempo_map();
        let bucket = super::app::WAVE_BUCKET as f64;
        for px in x0.floor() as i32..x1.ceil() as i32 {
            let tick_a = tick_at(px as f32) as f64;
            let tick_b = tick_at(px as f32 + 1.0) as f64;
            let (Some(a), Some(b)) = (self.clip_file_secs(clip, tick_a, &map, duration), self.clip_file_secs(clip, tick_b, &map, duration)) else {
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
                c.fill_rect(Rect::new(px as f32, mid - hi * amp, 1.0, ((hi - lo) * amp).max(1.0)), col);
            }
        }
    }

    /// Draws the take groups: comp lanes, then the take lanes of the unfolded ones.
    pub(super) fn draw_comps(&mut self, c: &mut Canvas, input: &Input, lay: &Lanes, area: Rect, x_of: &dyn Fn(f32) -> f32, tick_at: &dyn Fn(f32) -> f32) {
        let t = self.theme;
        let hover = if input.inside && self.drag.is_none() { self.comp_hit(input.x, input.y, lay, area, x_of) } else { None };
        let swiping = match &self.comp.drag {
            Some(CompDrag::Swipe { group, take, .. }) => Some((*group, *take)),
            _ => None,
        };
        let map = self.project.tempo_map();
        for gi in 0..self.project.take_groups.len() {
            let g = self.project.take_groups[gi].clone();
            let r = self.comp_rect(&g, lay, x_of);
            if r.right() < area.x || r.x > area.right() {
                continue;
            }
            let selected = self.comp.selected == Some(gi);
            let hv = self.hover_anim(wid("pl-comp-g", gi, 0), hover.is_some_and(|h| h.0 == gi));
            // Muted, or a take auditioned instead: the comp is not what plays.
            let dim = if g.muted { 0.45 } else if g.solo.is_some() { 0.6 } else { 1.0 };
            let base = take_color(&self.theme, &g, g.comp.first().map_or(0, |s| s.take));
            // Comp lane: the frame of the whole group, its segments inside.
            c.fill_rrect(r, 8.0, t.panel.mix(t.well, 0.3).alpha(0.95));
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, HEADER - 1.0), base.mix(t.panel, 0.55).alpha(0.9 * dim));
            let prev = c.push_clip(r);
            let body = Rect::new(r.x, r.y + HEADER, r.w, r.h - HEADER);
            let mid = body.y + body.h * 0.5;
            for s in &g.comp {
                let (x0, x1) = (x_of(s.start as f32), x_of(s.end as f32));
                let col = take_color(&self.theme, &g, s.take);
                c.fill_rect(Rect::new(x0, body.y, x1 - x0, body.h - 1.0), col.mix(t.panel, 0.62 - hv * 0.06).alpha(dim));
                if let Some(take) = g.takes.get(s.take) {
                    let clip = take.clip.clone();
                    self.take_wave(c, &clip, x0.max(area.x), x1.min(area.right()), mid, body.h * 0.5 - 2.0, t.text.alpha(0.7 * dim), tick_at);
                }
            }
            // Boundaries: a line, and the crossfade as an X as wide as it lasts.
            for (si, s) in g.comp.iter().enumerate() {
                let x = x_of(s.end as f32);
                let touching = g.comp.get(si + 1).is_some_and(|n| n.start == s.end);
                let grabbed = matches!(hover, Some((h, Hit::Edge(e, Edge::End))) if h == gi && e == si);
                c.fill_rect(Rect::new(x.round() - 1.0, body.y, 2.0, body.h), t.text.alpha(if grabbed { 0.9 } else { 0.45 }));
                if touching && g.crossfade > 0.0 {
                    let secs = map.secs_at(s.end as f64);
                    let half = g.crossfade as f64 * 0.5;
                    let (xa, xb) = (x_of(map.tick_at_secs(secs - half) as f32), x_of(map.tick_at_secs(secs + half) as f32));
                    let (xa, xb) = (xa.min(x - 3.0), xb.max(x + 3.0));
                    let col = t.text.alpha(0.55 * dim);
                    c.stroke_polyline(&[(xa, body.bottom() - 3.0), (xb, body.y + 3.0)], 1.2, col);
                    c.stroke_polyline(&[(xa, body.y + 3.0), (xb, body.bottom() - 3.0)], 1.2, col);
                }
            }
            if let Some(first) = g.comp.first()
                && !g.comp.iter().skip(1).any(|s| s.start == first.start)
            {
                let x = x_of(first.start as f32);
                c.fill_rect(Rect::new(x.round(), body.y, 1.0, body.h), t.text.alpha(0.3));
            }
            c.set_clip(Some(prev));
            // Header: fold chevron, name, crossfade.
            let left = r.x.max(area.x);
            let chevron = if g.folded { Icon::ChevronRight } else { Icon::ChevronDown };
            let fold_hot = matches!(hover, Some((h, Hit::Fold)) if h == gi);
            draw_icon(c, chevron, left + 11.0, r.y + 9.5, 12.0, t.text.alpha(if fold_hot { 1.0 } else { 0.75 }));
            let track = self.project.tracks.get(g.track).map_or(String::new(), |t| t.name.clone());
            let label = if g.muted {
                tr_fmt!("{} · flattened, muted", track)
            } else if let Some(k) = g.solo {
                tr_fmt!("{} · auditioning {}", track, g.takes.get(k).map_or("", |t| t.name.as_str()))
            } else {
                tr_fmt_n!("{} · comp of {} take", "{} · comp of {} takes", g.takes.len(), track, g.takes.len())
            };
            let fade_w = if r.w > 150.0 { 66.0 } else { 0.0 };
            let name_w = r.right().min(area.right()) - left - 26.0 - fade_w;
            self.text_fit(c, self.f.sans_med, 11.0, left + 22.0, r.y + 9.5, &label, name_w, t.text.alpha(dim));
            if fade_w > 0.0 {
                let fx = r.right().min(area.right()) - fade_w;
                let hot = matches!(hover, Some((h, Hit::Fade)) if h == gi) || matches!(self.comp.drag, Some(CompDrag::Fade { group, .. }) if group == gi);
                let chip = Rect::new(fx, r.y + 2.0, fade_w - 4.0, HEADER - 4.0);
                c.fill_rrect(chip, 5.0, t.bg_deep.alpha(if hot { 0.75 } else { 0.45 }));
                let col = t.text2.mix(t.accent, hot as u8 as f32);
                // A small crossfade X, then its length.
                let (xa, cy) = (chip.x + 6.0, chip.y + chip.h * 0.5);
                c.stroke_polyline(&[(xa, cy - 4.0), (xa + 10.0, cy + 4.0)], 1.2, col);
                c.stroke_polyline(&[(xa, cy + 4.0), (xa + 10.0, cy - 4.0)], 1.2, col);
                let ms = num(g.crossfade as f64 * 1000.0, 0);
                self.text_fit(c, self.f.mono, 10.0, xa + 15.0, cy, &tr_fmt!("{} ms", ms), chip.w - 22.0, col);
            }
            if selected {
                c.stroke_rrect(r, 8.0, 1.5, t.select);
            } else {
                c.stroke_rrect(r, 8.0, 1.0, base.alpha(0.5 * dim));
            }
            if g.folded {
                continue;
            }
            // Take lanes, the newest on top; the parts in the comp light up.
            for k in (0..g.takes.len()).rev() {
                let tr_ = self.take_rect(&g, k, lay, x_of);
                if tr_.y > area.bottom() {
                    continue;
                }
                let col = take_color(&self.theme, &g, k);
                let soloed = g.solo == Some(k);
                let hot = hover.is_some_and(|h| h.0 == gi && matches!(h.1, Hit::Take(j) | Hit::Solo(j) if j == k)) || swiping == Some((gi, k));
                let hv = self.hover_anim(wid("pl-take", gi, k), hot);
                c.fill_rrect(tr_, 6.0, col.mix(t.panel, 0.86 - hv * 0.06).alpha(dim));
                let prev = c.push_clip(tr_);
                for s in g.comp.iter().filter(|s| s.take == k) {
                    let (x0, x1) = (x_of(s.start as f32), x_of(s.end as f32));
                    c.fill_rect(Rect::new(x0, tr_.y, x1 - x0, tr_.h), col.mix(t.panel, 0.55).alpha(dim));
                    c.fill_rect(Rect::new(x0, tr_.bottom() - 2.0, x1 - x0, 2.0), col.alpha(dim));
                }
                let clip = g.takes[k].clip.clone();
                let mid = tr_.y + tr_.h * 0.5 + 3.0;
                self.take_wave(c, &clip, tr_.x.max(area.x), tr_.right().min(area.right()), mid, tr_.h * 0.5 - 6.0, t.text.alpha(0.45 * dim), tick_at);
                // In the comp: full-strength waveform on top.
                for s in g.comp.iter().filter(|s| s.take == k) {
                    let (x0, x1) = (x_of(s.start as f32).max(tr_.x), x_of(s.end as f32).min(tr_.right()));
                    self.take_wave(c, &clip, x0.max(area.x), x1.min(area.right()), mid, tr_.h * 0.5 - 6.0, t.text.alpha(0.8 * dim), tick_at);
                }
                c.set_clip(Some(prev));
                // Header: audition button and name.
                let left = tr_.x.max(area.x);
                let solo = Rect::new(left + 4.0, tr_.y + 3.0, 20.0, 18.0);
                let solo_hot = matches!(hover, Some((h, Hit::Solo(j))) if h == gi && j == k);
                c.fill_rrect(solo, 5.0, if soloed { t.accent.alpha(0.9) } else { t.bg_deep.alpha(if solo_hot { 0.8 } else { 0.5 }) });
                draw_icon(c, Icon::Solo, solo.x + 10.0, solo.y + 9.0, 11.0, if soloed { t.on_accent } else { t.text2 });
                let name = g.takes[k].name.clone();
                let name_w = (self.fonts.measure(self.f.sans_med, 11.0, &name) + 22.0).min(tr_.right() - left - 30.0);
                if name_w > 12.0 {
                    c.fill_rrect(Rect::new(left + 26.0, tr_.y + 3.0, name_w, 18.0), 5.0, t.bg_deep.alpha(0.6));
                }
                c.fill_circle(left + 33.0, tr_.y + 12.0, 3.5, col);
                self.text_fit(c, self.f.sans_med, 11.0, left + 41.0, tr_.y + 12.0, &name, name_w - 18.0, t.text.alpha(0.9 * dim));
                if soloed {
                    c.stroke_rrect(tr_, 6.0, 1.5, t.accent);
                }
            }
        }
    }

    /// Screenshot: a take group of three synthetic vocal passes, comped from
    /// all three. `spec`: `1` (unfolded), `folded`, `menu` (comp menu),
    /// `take` (take menu), `solo` (a take auditioned), `flat` (flattened).
    pub fn screenshot_comp(&mut self, spec: &str) {
        use ponzik_engine::{PPQ, TICKS_PER_BAR};
        self.rec.demo = true;
        let sr = self.sample_rate as f32;
        let track = 3;
        self.project.tracks[track].name = "Vocals".into();
        let bar = TICKS_PER_BAR;
        let spt = sr as f64 * 60.0 / (self.project.bpm as f64 * PPQ as f64);
        let pass = (4.0 * bar as f64 * spt) as usize;
        // Three passes of a hummed phrase: notes with breaths, each pass sung a bit differently.
        let mut left = vec![0.0f32; pass * 3];
        let mut seed = 11u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0
        };
        for p in 0..3 {
            let notes = [(0.0, 0.9, 220.0), (1.0, 0.7, 246.9), (2.0, 1.6, 261.6), (4.0, 0.8, 293.7), (5.0, 1.0, 261.6), (6.5, 1.3, 220.0)];
            for (i, &(at, len, f)) in notes.iter().enumerate() {
                let loud = 0.25 + 0.2 * (((p * 7 + i * 3) % 5) as f32 / 4.0);
                let (a, n) = ((at * sr) as usize + p * pass + p * 900, (len * sr) as usize);
                for j in 0..n.min(pass * 3 - a.min(pass * 3)) {
                    let ts = j as f32 / sr;
                    let env = (ts * 18.0).min(1.0) * ((len - ts) * 8.0).clamp(0.0, 1.0) * (1.0 + 0.25 * (ts * 5.5).sin());
                    let vib = (ts * 5.2 * std::f32::consts::TAU).sin() * 0.004 * (p as f32 + 1.0);
                    let ph = ts * f * (1.0 + vib) * std::f32::consts::TAU;
                    left[a + j] += loud * env * (ph.sin() + 0.35 * (2.0 * ph).sin() + 0.04 * noise());
                }
            }
        }
        let name = "Vocals takes (demo)".to_string();
        let key = std::env::temp_dir().join("ponzik-takes").join("Vocals takes demo.wav").to_string_lossy().into_owned();
        self.samples.insert(key.clone(), Arc::new(Sample { name, sample_rate: sr, right: left.clone(), left, markers: Vec::new() }));
        self.waveforms.remove(&key);
        let start = 4 * bar;
        let takes = (0..3)
            .map(|k| ponzik_project::Take {
                name: tr_fmt!("Take {}", k + 1),
                clip: AudioClipDef { path: key.clone(), start, len: 4 * bar, offset: (k * pass) as f64 / sr as f64, fade_in: 0.003, fade_out: 0.003, ..Default::default() },
            })
            .collect();
        let lane = self.used_lanes();
        let mut g = TakeGroup::new(lane, track, takes);
        g.choose_whole(2);
        g.swipe(0, start + bar / 2, start + bar + bar / 2);
        g.swipe(1, start + 2 * bar + PPQ, start + 3 * bar);
        g.swipe(0, start + 3 * bar + 2 * PPQ, start + 4 * bar);
        g.folded = spec == "folded";
        self.project.take_groups.push(g);
        let gi = self.project.take_groups.len() - 1;
        self.comp.selected = Some(gi);
        // Zoomed on the group: a bar before it to a bar after.
        self.playlist.zoom = 220.0;
        self.playlist.scroll_x = (start - bar / 2) as f32;
        self.anim.set(wid("pl-sx", 0, 0), self.playlist.scroll_x);
        self.start_marker = (start + 2 * bar + 2 * PPQ) as f64;
        self.position = self.start_marker;
        self.commit(tr("Record audio"));
        let anchor = Rect::new(720.0, 300.0, 0.0, 0.0);
        match spec {
            "menu" => self.open_popup(Popup::CompMenu(gi), anchor),
            "take" => self.open_popup(Popup::TakeMenu(gi, 1), anchor),
            "solo" => self.comp_op(gi, CompOp::Solo(1)),
            "flat" => self.comp_op(gi, CompOp::Flatten),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::app::View;
    use super::*;
    use ponzik_engine::{PPQ, TICKS_PER_BAR};
    use ponzik_window::Modifiers;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.set_view(View::Playlist);
        app.set_mode(ponzik_engine::PlayMode::Song);
        app
    }

    fn step(app: &mut App, c: &mut Canvas, input: &mut Input, x: f32, y: f32, down: bool) {
        input.pressed = down && !input.down;
        input.released = !down && input.down;
        (input.x, input.y, input.inside, input.down) = (x, y, true, down);
        app.frame(c, input, 1.0 / 60.0);
        input.keys.clear();
    }

    fn key(app: &mut App, c: &mut Canvas, input: &mut Input, k: Key) {
        input.keys.push((k, Modifiers::default()));
        step(app, c, input, 700.0, 800.0, false);
    }

    /// The group of the screenshot: bars 5–8 at 220 px a bar (bar 6 at
    /// x ≈ 804), comp lane at y ≈ 365, take lanes (3, 2, 1) at y ≈ 403, 437, 471.
    #[test]
    fn swipes_keys_and_menus_edit_the_comp() {
        let mut app = app();
        let mut c = Canvas::new(1600, 1000);
        let mut i = Input::default();
        app.screenshot_comp("1");
        step(&mut app, &mut c, &mut i, 700.0, 800.0, false);
        let bar = TICKS_PER_BAR;
        let start = 4 * bar;
        let before = app.project.take_groups[0].comp.clone();
        // Swipe Take 1 across bar 6.
        for (x, down) in [(840.0, true), (900.0, true), (980.0, true), (980.0, false)] {
            step(&mut app, &mut c, &mut i, x, 471.0, down);
        }
        let g = &app.project.take_groups[0];
        assert_ne!(g.comp, before);
        // Swiped from about 1.13 to 1.77 bars into the group (it merged with
        // the take's segment before it); take 3 still plays after.
        let take_of = |tick: u32| g.segment_at(tick).map(|s| g.comp[s].take);
        assert_eq!(take_of(start + bar + bar * 6 / 10), Some(0));
        assert_eq!(take_of(start + bar + bar * 9 / 10), Some(2));
        app.undo();
        assert_eq!(app.project.take_groups[0].comp, before);

        // Up: the next take plays under the playhead; S splits there.
        let at = app.playhead() as u32;
        let take_at = |app: &App| {
            let g = &app.project.take_groups[0];
            g.comp[g.segment_at(at).expect("segment")].take
        };
        assert_eq!(take_at(&app), 1);
        key(&mut app, &mut c, &mut i, Key::Up);
        assert_eq!(take_at(&app), 2);
        let n = app.project.take_groups[0].comp.len();
        key(&mut app, &mut c, &mut i, Key::Char('s'));
        assert_eq!(app.project.take_groups[0].comp.len(), n + 1);

        // Quick swipe off: a click on a take lane plays the whole take.
        key(&mut app, &mut c, &mut i, Key::Char('q'));
        assert!(!app.comp.quick_swipe);
        step(&mut app, &mut c, &mut i, 1000.0, 437.0, true);
        step(&mut app, &mut c, &mut i, 1000.0, 437.0, false);
        assert_eq!(app.project.take_groups[0].comp, [CompSegment { take: 1, start, end: start + 4 * bar }]);

        // A boundary dragged on the comp lane, then flattening.
        app.comp_op(0, CompOp::Whole(0));
        app.comp_op(0, CompOp::Split);
        let edge_x = 804.0 + (at as f32 - (start + bar) as f32) / bar as f32 * 220.0;
        for (x, down) in [(edge_x, true), (edge_x + 22.0, true), (edge_x + 22.0, false)] {
            step(&mut app, &mut c, &mut i, x, 372.0, down);
        }
        let g = &app.project.take_groups[0];
        assert_eq!(g.comp.len(), 2);
        assert!(g.comp[0].end > at + PPQ / 4, "edge moved right: {:?}", g.comp);
        let clips = app.project.audio_clips.len();
        app.comp_op(0, CompOp::Flatten);
        assert_eq!(app.project.audio_clips.len(), clips + 1);
        assert!(app.project.take_groups[0].muted);
        assert_eq!(app.project.audio_clips[clips].start, start);
    }
}
