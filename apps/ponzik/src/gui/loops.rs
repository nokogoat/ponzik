//! Loops that last: a looped clip's right edge repeats it, one-gesture
//! fills (to the end of the song, over the time selection, ×2 ×4 ×8) and the
//! paint brush that lays copies of a clip along a lane, as in FL Studio.

use ponzik_engine::TICKS_PER_BAR;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::ClipDef;

use super::app::{Action, App, MenuItem, View};
use super::clip::ClipSel;
use super::hints::Hint;
use super::widgets::Icon;

/// What a loop command does to a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopOp {
    /// Repeats up to the end of the rest of the song.
    SongEnd,
    /// Fills the playlist's time selection.
    Selection,
    /// Plays its content this many times.
    Repeat(u32),
    /// A copy right after it.
    Duplicate,
}

/// Most copies one paint gesture lays.
const PAINT_MAX: usize = 256;

/// Bars, for menus and toasts: « 4 », « 2.5 ».
fn bars(ticks: u32) -> String {
    let b = ticks as f64 / TICKS_PER_BAR as f64;
    ponzik_i18n::num(b, if (b - b.round()).abs() < 1e-6 { 0 } else { 1 })
}

impl App {
    /// Start and length of a clip.
    fn clip_span(&self, sel: ClipSel) -> Option<(u32, u32)> {
        match sel {
            ClipSel::Audio(i) => self.project.audio_clips.get(i).map(|c| (c.start, c.len)),
            ClipSel::Pattern(i) => self.project.clips.get(i).map(|c| (c.start, c.len)),
        }
    }

    /// Length of one repetition of a clip: its whole file, or its pattern.
    pub fn clip_period(&self, sel: ClipSel) -> Option<u32> {
        match sel {
            ClipSel::Audio(i) => {
                let c = self.project.audio_clips.get(i)?;
                let duration = self.samples.get(&c.path).map(|s| s.duration() as f64).filter(|&d| d > 0.0);
                Some(duration.map_or(c.len, |d| c.loop_period(d, &self.project.tempo_map())))
            }
            ClipSel::Pattern(i) => {
                let c = self.project.clips.get(i)?;
                Some(self.project.patterns.get(c.pattern)?.len.max(1))
            }
        }
    }

    /// End of the song without this clip (every other clip and take), on a bar.
    pub fn song_end_without(&self, sel: ClipSel) -> u32 {
        let p = &self.project;
        let patterns = p.clips.iter().enumerate().filter(|&(i, _)| sel != ClipSel::Pattern(i)).map(|(_, c)| c.start + c.len);
        let audio = p.audio_clips.iter().enumerate().filter(|&(i, _)| sel != ClipSel::Audio(i)).map(|(_, c)| c.start + c.len);
        let takes = p.take_groups.iter().map(|g| g.span().1);
        let end = patterns.chain(audio).chain(takes).max().unwrap_or(0);
        p.tempo_map().ceil_bar(end as f64)
    }

    /// New place and length of a clip; an audio clip loops from now on.
    fn set_clip_span(&mut self, sel: ClipSel, start: Option<u32>, len: u32, label: &str) {
        match sel {
            ClipSel::Audio(i) => {
                let Some(c) = self.project.audio_clips.get_mut(i) else { return };
                c.looped = true;
                c.len = len.max(1);
                if let Some(s) = start {
                    c.start = s;
                }
            }
            ClipSel::Pattern(i) => {
                let Some(c) = self.project.clips.get_mut(i) else { return };
                c.len = len.max(1);
                if let Some(s) = start {
                    c.start = s;
                }
            }
        }
        self.hint_seen(Hint::DragToRepeat);
        self.commit(label);
    }

    pub fn loop_clip(&mut self, sel: ClipSel, op: LoopOp) {
        let (Some((start, _)), Some(period)) = (self.clip_span(sel), self.clip_period(sel)) else { return };
        match op {
            LoopOp::SongEnd => {
                let end = self.song_end_without(sel);
                if end <= start {
                    self.toast(tr("Nothing plays after this clip yet: try Repeat ×4"));
                    return;
                }
                self.set_clip_span(sel, None, end - start, tr("Loop to end of song"));
                let bar = self.project.tempo_map().bar_beat(end as f64).bar;
                self.toast(&tr_fmt!("Looped to the end of the song · bar {}", bar + 1));
            }
            LoopOp::Selection => match self.time_selection {
                Some((View::Playlist, a, b)) if b > a => {
                    self.set_clip_span(sel, Some(a), b - a, tr("Loop to time selection"));
                    self.toast(&tr_fmt!("Looped over the time selection · {} bars", bars(b - a)));
                }
                _ => self.toast(tr("Drag a time selection in the ruler first")),
            },
            LoopOp::Repeat(n) => {
                let len = period.saturating_mul(n.max(1));
                self.set_clip_span(sel, None, len, &tr_fmt!("Repeat ×{}", n));
                self.toast(&tr_fmt!("Repeated ×{} · {} bars", n, bars(len)));
            }
            LoopOp::Duplicate => self.duplicate_clip(sel),
        }
    }

    /// A copy of the clip right after it (Ctrl+B), selected.
    pub fn duplicate_clip(&mut self, sel: ClipSel) {
        match sel {
            ClipSel::Pattern(i) => {
                let Some(&clip) = self.project.clips.get(i) else { return };
                self.project.clips.push(ClipDef { start: clip.start + clip.len, ..clip });
                let j = self.project.clips.len() - 1;
                self.playlist.selected = Some(j);
                self.select_clip(ClipSel::Pattern(j));
            }
            ClipSel::Audio(i) => {
                let Some(clip) = self.project.audio_clips.get(i).cloned() else { return };
                let start = clip.start + clip.len;
                self.project.audio_clips.push(ponzik_project::AudioClipDef { start, ..clip });
                self.select_clip(ClipSel::Audio(self.project.audio_clips.len() - 1));
            }
        }
        self.commit(tr("Duplicate clip"));
    }

    /// Loop commands of a clip's menu.
    pub(super) fn loop_menu_items(&self, sel: ClipSel) -> Vec<MenuItem> {
        let Some((start, _)) = self.clip_span(sel) else { return Vec::new() };
        let period = self.clip_period(sel).unwrap_or(TICKS_PER_BAR);
        let end = self.song_end_without(sel);
        let selection = matches!(self.time_selection, Some((View::Playlist, a, b)) if b > a);
        let bar = self.project.tempo_map().bar_beat(end as f64).bar + 1;
        let to_end = MenuItem::new(Icon::Loop, tr("Loop to end of song"), Action::Loop(sel, LoopOp::SongEnd))
            .tip(tr("Repeats the clip up to the end of the other clips"));
        let mut v = vec![
            if end > start { to_end.detail(tr_fmt!("bar {}", bar)) } else { to_end.dim(true) },
            MenuItem::new(Icon::Loop, tr("Loop to time selection"), Action::Loop(sel, LoopOp::Selection))
                .tip(tr("Fills the time selection dragged in the ruler"))
                .dim(!selection),
        ];
        for n in [2, 4, 8] {
            let it = MenuItem::new(Icon::Repeat, &tr_fmt!("Repeat ×{}", n), Action::Loop(sel, LoopOp::Repeat(n)))
                .detail(tr_fmt!("{} bars", bars(period * n)))
                .tip(tr("The clip plays its loop this many times"));
            v.push(it);
        }
        v.push(MenuItem::new(Icon::Duplicate, tr("Duplicate"), Action::Loop(sel, LoopOp::Duplicate)).detail("Ctrl B".into()).tip(tr("A copy right after it")));
        if let Some(first) = v.first_mut() {
            first.sep = true;
        }
        v
    }

    /// Paint brush: what a press on an empty lane lays — the audio clip shown
    /// in the clip panel, otherwise the pattern of the palette.
    pub(super) fn paint_begin(&mut self, lane: usize, start: u32) -> Option<super::playlist::ClipDrag> {
        let audio = match self.clip_panel_shown() {
            Some(ClipSel::Audio(i)) => self.project.audio_clips.get(i).cloned(),
            _ => None,
        };
        let sel = match audio {
            Some(src) => {
                let period = src.len.max(1);
                self.project.audio_clips.push(ponzik_project::AudioClipDef { lane, start, take: 0, ..src });
                (ClipSel::Audio(self.project.audio_clips.len() - 1), period)
            }
            None => {
                let p = self.playlist.brush.min(self.project.patterns.len().saturating_sub(1));
                let period = self.project.patterns.get(p)?.len.max(96);
                self.project.clips.push(ClipDef { pattern: p, lane, start, len: period, offset: 0, muted: false });
                let i = self.project.clips.len() - 1;
                self.playlist.selected = Some(i);
                (ClipSel::Pattern(i), period)
            }
        };
        self.commit(tr("Paint clips"));
        Some(super::playlist::ClipDrag::Paint { src: sel.0, start, period: sel.1, count: 1 })
    }

    /// Paint brush dragged to `tick`: as many copies as fit up to there.
    /// Returns the new count.
    pub(super) fn paint_to(&mut self, src: ClipSel, start: u32, period: u32, count: usize, tick: f32) -> usize {
        let want = (((tick - start as f32) / period as f32).floor() as i64 + 1).clamp(1, PAINT_MAX as i64) as usize;
        if want == count {
            return count;
        }
        match src {
            ClipSel::Pattern(i) => {
                let Some(&first) = self.project.clips.get(i) else { return count };
                if want > count {
                    for k in count..want {
                        self.project.clips.push(ClipDef { start: start + k as u32 * period, ..first });
                    }
                } else {
                    let n = self.project.clips.len();
                    self.project.clips.truncate(n - (count - want));
                }
            }
            ClipSel::Audio(i) => {
                let Some(first) = self.project.audio_clips.get(i).cloned() else { return count };
                if want > count {
                    for k in count..want {
                        self.project.audio_clips.push(ponzik_project::AudioClipDef { start: start + k as u32 * period, ..first.clone() });
                    }
                } else {
                    let n = self.project.audio_clips.len();
                    self.project.audio_clips.truncate(n - (count - want));
                }
            }
        }
        self.commit(tr("Paint clips"));
        want
    }

    /// Paint mode on or off.
    pub fn toggle_paint(&mut self) {
        self.playlist.paint = !self.playlist.paint;
        self.toast(if self.playlist.paint {
            tr("Paint mode: drag along a lane to lay copies · B to stop")
        } else {
            tr("Paint mode off")
        });
    }
}

impl App {
    /// Screenshot setup (`PONZIK_LOOPS=place:<file>,extend:8,pattern:4,menu,paint,seen`):
    /// a loop placed in the playlist, stretched, its menu, the brush.
    pub fn screenshot_loops(&mut self, spec: &str) {
        for part in spec.split(',').map(str::trim) {
            let (key, arg) = part.split_once(':').unwrap_or((part, ""));
            let last_audio = self.project.audio_clips.len().checked_sub(1);
            let free_lane = |app: &App| app.project.clips.iter().map(|c| c.lane + 1).chain(app.project.audio_clips.iter().map(|c| c.lane + 1)).max().unwrap_or(0);
            match key {
                "place" => {
                    let lane = free_lane(self);
                    self.add_audio_clip(arg, Some((lane, 0)));
                }
                "extend" => {
                    if let (Some(i), Ok(n)) = (last_audio, arg.parse::<u32>()) {
                        self.project.audio_clips[i].looped = true;
                        self.project.audio_clips[i].len = n * TICKS_PER_BAR;
                        self.commit(tr("Audio clip length"));
                    }
                }
                "pattern" => {
                    if let (Some(c), Ok(n)) = (self.project.clips.first_mut(), arg.parse::<u32>()) {
                        c.len = n * TICKS_PER_BAR;
                        self.commit(tr("Clip length"));
                    }
                }
                "menu" => {
                    if let Some(i) = last_audio {
                        let y = 150.0 + self.project.audio_clips[i].lane as f32 * super::playlist::LANE_H;
                        self.select_clip(ClipSel::Audio(i));
                        self.open_popup(super::app::Popup::AudioClipMenu(i), ponzik_ui::Rect::new(520.0, y, 0.0, 19.0));
                    }
                }
                "paint" => {
                    self.playlist.paint = true;
                    let lane = free_lane(self);
                    if let Some(super::playlist::ClipDrag::Paint { src, start, period, count }) = self.paint_begin(lane, 0) {
                        self.paint_to(src, start, period, count, (start + period * 5 + 10) as f32);
                    }
                }
                "seen" => self.hint_seen(Hint::DragToRepeat),
                _ => {}
            }
        }
    }
}
