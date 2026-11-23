//! Pitch correction card: a scrolling pitch graph (the voice, the corrected
//! pitch in accent, the lanes of the scale beside a small keyboard whose keys
//! remove or bypass a note), a tuner with its cents bar, a large retune speed
//! knob, quick modes and the settings in three tabs. Reads the pitch points
//! the effect publishes ([`AutoTuneShared`]) without locks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::Arc;

use ponzik_engine::effects::autotune::{self as at, AutoTuneShared, PitchPoint};
use ponzik_engine::{Command, ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::scale::key_name;
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Popup};
use super::devices::{Device, device_target};
use super::visuals::device_key;
use super::widgets::{hatch, wid};

const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GAP: f32 = 14.0;
const KEYS_W: f32 = 34.0;
const GRAPH_W: f32 = 430.0;
const TUNER_W: f32 = 150.0;
const CHIPS_H: f32 = 24.0;
/// Width of the card.
pub const WIDTH: f32 = 28.0 + KEYS_W + GRAPH_W + GAP + TUNER_W + GAP + 5.0 * CELL_W;
/// Seconds of pitch shown.
const SHOWN_SECS: f32 = 5.0;

/// Tabs of settings, each two rows of five (`None`: an empty cell).
const TABS: [(&str, [[Option<u32>; 5]; 2]); 3] = [
    (
        tr_noop("Correction"),
        [
            [Some(at::P_HUMANIZE), Some(at::P_FLEX), Some(at::P_NATURAL_VIBRATO), Some(at::P_TRANSITION), Some(at::P_TRANSPOSE)],
            [Some(at::P_KEY), Some(at::P_SCALE), Some(at::P_FOLLOW), Some(at::P_RANGE), Some(at::P_MODE)],
        ],
    ),
    (
        tr_noop("Voice"),
        [
            [Some(at::P_FORMANT), Some(at::P_FORMANT_SHIFT), Some(at::P_THROAT), Some(at::P_WIDTH), Some(at::P_MIX)],
            [Some(at::P_VIB_DEPTH), Some(at::P_VIB_RATE), Some(at::P_VIB_DELAY), Some(at::P_VIB_ONSET), None],
        ],
    ),
    (
        tr_noop("Input"),
        [
            [Some(at::P_LATENCY), Some(at::P_TRACKING), Some(at::P_REFERENCE), Some(at::P_MIDI_MODE), Some(at::P_MIDI_CHANNEL)],
            [None; 5],
        ],
    ),
];
const TAB_TIPS: [&str; 3] = [
    tr_noop("Correction\nHow hard and how fast the voice is tuned, to which notes"),
    tr_noop("Voice\nFormants, throat, created vibrato, width and mix"),
    tr_noop("Input\nLatency, tracking, A4 reference and MIDI target notes"),
];
/// Names of the quick modes ([`at::QUICK_MODES`]), for the translation.
const MODE_NAMES: [&str; 6] = [tr_noop("Default"), tr_noop("Hard Tune"), tr_noop("Natural"), tr_noop("Pop"), tr_noop("Rap / Trap"), tr_noop("Choir / Wide")];
const MODE_TIPS: [&str; 6] = [
    tr_noop("Default\nA quick, clean correction"),
    tr_noop("Hard Tune\nInstant retune: the robotic effect"),
    tr_noop("Natural\nSlow, humanized retune that keeps the expression"),
    tr_noop("Pop\nTight but smooth, for polished leads"),
    tr_noop("Rap / Trap\nClassic mode hard tune with quick note jumps"),
    tr_noop("Choir / Wide\nSoft retune, created vibrato, a wide stereo voice"),
];

/// What the interface keeps of a corrector.
#[derive(Default)]
struct View {
    shared: Option<Arc<AutoTuneShared>>,
    tab: usize,
}

thread_local! {
    static VIEWS: RefCell<HashMap<(usize, usize), View>> = RefCell::new(HashMap::new());
    static POINTS: RefCell<Vec<PitchPoint>> = RefCell::new(vec![PitchPoint::default(); at::HISTORY]);
}

fn with<R>(key: (usize, usize), f: impl FnOnce(&mut View) -> R) -> R {
    VIEWS.with(|m| f(m.borrow_mut().entry(key).or_default()))
}

/// Keeps the shared state of the correctors sent to the engine; called on every command sent.
pub fn capture(cmd: &Command) {
    let Command::SetInsert { track, slot, effect } = cmd else { return };
    let shared = effect.as_ref().and_then(|e| e.shared()).and_then(|s| s.downcast::<AutoTuneShared>().ok());
    VIEWS.with(|m| {
        let mut m = m.borrow_mut();
        match shared {
            Some(s) => m.entry((*track, *slot)).or_default().shared = Some(s),
            None => {
                m.remove(&(*track, *slot));
            }
        }
    });
}

/// Note name with its octave (C4 = 60).
fn note_label(n: u8) -> String {
    format!("{}{}", key_name(n % 12), i32::from(n) / 12 - 1)
}

/// State of a pitch class and its tooltip.
fn note_state_tip(pc: usize, state: usize, in_scale: bool) -> String {
    let name = key_name(pc as u8);
    let now = match state {
        at::NOTE_ON => tr("added to the scale"),
        at::NOTE_OFF => tr("removed: never a target"),
        at::NOTE_BYPASS => tr("bypassed: sung freely"),
        _ if in_scale => tr("in the scale"),
        _ => tr("not in the scale"),
    };
    tr_fmt!("{}: {}\nClick: next state (on, off, bypass) · right-click: back to the scale", name, now)
}

/// Next state on a click: a note of the scale goes off, then bypassed; a
/// note outside goes on, then bypassed; then back to the scale.
fn next_state(state: usize, in_scale: bool) -> usize {
    match (state, in_scale) {
        (at::NOTE_AUTO, true) => at::NOTE_OFF,
        (at::NOTE_AUTO, false) => at::NOTE_ON,
        (at::NOTE_OFF | at::NOTE_ON, _) => at::NOTE_BYPASS,
        _ => at::NOTE_AUTO,
    }
}

impl App {
    fn at_value(&self, device: Device, id: u32) -> f32 {
        self.param_value(device, &at::params()[id as usize])
    }

    /// Shared state of the corrector; when missing (project opened at start),
    /// the effect is made again with the same settings to get it.
    fn at_shared(&mut self, track: usize, slot: usize) -> Option<Arc<AutoTuneShared>> {
        if let Some(s) = with((track, slot), |v| v.shared.clone()) {
            return Some(s);
        }
        let def = self.project.tracks[track].inserts[slot].clone()?;
        let fx = ponzik_project::Project::make_effect(&def, self.sample_rate as f32, self.project.bpm)?;
        let cmd = Command::SetInsert { track, slot, effect: Some(fx) };
        capture(&cmd);
        self.send(cmd);
        with((track, slot), |v| v.shared.clone())
    }

    pub(super) fn autotune_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, device: Device) {
        let Device::Effect { track, slot } = device else { return };
        let shared = self.at_shared(track, slot);
        let fixed = TUNER_W + GAP + 5.0 * CELL_W + GAP;
        let graph_w = (body.w - KEYS_W - fixed).max(200.0);
        let graph = Rect::new(body.x + KEYS_W, body.y + 4.0, graph_w, body.h - CHIPS_H - 14.0);
        let keys = Rect::new(body.x, graph.y, KEYS_W - 4.0, graph.h);
        POINTS.with(|p| {
            let mut points = p.borrow_mut();
            let secs = shared.as_ref().map_or(0.004, |s| s.point_secs()).max(1e-3);
            let want = ((SHOWN_SECS / secs) as usize).min(points.len());
            let n = shared.as_ref().map_or(0, |s| s.recent(&mut points[..want]));
            self.at_graph(c, input, keys, graph, device, &points[..n], secs, shared.as_deref());
        });
        let chips = Rect::new(graph.x, graph.bottom() + 8.0, graph.w, CHIPS_H);
        self.at_modes(c, input, chips, device);
        let tuner = Rect::new(graph.right() + GAP, body.y + 4.0, TUNER_W, body.h - 8.0);
        let now = shared.as_ref().and_then(|s| {
            let mut one = [PitchPoint::default()];
            (s.recent(&mut one) == 1).then_some(one[0])
        });
        self.at_tuner(c, input, tuner, device, now, shared.as_deref());
        let knobs = Rect::new(tuner.right() + GAP, body.y, 5.0 * CELL_W, body.h);
        self.at_settings(c, input, knobs, device, track, slot);
    }

    /// The pitch graph and its keyboard.
    #[allow(clippy::too_many_arguments)]
    fn at_graph(&mut self, c: &mut Canvas, input: &Input, keys: Rect, g: Rect, device: Device, points: &[PitchPoint], secs: f32, shared: Option<&AutoTuneShared>) {
        let t = self.theme;
        let dk = device_key(device);
        c.fill_rrect(g, 10.0, t.well);
        // Rows shown: 11 to 24 semitones, centered on the recent voice.
        let span = (g.h / 13.0).round().clamp(9.0, 24.0);
        let recent = &points[points.len().saturating_sub((1.5 / secs) as usize)..];
        let (sum, count) = recent.iter().filter(|p| p.input.is_finite()).fold((0.0, 0.0), |(s, n), p| (s + p.input, n + 1.0));
        let range = at::RANGE_NOTES[self.at_value(device, at::P_RANGE).round() as usize % at::RANGE_NOTES.len()];
        let rest = (f32::from(range.0) + f32::from(range.1)) * 0.5;
        let target = if count > 0.0 { sum / count } else { rest };
        let center = self.anim.to(wid("at-center", dk, 0), target.round(), 0.25);
        let low = center - span * 0.5;
        let row_h = g.h / span;
        let y_of = |m: f32| g.bottom() - (m - low + 0.5) * row_h;

        let vals: [f32; at::PARAM_COUNT] = std::array::from_fn(|i| self.at_value(device, i as u32));
        let get = |id: u32| vals[id as usize];
        let scale = at::scale_mask(get(at::P_KEY).round() as usize % 12, get(at::P_SCALE).round() as usize);
        let (allowed, bypass) = at::note_masks(&get);
        let midi_mode = get(at::P_MIDI_MODE).round() as usize;
        let midi = shared.map(AutoTuneShared::midi).filter(|_| midi_mode > 0);
        let current = points.last().and_then(|p| p.note);

        let prev = c.push_clip(g);
        let first = low.floor() as i32;
        for n in first..=(low + span).ceil() as i32 {
            if !(0..128).contains(&n) {
                continue;
            }
            let pc = n.rem_euclid(12) as usize;
            let bit = 1u16 << pc;
            let y = y_of(n as f32);
            let lane = Rect::new(g.x, y - row_h * 0.5, g.w, row_h);
            let held = midi.is_some_and(|m| m.is_held(n as u8));
            if held {
                c.fill_rect(lane, t.accent.alpha(0.16));
            } else if bypass & bit != 0 {
                hatch(c, lane, 7.0, t.muted.alpha(0.18));
            } else if allowed & bit != 0 {
                c.fill_rect(lane, t.accent.alpha(if current == Some(n as u8) { 0.13 } else { 0.05 }));
            }
            if pc == 0 {
                c.fill_rect(Rect::new(g.x, lane.bottom() - 0.5, g.w, 1.0), t.hairline);
            }
        }
        // Seconds, from the right edge (now).
        let px_per_point = g.w / (SHOWN_SECS / secs);
        for s in 1..SHOWN_SECS as usize {
            let x = (g.right() - s as f32 / secs * px_per_point).round();
            c.fill_rect(Rect::new(x, g.y + 4.0, 1.0, g.h - 8.0), t.hairline.alpha(0.6));
        }
        // Lines: one point per pixel at most, broken where the voice stops.
        let draw = |c: &mut Canvas, pick: &dyn Fn(&PitchPoint) -> f32, width: f32, col: ponzik_ui::Color| {
            let mut line: Vec<(f32, f32)> = Vec::with_capacity(g.w as usize + 2);
            let mut last_x = f32::MIN;
            for (i, p) in points.iter().enumerate() {
                let x = g.right() - (points.len() - 1 - i) as f32 * px_per_point;
                let v = pick(p);
                if !v.is_finite() {
                    if line.len() > 1 {
                        c.stroke_polyline(&line, width, col);
                    }
                    line.clear();
                    last_x = f32::MIN;
                    continue;
                }
                if x - last_x < 1.0 && i + 1 < points.len() {
                    continue;
                }
                last_x = x;
                line.push((x, y_of(v)));
            }
            if line.len() > 1 {
                c.stroke_polyline(&line, width, col);
            }
        };
        draw(c, &|p| p.input, 1.2, t.text2.alpha(0.75));
        draw(c, &|p| p.output, 1.8, t.accent);
        if let Some(p) = points.last().filter(|p| p.output.is_finite()) {
            c.fill_circle(g.right() - 3.0, y_of(p.output), 3.0, t.accent);
        }
        c.set_clip(Some(prev));
        c.stroke_rrect(g, 10.0, 1.0, t.hairline);
        if let Some(sh) = shared {
            let ms = sh.latency() as f32 * 1000.0 / self.sample_rate.max(1) as f32;
            self.text_right(c, self.f.mono, 10.0, g.right() - 8.0, g.y + 10.0, &tr_fmt!("{} ms", ponzik_i18n::num(f64::from(ms), 1)), t.muted);
        }
        if points.iter().all(|p| !p.input.is_finite()) {
            self.text_center(c, self.f.sans, 12.0, g.x + g.w * 0.5, g.y + g.h * 0.5, tr("Sing or play: the pitch shows here"), t.muted);
        }
        let ghov = self.hovered(input, wid("at-graph", dk, 0), g);
        if ghov {
            self.set_tip(tr("Pitch of the last five seconds\nGray: the voice · green: the corrected pitch · bands: the notes of the scale"), "");
        }

        // Keyboard: a click changes the note of that pitch class.
        let prev = c.push_clip(keys);
        let mut changed: Option<(usize, usize)> = None;
        for n in first..=(low + span).ceil() as i32 {
            if !(0..128).contains(&n) {
                continue;
            }
            let pc = n.rem_euclid(12) as usize;
            let y = y_of(n as f32);
            let key = Rect::new(keys.x, y - row_h * 0.5 + 0.5, keys.w, row_h - 1.0);
            let black = matches!(pc, 1 | 3 | 6 | 8 | 10);
            let state = get(at::P_NOTE + pc as u32).round() as usize;
            let in_scale = scale & (1 << pc) != 0;
            let id = wid("at-key", dk, n as usize);
            let hov = self.hovered(input, id, key);
            let hv = self.hover_anim(id, hov);
            let base = if black { t.key_black } else { t.key_white.alpha(0.85) };
            let w = if black { keys.w * 0.62 } else { keys.w };
            let k = Rect::new(keys.x, key.y, w, key.h);
            c.fill_rrect(k, 3.0, base.mix(t.accent, hv * 0.25));
            let on = allowed & (1 << pc) != 0;
            let dot = Rect::new(keys.right() - 6.0, key.y + key.h * 0.5 - 2.0, 4.0, 4.0);
            match state {
                at::NOTE_BYPASS => c.stroke_rrect(dot.inset(-0.5), 2.0, 1.0, t.text2),
                _ if on => c.fill_rrect(dot, 2.0, t.accent),
                _ => c.fill_rrect(dot, 2.0, t.muted.alpha(0.35)),
            }
            if pc == 0 || current == Some(n as u8) {
                let col = if black { t.text } else { t.bg_deep };
                self.text(c, self.f.mono, 9.0, k.x + 2.0, key.y + key.h * 0.5, &note_label(n as u8), col);
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(&note_state_tip(pc, state, in_scale), "");
                if input.pressed {
                    changed = Some((pc, next_state(state, in_scale)));
                } else if input.right_pressed {
                    changed = Some((pc, at::NOTE_AUTO));
                }
            }
        }
        c.set_clip(Some(prev));
        if let Some((pc, state)) = changed {
            self.set_param_value(device, &at::params()[(at::P_NOTE + pc as u32) as usize], state as f32);
        }
    }

    /// Quick modes, as chips under the graph.
    fn at_modes(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device) {
        let t = self.theme;
        let dk = device_key(device);
        let p = at::params();
        let active = at::QUICK_MODES.iter().position(|(_, values)| {
            p.iter().filter(|q| !at::quick_mode_keeps(q.id)).all(|q| {
                let want = values.iter().find(|(id, _)| *id == q.id).map_or(q.default, |v| v.1);
                (self.param_value(device, q) - want).abs() < 1e-4
            })
        });
        let gap = 6.0;
        let w = (r.w - gap * 5.0) / 6.0;
        for (i, (_, values)) in at::QUICK_MODES.iter().enumerate() {
            let name = MODE_NAMES[i];
            let chip = Rect::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
            let id = wid("at-mode", dk, i);
            let hov = self.hovered(input, id, chip);
            let hv = self.hover_anim(id, hov);
            let on = active == Some(i);
            let fill = if on { t.accent.alpha(0.18) } else { t.raised.mix(t.raised_hi, hv) };
            c.fill_rrect(chip, chip.h * 0.5, fill);
            c.stroke_rrect(chip, chip.h * 0.5, 1.0, if on { t.accent.alpha(0.7) } else { t.hairline });
            let col = if on { t.accent } else { t.text2.mix(t.text, hv) };
            let (size, label) = self.fit_shrink(self.f.sans_med, 12.0, tr(name), w - 10.0);
            let (cx, cy) = chip.center();
            self.text_center(c, self.f.sans_med, size, cx, cy, &label, col);
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(tr(MODE_TIPS[i]), "");
                if input.pressed {
                    let Device::Effect { track, slot } = device else { return };
                    if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                        fx.params.retain(|(id, _)| at::quick_mode_keeps(*id));
                        for &(id, v) in values.iter() {
                            fx.set(id, v);
                        }
                    }
                    self.commit(tr("Pitch correction mode"));
                }
            }
        }
    }

    /// Tuner (note, cents bar) and the retune speed knob.
    fn at_tuner(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, now: Option<PitchPoint>, shared: Option<&AutoTuneShared>) {
        let t = self.theme;
        let dk = device_key(device);
        let voiced = now.filter(|p| p.input.is_finite());
        let panel = Rect::new(r.x, r.y, r.w, 62.0);
        c.fill_rrect(panel, 10.0, t.well);
        c.stroke_rrect(panel, 10.0, 1.0, t.hairline);
        let (cx, _) = panel.center();
        let note = voiced.and_then(|p| p.note.or(Some(p.input.round().clamp(0.0, 127.0) as u8)));
        match (voiced, note) {
            (Some(p), Some(n)) => {
                let label = note_label(n);
                let col = if p.bypassed { t.text2 } else { t.text };
                self.text_center(c, self.f.sans_med, 22.0, cx, panel.y + 20.0, &label, col);
                let cents = (p.input - f32::from(n)) * 100.0;
                self.text_right(c, self.f.mono, 10.0, panel.right() - 8.0, panel.y + 12.0, &format!("{}{:.0}", if cents > 0.5 { "+" } else { "" }, cents), t.muted);
                // Cents bar: the voice (needle) and the corrected pitch (dot).
                let bar = Rect::new(panel.x + 12.0, panel.y + 42.0, panel.w - 24.0, 6.0);
                c.fill_rrect(bar, 3.0, t.knob_track);
                let (bx, by) = bar.center();
                c.fill_rect(Rect::new(bx - 0.5, bar.y - 4.0, 1.0, bar.h + 8.0), t.muted);
                let x_of = |ct: f32| bx + (ct / 50.0).clamp(-1.0, 1.0) * bar.w * 0.5;
                let shown = self.anim.to(wid("at-needle", dk, 0), cents, 0.05);
                let near = shown.abs() < 10.0;
                let x = x_of(shown);
                c.fill_rrect(Rect::new(bx.min(x), bar.y, (x - bx).abs(), bar.h), 3.0, if near { t.accent.alpha(0.7) } else { t.text2.alpha(0.6) });
                c.fill_rrect(Rect::new(x - 1.5, bar.y - 5.0, 3.0, bar.h + 10.0), 1.5, if near { t.accent } else { t.text });
                if p.output.is_finite() {
                    let ox = x_of((p.output - f32::from(n)) * 100.0);
                    c.fill_circle(ox, by, 3.0, t.accent_hot);
                }
            }
            _ => {
                self.text_center(c, self.f.sans_med, 22.0, cx, panel.y + 20.0, "–", t.muted);
                let bar = Rect::new(panel.x + 12.0, panel.y + 42.0, panel.w - 24.0, 6.0);
                c.fill_rrect(bar, 3.0, t.knob_track);
            }
        }
        if self.hovered(input, wid("at-tuner", dk, 0), panel) {
            self.set_tip(tr("Note sung and its offset in cents\nBar: the voice · dot: the corrected pitch"), "");
        }
        let _ = shared;
        let knob = Rect::new(r.x, panel.bottom() + 4.0, r.w, r.bottom() - panel.bottom() - 4.0);
        self.at_retune_knob(c, input, knob, device);
    }

    /// The retune speed, large: the main control.
    fn at_retune_knob(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device) {
        let t = self.theme;
        let p = &at::params()[at::P_RETUNE as usize];
        let value = self.param_value(device, p);
        let id = wid("prm", device_key(device), p.id as usize);
        let radius = ((r.h - 34.0) * 0.5).clamp(18.0, 34.0);
        let (cx, cy) = (r.x + r.w * 0.5, r.y + 14.0 + radius + 4.0);
        let hit = Rect::new(cx - radius - 8.0, cy - radius - 8.0, 2.0 * radius + 16.0, 2.0 * radius + 16.0);
        let hov = self.hovered(input, id, hit);
        // Square-root travel: the fast settings get most of the turn.
        let to_n = |v: f32| (v / p.max).clamp(0.0, 1.0).sqrt();
        let from_n = |n: f32| p.max * n.clamp(0.0, 1.0).powi(2);
        let norm = to_n(value);
        let mut changed = None;
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                changed = Some(p.default);
            } else {
                self.begin_drag(id, input, norm);
            }
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let per_px = if d.fine { 0.001 } else { 0.005 };
            let v = from_n(d.v0 + (d.y0 - input.y) * per_px);
            if (v - value).abs() > 1e-6 {
                changed = Some(v);
            }
        }
        if hov {
            self.claim_wheel();
            if input.wheel != 0.0 {
                changed = Some(from_n(norm + input.wheel * 0.03));
            }
            self.set_cursor(Cursor::ResizeV);
            self.set_tip(tr("Retune speed\n0 ms: instant, robotic hard tune · higher: slower and more natural\nDrag or wheel · double-click: default"), "");
            if input.right_pressed && self.drag.is_none() {
                let target = device_target(device, p.id);
                self.open_popup(Popup::Param { target, value, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
            }
        }
        let active = hov || self.is_dragging(id);
        let hv = self.hover_anim(id, active);
        self.text_center(c, self.f.sans_med, 12.0, cx, r.y + 6.0, tr("Retune speed"), t.text2.mix(t.text, hv));
        let shown = changed.map_or(norm, to_n);
        let (a0, sweep) = (0.75 * PI, 1.5 * PI);
        c.stroke_arc(cx, cy, radius, a0, a0 + sweep, 5.0, t.knob_track);
        if shown > 0.002 {
            c.stroke_arc(cx, cy, radius, a0, a0 + sweep * shown, 5.0, t.accent.mix(t.accent_hot, hv * 0.3));
        }
        c.fill_circle(cx, cy, radius - 9.0, t.knob_cap.mix(t.knob_cap_hover, hv));
        c.stroke_circle(cx, cy, radius - 9.0, 1.0, t.hairline);
        let a = a0 + sweep * shown;
        let (dx, dy) = (a.cos(), a.sin());
        c.stroke_polyline(&[(cx + dx * (radius - 20.0), cy + dy * (radius - 20.0)), (cx + dx * (radius - 11.0), cy + dy * (radius - 11.0))], 2.5, t.knob_pointer);
        let v = changed.unwrap_or(value);
        let label = if v < 0.0005 { tr("Hard").to_string() } else { p.format(v) };
        self.text_center(c, self.f.mono_med, 12.0, cx, cy, &label, t.text);
        let target = device_target(device, p.id);
        if self.is_automated(target) {
            c.fill_circle(hit.right() - 4.0, hit.y + 4.0, 3.0, t.select);
        }
        if let Some(v) = changed {
            self.set_param_value(device, p, v);
        }
    }

    /// Settings, in tabs (or all of them when the window is tall enough).
    fn at_settings(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, track: usize, slot: usize) {
        let t = self.theme;
        let dk = device_key(device);
        let all = r.h >= 16.0 + 5.0 * CELL_H + 30.0;
        let tab = with((track, slot), |v| v.tab).min(TABS.len() - 1);
        let mut rows: Vec<[Option<u32>; 5]> = Vec::new();
        if all {
            for (name, tab_rows) in TABS {
                rows.extend(tab_rows.iter().filter(|row| row.iter().any(Option::is_some)));
                let _ = name;
            }
        } else {
            // Tabs, in the label line of the cells.
            let mut x = r.x + 4.0;
            for (i, (name, _)) in TABS.iter().enumerate() {
                let label = tr(name);
                let w = self.fonts.measure(self.f.sans_med, 10.5, label) + 12.0;
                let chip = Rect::new(x, r.y - 2.0, w, 15.0);
                let id = wid("at-tab", dk, i);
                let hov = self.hovered(input, id, chip);
                let on = i == tab;
                if on {
                    c.fill_rrect(chip, 7.5, t.raised_hi);
                }
                let col = if on { t.text } else if hov { t.text2 } else { t.muted };
                self.text_center(c, self.f.sans_med, 10.5, chip.x + chip.w * 0.5, chip.y + 7.5, label, col);
                if hov {
                    self.set_cursor(Cursor::Hand);
                    self.set_tip(tr(TAB_TIPS[i]), "");
                    if input.pressed {
                        with((track, slot), |v| v.tab = i);
                    }
                }
                x += w + 4.0;
            }
            rows.extend(TABS[tab].1);
        }
        for (row, ids) in rows.iter().enumerate() {
            for (col, id) in ids.iter().enumerate() {
                let Some(id) = *id else { continue };
                let cell = Rect::new(r.x + col as f32 * CELL_W, r.y + 16.0 + row as f32 * CELL_H, CELL_W, CELL_H);
                if cell.bottom() > r.bottom() + 6.0 {
                    continue;
                }
                let p = &at::params()[id as usize];
                if id == at::P_MIDI_CHANNEL {
                    self.at_channel_cell(c, input, cell, device, p);
                } else {
                    self.at_cell(c, input, cell, device, p);
                }
            }
        }
    }

    /// Sets a setting of the corrector. Picking a key or scale by hand stops
    /// following the project's, which would otherwise put it back at once.
    fn at_set(&mut self, device: Device, p: &ParamInfo, v: f32) {
        let follow = at::PARAMS.iter().find(|q| q.id == at::P_FOLLOW);
        if let Some(follow) = follow.filter(|f| matches!(p.id, at::P_KEY | at::P_SCALE) && self.param_value(device, f) >= 0.5) {
            self.set_param_value(device, follow, 0.0);
        }
        self.set_param_value(device, p, v);
    }

    /// One knob or choice, with automation and modulation like any device's.
    fn at_cell(&mut self, c: &mut Canvas, input: &Input, cell: Rect, device: Device, p: &'static ParamInfo) {
        let t = self.theme;
        let value = self.param_value(device, p);
        let id = wid("prm", device_key(device), p.id as usize);
        let mut changed = if p.unit == Unit::Choice { self.choice(c, input, id, cell, p, value) } else { self.knob(c, input, id, cell, p, value) };
        self.mod_knob(c, input, id, cell, device, p, value, &mut changed);
        if let Some(v) = changed {
            self.at_set(device, p, v);
        }
        let target = device_target(device, p.id);
        if p.unit != Unit::Choice && self.kb_enter(id) {
            self.open_popup(Popup::Rename { target: super::app::Rename::Value(target), text: String::new() }, Rect::new(cell.x - 60.0, cell.y + 20.0, cell.w + 120.0, 36.0));
        }
        if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
            self.open_popup(Popup::Param { target, value, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
        }
        self.param_mark(c, cell, target);
        if self.is_automated(target) {
            c.fill_circle(cell.right() - 10.0, cell.y + 6.0, 3.0, t.select);
        }
    }

    /// The channel whose notes are the target: its name; click for the next one.
    fn at_channel_cell(&mut self, c: &mut Canvas, input: &Input, cell: Rect, device: Device, p: &'static ParamInfo) {
        let t = self.theme;
        let value = self.param_value(device, p).round() as usize;
        let count = self.project.channels.len();
        self.text_center(c, self.f.sans, 12.0, cell.x + cell.w * 0.5, cell.y + 6.0, tr(p.name), t.text2);
        let field = Rect::new(cell.x + 2.0, cell.y + 20.0, cell.w - 4.0, 30.0);
        let id = wid("prm", device_key(device), p.id as usize);
        let hov = self.hovered(input, id, field);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(field, 8.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(field, 8.0, 1.0, t.hairline);
        let name = match value.checked_sub(1).and_then(|i| self.project.channels.get(i)) {
            Some(ch) => ch.name.clone(),
            None => tr("None").to_string(),
        };
        let (size, shown) = self.fit_shrink(self.f.sans_med, 12.0, &name, field.w - 10.0);
        let (fx, fy) = field.center();
        self.text_center(c, self.f.sans_med, size, fx, fy, &shown, if value > 0 { t.text } else { t.muted });
        let mut changed = None;
        if hov {
            self.set_cursor(Cursor::Hand);
            self.set_tip(tr("Channel whose notes are the target (MIDI target: Notes or Melody)\nClick: next channel · right-click: none · wheel"), "");
            self.claim_wheel();
            if input.pressed {
                changed = Some((value + 1) % (count + 1));
            } else if input.right_pressed {
                changed = Some(0);
            } else if input.wheel != 0.0 {
                changed = Some((value as i32 - input.wheel.signum() as i32).clamp(0, count as i32) as usize);
            }
        }
        if let Some(v) = changed.filter(|&v| v != value) {
            self.set_param_value(device, p, v as f32);
        }
    }

    /// Captures: a corrector on track 1 in A minor, fed five seconds of a
    /// synthetic singer so the graph has a live look. `spec`:
    /// `hard|natural|classic|choir[,float][,tab:n]`.
    pub fn screenshot_autotune(&mut self, spec: &str) {
        let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
        let mode = match parts[0] {
            "natural" => 2,
            "pop" => 3,
            "classic" | "rap" => 4,
            "choir" => 5,
            "default" => 0,
            _ => 1,
        };
        let track = 1.min(self.project.tracks.len() - 1);
        (self.project.key, self.project.scale) = (9, ponzik_project::ScaleKind::Minor);
        let mut def = ponzik_project::EffectDef::new("autotune");
        for &(id, v) in at::QUICK_MODES[mode].1 {
            def.set(id, v);
        }
        def.set(at::P_RANGE, 2.0);
        // A note left alone, to show it.
        def.set(at::P_NOTE + 7, at::NOTE_BYPASS as f32);
        let slot = self.project.tracks[track].inserts.iter().position(Option::is_none).unwrap_or(0);
        self.project.tracks[track].inserts[slot] = Some(def.clone());
        self.project.tracks[track].name = tr("Vocals").to_string();
        self.focus = super::app::Focus::Track(track);
        self.panel = true;
        self.commit(tr("Capture effects"));
        // The singer, through a corrector of the same settings.
        let sr = self.sample_rate as f32;
        let def = self.project.tracks[track].inserts[slot].clone().unwrap_or(def);
        if let Some(mut fx) = ponzik_project::Project::make_effect(&def, sr, self.project.bpm) {
            let shared = fx.shared().and_then(|s| s.downcast::<AutoTuneShared>().ok());
            let x = demo_singer(sr);
            let (mut l, mut r) = (x.clone(), x);
            for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                fx.process(a, b);
            }
            with((track, slot), |v| v.shared = shared);
        }
        for part in &parts[1..] {
            if let Some(n) = part.strip_prefix("tab:").and_then(|n| n.parse::<usize>().ok()) {
                with((track, slot), |v| v.tab = n.min(TABS.len() - 1));
            }
        }
        if parts.contains(&"float") {
            let device = Device::Effect { track, slot };
            self.detach(device, None);
            if let Some(w) = self.layout.wins.iter_mut().find(|w| w.device == device) {
                w.rect.w = WIDTH + 80.0;
                w.rect.h = 520.0;
            }
        }
    }
}

/// A short hook in A minor sung off pitch, ending on a held note: what the capture shows.
fn demo_singer(sr: f32) -> Vec<f32> {
    use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
    let steps = [57.0, 60.0, 62.0, 64.0, 62.0, 60.0, 64.0, 67.0, 64.0];
    let offs = [0.3, -0.25, 0.22, -0.32, 0.28, -0.2, 0.33, -0.24, 0.12];
    let mut notes = Vec::new();
    let mut start = 0.2;
    for (i, (&m, &o)) in steps.iter().zip(&offs).enumerate() {
        let dur = if i == steps.len() - 1 { 1.6 } else { 0.42 };
        if i == 3 || i == 6 {
            notes.push(Sung { start, dur: 0.09, midi: f32::NAN, vowel: 0 });
            start += 0.11;
        }
        notes.push(Sung { start, dur, midi: m + o, vowel: (i * 2) % 5 });
        start += dur + 0.02;
    }
    let style = Style { vibrato_cents: 26.0, drift_cents: 14.0, ..Style::default() };
    // Cut while the last note is held: the tuner shows it.
    sing(sr, &notes, &style, start - 0.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_states_cycle() {
        assert_eq!(next_state(at::NOTE_AUTO, true), at::NOTE_OFF);
        assert_eq!(next_state(at::NOTE_OFF, true), at::NOTE_BYPASS);
        assert_eq!(next_state(at::NOTE_BYPASS, true), at::NOTE_AUTO);
        assert_eq!(next_state(at::NOTE_AUTO, false), at::NOTE_ON);
        assert_eq!(next_state(at::NOTE_ON, false), at::NOTE_BYPASS);
    }

    #[test]
    fn capture_shows_the_singer_and_follows_the_key() {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.screenshot_autotune("hard");
        let fx = app.project.tracks[1].inserts.iter().flatten().find(|f| f.kind == "autotune").unwrap();
        // A minor, taken from the project.
        assert_eq!((fx.get(at::P_KEY), fx.get(at::P_SCALE)), (Some(9.0), Some(1.0)));
        let slot = app.project.tracks[1].inserts.iter().position(|f| f.as_ref().is_some_and(|f| f.kind == "autotune")).unwrap();
        let shared = with((1, slot), |v| v.shared.clone()).unwrap();
        let mut pts = vec![PitchPoint::default(); 1_000];
        let n = shared.recent(&mut pts);
        let voiced = pts[..n].iter().filter(|p| p.input.is_finite()).count();
        assert!(n > 500 && voiced > n / 2, "{n} points, {voiced} voiced");
        let mut c = Canvas::new(1_600, 900);
        app.frame(&mut c, &Input::default(), 1.0 / 60.0);
        // A key picked by hand sticks: the corrector stops following the project.
        let device = Device::Effect { track: 1, slot };
        let key = at::PARAMS.iter().find(|q| q.id == at::P_KEY).unwrap();
        app.at_set(device, key, 2.0);
        app.commit("Key");
        let fx = app.project.tracks[1].inserts[slot].as_ref().unwrap();
        assert_eq!((fx.get(at::P_KEY), fx.get(at::P_FOLLOW)), (Some(2.0), Some(0.0)));
    }
}
