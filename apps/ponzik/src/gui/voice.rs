//! Voice to notes in the interface: the Voice toggle of the top bar and its
//! panel (live pitch with a cents needle, a scrolling pitch trail over the
//! notes played, the options), the settings sent to the engine, and the
//! notes it reports: written into the current pattern while recording (like
//! the typing keyboard's), fed to MIDI capture, lit on the piano roll's keys.
//! Also the offline variant: an audio clip turned into a pattern.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use ponzik_audio::Latency;
use ponzik_dsp::voice::VoiceTracker;
use ponzik_engine::{Command, InputSource, PPQ, PlayMode, TICKS_PER_BAR, VoiceConfig, VoiceMode, VoiceReport, VoiceSettings};
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n};
use ponzik_project::ScaleKind;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App};
use super::keys::HeldNote;
use super::rack::note_name;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Held-note codes of the voice's notes, one per key: outside the typing
/// keyboard's and the MIDI controllers' (`0x1000 +`).
pub const VOICE_CODE: u16 = 0xE000;
const PANEL_W: f32 = 404.0;
const PANEL_H: f32 = 616.0;
/// Seconds of pitch shown on the trail, and kept.
const TRAIL_SECONDS: f64 = 4.0;
const TRAIL_KEEP: f64 = 4.5;
/// Semitones shown on the trail.
const TRAIL_SPAN: f32 = 15.0;

pub fn is_voice_code(code: u16) -> bool {
    (VOICE_CODE..VOICE_CODE + 128).contains(&code)
}

/// Options of the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoicePrefs {
    /// Snap to the project's scale (when it has one), else to every semitone.
    pub snap_scale: bool,
    pub retune: f32,
    pub transpose: i8,
    pub velocity: f32,
    pub low: u8,
    pub high: u8,
    pub mode: VoiceMode,
    pub gate_db: f32,
    /// Hear the dry voice on the channel's mixer track.
    pub hear: bool,
}

impl Default for VoicePrefs {
    fn default() -> Self {
        let c = VoiceConfig::default();
        Self {
            snap_scale: true,
            retune: c.retune,
            transpose: 0,
            velocity: c.velocity,
            low: c.low,
            high: c.high,
            mode: c.mode,
            gate_db: c.gate_db,
            hear: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TrailPoint {
    t: f64,
    /// Voice pitch, transposed (NaN: not pitched).
    pitch: f32,
    clarity: f32,
}

/// Everything about the voice on the interface side.
#[derive(Default)]
pub struct VoiceState {
    pub enabled: bool,
    pub panel: bool,
    pub panel_anchor: (f32, f32),
    panel_rect: Option<Rect>,
    pub prefs: VoicePrefs,
    /// Last settings sent to the engine.
    sent: Option<VoiceSettings>,
    /// Mixer track where the dry voice is heard.
    monitored: Option<usize>,
    inbox: Vec<VoiceReport>,
    trail: VecDeque<TrailPoint>,
    /// Notes played: key, start and end on the trail's clock.
    notes: VecDeque<(u8, f64, Option<f64>)>,
    clock: f64,
    /// Channel and key playing now.
    pub sounding: Option<(usize, u8)>,
    /// Last analysis: pitch (transposed, NaN when unpitched), clarity, level.
    pitch: f32,
    clarity: f32,
    level_db: f32,
    needle: f32,
    center: f32,
    /// Last reaction time measured (attack to note), in ms.
    pub reaction_ms: f32,
}

impl VoiceState {
    pub fn needs_redraw(&self) -> bool {
        self.enabled
    }

    /// Fractional pitch of the voice now (transposed), if it is pitched.
    pub fn live_pitch(&self) -> Option<f32> {
        (self.enabled && self.pitch.is_finite()).then_some(self.pitch)
    }
}

impl App {
    /// Settings the engine should have now.
    fn voice_settings(&self) -> VoiceSettings {
        let p = self.voice.prefs;
        let scale = if p.snap_scale && self.project.scale != ScaleKind::Chromatic { self.project.scale_mask() } else { 0xFFF };
        let channel = self.voice_channel().unwrap_or(0);
        let config = VoiceConfig {
            low: p.low,
            high: p.high,
            gate_db: p.gate_db,
            scale,
            transpose: p.transpose,
            retune: p.retune,
            velocity: p.velocity,
            mode: p.mode,
            ..VoiceConfig::default()
        };
        VoiceSettings { enabled: self.voice.enabled && !self.project.channels.is_empty(), channel, source: self.rec.settings.source, config }
    }

    /// The channel the voice plays: the selected one, else the first.
    pub fn voice_channel(&self) -> Option<usize> {
        self.selected_channel().or_else(|| (!self.project.channels.is_empty()).then_some(0))
    }

    pub fn toggle_voice(&mut self) {
        self.voice.enabled = !self.voice.enabled;
        if self.voice.enabled {
            self.open_voice_panel();
            let name = self.voice_channel().map_or_else(String::new, |c| self.project.channels[c].name.clone());
            self.toast(&tr_fmt!("Voice to notes: sing or hum to play {}", name));
        } else {
            self.voice.panel = false;
            self.toast(tr("Voice to notes off"));
        }
    }

    pub fn open_voice_panel(&mut self) {
        self.voice.panel = true;
        self.rec.panel = false;
    }

    /// Every image: settings to the engine, dry monitoring, the engine's reports.
    pub(super) fn voice_tick(&mut self) {
        let want = self.voice_settings();
        if self.voice.sent != Some(want) {
            self.send(Command::SetVoice(want));
            self.voice.sent = Some(want);
        }
        let open = self.rec.input.is_some() || self.rec.demo;
        let monitor = (want.enabled && self.voice.prefs.hear && open).then(|| self.project.channels[want.channel].track);
        if monitor != self.voice.monitored {
            if let Some(old) = self.voice.monitored.take() {
                self.send(Command::SetMonitor { track: old, source: None });
                // An armed track keeps its own monitoring.
                self.update_monitors();
            }
            if let Some(track) = monitor {
                self.send(Command::SetMonitor { track, source: Some(want.source) });
            }
            self.voice.monitored = monitor;
        }
        let mut inbox = std::mem::take(&mut self.voice.inbox);
        self.engine.drain_voice(|r| inbox.push(r));
        let hop = VoiceTracker::hop_seconds(self.sample_rate as f32) as f64;
        let transpose = f32::from(self.voice.prefs.transpose);
        for r in inbox.drain(..) {
            match r {
                VoiceReport::Frame(f) => {
                    let v = &mut self.voice;
                    v.clock += hop;
                    v.pitch = f.pitch + transpose;
                    v.clarity = f.clarity;
                    v.level_db = f.level_db;
                    v.trail.push_back(TrailPoint { t: v.clock, pitch: v.pitch, clarity: f.clarity });
                }
                VoiceReport::NoteOn { channel, key, velocity, tick, onset } => self.voice_note_on(channel, key, velocity, tick, onset),
                VoiceReport::NoteOff { channel, key, tick } => self.voice_note_off(channel, key, tick),
            }
        }
        self.voice.inbox = inbox;
        let v = &mut self.voice;
        while v.trail.front().is_some_and(|p| p.t < v.clock - TRAIL_KEEP) {
            v.trail.pop_front();
        }
        while v.notes.front().is_some_and(|n| n.2.is_some_and(|e| e < v.clock - TRAIL_KEEP)) {
            v.notes.pop_front();
        }
        if !v.enabled {
            v.pitch = f32::NAN;
        }
    }

    /// Delay of a voice note in ticks: the reaction (`onset` samples), the
    /// driver's round trip, the mixer's compensation and the manual offset,
    /// like an audio take: the note lands where it was sung.
    fn voice_shift_ticks(&self, onset: u32) -> f64 {
        let sr = self.sample_rate.max(1) as f64;
        let frames = onset as f64 + self.rec.latency.round_trip() as f64 + self.engine.latency() as f64;
        let secs = frames / sr + self.rec.settings.offset_ms as f64 / 1000.0;
        secs * self.tempo_here() as f64 / 60.0 * PPQ as f64
    }

    fn voice_note_on(&mut self, channel: usize, key: u8, vel: f32, tick: f64, onset: u32) {
        let code = VOICE_CODE + u16::from(key);
        if self.held.iter().any(|h| h.code == code) {
            self.voice_note_off(channel, key, tick);
        }
        let shift = self.voice_shift_ticks(onset);
        let recording = self.record && self.playing && self.counting <= 0.0 && matches!(self.project.mode, PlayMode::Pattern(_)) && tick.is_finite();
        let start = recording.then(|| self.cycle_of(tick - shift));
        self.held.push(HeldNote { code, channel, key, vel, start });
        let sr = self.sample_rate.max(1) as f64;
        let now = self.ctl.now() - onset as f64 / sr;
        self.note_log.note_on(channel, key, vel, now, tick.is_finite().then_some(tick - shift));
        if onset > 0 {
            self.voice.reaction_ms = (onset as f64 / sr * 1000.0) as f32;
        }
        let v = &mut self.voice;
        v.sounding = Some((channel, key));
        v.notes.push_back((key, v.clock, None));
    }

    fn voice_note_off(&mut self, channel: usize, key: u8, tick: f64) {
        let v = &mut self.voice;
        if let Some(n) = v.notes.iter_mut().rev().find(|n| n.0 == key && n.2.is_none()) {
            n.2 = Some(v.clock);
        }
        if v.sounding == Some((channel, key)) {
            v.sounding = None;
        }
        let end = tick.is_finite().then(|| self.cycle_of(tick - self.voice_shift_ticks(0)));
        self.end_held(VOICE_CODE + u16::from(key), end);
    }

    /// Vrai si le panneau est sous la souris (il la capte).
    pub(super) fn voice_covers(&self, input: &Input) -> bool {
        self.voice.panel && input.inside && self.voice.panel_rect.is_some_and(|r| r.contains(input.x, input.y))
    }

    /// The Voice toggle of the top bar: click to sing notes, right-click for
    /// the panel. A dot under it lights while a note plays.
    pub(super) fn voice_button(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        self.voice.panel_anchor = (r.x + r.w * 0.5, r.bottom() + 6.0);
        let id = wid("tb-voice", 0, 0);
        let tip = tr("Voice to notes: sing or hum to play the selected instrument\nRight-click: options");
        if self.icon_button(c, input, id, r, Icon::Voice, self.voice.enabled, (tip, "")) {
            self.toggle_voice();
        }
        if self.hovered(input, id, r) && input.right_pressed {
            if self.voice.panel {
                self.voice.panel = false;
            } else {
                self.open_voice_panel();
            }
        }
        if self.voice.enabled {
            let lit = self.anim.to(id ^ 0x5349, self.voice.sounding.is_some() as u8 as f32, T_PRESS);
            let (cx, by) = (r.x + r.w * 0.5, r.bottom() - 4.0);
            c.fill_circle(cx, by, 2.0 + lit, t.muted.mix(t.accent, lit.max(0.35)));
            if lit > 0.01 {
                c.fill_circle(cx, by, 5.0, t.accent.alpha(0.2 * lit));
            }
        }
    }

    /// The Voice panel.
    pub(super) fn voice_panel(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        if !self.voice.panel {
            self.voice.panel_rect = None;
            return;
        }
        let t = self.theme;
        let (ax, ay) = self.voice.panel_anchor;
        let x = (ax - PANEL_W * 0.5).clamp(8.0, (screen.w - PANEL_W - 8.0).max(8.0));
        let y = ay.clamp(TOP_BAR, (screen.h - PANEL_H - 8.0).max(TOP_BAR));
        let r = Rect::new(x, y, PANEL_W, PANEL_H);
        self.voice.panel_rect = Some(r);
        let appear = self.anim.to(wid("voice-panel", 0, 0), 1.0, T_LAYOUT);
        for k in 1..=4 {
            let k = k as f32;
            c.fill_rrect(r.inset(-2.0 * k).translate(0.0, 2.0 * k), 16.0 + 2.0 * k, t.shadow.alpha(0.07 * appear * t.shadow_strength));
        }
        c.fill_rrect_vgrad(r, 16.0, t.panel.mix(t.raised, 0.35), t.panel);
        c.stroke_rrect(r, 16.0, 1.0, t.hairline.mix(t.accent.alpha(0.25), 0.4));
        let inner = r.inset(16.0);

        // Header: icon, title, what it plays, on/off, close.
        let hy = inner.y + 14.0;
        let on = self.voice.enabled;
        let onv = self.anim.to(wid("voice-on", 0, 0), on as u8 as f32, T_HOVER);
        c.fill_circle(inner.x + 12.0, hy, 14.0, t.raised.mix(t.accent_deep, onv * 0.6));
        draw_icon(c, Icon::Voice, inner.x + 12.0, hy, 16.0, t.text2.mix(t.accent, onv));
        self.text(c, self.f.sans_med, 16.0, inner.x + 34.0, hy - 1.0, tr("Voice to Notes"), t.text);
        let close = Rect::new(inner.right() - 28.0, hy - 14.0, 28.0, 28.0);
        if self.icon_button(c, input, wid("voice-close", 0, 0), close, Icon::Close, false, (tr("Close"), "")) {
            self.voice.panel = false;
        }
        let power = Rect::new(close.x - 66.0, hy - 14.0, 60.0, 28.0);
        if self.chip(c, input, wid("voice-power", 0, 0), power, if on { tr("On") } else { tr("Off") }, on, tr("Voice to notes on or off")) {
            self.toggle_voice();
            self.voice.panel = true;
        }
        let target = self.voice_channel().map_or_else(String::new, |ch| self.project.channels[ch].name.clone());
        let status = if on { tr_fmt!("Plays {}", target) } else { tr("Off").to_string() };
        let tw = self.fonts.measure(self.f.sans_med, 16.0, tr("Voice to Notes"));
        self.text_fit(c, self.f.sans, 12.0, inner.x + 42.0 + tw, hy, &status, power.x - inner.x - 48.0 - tw, if on { t.accent } else { t.muted });

        let mut y = inner.y + 40.0;
        self.voice_meter(c, Rect::new(inner.x, y, inner.w, 100.0));
        y += 112.0;
        self.voice_trail(c, Rect::new(inner.x, y, inner.w, 150.0));
        y += 162.0;

        // Snap and mode.
        let p = self.voice.prefs;
        let chromatic = self.project.scale == ScaleKind::Chromatic;
        self.text(c, self.f.sans, 11.0, inner.x, y + 6.0, tr("Snap"), t.muted);
        self.text(c, self.f.sans, 11.0, inner.x + 178.0, y + 6.0, tr("Notes"), t.muted);
        y += 16.0;
        let scale_label = if chromatic { tr("Scale").to_string() } else { super::app::scale_label(self.project.key, self.project.scale) };
        let snaps = [scale_label.as_str(), tr("Chromatic")];
        let scale_tip = if chromatic { tr("Snap to the project scale\nPick a scale in the top bar first") } else { tr("Snap to the project scale\nOnly its notes are played") };
        let stips: [(&str, &str); 2] = [(scale_tip, ""), (tr("Every semitone"), "")];
        let sel = if p.snap_scale && !chromatic { 0 } else { 1 };
        if let Some(i) = self.segmented_tips(c, input, wid("voice-snap", 0, 0), Rect::new(inner.x, y, 170.0, 32.0), &snaps, &stips, sel) {
            self.voice.prefs.snap_scale = i == 0;
            if i == 0 && chromatic {
                self.toast(tr("Pick a project scale in the top bar to snap to it"));
            }
        }
        let modes = [tr("Normal"), tr("Legato"), tr("Hold")];
        let mtips: [(&str, &str); 3] = [
            (tr("A note per sung note\nNew pitches and new attacks retrigger"), ""),
            (tr("Notes overlap when the pitch moves\nSynths with portamento glide"), ""),
            (tr("The last note keeps sounding through silences"), ""),
        ];
        let msel = VoiceMode::ALL.iter().position(|&m| m == p.mode).unwrap_or(0);
        if let Some(i) = self.segmented_tips(c, input, wid("voice-mode", 0, 0), Rect::new(inner.x + 178.0, y, inner.w - 178.0, 32.0), &modes, &mtips, msel) {
            self.voice.prefs.mode = VoiceMode::ALL[i];
        }
        y += 42.0;

        // Retune, velocity, gate.
        let fw = (inner.w - 16.0) / 3.0;
        let labels = [tr("Retune"), tr("Velocity"), tr("Gate")];
        for (k, l) in labels.iter().enumerate() {
            self.text(c, self.f.sans, 11.0, inner.x + k as f32 * (fw + 8.0), y + 6.0, l, t.muted);
        }
        y += 16.0;
        let rr = Rect::new(inner.x, y, fw, 40.0);
        let text = format!("{}", (p.retune * 100.0).round() as i32);
        if let Some(v) = self.drag_field(c, input, wid("voice-retune", 0, 0), rr, p.retune * 100.0, 0.5, 5.0, &text, "%") {
            self.voice.prefs.retune = (v.round() / 100.0).clamp(0.0, 1.0);
        }
        self.tip_zone(input, rr, tr("Retune\n0 %: hard snap to the note (robotic)\n100 %: the note follows your bends and vibrato"), "");
        let vr = Rect::new(rr.right() + 8.0, y, fw, 40.0);
        let text = format!("{}", (p.velocity * 100.0).round() as i32);
        if let Some(v) = self.drag_field(c, input, wid("voice-vel", 0, 0), vr, p.velocity * 100.0, 0.5, 5.0, &text, "%") {
            self.voice.prefs.velocity = (v.round() / 100.0).clamp(0.0, 1.0);
        }
        self.tip_zone(input, vr, tr("Velocity sensitivity\nHow much your loudness sets the velocity"), "");
        let gr = Rect::new(vr.right() + 8.0, y, fw, 40.0);
        let text = format!("{}", p.gate_db.round() as i32);
        if let Some(v) = self.drag_field(c, input, wid("voice-gate", 0, 0), gr, p.gate_db, 0.2, 1.0, &text, "dB") {
            self.voice.prefs.gate_db = v.round().clamp(-80.0, -20.0);
        }
        self.tip_zone(input, gr, tr("Gate\nQuieter than this is ignored (room noise, breath)"), "");
        y += 50.0;

        // Transpose with octave buttons, and the voice's range.
        self.text(c, self.f.sans, 11.0, inner.x, y + 6.0, tr("Transpose"), t.muted);
        let range_x = inner.x + 172.0;
        self.text(c, self.f.sans, 11.0, range_x, y + 6.0, tr("Voice range"), t.muted);
        y += 16.0;
        let tr_r = Rect::new(inner.x, y, 100.0, 40.0);
        let text = format!("{:+}", p.transpose);
        if let Some(v) = self.drag_field(c, input, wid("voice-transpose", 0, 0), tr_r, f32::from(p.transpose), 0.08, 1.0, &text, "st") {
            self.voice.prefs.transpose = v.round().clamp(-24.0, 24.0) as i8;
        }
        if input.double && tr_r.contains(input.x, input.y) {
            self.voice.prefs.transpose = 0;
        }
        self.tip_zone(input, tr_r, tr("Transpose the notes, in semitones\nDouble-click: 0"), "");
        for (k, (label, d)) in [("-12", -12i8), ("+12", 12)].into_iter().enumerate() {
            let b = Rect::new(tr_r.right() + 4.0 + k as f32 * 32.0, y, 28.0, 40.0);
            let id = wid("voice-oct", k, 0);
            let hov = self.hovered(input, id, b);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(b, 8.0, t.raised.mix(t.raised_hi, hv));
            c.stroke_rrect(b, 8.0, 1.0, t.hairline);
            let (bx, by) = b.center();
            self.text_center(c, self.f.mono_med, 10.0, bx, by, label, t.text2.mix(t.text, hv));
            if hov {
                self.set_tip(if d < 0 { tr("An octave down") } else { tr("An octave up") }, "");
                if input.pressed {
                    self.voice.prefs.transpose = (p.transpose + d).clamp(-24, 24);
                }
            }
        }
        let rw = (inner.right() - range_x - 6.0) * 0.5;
        let lo_r = Rect::new(range_x, y, rw, 40.0);
        if let Some(v) = self.drag_field(c, input, wid("voice-low", 0, 0), lo_r, f32::from(p.low), 0.1, 1.0, &note_name(p.low), "") {
            self.voice.prefs.low = (v.round() as i32).clamp(i32::from(ponzik_dsp::voice::LOWEST), i32::from(p.high) - 5) as u8;
        }
        self.tip_zone(input, lo_r, tr("Lowest note of your voice\nHigher reacts faster (shorter analysis)"), "");
        let hi_r = Rect::new(lo_r.right() + 6.0, y, rw, 40.0);
        if let Some(v) = self.drag_field(c, input, wid("voice-high", 0, 0), hi_r, f32::from(p.high), 0.1, 1.0, &note_name(p.high), "") {
            self.voice.prefs.high = (v.round() as i32).clamp(i32::from(p.low) + 5, i32::from(ponzik_dsp::voice::HIGHEST)) as u8;
        }
        self.tip_zone(input, hi_r, tr("Highest note of your voice"), "");
        y += 50.0;

        // Dry voice and input.
        let half = (inner.w - 8.0) * 0.5;
        if self.chip(c, input, wid("voice-hear", 0, 0), Rect::new(inner.x, y, half, 34.0), tr("Hear voice"), p.hear, tr("Hear your dry voice on the instrument's mixer track\nUse headphones to avoid feedback")) {
            self.voice.prefs.hear = !p.hear;
        }
        let in_r = Rect::new(inner.x + half + 8.0, y, half, 34.0);
        let iid = wid("voice-input", 0, 0);
        let ihov = self.hovered(input, iid, in_r);
        let ihv = self.hover_anim(iid, ihov);
        c.fill_rrect(in_r, 17.0, t.raised.mix(t.raised_hi, ihv * 0.6));
        c.stroke_rrect(in_r, 17.0, 1.0, t.hairline);
        let (_, icy) = in_r.center();
        draw_icon(c, Icon::Mic, in_r.x + 15.0, icy, 14.0, if self.rec.input.is_some() { t.accent } else { t.muted });
        let device = self.rec.input.as_ref().map_or_else(|| tr("No input").to_string(), |(n, _)| n.clone());
        let source = match self.rec.settings.source {
            InputSource::Stereo => tr("Stereo"),
            InputSource::Left => tr("In 1"),
            InputSource::Right => tr("In 2"),
        };
        self.text_fit(c, self.f.sans_med, 12.0, in_r.x + 28.0, icy, &format!("{device} · {source}"), in_r.w - 46.0, t.text2.mix(t.text, ihv));
        draw_icon(c, Icon::ChevronRight, in_r.right() - 14.0, icy, 12.0, t.muted);
        if ihov {
            self.set_tip(tr("Microphone, input channel and level\nOpens the recording panel"), "");
            if input.pressed {
                let anchor = self.voice.panel_anchor;
                self.voice.panel = false;
                self.open_record_panel(Some(anchor));
            }
        }
        y += 44.0;

        // Latency and what happens to the notes.
        let audio = Latency::ms(self.rec.latency.round_trip(), self.sample_rate.max(1));
        let react = self.voice.reaction_ms;
        let line = if react > 0.0 {
            tr_fmt!("Reaction {} ms + audio {} ms = {} ms from voice to sound", react.round() as i32, audio.round() as i32, (react + audio).round() as i32)
        } else {
            tr_fmt!("Reaction about {} ms + audio {} ms", self.voice_expected_ms().round() as i32, audio.round() as i32)
        };
        self.text_fit(c, self.f.sans, 11.0, inner.x, y + 6.0, &line, inner.w, t.text2);
        let recording = self.record && matches!(self.project.mode, PlayMode::Pattern(_));
        let hint = if recording {
            tr_fmt!("Recording: played notes go into \"{}\"", self.project.patterns[self.project.current_pattern].name)
        } else {
            tr("Record in pattern mode to write the notes · Ctrl+Shift+C: capture").to_string()
        };
        self.text_fit(c, self.f.sans, 11.0, inner.x, y + 22.0, &hint, inner.w, if recording { t.negative.mix(t.text, 0.3) } else { t.muted });
    }

    /// Expected reaction from the analysis window (before any note was measured).
    fn voice_expected_ms(&self) -> f32 {
        let low = ponzik_dsp::util::midi_to_hz(f32::from(self.voice.prefs.low) - 1.0);
        // About a period and a half of the lowest note, plus two hops.
        1500.0 / low + 5.0
    }

    /// Live pitch card: note, cents needle, frequency, clarity and level.
    fn voice_meter(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 12.0, t.bg_deep);
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
        let pitch = self.voice.live_pitch();
        let vis = self.anim.to(wid("voice-vis", 0, 0), pitch.is_some() as u8 as f32, T_FLASH);
        let (cy, x0) = (r.y + r.h * 0.5, r.x + 16.0);
        // Note name, big.
        let (name, cents, hz) = match pitch {
            Some(p) => {
                let n = p.round();
                (note_name(n.clamp(0.0, 127.0) as u8), (p - n) * 100.0, ponzik_dsp::util::midi_to_hz(p - f32::from(self.voice.prefs.transpose)))
            }
            None => ("--".to_string(), 0.0, 0.0),
        };
        let sounding = self.voice.sounding.is_some();
        let col = t.muted.mix(if sounding { t.accent } else { t.text }, vis);
        self.text(c, self.f.mono_med, 34.0, x0, cy - 12.0, &name, col);
        let sub = if pitch.is_some() { format!("{} Hz · {:+} ¢", ponzik_i18n::num(hz as f64, 1), cents.round() as i32) } else { tr("Sing or hum").to_string() };
        self.text(c, self.f.sans, 11.0, x0, cy + 18.0, &sub, t.text2);
        if let Some((_, key)) = self.voice.sounding {
            let played = tr_fmt!("Plays {}", note_name(key));
            self.text(c, self.f.sans_med, 11.0, x0, cy + 34.0, &played, t.accent);
        }

        // Cents needle on an arc: in tune within ±10 cents.
        let (gx, gy, gr) = (r.x + 196.0, r.bottom() - 16.0, 52.0);
        let target = if pitch.is_some() { cents.clamp(-50.0, 50.0) } else { 0.0 };
        self.voice.needle += (target - self.voice.needle) * 0.35;
        let angle = |cents: f32| -std::f32::consts::FRAC_PI_2 + cents / 50.0 * 1.15;
        let a0 = angle(-50.0);
        let a1 = angle(50.0);
        c.stroke_arc(gx, gy, gr, a0, a1, 5.0, t.raised_hi);
        c.stroke_arc(gx, gy, gr, angle(-10.0), angle(10.0), 5.0, t.accent.alpha(0.7));
        for k in [-50.0f32, -25.0, 0.0, 25.0, 50.0] {
            let a = angle(k);
            let (dx, dy) = (a.cos(), a.sin());
            c.stroke_polyline(&[(gx + dx * (gr + 6.0), gy + dy * (gr + 6.0)), (gx + dx * (gr + 11.0), gy + dy * (gr + 11.0))], 1.2, t.muted.alpha(0.7));
        }
        self.text_center(c, self.f.mono, 9.0, gx + angle(-50.0).cos() * gr, gy + 8.0, "-50", t.muted);
        self.text_center(c, self.f.mono, 9.0, gx + angle(50.0).cos() * gr, gy + 8.0, "+50", t.muted);
        self.text_center(c, self.f.mono, 9.0, gx, gy - gr - 16.0, "0", t.muted);
        let a = angle(self.voice.needle);
        let in_tune = self.voice.needle.abs() <= 10.0 && pitch.is_some();
        let ncol = t.muted.mix(if in_tune { t.accent } else { t.text }, vis);
        c.stroke_polyline(&[(gx, gy), (gx + a.cos() * (gr - 4.0), gy + a.sin() * (gr - 4.0))], 2.0, ncol);
        c.fill_circle(gx, gy, 4.0, ncol);

        // Clarity and level, with the gate.
        let bx = r.right() - 84.0;
        let bw = 68.0;
        let bars = [
            (tr("Confidence"), if pitch.is_some() { self.voice.clarity } else { 0.0 }, None),
            (tr("Level"), ((self.voice.level_db + 70.0) / 70.0).clamp(0.0, 1.0), Some(((self.voice.prefs.gate_db + 70.0) / 70.0).clamp(0.0, 1.0))),
        ];
        for (k, (label, v, mark)) in bars.into_iter().enumerate() {
            let by = r.y + 26.0 + k as f32 * 36.0;
            self.text(c, self.f.sans, 10.0, bx, by - 8.0, label, t.muted);
            let bar = Rect::new(bx, by, bw, 5.0);
            c.fill_rrect(bar, 2.5, t.well);
            if v > 0.0 {
                c.fill_rrect(Rect::new(bx, by, bw * v, 5.0), 2.5, t.accent.alpha(0.5 + 0.5 * vis));
            }
            if let Some(m) = mark {
                c.fill_rect(Rect::new(bx + bw * m - 0.5, by - 3.0, 1.5, 11.0), t.text2);
            }
        }
    }

    /// Scrolling pitch trail over the notes played, like a little piano roll.
    fn voice_trail(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 12.0, t.bg_deep);
        let prev = c.push_clip(r.inset(1.0));
        let labels_w = 30.0;
        let area = Rect::new(r.x + labels_w, r.y + 6.0, r.w - labels_w - 8.0, r.h - 12.0);
        // Follows the voice gently (the played note first).
        let target = self.voice.sounding.map(|(_, k)| f32::from(k)).or(self.voice.live_pitch());
        if self.voice.center == 0.0 {
            self.voice.center = target.unwrap_or(57.0);
        }
        if let Some(tg) = target {
            self.voice.center += (tg - self.voice.center) * 0.06;
        }
        let center = self.voice.center;
        let row = area.h / TRAIL_SPAN;
        let y_of = |p: f32| area.y + area.h * 0.5 - (p - center) * row;
        let clock = self.voice.clock;
        let x_of = |s: f64| area.right() - ((clock - s) / TRAIL_SECONDS) as f32 * area.w;
        let snap = self.voice.prefs.snap_scale && self.project.scale != ScaleKind::Chromatic;
        let lo = (center - TRAIL_SPAN * 0.5 - 1.0).floor() as i32;
        let hi = (center + TRAIL_SPAN * 0.5 + 1.0).ceil() as i32;
        for k in lo.max(0)..=hi.min(127) {
            let key = k as u8;
            let y = y_of(k as f32) - row * 0.5;
            let band = Rect::new(area.x, y, area.w, row);
            if matches!(key % 12, 1 | 3 | 6 | 8 | 10) {
                c.fill_rect(band, t.well.alpha(0.5));
            }
            if snap && self.project.in_scale(key) {
                c.fill_rect(band, t.accent.alpha(0.05));
            }
            if key % 12 == 0 {
                c.fill_rect(Rect::new(area.x, y + row, area.w, 1.0), t.hairline);
                self.text_right(c, self.f.mono, 9.0, area.x - 4.0, y + row * 0.5, &note_name(key), t.muted);
            }
        }
        // Seconds.
        for s in 1..TRAIL_SECONDS as i32 {
            let x = area.right() - s as f32 / TRAIL_SECONDS as f32 * area.w;
            c.fill_rect(Rect::new(x, area.y, 1.0, area.h), t.hairline.alpha(0.5));
        }
        // Notes played.
        for &(key, start, end) in &self.voice.notes {
            let (xa, xb) = (x_of(start).max(area.x), x_of(end.unwrap_or(clock)));
            if xb < area.x {
                continue;
            }
            let nr = Rect::new(xa, y_of(f32::from(key)) - row * 0.5 + 1.0, (xb - xa).max(3.0), (row - 2.0).max(2.0));
            let live = end.is_none();
            c.fill_rrect(nr, 3.0, t.accent.alpha(if live { 0.7 } else { 0.45 }));
            c.stroke_rrect(nr, 3.0, 1.0, t.accent.alpha(0.9));
        }
        // The voice's pitch, broken where it is not pitched.
        let mut line: Vec<(f32, f32)> = Vec::with_capacity(64);
        let mut alpha = 0.0f32;
        let draw = |c: &mut Canvas, line: &mut Vec<(f32, f32)>, alpha: f32| {
            if line.len() >= 2 {
                c.stroke_polyline(line, 2.0, t.text.alpha(0.35 + 0.6 * alpha));
            }
            line.clear();
        };
        for p in &self.voice.trail {
            let x = x_of(p.t);
            if x < area.x - 2.0 {
                continue;
            }
            if !p.pitch.is_finite() {
                draw(c, &mut line, alpha);
                continue;
            }
            if line.is_empty() {
                alpha = p.clarity;
            }
            alpha = alpha.min(p.clarity);
            line.push((x, y_of(p.pitch)));
        }
        draw(c, &mut line, alpha);
        // Now: a dot at the voice's pitch.
        if let Some(pv) = self.voice.live_pitch() {
            let y = y_of(pv);
            c.fill_circle(area.right(), y, 7.0, t.accent.alpha(0.2));
            c.fill_circle(area.right(), y, 3.5, t.accent);
        }
        c.set_clip(Some(prev));
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
        if self.voice.trail.is_empty() {
            let (cx, cy) = r.center();
            let msg = if self.voice.enabled { tr("Waiting for your voice…") } else { tr("Turn the voice on to see your pitch") };
            self.text_center(c, self.f.sans, 12.0, cx, cy, msg, t.muted);
        }
    }

    /// Right-click menu of an audio clip: its monophonic line into a new
    /// pattern on the selected channel, placed under the clip.
    pub(super) fn audio_clip_to_notes(&mut self, i: usize) {
        let Some(clip) = self.project.audio_clips.get(i).cloned() else { return };
        let Some(sample) = self.samples.get(&clip.path).cloned() else {
            self.toast(tr("The clip's file is not loaded"));
            return;
        };
        let Some(ch) = self.voice_channel() else {
            self.toast(tr("Add a channel to play the notes first"));
            return;
        };
        let mut config = self.voice_settings().config;
        config.transpose = 0;
        let notes = ponzik_project::voice::notes_from_audio(&clip, &sample, &self.project.tempo_map(), &config);
        if notes.is_empty() {
            self.toast(tr("No pitched notes found in the clip"));
            return;
        }
        let name = std::path::Path::new(&clip.path).file_stem().map_or_else(|| tr("Audio").to_string(), |s| s.to_string_lossy().into_owned());
        let p = self.project.add_pattern();
        self.project.patterns[p].name = tr_fmt!("{} notes", name);
        self.project.patterns[p].len = clip.len.div_ceil(TICKS_PER_BAR).max(1) * TICKS_PER_BAR;
        let count = notes.len();
        for n in notes {
            self.project.add_note(p, ch, n);
        }
        // Under the clip, on a free lane: the notes play along with it.
        let lane = self.project.clips.iter().map(|c| c.lane).chain(self.project.audio_clips.iter().map(|c| c.lane)).max().map_or(0, |l| l + 1);
        self.project.clips.push(ponzik_project::ClipDef { pattern: p, lane, start: clip.start, len: clip.len, offset: 0, muted: false });
        self.project.current_pattern = p;
        self.playlist.brush = p;
        self.break_undo_merge();
        self.commit(tr("Audio to notes"));
        let text = tr_fmt!("{} on {}", tr_fmt_n!("{} note", "{} notes", count, count), self.project.channels[ch].name.clone());
        self.toast_action(&text, tr("Undo"), Action::Undo);
    }

    /// `PONZIK_VOICE=1|piano|chord|clip|clipmenu`: a simulated singer on the
    /// input. `1`: the panel open; `piano`: recording into the piano roll,
    /// keys lit; `chord`: a chord effect on the channel (one hummed note, a
    /// chord); `clip`: a sung audio clip converted to a pattern under it;
    /// `clipmenu`: that clip's menu.
    pub fn screenshot_voice(&mut self, mode: &str) {
        if mode.starts_with("clip") {
            self.screenshot_voice_clip(mode == "clipmenu");
            return;
        }
        self.rec.demo = true;
        self.rec.input = Some(("Scarlett 2i2 USB".into(), 2));
        self.rec.latency = Latency { output: 288, input: 192 };
        self.rec.settings.source = InputSource::Left;
        if self.project.scale == ScaleKind::Chromatic {
            self.project.key = 9;
            self.project.scale = ScaleKind::Minor;
        }
        // The bass of the demo (or the channel asked for), not the kick.
        let asked = std::env::var_os("PONZIK_CHANNEL").is_some();
        let ch = if asked { self.selected_channel().unwrap_or(0) } else { 4.min(self.project.channels.len().saturating_sub(1)) };
        self.focus = super::app::Focus::Channel(ch);
        self.voice.enabled = true;
        self.voice.prefs.retune = 0.35;
        if mode.contains("chord") {
            self.add_note_fx(ch, 0, "chord");
        }
        if mode.contains("piano") {
            let pat = self.project.current_pattern;
            if let Some(notes) = self.project.patterns[pat].notes.get_mut(ch) {
                notes.clear();
            }
            // Four bars to sing into, the view framed on the voice.
            self.project.patterns[pat].len = 4 * TICKS_PER_BAR;
            self.project.channels[ch].root_note = 62;
            self.piano.fitted_for = None;
            self.set_mode(PlayMode::Pattern(pat));
            self.set_view(super::app::View::Piano);
            self.record = true;
        } else {
            self.open_voice_panel();
        }
    }
}

impl App {
    /// A sung phrase as an audio clip in the playlist, then its notes.
    fn screenshot_voice_clip(&mut self, menu: bool) {
        let sr = 48_000.0f32;
        let mut singer = DemoSinger::new(sr);
        let left: Vec<f32> = (0..(sr * DemoSinger::seconds()) as usize).map(|_| singer.next_sample()).collect();
        let path = "Hummed melody.wav".to_string();
        let sample = ponzik_engine::Sample { name: path.clone(), sample_rate: sr, right: left.clone(), left, markers: Vec::new() };
        self.samples.insert(path.clone(), std::sync::Arc::new(sample));
        let lane = self.project.clips.iter().map(|c| c.lane + 1).max().unwrap_or(0);
        let bpm = self.tempo_here() as f64;
        let len = (DemoSinger::seconds() as f64 * bpm / 60.0 * PPQ as f64) as u32;
        let start = TICKS_PER_BAR;
        self.project.audio_clips.push(ponzik_project::AudioClipDef { path, lane, track: 3, start, len, color: 0x5ec8a8, ..Default::default() });
        let i = self.project.audio_clips.len() - 1;
        self.focus = super::app::Focus::Channel(4.min(self.project.channels.len().saturating_sub(1)));
        self.set_mode(PlayMode::Song);
        self.set_view(super::app::View::Playlist);
        if menu {
            self.playlist.selected = None;
            self.open_popup(super::app::Popup::AudioClipMenu(i), Rect::new(420.0, 150.0 + lane as f32 * 60.0, 0.0, 19.0));
        } else {
            self.audio_clip_to_notes(i);
        }
    }
}

/// A simulated singer for the screenshots: a phrase in A minor with
/// vibrato, a slide and breaths, on the input.
pub struct DemoSinger {
    at: usize,
    phase: f64,
    sample_rate: f64,
}

impl DemoSinger {
    /// (key at the start, key at the end, seconds); 0: a breath.
    const PHRASE: [(f64, f64, f64); 12] = [
        (57.0, 57.0, 0.34),
        (0.0, 0.0, 0.08),
        (60.0, 60.0, 0.3),
        (64.0, 64.0, 0.42),
        (0.0, 0.0, 0.1),
        (62.0, 67.0, 0.5),
        (65.0, 65.0, 0.3),
        (64.0, 64.0, 0.3),
        (0.0, 0.0, 0.08),
        (60.0, 60.0, 0.28),
        (62.0, 62.0, 0.28),
        (57.0, 57.0, 0.7),
    ];

    pub fn new(sample_rate: f32) -> Self {
        Self { at: 0, phase: 0.0, sample_rate: sample_rate as f64 }
    }

    /// Length of the phrase, with its final breath.
    pub fn seconds() -> f32 {
        (Self::PHRASE.iter().map(|p| p.2).sum::<f64>() + 0.4) as f32
    }

    pub fn next_sample(&mut self) -> f32 {
        let total = Self::seconds() as f64;
        let t = self.at as f64 / self.sample_rate % total;
        self.at += 1;
        let mut start = 0.0;
        for &(a, b, len) in &Self::PHRASE {
            if t < start + len {
                if a == 0.0 {
                    return 0.0;
                }
                let u = t - start;
                let glide = (u / 0.25).min(1.0);
                let key = a + (b - a) * glide * glide * (3.0 - 2.0 * glide);
                let midi = key + 0.35 * (TAU * 5.6 * u).sin() * (u / 0.15).min(1.0);
                self.phase += 440.0 * 2f64.powf((midi - 69.0) / 12.0) / self.sample_rate;
                let x: f64 = (1..=12).map(|h| (TAU * self.phase * h as f64).sin() / (h as f64).powf(1.15)).sum();
                let env = (u / 0.02).min(1.0) * ((len - u) / 0.03).clamp(0.0, 1.0);
                return (x * 0.07 * env) as f32;
            }
            start += len;
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    #[test]
    fn voice_notes_are_recorded_into_the_pattern_and_captured() {
        let mut app = app();
        let pat = app.project.current_pattern;
        app.set_mode(PlayMode::Pattern(pat));
        app.focus = super::super::app::Focus::Channel(4);
        app.project.patterns[pat].notes[4].clear();
        app.record = true;
        app.playing = true;
        // Played on beat 2 with a 10 ms reaction: the note lands at most a
        // step earlier (its start is quantized to the step).
        let beat = ponzik_engine::PPQ as f64;
        app.voice_note_on(4, 64, 0.8, beat, 480);
        app.voice_note_off(4, 64, 2.0 * beat);
        let notes = &app.project.patterns[pat].notes[4];
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].key, 64);
        let step = ponzik_engine::TICKS_PER_STEP;
        assert!(notes[0].start <= beat as u32 && notes[0].start + step >= beat as u32, "{}", notes[0].start);
        assert!(app.held.is_empty());
        // MIDI capture saw it too.
        assert_eq!(app.note_log.len(), 1);
        // Stopped: played, not written.
        app.playing = false;
        app.voice_note_on(4, 65, 0.8, f64::NAN, 0);
        app.voice_note_off(4, 65, f64::NAN);
        assert_eq!(app.project.patterns[pat].notes[4].len(), 1);
        assert_eq!(app.note_log.len(), 2);
    }

    #[test]
    fn settings_follow_the_project_scale_and_the_channel() {
        let mut app = app();
        app.voice.enabled = true;
        app.focus = super::super::app::Focus::Channel(4);
        app.project.key = 0;
        app.project.scale = ScaleKind::Major;
        let s = app.voice_settings();
        assert!(s.enabled && s.channel == 4);
        assert_eq!(s.config.scale, app.project.scale_mask());
        app.voice.prefs.snap_scale = false;
        assert_eq!(app.voice_settings().config.scale, 0xFFF);
        app.voice.prefs.snap_scale = true;
        app.project.scale = ScaleKind::Chromatic;
        assert_eq!(app.voice_settings().config.scale, 0xFFF);
        app.voice_tick();
        assert_eq!(app.voice.sent, Some(app.voice_settings()));
    }

    #[test]
    fn the_demo_singer_is_tracked() {
        let mut singer = DemoSinger::new(48_000.0);
        let x: Vec<f32> = (0..(48_000.0 * DemoSinger::seconds()) as usize).map(|_| singer.next_sample()).collect();
        let notes = ponzik_dsp::voice::transcribe(&x, 48_000.0, &VoiceConfig::default());
        assert!(notes.len() >= 9, "{notes:?}");
        assert_eq!(notes[0].key, 57);
    }
}
