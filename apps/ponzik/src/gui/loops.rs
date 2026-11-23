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
                "fit" => {
                    if let Some(ch) = self.selected_channel() {
                        self.fit_loop_to_pattern(ch);
                    }
                }
                _ => {}
            }
        }
    }
}

/// A sampler channel's loop: its tempo and length in beats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelLoop {
    pub bpm: f32,
    pub beats: f32,
    /// Already played once per pattern, in tempo sync.
    pub fitted: bool,
}

impl App {
    /// Loop tempo of a file (`None`: not a loop), detected once.
    fn file_loop_bpm(&mut self, path: &str) -> Option<f32> {
        if let Some(v) = self.loop_bpms.get(path) {
            return *v;
        }
        let s = self.samples.get(path)?.clone();
        let bpm = ponzik_project::warp::detect_tempo(&s).and_then(|h| ponzik_project::warp::loop_bpm(&s, h, self.project.bpm));
        self.loop_bpms.insert(path.to_string(), bpm);
        bpm
    }

    /// The loop a sampler channel plays, if its sample is one (detected the
    /// first time; `cached_only` never analyses, for menus).
    pub fn channel_loop(&mut self, ch: usize, cached_only: bool) -> Option<ChannelLoop> {
        use ponzik_engine::instruments::{SAMPLER_BEATS, SAMPLER_MODE, SAMPLER_SYNC};
        let def = &self.project.channels.get(ch)?.instrument;
        if def.kind != "sampler" {
            return None;
        }
        let path = def.sample.clone()?;
        let duration = self.samples.get(&path)?.duration();
        let bpm = if cached_only { (*self.loop_bpms.get(&path)?)? } else { self.file_loop_bpm(&path)? };
        let beats = (duration * bpm / 60.0).round().max(1.0);
        let def = &self.project.channels[ch].instrument;
        let synced = def.get(SAMPLER_SYNC).unwrap_or(0.0) > 0.5 && def.get(SAMPLER_BEATS) == Some(beats);
        let one_shot = def.get(SAMPLER_MODE).unwrap_or(1.0) < 0.5;
        let pat = &self.project.patterns[self.project.current_pattern];
        let first = pat.notes.get(ch).and_then(|n| n.first()).is_some_and(|n| n.start == 0);
        Some(ChannelLoop { bpm, beats, fitted: synced && one_shot && first })
    }

    /// « Fit loop to pattern »: the sampler plays the whole loop (one-shot)
    /// from step 1, once per loop length, stretched to the project tempo;
    /// the pattern grows to hold the loop.
    pub fn fit_loop_to_pattern(&mut self, ch: usize) {
        use ponzik_engine::instruments::{SAMPLER_BEATS, SAMPLER_MODE, SAMPLER_SYNC};
        let Some(lp) = self.channel_loop(ch, false) else {
            self.toast(tr("This channel plays no loop: drop a loop on it first"));
            return;
        };
        let def = &mut self.project.channels[ch].instrument;
        def.set(SAMPLER_MODE, 0.0);
        def.set(SAMPLER_SYNC, 1.0);
        def.set(SAMPLER_BEATS, lp.beats);
        let pat = self.project.current_pattern;
        let loop_ticks = (lp.beats as u32 * ponzik_engine::PPQ).max(ponzik_engine::TICKS_PER_STEP);
        let p = &mut self.project.patterns[pat];
        if p.len < loop_ticks {
            // The pattern grows to hold the loop; the other channels repeat
            // what they played, so the beat goes on over the new bars.
            let old = p.len.max(1);
            p.len = loop_ticks.div_ceil(TICKS_PER_BAR) * TICKS_PER_BAR;
            for notes in p.notes.iter_mut().enumerate().filter(|(c, _)| *c != ch).map(|(_, n)| n) {
                let first: Vec<_> = notes.iter().filter(|n| n.start < old).copied().collect();
                let mut k = 1;
                while k * old < p.len {
                    notes.extend(first.iter().map(|n| ponzik_engine::Note { start: n.start + k * old, ..*n }).filter(|n| n.start < p.len));
                    k += 1;
                }
                notes.sort_by_key(|n| n.start);
            }
        }
        let len = p.len;
        if let Some(notes) = p.notes.get_mut(ch) {
            notes.clear();
        }
        let steps = loop_ticks / ponzik_engine::TICKS_PER_STEP;
        let mut at = 0;
        while at * ponzik_engine::TICKS_PER_STEP < len {
            self.project.set_step(pat, ch, at, true);
            at += steps;
        }
        self.hint_seen(Hint::FitLoop);
        self.commit(tr("Fit loop to pattern"));
        let bpm = ponzik_i18n::num(self.project.bpm as f64, 1);
        self.toast(&tr_fmt!("Loop fitted: {} beats from step 1, in time at {} BPM", lp.beats, bpm));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.set_view(View::Playlist);
        app
    }

    /// A 2 s loop (one bar at 120 BPM) in the samples, and its clip.
    fn with_loop(app: &mut App) -> usize {
        let left: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let s = ponzik_engine::Sample { name: "l".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() };
        app.samples.insert("l.wav".into(), std::sync::Arc::new(s));
        app.project.audio_clips.push(ponzik_project::AudioClipDef { path: "l.wav".into(), lane: 6, len: 4 * ponzik_engine::PPQ, warp: true, bpm: 120.0, ..Default::default() });
        app.project.audio_clips.len() - 1
    }

    #[test]
    fn repeats_and_fills_make_a_clip_loop() {
        let mut app = app();
        let i = with_loop(&mut app);
        app.loop_clip(ClipSel::Audio(i), LoopOp::Repeat(4));
        let c = &app.project.audio_clips[i];
        assert!(c.looped);
        assert_eq!(c.len, 4 * TICKS_PER_BAR);
        // To the end of the song: as far as the other clips go.
        let end = app.song_end_without(ClipSel::Audio(i));
        app.loop_clip(ClipSel::Audio(i), LoopOp::SongEnd);
        assert_eq!(app.project.audio_clips[i].len, end);
        // Over the time selection.
        app.time_selection = Some((View::Playlist, 2 * TICKS_PER_BAR, 5 * TICKS_PER_BAR));
        app.loop_clip(ClipSel::Audio(i), LoopOp::Selection);
        let c = &app.project.audio_clips[i];
        assert_eq!((c.start, c.len), (2 * TICKS_PER_BAR, 3 * TICKS_PER_BAR));
        // A pattern clip repeats its pattern.
        let plen = app.project.patterns[app.project.clips[0].pattern].len;
        app.loop_clip(ClipSel::Pattern(0), LoopOp::Repeat(8));
        assert_eq!(app.project.clips[0].len, plen * 8);
        // Ctrl+B: a copy right after.
        let n = app.project.audio_clips.len();
        app.loop_clip(ClipSel::Audio(i), LoopOp::Duplicate);
        assert_eq!(app.project.audio_clips.len(), n + 1);
        assert_eq!(app.project.audio_clips[n].start, 5 * TICKS_PER_BAR);
    }

    #[test]
    fn the_brush_lays_copies_along_the_drag_and_takes_them_back() {
        let mut app = app();
        let n = app.project.clips.len();
        let Some(super::super::playlist::ClipDrag::Paint { src, start, period, count }) = app.paint_begin(9, 0) else { panic!("no paint") };
        let count = app.paint_to(src, start, period, count, (period * 3 + 1) as f32);
        assert_eq!(count, 4);
        assert_eq!(app.project.clips.len(), n + 4);
        assert!(app.project.clips[n..].iter().enumerate().all(|(k, c)| c.lane == 9 && c.start == k as u32 * period));
        // Back to the left: the extra copies go.
        assert_eq!(app.paint_to(src, start, period, count, 1.0), 1);
        assert_eq!(app.project.clips.len(), n + 1);
    }

    #[test]
    fn a_loop_on_a_sampler_fits_the_pattern() {
        use ponzik_engine::instruments::{SAMPLER_BEATS, SAMPLER_MODE, SAMPLER_SYNC};
        let mut app = app();
        // Two bars at 120 BPM: a pulse on every beat.
        let mut left = vec![0.0f32; 4 * 48_000];
        for beat in 0..8 {
            for k in 0..2_000 {
                left[beat * 24_000 + k] = (-(k as f32) / 300.0).exp() * if k % 2 == 0 { 0.9 } else { -0.9 };
            }
        }
        let s = ponzik_engine::Sample { name: "b".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() };
        app.samples.insert("b.wav".into(), std::sync::Arc::new(s));
        let mut def = ponzik_project::InstrumentDef::new("sampler");
        def.sample = Some("b.wav".into());
        let ch = app.project.add_channel("Break", def, 60);
        let pat = app.project.current_pattern;
        app.project.patterns[pat].len = TICKS_PER_BAR;
        let kick = app.project.patterns[pat].notes[0].len();
        let lp = app.channel_loop(ch, false).expect("a loop");
        assert_eq!(lp.beats, 8.0);
        assert!(!lp.fitted);
        app.fit_loop_to_pattern(ch);
        let d = &app.project.channels[ch].instrument;
        assert_eq!((d.get(SAMPLER_MODE), d.get(SAMPLER_SYNC), d.get(SAMPLER_BEATS)), (Some(0.0), Some(1.0), Some(8.0)));
        let p = &app.project.patterns[pat];
        assert_eq!(p.len, 2 * TICKS_PER_BAR);
        assert_eq!(p.notes[ch].iter().map(|n| n.start).collect::<Vec<_>>(), [0]);
        // The other channels play their bar twice.
        assert_eq!(p.notes[0].len(), 2 * kick);
        assert!(app.channel_loop(ch, true).expect("cached").fitted);
    }
}
