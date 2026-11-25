//! Live performance in the interface: the playlist's performance mode (FL
//! Studio), pad controllers as clip launchers, the crossfader and the cue.
//!
//! Performance mode: from the performance marker on, the playlist's clips
//! are pads. A click (or a key, or a pad of a controller) launches a clip on
//! the launch quantization, one per lane; a ring shows where the playing
//! clip is in its loop, a blinking outline the clips about to start. The red
//! dot records the performance into the arrangement, left of the marker.
//! The player itself is [`ponzik_project::perform::PerfPlayer`].
//!
//! Controllers: a Launchpad, an APC, a Push (user mode) or any 8 by 8 note
//! grid found among the MIDI inputs launches the Session view's slots (or
//! the performance zone's clips while performance mode is on), and its LEDs
//! show the clips in their colors when its protocol is known
//! ([`ponzik_project::controllers`]).

use std::collections::HashMap;

use ponzik_engine::{Command, PlayMode};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_midi::MidiEvent;
use ponzik_project::controllers::{GridButton, GridDevice, Led, button_of, detect, init_message, led_message};
use ponzik_project::live::{PERF_QUANTS, PerfLaunch};
use ponzik_project::perform::{PerfClip, PerfKey, PerfPlayer, column_key};
use ponzik_project::session::Quant;
use ponzik_project::{SESSION_COLUMNS, SESSION_SCENES};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, View};
use super::learn::ctl_event;
use super::widgets::{Icon, draw_icon, wid};

/// A pad controller opened as a launcher.
pub struct Grid {
    pub dev: GridDevice,
    /// Index of its port in [`App::midi_inputs`].
    pub input: usize,
    out: Option<ponzik_midi::MidiOutput>,
    /// What each LED shows now.
    leds: HashMap<GridButton, Led>,
    /// Clips whose pad is held (gate launches), by button.
    held: HashMap<GridButton, Held>,
}

impl Drop for Grid {
    fn drop(&mut self) {
        let bytes = ponzik_project::controllers::exit_message(self.dev);
        if let Some(out) = self.out.as_mut()
            && !bytes.is_empty()
        {
            let _ = out.send(&bytes);
        }
    }
}

/// What a held pad launched.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    Slot(usize, usize),
    Perf(PerfKey),
}

/// Live performance state of the interface (not saved).
#[derive(Default)]
pub struct LiveUi {
    pub perf: PerfPlayer,
    /// Performance clip whose pad the mouse holds.
    pub mouse: Option<PerfKey>,
    /// Number keys held (performance columns).
    pub keys: [bool; 8],
    /// Cued tracks, by identity (they follow moves).
    pub cued: Vec<u32>,
    pub grids: Vec<Grid>,
    /// The MIDI inputs were looked at for controllers.
    scanned: bool,
}

/// Velocity of a pad press (127 for anything else).
pub fn pad_velocity(v: ponzik_project::midi_map::CtlValue) -> u8 {
    match v {
        ponzik_project::midi_map::CtlValue::Note { velocity, .. } => velocity.max(1),
        _ => 127,
    }
}

/// 0xRRGGBB of a color, for a controller's LEDs.
fn hex(c: Color) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

/// Short label of a performance quantization.
fn quant_label(q: Quant) -> &'static str {
    match q {
        Quant::None => tr("Off"),
        Quant::Beat => "1/4",
        Quant::Bar => "1",
        _ => "2",
    }
}

impl App {
    // --- Performance mode ---

    pub fn perf_on(&self) -> bool {
        self.project.live.perf.on
    }

    /// Turns the playlist's performance mode on or off. The first time, the
    /// marker goes after the song: the arrangement stays, the zone starts empty.
    pub fn toggle_perf_mode(&mut self) {
        if self.perf_on() {
            self.clear_perf();
            self.project.live.perf.on = false;
            if self.project.mode == PlayMode::Session && self.session.is_idle() {
                self.set_mode(PlayMode::Song);
            }
            self.commit(tr("Performance mode off"));
            return;
        }
        if self.project.live.perf.zone == 0 && (!self.project.clips.is_empty() || !self.project.audio_clips.is_empty()) {
            self.project.live.perf.zone = self.project.song_len();
        }
        self.project.live.perf.on = true;
        self.commit(tr("Performance mode"));
        self.toast(tr("Performance mode: the clips right of the marker are pads"));
    }

    /// Engine clips of the performance, sent when they changed (no undo step).
    fn push_perf(&mut self) {
        let live = self.live.perf.live();
        if self.project.live.playing != live {
            self.project.live.playing = live;
            self.sync_session();
        }
    }

    fn perf_clock(&self) -> (f64, bool) {
        (self.position, self.playing && self.project.mode == PlayMode::Session)
    }

    /// A performance clip's pad goes down.
    pub fn perf_press(&mut self, key: PerfKey) {
        let (pos, playing) = self.perf_clock();
        if self.live.perf.press(&self.project, key, pos, playing) {
            self.ensure_session_mode();
        }
        self.push_perf();
    }

    /// A performance clip's pad comes up (gate launches).
    pub fn perf_release(&mut self, key: PerfKey) {
        let (pos, playing) = self.perf_clock();
        self.live.perf.release(&self.project, key, pos, playing);
        self.push_perf();
    }

    pub fn perf_stop_lane(&mut self, lane: usize) {
        let (pos, playing) = self.perf_clock();
        self.live.perf.stop_lane(&self.project, lane, pos, playing);
        self.push_perf();
    }

    pub fn perf_stop_all(&mut self) {
        let (pos, playing) = self.perf_clock();
        self.live.perf.stop_all(&self.project, pos, playing);
        self.push_perf();
    }

    /// Everything of the performance stops (transport stopped, project replaced).
    pub(super) fn clear_perf(&mut self) {
        self.finish_perf_record();
        self.live.perf.reset();
        self.live.mouse = None;
        self.live.keys = [false; 8];
        if !self.project.live.playing.is_empty() {
            self.project.live.playing.clear();
            self.sync_engine();
        }
    }

    /// A new project: nothing plays, nothing is cued.
    pub(super) fn live_project_replaced(&mut self) {
        self.live.perf = PerfPlayer::default();
        self.live.mouse = None;
        self.live.cued.clear();
        self.project.live.playing.clear();
    }

    pub fn toggle_perf_record(&mut self) {
        if self.live.perf.recorder.is_some() {
            self.finish_perf_record();
        } else {
            let (pos, playing) = self.perf_clock();
            self.live.perf.start_recording(if playing { pos } else { 0.0 });
            self.toast(tr("Performance record: what you launch goes into the arrangement"));
        }
    }

    /// Writes the recorded performance into the arrangement (one undo step).
    pub(super) fn finish_perf_record(&mut self) {
        if self.live.perf.recorder.is_none() {
            return;
        }
        let takes = self.live.perf.finish_recording(self.position);
        let n = self.project.write_performance(&takes);
        if n > 0 {
            self.commit(tr("Record performance"));
            self.toast(&tr_fmt!("{} clips written to the arrangement", n));
        } else {
            self.toast(tr("Performance record: nothing was played"));
        }
    }

    /// Every frame: planned starts and stops happen, scenes apply their
    /// tempo and meter, controllers are found and their LEDs follow.
    pub(super) fn performance_tick(&mut self) {
        if self.project.mode == PlayMode::Session {
            let (pos, playing) = self.perf_clock();
            if playing {
                self.live.perf.advance(pos);
            }
            self.push_perf();
        }
        self.apply_started_scenes();
        if !self.live.scanned {
            self.live.scanned = true;
            self.open_grids();
        }
        self.update_grid_leds();
    }

    /// Scenes that just started set their tempo and time signature.
    fn apply_started_scenes(&mut self) {
        let started = std::mem::take(&mut self.session.started);
        let mut changed = false;
        for row in started {
            let scene = self.project.session_scenes.get(row).copied().unwrap_or_default();
            if let Some(bpm) = scene.bpm()
                && self.project.bpm != bpm
            {
                self.project.bpm = bpm;
                changed = true;
            }
            if let Some((num, den)) = scene.sig {
                let sig = ponzik_engine::TimeSig { tick: 0, num, den };
                let sigs = &mut self.project.signatures;
                match sigs.first_mut() {
                    Some(s) if s.tick == 0 => {
                        changed |= *s != sig;
                        *s = sig;
                    }
                    _ => {
                        sigs.insert(0, sig);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.sync_engine();
        }
    }

    /// Number keys 1 to 8 in the playlist, performance mode on: the same
    /// clip number on every lane (a column of the grid).
    pub(super) fn perf_keys(&mut self, input: &Input) {
        if self.view != View::Playlist || !self.perf_on() || input.mods.ctrl || input.mods.alt {
            return;
        }
        for &(code, down) in &input.physical {
            let Some(k) = column_key(code) else { continue };
            let keys: Vec<PerfKey> = self.project.perf_lanes().iter().filter_map(|(_, ks)| ks.get(k).copied()).collect();
            match (down, self.live.keys[k]) {
                (true, false) => {
                    self.live.keys[k] = true;
                    for key in keys {
                        self.perf_press(key);
                    }
                }
                (false, true) => {
                    self.live.keys[k] = false;
                    for key in keys {
                        self.perf_release(key);
                    }
                }
                _ => {}
            }
        }
    }

    /// Color of a performance clip.
    fn perf_color(&self, clip: &PerfClip) -> Color {
        match clip {
            PerfClip::Pattern(c) => self.pattern_tint(c.pattern),
            PerfClip::Audio(a) => super::clip::audio_color(a, &self.theme),
        }
    }

    // --- Playlist ---

    /// Performance controls in the playlist's header, right to left from
    /// `right`; returns the x left of them.
    pub(super) fn perf_header(&mut self, c: &mut Canvas, input: &Input, header: Rect, right: f32) -> f32 {
        let t = self.theme;
        let on = self.perf_on();
        let toggle = Rect::new(right - 36.0, header.y, 36.0, header.h);
        let tip = if on {
            (tr("Performance mode on\nThe clips right of the marker are pads · click to turn off"), "")
        } else {
            (tr("Performance mode\nThe clips right of a marker become pads, launched live"), "")
        };
        if self.icon_button(c, input, wid("perf-mode", 0, 0), toggle, Icon::Session, on, tip) {
            self.toggle_perf_mode();
        }
        if !self.perf_on() {
            return toggle.x - 8.0;
        }
        let stop = Rect::new(toggle.x - 44.0, header.y, 36.0, header.h);
        if self.icon_button(c, input, wid("perf-stop-all", 0, 0), stop, Icon::Stop, false, (tr("Stop all clips"), "")) {
            self.perf_stop_all();
        }
        let rec = Rect::new(stop.x - 44.0, header.y, 36.0, header.h);
        let recording = self.live.perf.recorder.is_some();
        let tip = if recording {
            tr("Stop recording the performance\nThe clips played are written into the arrangement")
        } else {
            tr("Record performance\nWrites the clips you launch into the arrangement, where they played")
        };
        if self.icon_button(c, input, wid("perf-rec", 0, 0), rec, Icon::Record, recording, (tip, "")) {
            self.toggle_perf_record();
        }
        if recording {
            let k = 0.5 + 0.5 * (self.time * 5.0).sin();
            let (cx, cy) = rec.center();
            c.stroke_circle(cx, cy, 13.0, 2.0, t.negative.alpha(0.4 + 0.5 * k));
        }
        // Launch mode and quantization.
        let modes_r = Rect::new(rec.x - 8.0 - 236.0, header.y + (header.h - 28.0) * 0.5, 236.0, 28.0);
        let labels: Vec<&str> = PerfLaunch::ALL.iter().map(|m| tr(m.label())).collect();
        let tips = [
            (tr("Trigger: a press starts the clip"), ""),
            (tr("Gate: the clip plays while held"), ""),
            (tr("Toggle: a press starts the clip, the next one stops it"), ""),
            (tr("Retrigger: each press starts the clip over"), ""),
        ];
        let perf = self.project.live.perf;
        let sel = PerfLaunch::ALL.iter().position(|m| *m == perf.launch).unwrap_or(0);
        if let Some(i) = self.segmented_tips(c, input, wid("perf-launch", 0, 0), modes_r, &labels, &tips, sel) {
            self.project.live.perf.launch = PerfLaunch::ALL[i];
            self.commit(tr("Launch mode"));
        }
        let q_r = Rect::new(modes_r.x - 8.0 - 128.0, modes_r.y, 128.0, 28.0);
        let labels: Vec<&str> = PERF_QUANTS.iter().map(|q| quant_label(*q)).collect();
        let tips: Vec<(String, &str)> = PERF_QUANTS.iter().map(|q| (tr_fmt!("Launch quantization: {}", tr(q.label())), "")).collect();
        let tips: Vec<(&str, &str)> = tips.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        let sel = PERF_QUANTS.iter().position(|q| *q == perf.quant).unwrap_or(2);
        if let Some(i) = self.segmented_tips(c, input, wid("perf-quant", 0, 0), q_r, &labels, &tips, sel) {
            self.project.live.perf.quant = PERF_QUANTS[i];
            self.commit(tr("Launch quantization"));
        }
        q_r.x - 8.0
    }

    /// The zone's clip under the pointer.
    fn perf_hit(&self, x: f32, y: f32, lane_at: &dyn Fn(f32) -> f32, x_of: &dyn Fn(f32) -> f32) -> Option<PerfKey> {
        let lane = lane_at(y);
        if lane < 0.0 {
            return None;
        }
        let lane = lane as usize;
        let inside = |start: u32, len: u32| x >= x_of(start as f32) && x <= x_of((start + len) as f32);
        let audio = self.project.audio_clips.iter().enumerate().rev().find(|(_, c)| c.lane == lane && inside(c.start, c.len)).map(|(i, _)| PerfKey::Audio(i));
        let pattern = || self.project.clips.iter().enumerate().rev().find(|(_, c)| c.lane == lane && inside(c.start, c.len)).map(|(i, _)| PerfKey::Pattern(i));
        audio.or_else(pattern).filter(|&k| self.project.perf_clip(k).is_some())
    }

    /// Stop button of a lane in the zone, left of the marker.
    fn perf_stop_rect(zone_x: f32, ly: f32, lane_h: f32) -> Rect {
        Rect::new(zone_x - 26.0, ly + lane_h - 27.0, 22.0, 22.0)
    }

    /// Clicks of performance mode in the playlist's lanes, before the
    /// clips': the marker's handle, the lanes' stop buttons, the pads. True
    /// if the press was taken (Shift lets the clips be edited as usual).
    pub(super) fn perf_input(
        &mut self,
        input: &Input,
        lanes_r: Rect,
        lane_h: f32,
        lane_at: &dyn Fn(f32) -> f32,
        lane_y: &dyn Fn(usize) -> f32,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) -> bool {
        if !self.perf_on() {
            return false;
        }
        // The mouse let go of a gate pad.
        if let Some(key) = self.live.mouse
            && !input.down
        {
            self.live.mouse = None;
            self.perf_release(key);
        }
        let zone_x = x_of(self.project.live.perf.zone as f32);
        // The marker's handle: drag it along the bars.
        let handle = Rect::new(zone_x - 46.0, lanes_r.y, 52.0, 18.0);
        let hid = wid("perf-zone", 0, 0);
        let hov = self.hovered(input, hid, handle);
        if hov {
            self.set_tip(tr("Performance marker\nDrag it: the clips from here on are pads"), "");
            self.set_cursor(Cursor::ResizeH);
        }
        if hov && input.pressed && !input.mods.shift {
            self.begin_drag(hid, input, 0.0);
            return true;
        }
        if self.is_dragging(hid) {
            let map = self.project.tempo_map();
            let tick = map.snap_bar(tick_at(input.x).max(0.0) as f64);
            if tick != self.project.live.perf.zone && input.down {
                self.clear_perf();
                self.project.live.perf.zone = tick;
                self.commit(tr("Move performance marker"));
            }
            return input.pressed;
        }
        if !input.inside || !lanes_r.contains(input.x, input.y) || input.mods.shift {
            return false;
        }
        // Stop buttons of the lanes that hold pads.
        if input.pressed {
            for (lane, _) in self.project.perf_lanes() {
                if Self::perf_stop_rect(zone_x, lane_y(lane), lane_h).contains(input.x, input.y) {
                    self.perf_stop_lane(lane);
                    return true;
                }
            }
        }
        let Some(key) = self.perf_hit(input.x, input.y, lane_at, x_of) else { return false };
        self.set_tip(tr("Pad: click to launch on the next boundary\nShift: edit the clip"), "");
        if input.pressed {
            self.perf_press(key);
            if self.project.live.perf.launch == PerfLaunch::Gate {
                self.live.mouse = Some(key);
            }
            return true;
        }
        input.double
    }

    /// Draws performance mode over the playlist's lanes: the zone, its
    /// marker, the lanes' stop buttons, and each pad's state.
    pub(super) fn perf_overlay(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        lanes_r: Rect,
        lane_h: f32,
        lane_y: &dyn Fn(usize) -> f32,
        x_of: &dyn Fn(f32) -> f32,
    ) {
        if !self.perf_on() {
            return;
        }
        let t = self.theme;
        let zone_x = x_of(self.project.live.perf.zone as f32).max(lanes_r.x);
        if zone_x < lanes_r.right() {
            c.fill_rect(Rect::new(zone_x, lanes_r.y, lanes_r.right() - zone_x, lanes_r.h), t.accent.alpha(0.05));
            c.fill_rect(Rect::new(zone_x.round() - 1.0, lanes_r.y, 2.0, lanes_r.h), t.accent.alpha(0.7));
            // The handle: a flag at the top.
            // The handle: a tab left of the line, over the arrangement's end.
            let hov = self.hovered(input, wid("perf-zone", 0, 0), Rect::new(zone_x - 46.0, lanes_r.y, 52.0, 18.0));
            let flag = Rect::new(zone_x - 46.0, lanes_r.y, 47.0, 18.0);
            c.fill_rrect(flag, 5.0, t.accent.mix(t.text, if hov { 0.25 } else { 0.0 }));
            draw_icon(c, Icon::Play, flag.x + 9.0, flag.y + 9.0, 9.0, t.bg_deep);
            self.text(c, self.f.sans_med, 10.0, flag.x + 17.0, flag.y + 9.0, tr("Live"), t.bg_deep);
        }
        let pulse = 0.5 + 0.5 * (self.time * 8.0).sin();
        let pos = self.position;
        for (lane, keys) in self.project.perf_lanes() {
            let ly = lane_y(lane);
            if ly + lane_h < lanes_r.y || ly > lanes_r.bottom() {
                continue;
            }
            // Stop button of the lane.
            let state = self.live.perf.lane(lane).cloned().unwrap_or_default();
            let stop = Self::perf_stop_rect(zone_x, ly, lane_h);
            let id = wid("perf-stop", lane, 0);
            let hov = self.hovered(input, id, stop);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(stop, 6.0, t.raised.mix(t.raised_hi, hv));
            let ink = if state.stopping() {
                t.text.alpha(0.4 + 0.6 * pulse)
            } else if state.is_idle() {
                t.muted
            } else {
                t.text
            };
            draw_icon(c, Icon::Stop, stop.x + 11.0, stop.y + 11.0, 9.0, ink);
            if hov {
                self.set_tip(&tr_fmt!("Stop lane {}", lane + 1), "");
            }
            for key in keys {
                let Some(clip) = self.project.perf_clip(key) else { continue };
                let col = self.perf_color(&clip);
                let r = Rect::new(x_of(clip.start() as f32) + 1.0, ly + 3.0, (x_of((clip.start() + clip.len()) as f32) - x_of(clip.start() as f32) - 2.0).max(4.0), lane_h - 6.0);
                if r.right() < lanes_r.x || r.x > lanes_r.right() {
                    continue;
                }
                let playing = self.live.perf.is_playing(key);
                let queued = self.live.perf.is_queued(key);
                if playing {
                    c.stroke_rrect(r, 8.0, 2.0, col.mix(t.text, 0.3));
                } else if queued {
                    c.stroke_rrect(r, 8.0, 2.0, col.alpha(0.3 + 0.7 * pulse));
                }
                // Pad: a play triangle in a ring, the ring the clip's loop.
                let (cx, cy) = (r.right() - 13.0, r.y + 10.0);
                if r.w > 40.0 {
                    c.fill_circle(cx, cy, 8.0, t.bg_deep.alpha(0.6));
                    c.stroke_circle(cx, cy, 7.0, 1.5, col.alpha(0.35));
                    if let Some(f) = self.live.perf.progress(key, pos) {
                        let n = (f * 40.0).ceil().max(1.0) as usize;
                        let pts: Vec<(f32, f32)> = (0..=n)
                            .map(|k| {
                                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * f * k as f32 / n as f32;
                                (cx + 7.0 * a.cos(), cy + 7.0 * a.sin())
                            })
                            .collect();
                        c.stroke_polyline(&pts, 2.0, col.mix(t.text, 0.4));
                    }
                    let ink = if playing { t.text } else if queued { t.text.alpha(0.4 + 0.6 * pulse) } else { t.text2 };
                    draw_icon(c, if playing { Icon::Stop } else { Icon::Play }, cx + if playing { 0.0 } else { 1.0 }, cy, 7.0, ink);
                }
            }
        }
    }

    // --- Pad controllers ---

    /// Opens the pad controllers found among the MIDI inputs, with their
    /// LED output when the device has one.
    fn open_grids(&mut self) {
        let outputs = ponzik_midi::list_outputs().unwrap_or_default();
        let mut grids = Vec::new();
        for (i, input) in self.midi_inputs.iter().enumerate() {
            let Some(dev) = detect(input.name()) else { continue };
            let out = outputs.iter().find(|p| detect(&p.name) == Some(dev)).and_then(|p| ponzik_midi::open_output(&p.id).ok());
            let mut grid = Grid { dev, input: i, out, leds: HashMap::new(), held: HashMap::new() };
            let init = init_message(dev);
            if let Some(out) = grid.out.as_mut()
                && !init.is_empty()
            {
                let _ = out.send(&init);
            }
            eprintln!("Launcher: {} ({})", dev.name(), if grid.out.is_some() { "LEDs" } else { "no LEDs" });
            grids.push(grid);
        }
        if let Some(g) = grids.first() {
            self.toast(&tr_fmt!("{} launches clips", g.dev.name()));
        }
        self.live.grids = grids;
    }

    /// Polls the MIDI inputs: a pad controller's buttons launch, the rest
    /// (and everything while MIDI learn listens) goes on as usual.
    pub(super) fn poll_midi_inputs(&mut self, events: &mut Vec<MidiEvent>) {
        let mut grid_events: Vec<(usize, Vec<MidiEvent>)> = Vec::new();
        for (i, input) in self.midi_inputs.iter().enumerate() {
            match self.live.grids.iter().position(|g| g.input == i) {
                Some(g) if !self.ctl.learn.is_active() => {
                    let mut ev = Vec::new();
                    input.poll(&mut ev);
                    if !ev.is_empty() {
                        grid_events.push((g, ev));
                    }
                }
                _ => input.poll(events),
            }
        }
        for (g, ev) in grid_events {
            for e in ev {
                if !self.grid_event(g, e) {
                    events.push(e);
                }
            }
        }
    }

    /// One message of a pad controller; false if it is not a launcher button.
    pub fn grid_event(&mut self, g: usize, e: MidiEvent) -> bool {
        let Some(dev) = self.live.grids.get(g).map(|g| g.dev) else { return false };
        let Some((src, value)) = ctl_event(e) else { return false };
        let Some(b) = button_of(dev, src) else { return false };
        let down = value.pressed();
        let velocity = pad_velocity(value);
        if down {
            self.grid_press(g, b, velocity);
        } else if let Some(held) = self.live.grids[g].held.remove(&b) {
            match held {
                Held::Slot(col, row) => self.release_clip(col, row),
                Held::Perf(key) => self.perf_release(key),
            }
        }
        true
    }

    fn grid_press(&mut self, g: usize, b: GridButton, velocity: u8) {
        let perf = self.perf_on();
        match b {
            GridButton::Clip { col, row } if perf => {
                let Some(key) = self.project.perf_grid(8, 8).get(col).and_then(|c| c.get(row)).copied() else { return };
                self.perf_press(key);
                self.live.grids[g].held.insert(b, Held::Perf(key));
            }
            GridButton::Clip { col, row } => {
                if col < SESSION_COLUMNS && row < SESSION_SCENES {
                    self.launch_clip_with(col, row, velocity);
                    self.live.grids[g].held.insert(b, Held::Slot(col, row));
                }
            }
            GridButton::Scene(row) if perf => {
                // A row of the zone: the same clip number on every lane.
                let keys: Vec<PerfKey> = self.project.perf_grid(8, 8).iter().filter_map(|c| c.get(row).copied()).collect();
                for key in keys {
                    self.perf_press(key);
                }
            }
            GridButton::Scene(row) => {
                if row < SESSION_SCENES {
                    self.launch_scene(row);
                }
            }
            GridButton::Stop(col) => self.stop_launcher_column(col),
            GridButton::StopAll => self.stop_all_launched(),
        }
    }

    /// Stops a column of the launcher in use: a Session column, or in
    /// performance mode the `col`-th lane holding pads.
    pub fn stop_launcher_column(&mut self, col: usize) {
        if self.perf_on() {
            if let Some(&(lane, _)) = self.project.perf_lanes().get(col) {
                self.perf_stop_lane(lane);
            }
        } else if col < SESSION_COLUMNS {
            self.stop_column(col);
        }
    }

    /// Stops every launched clip (Session view and performance mode).
    pub fn stop_all_launched(&mut self) {
        self.perf_stop_all();
        self.session.stop_all(&self.project, self.position, self.playing);
        let live = self.session.live(&self.project);
        if self.project.session_live != live {
            self.project.session_live = live;
            self.sync_session();
        }
    }

    /// What each launcher button shows now.
    pub fn launcher_leds(&self, dev: GridDevice) -> Vec<(GridButton, Led)> {
        let (cols, rows) = dev.size();
        let mut out = Vec::new();
        if self.perf_on() {
            let grid = self.project.perf_grid(cols, rows);
            for col in 0..cols {
                for row in 0..rows {
                    let b = GridButton::Clip { col, row };
                    let led = match grid.get(col).and_then(|c| c.get(row)).and_then(|&k| Some((k, self.project.perf_clip(k)?))) {
                        None => Led::Off,
                        Some((k, clip)) => {
                            let rgb = hex(self.perf_color(&clip));
                            if self.live.perf.is_queued(k) {
                                Led::Queued(rgb)
                            } else if self.live.perf.is_playing(k) {
                                Led::Playing(rgb)
                            } else {
                                Led::Idle(rgb)
                            }
                        }
                    };
                    out.push((b, led));
                }
            }
            let lanes = self.project.perf_lanes();
            for col in 0..cols {
                let lane = lanes.get(col).and_then(|(l, _)| self.live.perf.lane(*l));
                let led = match lane {
                    Some(l) if l.stopping() => Led::Queued(0xffffff),
                    Some(l) if !l.is_idle() => Led::Playing(0xffffff),
                    _ => Led::Off,
                };
                out.push((GridButton::Stop(col), led));
            }
            return out;
        }
        for col in 0..cols.min(SESSION_COLUMNS) {
            let state = &self.session.cols[col];
            for row in 0..rows.min(SESSION_SCENES) {
                let b = GridButton::Clip { col, row };
                let led = if !self.project.session_filled(col, row) {
                    Led::Off
                } else {
                    let rgb = hex(self.slot_look(col, row).0);
                    if state.queued().is_some_and(|(r, _)| r == row) {
                        Led::Queued(rgb)
                    } else if state.playing.is_some_and(|p| p.row == row) {
                        Led::Playing(rgb)
                    } else {
                        Led::Idle(rgb)
                    }
                };
                out.push((b, led));
            }
            let led = if state.stopping() {
                Led::Queued(0xffffff)
            } else if state.playing.is_some() {
                Led::Playing(0xffffff)
            } else {
                Led::Off
            };
            out.push((GridButton::Stop(col), led));
        }
        for row in 0..rows.min(SESSION_SCENES) {
            let led = if self.session.scene_queued.is_some_and(|(r, _)| r == row) {
                Led::Queued(0x3fd45a)
            } else if self.session.scene.is_some_and(|(r, _)| r == row) {
                Led::Playing(0x3fd45a)
            } else {
                Led::Off
            };
            out.push((GridButton::Scene(row), led));
        }
        out
    }

    /// Sends the LEDs that changed.
    fn update_grid_leds(&mut self) {
        for g in 0..self.live.grids.len() {
            if self.live.grids[g].out.is_none() {
                continue;
            }
            let dev = self.live.grids[g].dev;
            let want = self.launcher_leds(dev);
            let grid = &mut self.live.grids[g];
            let mut bytes = Vec::new();
            for (b, led) in want {
                if grid.leds.get(&b) != Some(&led) {
                    bytes.extend(led_message(dev, b, led));
                    grid.leds.insert(b, led);
                }
            }
            if !bytes.is_empty()
                && let Some(out) = grid.out.as_mut()
                && out.send(&bytes).is_err()
            {
                // The device went away: no more LEDs.
                grid.out = None;
            }
        }
    }

    // --- Crossfader and cue ---

    /// True if the track is cued.
    pub fn is_cued(&self, track: usize) -> bool {
        self.project.tracks.get(track).is_some_and(|t| self.live.cued.contains(&t.id))
    }

    /// Cue (prelisten) of a track: heard through the cue blend, never in the master.
    pub fn toggle_cue(&mut self, track: usize) {
        let Some(id) = self.project.tracks.get(track).map(|t| t.id) else { return };
        let on = !self.live.cued.contains(&id);
        if on {
            self.live.cued.push(id);
        } else {
            self.live.cued.retain(|&x| x != id);
        }
        self.send(Command::SetTrackCue { track, on });
    }

    /// Screenshot setup (`PONZIK_PERFORM=demo|record`): a playlist in
    /// performance mode, pads playing from the start and one waiting.
    pub fn screenshot_perform(&mut self, spec: &str) {
        let bar = ponzik_engine::TICKS_PER_BAR;
        // The arrangement left of bar 9, a zone of pads from there.
        let zone = 8 * bar;
        self.project.clips.retain(|c| c.start < zone);
        self.project.audio_clips.retain(|c| c.start < zone);
        let n = self.project.patterns.len().max(1);
        for lane in 0..5usize {
            for (k, (at, len)) in [(0, bar), (2 * bar, 2 * bar), (5 * bar, bar)].into_iter().enumerate() {
                let pattern = (lane * 3 + k) % n;
                self.project.clips.push(ponzik_project::ClipDef { pattern, lane, start: zone + at, len, offset: 0, muted: false });
            }
        }
        self.project.live.perf = ponzik_project::live::Perf { on: true, zone, ..Default::default() };
        self.commit("perform");
        let grid = self.project.perf_grid(8, 8);
        // From the start: pads on lanes 0, 1 and 3.
        for (lane, k) in [(0, 0), (1, 1), (3, 2)] {
            if let Some(&key) = grid.get(lane).and_then(|keys| keys.get(k)) {
                self.live.perf.press(&self.project, key, 0.0, false);
            }
        }
        if spec.contains("record") {
            self.live.perf.start_recording(0.0);
        }
        self.ensure_session_mode();
        // On the next bar: lane 1 changes clip, lane 2 starts.
        for (lane, k) in [(1, 2), (2, 0)] {
            if let Some(&key) = grid.get(lane).and_then(|keys| keys.get(k)) {
                self.perf_press(key);
            }
        }
        self.playlist.scroll_x = 0.0;
        self.push_perf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_project::ClipDef;
    use ponzik_project::controllers::GridDevice;

    fn app() -> App {
        let mut project = ponzik_project::demo_project();
        project.clips.clear();
        let bar = ponzik_engine::TICKS_PER_BAR;
        project.clips.push(ClipDef { pattern: 0, lane: 0, start: 0, len: bar, offset: 0, muted: false });
        project.clips.push(ClipDef { pattern: 0, lane: 0, start: 4 * bar, len: bar, offset: 0, muted: false });
        project.clips.push(ClipDef { pattern: 1, lane: 0, start: 5 * bar, len: bar, offset: 0, muted: false });
        project.clips.push(ClipDef { pattern: 1, lane: 1, start: 4 * bar, len: 2 * bar, offset: 0, muted: false });
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        let mut app = App::new(handle, project, 48_000);
        app.view = View::Playlist;
        app
    }

    #[test]
    fn performance_mode_launches_records_and_undoes() {
        let mut app = app();
        app.toggle_perf_mode();
        assert!(app.perf_on());
        assert_eq!(app.project.live.perf.zone, 6 * ponzik_engine::TICKS_PER_BAR, "the marker after the song");
        app.project.live.perf.zone = 4 * ponzik_engine::TICKS_PER_BAR;
        app.commit("zone");
        assert_eq!(app.project.perf_lanes().len(), 2);
        app.toggle_perf_record();
        app.perf_press(PerfKey::Pattern(1));
        assert!(app.playing);
        assert_eq!(app.project.mode, PlayMode::Session);
        assert_eq!(app.project.live.playing.len(), 1, "the engine gets the clip");
        app.position = 0.0;
        app.performance_tick();
        assert!(app.live.perf.is_playing(PerfKey::Pattern(1)));
        app.position = 700.0;
        let clips = app.project.clips.len();
        app.toggle_perf_record();
        assert_eq!(app.project.clips.len(), clips + 2, "two loops of one bar");
        app.undo();
        assert_eq!(app.project.clips.len(), clips);
        // Stopping the transport stops the pads and ends a recording.
        app.toggle_perf_record();
        app.stop();
        assert!(app.live.perf.is_idle());
        assert!(app.project.live.playing.is_empty());
        assert!(app.live.perf.recorder.is_none());
    }

    #[test]
    fn number_keys_launch_a_column_of_pads() {
        let mut app = app();
        app.project.live.perf = ponzik_project::live::Perf { on: true, zone: 4 * ponzik_engine::TICKS_PER_BAR, ..Default::default() };
        app.commit("perf");
        let input = Input { physical: vec![(2, true)], ..Input::default() };
        app.perf_keys(&input);
        assert!(app.live.perf.is_queued(PerfKey::Pattern(1)));
        assert!(app.live.perf.is_queued(PerfKey::Pattern(3)));
    }

    #[test]
    fn a_launchpad_launches_slots_and_lights_them() {
        let mut app = app();
        app.project.set_session_pattern(0, 0, 0);
        app.project.session_slot_mut(0, 0).unwrap().velocity = 100;
        app.commit("slot");
        app.live.grids.push(Grid { dev: GridDevice::LaunchpadX, input: 99, out: None, leds: HashMap::new(), held: HashMap::new() });
        // Top left pad (note 81), hit softly.
        assert!(app.grid_event(0, MidiEvent::NoteOn { channel: 0, key: 81, velocity: 64 }));
        assert_eq!(app.project.session_live.len(), 1);
        assert_eq!(app.project.session_live[0].soft, 50, "velocity-sensitive slot");
        let leds = app.launcher_leds(GridDevice::LaunchpadX);
        assert!(leds.iter().any(|(b, l)| *b == GridButton::Clip { col: 0, row: 0 } && matches!(l, Led::Queued(_))));
        assert!(leds.iter().any(|(b, l)| *b == GridButton::Clip { col: 1, row: 0 } && *l == Led::Off));
        assert!(app.grid_event(0, MidiEvent::NoteOff { channel: 0, key: 81, velocity: 0 }));
        // Not a launcher button: left to the rest (keys, learn).
        assert!(!app.grid_event(0, MidiEvent::ControlChange { channel: 0, controller: 1, value: 3 }));
        // Stop all: the top right button.
        assert!(app.grid_event(0, MidiEvent::ControlChange { channel: 0, controller: 98, value: 127 }));
    }

    #[test]
    fn cue_follows_the_track() {
        let mut app = app();
        app.toggle_cue(3);
        assert!(app.is_cued(3));
        app.toggle_cue(3);
        assert!(!app.is_cued(3));
    }
}
