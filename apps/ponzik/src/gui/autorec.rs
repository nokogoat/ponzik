//! Live automation recording in the interface: the arm button of the
//! transport and of each lane, Touch / Latch / Write passes while the song
//! plays, one undo step per pass.

use ponzik_engine::{AutoTarget, Command, PlayMode};
use ponzik_i18n::{tr, tr_fmt_n};
use ponzik_project::auto_rec::{AutoMode, Recorder, changed_targets};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App, MenuItem, Popup};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// A move older than this (seconds) no longer holds a Touch lane.
const TOUCH_HOLD: f32 = 0.3;
/// More parameters than this moved at once: a preset or a reset, not a hand.
const MAX_MOVES: usize = 8;
/// Thinning tolerance, as a share of the parameter's range.
const TOLERANCE: f32 = 0.004;

#[derive(Default)]
pub struct AutoRec {
    /// Global arm: `Read` is off.
    pub mode: AutoMode,
    /// Mode used by armed lanes while the global arm is off, and restored by
    /// the arm button.
    pub last: AutoMode,
    /// Lanes armed one by one.
    pub lanes: Vec<AutoTarget>,
    pub rec: Recorder,
    /// The undo step of this pass is the last one.
    pub pass_open: bool,
    /// The pass is being written into the project (one undo step with it).
    finishing: bool,
    /// Write lanes started for this pass.
    started: bool,
    /// Last move of each parameter (interface time).
    touched: Vec<(AutoTarget, f32)>,
    last_touched: Option<AutoTarget>,
    /// "Song mode only" already said.
    warned: bool,
    /// Fixed playhead (captures): ticks are not read from the engine.
    pub at: Option<u32>,
}

pub fn mode_name(m: AutoMode) -> &'static str {
    match m {
        AutoMode::Read => tr("Read"),
        AutoMode::Touch => tr("Touch"),
        AutoMode::Latch => tr("Latch"),
        AutoMode::Write => tr("Write"),
    }
}

fn mode_tip(m: AutoMode) -> &'static str {
    match m {
        AutoMode::Read => tr("Automation plays back, nothing is recorded"),
        AutoMode::Touch => tr("Records while you hold a control, then returns to the curve"),
        AutoMode::Latch => tr("Records from the first move until you stop"),
        AutoMode::Write => tr("Armed lanes are overwritten from play to stop"),
    }
}

impl AutoRec {
    /// Mode of the pass: the global one, or the last chosen for armed lanes.
    pub fn write_mode(&self) -> AutoMode {
        match (self.mode, self.last) {
            (AutoMode::Read, AutoMode::Read) => AutoMode::Touch,
            (AutoMode::Read, m) => m,
            (m, _) => m,
        }
    }

    pub fn active(&self) -> bool {
        self.mode != AutoMode::Read || !self.lanes.is_empty()
    }

    pub fn eligible(&self, t: AutoTarget) -> bool {
        t != AutoTarget::Tempo && (self.mode != AutoMode::Read || self.lanes.contains(&t))
    }
}

impl App {
    fn autorec_live(&self) -> bool {
        self.autorec.active() && self.playing && self.counting <= 0.0 && self.project.mode == PlayMode::Song
    }

    fn rec_tick(&self) -> u32 {
        self.autorec.at.unwrap_or_else(|| self.engine.position().max(0.0) as u32)
    }

    fn tolerance(&self, t: AutoTarget) -> f32 {
        self.target_info(t).map_or(0.001, |(_, i)| (i.max - i.min).abs() * TOLERANCE)
    }

    /// Called by `commit`: parameter moves made while recording become points.
    /// True if this edit is only recorded moves (it joins the pass's undo step).
    pub(super) fn record_moves(&mut self) -> bool {
        if self.autorec.finishing {
            return true;
        }
        if !self.autorec.active() || !self.playing {
            return false;
        }
        let moves = changed_targets(self.synced_project(), &self.project);
        if moves.is_empty() || moves.len() > MAX_MOVES || !moves.iter().all(|(t, _)| self.autorec.eligible(*t)) {
            return false;
        }
        if self.project.mode != PlayMode::Song || self.counting > 0.0 {
            if !self.autorec.warned && self.project.mode != PlayMode::Song {
                self.autorec.warned = true;
                self.toast(tr("Automation records while the song plays (Song mode)"));
            }
            return false;
        }
        self.autorec.rec.mode = self.autorec.write_mode();
        let tick = self.rec_tick();
        for (target, value) in moves {
            let tol = self.tolerance(target);
            if self.autorec.rec.touch(target, tick, value, tol) {
                self.send(Command::HoldAutomation { target, hold: true });
            }
            let now = self.time;
            self.autorec.touched.retain(|(t, _)| *t != target);
            self.autorec.touched.push((target, now));
            self.autorec.last_touched = Some(target);
        }
        true
    }

    /// Every frame: open lanes keep writing, Touch lanes released, and the
    /// pass written into the project once the song stops.
    pub(super) fn autorec_tick(&mut self) {
        let live = self.autorec_live();
        if !live {
            if !self.autorec.rec.writes.is_empty() || self.autorec.started {
                self.finish_pass();
            }
            return;
        }
        let tick = self.rec_tick();
        let mut rec = std::mem::take(&mut self.autorec.rec);
        rec.mode = self.autorec.write_mode();
        if !self.autorec.started {
            self.autorec.started = true;
            if rec.mode == AutoMode::Write {
                let lanes: Vec<(AutoTarget, f32, f32)> = self
                    .autorec
                    .lanes
                    .iter()
                    .filter_map(|&t| Some((t, self.auto_value(t)?, self.tolerance(t))))
                    .collect();
                rec.start_write(&lanes, tick);
                for (t, _, _) in lanes {
                    self.send(Command::HoldAutomation { target: t, hold: true });
                }
            }
        }
        let (now, drag) = (self.time, self.drag.is_some());
        let released = {
            let touched = &self.autorec.touched;
            let last = self.autorec.last_touched;
            let held = |t: AutoTarget| {
                touched.iter().any(|(x, at)| *x == t && now - at < TOUCH_HOLD) || (drag && last == Some(t))
            };
            rec.tick(tick, |t| self.auto_value(t), held)
        };
        for target in released {
            self.send(Command::HoldAutomation { target, hold: false });
        }
        self.autorec.rec = rec;
    }

    /// Writes the pass into the song lanes, as one undo step with its moves.
    fn finish_pass(&mut self) {
        self.autorec.started = false;
        let mode = self.autorec.write_mode();
        let mut rec = std::mem::replace(&mut self.autorec.rec, Recorder::new(mode));
        for w in &rec.writes {
            self.send(Command::HoldAutomation { target: w.target, hold: false });
        }
        let done = rec.finish(&mut self.project);
        if !done.is_empty() {
            self.autorec.finishing = true;
            self.commit(tr("Record automation"));
            self.autorec.finishing = false;
            let n = done.len();
            self.toast(&tr_fmt_n!("Automation recorded: {} lane", "Automation recorded: {} lanes", n, n));
        }
        self.autorec.pass_open = false;
        self.autorec.touched.clear();
        self.autorec.last_touched = None;
    }

    /// Song lanes shown in the playlist: the project's, then those being recorded.
    pub(super) fn song_lane_targets(&self) -> Vec<AutoTarget> {
        let mut v: Vec<AutoTarget> =
            self.project.automation.iter().filter(|a| a.scope == ponzik_engine::AutoScope::Song).map(|a| a.target).collect();
        for w in &self.autorec.rec.writes {
            if !v.contains(&w.target) && w.segments.iter().any(|s| !s.points.is_empty()) {
                v.push(w.target);
            }
        }
        v
    }

    pub fn set_auto_mode(&mut self, m: AutoMode) {
        if m == AutoMode::Read {
            self.autorec.mode = AutoMode::Read;
            self.autorec.lanes.clear();
            self.toast(tr("Automation: read"));
            return;
        }
        self.autorec.mode = m;
        self.autorec.last = m;
        self.autorec.warned = false;
        self.toast(&format!("{} · {}", tr("Automation armed"), mode_name(m)));
    }

    pub fn toggle_auto_arm(&mut self) {
        if self.autorec.mode == AutoMode::Read {
            let m = self.autorec.write_mode();
            self.set_auto_mode(m);
        } else {
            self.autorec.mode = AutoMode::Read;
            self.toast(tr("Automation: read"));
        }
    }

    pub fn toggle_lane_arm(&mut self, t: AutoTarget) {
        if let Some(i) = self.autorec.lanes.iter().position(|x| *x == t) {
            self.autorec.lanes.remove(i);
        } else {
            self.autorec.lanes.push(t);
        }
    }

    pub(super) fn auto_mode_items(&self) -> Vec<MenuItem> {
        AutoMode::ALL
            .into_iter()
            .map(|m| MenuItem::new(if m == AutoMode::Read { Icon::Play } else { Icon::Automation }, mode_name(m), Action::SetAutoMode(m)).tip(mode_tip(m)).on(self.autorec.mode == m))
            .collect()
    }

    /// Transport button: arm automation recording; right-click: the mode.
    pub(super) fn auto_arm_button(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let id = wid("tb-auto", 0, 0);
        let armed = self.autorec.mode != AutoMode::Read;
        let writing = self.autorec.rec.open_targets().next().is_some();
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(wid("tb-auto-on", 0, 0), armed as u8 as f32, T_HOVER);
        c.fill_rrect(r, 8.0, t.raised.alpha(hv.max(on * 0.6)));
        let (cx, cy) = r.center();
        if writing {
            let k = 0.6 + 0.4 * (self.time * 6.0).sin().abs();
            c.fill_rrect(r, 8.0, t.negative.alpha(0.18 * k));
        }
        let col = t.text2.mix(t.text, hv).mix(t.negative, on);
        draw_icon(c, Icon::Automation, cx, cy - 3.0, 16.0, col);
        let letter = match self.autorec.write_mode() {
            AutoMode::Touch => "T",
            AutoMode::Latch => "L",
            AutoMode::Write => "W",
            AutoMode::Read => "R",
        };
        let ink = if armed { t.negative } else { t.muted };
        self.text_center(c, self.f.mono_med, 9.0, cx, r.bottom() - 7.0, letter, ink);
        if hov {
            let tip = format!("{} · {}\n{}", tr("Record automation"), mode_name(if armed { self.autorec.mode } else { AutoMode::Read }), tr("Click: arm · right-click: Touch, Latch, Write"));
            self.set_tip(&tip, "");
            if input.pressed {
                self.toggle_auto_arm();
            }
            if input.right_pressed {
                self.open_popup(Popup::AutoMode, Rect::new(r.x, r.bottom(), r.w, 0.0));
            }
        }
    }

    /// Arm button of a playlist lane; true if it took the click.
    pub(super) fn lane_arm(&mut self, c: &mut Canvas, input: &Input, k: usize, cx: f32, cy: f32, target: AutoTarget) -> bool {
        let t = self.theme;
        let id = wid("auto-arm", k, 0);
        let r = Rect::new(cx - 8.0, cy - 8.0, 16.0, 16.0);
        let hov = self.hovered(input, id, r);
        let armed = self.autorec.lanes.contains(&target);
        let global = self.autorec.mode != AutoMode::Read;
        let writing = self.autorec.rec.write(target).is_some_and(|w| w.open);
        c.fill_circle(cx, cy, 8.0, t.bg_deep.alpha(0.85));
        if armed || writing {
            let k = if writing { 0.7 + 0.3 * (self.time * 6.0).sin().abs() } else { 1.0 };
            c.fill_circle(cx, cy, 4.5, t.negative.alpha(k));
        } else {
            c.stroke_circle(cx, cy, 4.5, 1.4, if global { t.negative.alpha(0.6) } else { t.muted.mix(t.text, hov as u8 as f32) });
        }
        if hov {
            self.set_tip(if armed { tr("Disarm the lane") } else { tr("Arm the lane: moves are recorded while the song plays") }, "");
            if input.pressed {
                self.toggle_lane_arm(target);
                return true;
            }
        }
        false
    }

    /// Stretches being recorded, drawn over a lane.
    pub(super) fn draw_pass(&mut self, c: &mut Canvas, target: AutoTarget, x_of: &dyn Fn(f32) -> f32, y_of: &dyn Fn(f32) -> f32, clip: Rect) {
        let Some(w) = self.autorec.rec.write(target) else { return };
        let col = self.theme.negative;
        for seg in &w.segments {
            if seg.points.len() == 1 {
                let p = seg.points[0];
                c.fill_circle(x_of(p.tick as f32), y_of(p.value), 2.5, col);
                continue;
            }
            let pts: Vec<(f32, f32)> = seg
                .points
                .iter()
                .map(|p| (x_of(p.tick as f32), y_of(p.value)))
                .filter(|(x, _)| *x >= clip.x - 40.0 && *x <= clip.right() + 40.0)
                .collect();
            if pts.len() > 1 {
                c.stroke_polyline(&pts, 2.2, col);
            }
        }
    }

    /// `PONZIK_AUTOREC=latch|touch|write[,done]`: a pass recorded (or finished) on a track's
    /// volume and pan, the transport still playing.
    pub fn screenshot_autorec(&mut self, spec: &str) {
        let (spec, done) = spec.split_once(',').map_or((spec, false), |(m, d)| (m, d.trim() == "done"));
        let mode = AutoMode::from_id(spec).unwrap_or(AutoMode::Latch);
        self.set_mode(PlayMode::Song);
        let track = self.project.channels.get(4).map_or(1, |c| c.track);
        // An older pan curve, partly overwritten by this pass.
        self.create_automation(AutoTarget::TrackPan(track), 0.0);
        self.project.add_automation_point(AutoTarget::TrackPan(track), ponzik_engine::AutoScope::Song, 1536, 0.8);
        self.commit(tr("Automation point"));
        self.set_auto_mode(mode);
        self.autorec.lanes.push(AutoTarget::TrackPan(track));
        self.playing = true;
        // A gesture over two bars: the volume swells and dips, the pan sweeps.
        for f in 0..=120 {
            let x = f as f32 / 120.0;
            self.autorec.at = Some(384 + f * 6);
            self.project.tracks[track].gain = 0.6 + 0.4 * (x * std::f32::consts::TAU).sin();
            if f < 80 {
                self.project.tracks[track].pan = ((x * 2.5 - 1.0) * 100.0).round().clamp(-100.0, 100.0) / 100.0;
            }
            self.commit(tr("Track volume"));
            self.time += 1.0 / 60.0;
            self.autorec_tick();
        }
        if done {
            // Stopped: the pass is written, thinned, then playback goes on reading.
            self.playing = false;
            self.autorec_tick();
            self.set_auto_mode(AutoMode::Read);
            self.autorec.at = None;
            self.playing = true;
            self.playlist.zoom = 120.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::AutoScope;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    #[test]
    fn a_latch_pass_is_one_undo_step_and_creates_the_lane() {
        let mut app = app();
        app.set_mode(PlayMode::Song);
        let steps = app.history_len();
        let gain0 = app.project.tracks[3].gain;
        app.set_auto_mode(AutoMode::Latch);
        app.playing = true;
        let gain = AutoTarget::TrackGain(3);
        assert!(app.project.automation_lane(gain, AutoScope::Song).is_none());
        for k in 0..5 {
            app.project.tracks[3].gain = 0.2 + 0.1 * k as f32;
            app.commit("Track volume");
            app.time += 0.1;
        }
        app.autorec_tick();
        assert!(app.autorec.rec.write(gain).is_some_and(|w| w.open));
        // Stopped: written in the song lane, one step for the whole pass.
        app.playing = false;
        app.autorec_tick();
        assert!(app.project.automation_lane(gain, AutoScope::Song).is_some());
        assert_eq!(app.history_len(), steps + 1);
        app.undo();
        assert!(app.project.automation_lane(gain, AutoScope::Song).is_none());
        assert_eq!(app.project.tracks[3].gain, gain0);
    }

    #[test]
    fn touch_releases_and_read_records_nothing() {
        let mut app = app();
        app.set_mode(PlayMode::Song);
        app.playing = true;
        // Read: a move is just a move.
        app.project.tracks[2].pan = 0.5;
        app.commit("Track pan");
        assert!(app.autorec.rec.writes.is_empty());
        // A lane armed alone records in Touch, then lets go.
        app.toggle_lane_arm(AutoTarget::TrackPan(2));
        app.project.tracks[2].pan = -0.5;
        app.commit("Track pan");
        assert!(app.autorec.rec.write(AutoTarget::TrackPan(2)).is_some_and(|w| w.open));
        app.time += 1.0;
        app.autorec_tick();
        assert!(app.autorec.rec.write(AutoTarget::TrackPan(2)).is_some_and(|w| !w.open));
        // Another parameter, not armed: not recorded.
        app.project.tracks[5].pan = 0.25;
        app.commit("Track pan");
        assert!(app.autorec.rec.write(AutoTarget::TrackPan(5)).is_none());
    }
}
