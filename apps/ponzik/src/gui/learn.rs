//! MIDI learn in the interface: "Link to controller" on any knob, fader or
//! transport button, the controller messages applied to the project, the
//! badges of linked controls and the "MIDI" tab of the settings.

use std::time::Instant;

use ponzik_engine::AutoTarget;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_midi::MidiEvent;
use ponzik_project::midi_map::{
    ControlTarget, CtlMode, CtlSource, CtlState, CtlValue, Learn, LearnEvent, MidiMapping, Outcome, TransportAction, source_label,
};
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{Action, App, MenuItem};
use super::mixer::{gain_to_pos, pos_to_gain};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Controller state of the interface (not saved, except `global`).
pub struct CtlUi {
    pub learn: Learn,
    /// Links of the controller layout, saved in the settings.
    pub global: Vec<MidiMapping>,
    /// Runtime state per link (pickup, held buttons, encoder history).
    states: Vec<(CtlSource, ControlTarget, CtlState)>,
    /// Pickup pending: target and when it was last reported.
    pub waiting: Vec<(ControlTarget, f64)>,
    /// Scroll of the settings list.
    pub scroll: f32,
    /// A link of the layout edited by a drag, saved on release.
    dirty: bool,
    /// The layout is written to the settings (off in captures and tests).
    pub persist: bool,
    clock: Instant,
}

impl Default for CtlUi {
    fn default() -> Self {
        Self { learn: Learn::Idle, global: Vec::new(), states: Vec::new(), waiting: Vec::new(), scroll: 0.0, dirty: false, persist: false, clock: Instant::now() }
    }
}

impl CtlUi {
    /// Seconds since start, for timestamps of controller and capture events.
    pub fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }

    fn state(&mut self, m: &MidiMapping) -> &mut CtlState {
        let i = match self.states.iter().position(|(s, t, _)| *s == m.source && *t == m.target) {
            Some(i) => i,
            None => {
                self.states.push((m.source, m.target, CtlState::default()));
                self.states.len() - 1
            }
        };
        &mut self.states[i].2
    }
}

/// A controller message: its source and value. `None` for clock and the rest.
pub fn ctl_event(ev: MidiEvent) -> Option<(CtlSource, CtlValue)> {
    Some(match ev {
        MidiEvent::NoteOn { channel, key, velocity } => (CtlSource::Note { channel, key }, CtlValue::Note { on: true, velocity }),
        MidiEvent::NoteOff { channel, key, velocity } => (CtlSource::Note { channel, key }, CtlValue::Note { on: false, velocity }),
        MidiEvent::ControlChange { channel, controller, value } => (CtlSource::Cc { channel, cc: controller }, CtlValue::Cc(value)),
        MidiEvent::PitchBend { channel, value } => (CtlSource::PitchBend { channel }, CtlValue::Bend((value as i32 + 8192).clamp(0, 16383) as u16)),
        _ => return None,
    })
}

/// Short tag of a source on a badge.
fn source_tag(s: CtlSource) -> &'static str {
    match s {
        CtlSource::Cc { .. } => "CC",
        CtlSource::Note { .. } => "N",
        CtlSource::PitchBend { .. } => "PB",
    }
}

pub fn mode_label(m: CtlMode) -> &'static str {
    match m {
        CtlMode::Absolute => tr("Absolute"),
        CtlMode::Relative(None) => tr("Relative (auto)"),
        CtlMode::Relative(Some(ponzik_project::midi_map::Encoding::TwosComplement)) => tr("Relative ±"),
        CtlMode::Relative(Some(ponzik_project::midi_map::Encoding::BinaryOffset)) => tr("Relative 64"),
        CtlMode::Relative(Some(ponzik_project::midi_map::Encoding::SignMagnitude)) => tr("Relative sign"),
        CtlMode::Toggle => tr("Toggle"),
    }
}

/// Next mode in the settings list.
fn next_mode(m: CtlMode) -> CtlMode {
    use ponzik_project::midi_map::Encoding as E;
    match m {
        CtlMode::Absolute => CtlMode::Relative(None),
        CtlMode::Relative(None) => CtlMode::Relative(Some(E::TwosComplement)),
        CtlMode::Relative(Some(E::TwosComplement)) => CtlMode::Relative(Some(E::BinaryOffset)),
        CtlMode::Relative(Some(E::BinaryOffset)) => CtlMode::Relative(Some(E::SignMagnitude)),
        CtlMode::Relative(Some(E::SignMagnitude)) => CtlMode::Toggle,
        CtlMode::Toggle => CtlMode::Absolute,
    }
}

impl App {
    // --- Targets ---

    /// Readable name of what a controller drives.
    pub fn ctl_name(&self, t: ControlTarget) -> String {
        match t {
            ControlTarget::Param(p) => self.target_info(p).map_or_else(|| tr("Missing parameter").to_string(), |(l, _)| l),
            ControlTarget::Send { track, slot } => {
                let tr_ = &self.project.tracks;
                let to = tr_.get(track).map_or(0, |t| t.sends[slot].0);
                tr_fmt!("Send {} → {} · {}", slot + 1, tr_.get(to).map_or("", |t| t.name.as_str()), tr_.get(track).map_or("", |t| t.name.as_str()))
            }
            ControlTarget::Transport(a) => match a {
                TransportAction::Play => tr("Play / pause").into(),
                TransportAction::Stop => tr("Stop").into(),
                TransportAction::Record => tr("Record").into(),
                TransportAction::Metronome => tr("Metronome").into(),
            },
        }
    }

    /// Position of a target in [0, 1], as its control shows it (faders on their curve).
    pub fn ctl_norm(&self, t: ControlTarget) -> Option<f32> {
        match t {
            ControlTarget::Param(AutoTarget::TrackGain(tr)) => Some(gain_to_pos(self.project.tracks.get(tr)?.gain)),
            ControlTarget::Param(p) => {
                let (_, info) = self.target_info(p)?;
                Some(info.to_norm(self.auto_value(p)?))
            }
            ControlTarget::Send { track, slot } => Some(self.project.tracks.get(track)?.sends[slot].1.clamp(0.0, 1.0)),
            ControlTarget::Transport(_) => Some(0.0),
        }
    }

    fn set_ctl_norm(&mut self, t: ControlTarget, n: f32) {
        let n = n.clamp(0.0, 1.0);
        match t {
            ControlTarget::Param(AutoTarget::TrackGain(tr)) => {
                if let Some(track) = self.project.tracks.get_mut(tr) {
                    track.gain = pos_to_gain(n);
                }
            }
            ControlTarget::Param(p) => {
                if let Some((_, info)) = self.target_info(p) {
                    self.set_auto_value(p, info.from_norm(n));
                }
            }
            ControlTarget::Send { track, slot } => {
                if let Some(tr) = self.project.tracks.get_mut(track) {
                    tr.sends[slot].1 = if n < 0.01 { 0.0 } else { n };
                }
            }
            ControlTarget::Transport(_) => {}
        }
    }

    /// The link of a target: in the controller layout (`true`) or the project.
    pub fn ctl_link(&self, t: ControlTarget) -> Option<(bool, MidiMapping)> {
        self.project
            .midi_map
            .iter()
            .find(|m| m.target == t)
            .map(|m| (false, *m))
            .or_else(|| self.ctl.global.iter().find(|m| m.target == t).map(|m| (true, *m)))
    }

    /// Writes the controller layout with the settings.
    fn save_layout(&mut self) {
        if self.ctl.persist {
            self.save_settings();
        }
    }

    // --- Learn ---

    pub fn start_learn(&mut self, t: ControlTarget) {
        let now = self.ctl.now();
        self.ctl.learn.start(t, now);
        self.release_all_held();
        if self.midi_inputs.is_empty() {
            self.toast(tr("No MIDI controller found: connect one and restart Ponzik"));
        }
    }

    pub fn unlink(&mut self, t: ControlTarget) {
        let before = self.ctl.global.len();
        self.ctl.global.retain(|m| m.target != t);
        if self.ctl.global.len() != before {
            self.save_layout();
        }
        if self.project.midi_map.iter().any(|m| m.target == t) {
            self.project.midi_map.retain(|m| m.target != t);
            self.commit(tr("Unlink a controller"));
        }
        self.toast(&tr_fmt!("Unlinked: {}", self.ctl_name(t)));
    }

    /// Stores a link: in the controller layout for the transport, the project
    /// otherwise. A target has one link; re-learning replaces it.
    pub fn add_link(&mut self, m: MidiMapping) {
        self.ctl.global.retain(|x| x.target != m.target);
        self.project.midi_map.retain(|x| x.target != m.target);
        if m.target.is_button() {
            self.ctl.global.push(m);
            self.save_layout();
        } else {
            self.project.midi_map.push(m);
            self.break_undo_merge();
            self.commit(tr("Link to controller"));
        }
    }

    fn finish_learn(&mut self, e: LearnEvent) {
        match e {
            LearnEvent::Linked(m) => {
                self.add_link(m);
                let name = self.ctl_name(m.target);
                self.toast(&tr_fmt!("Linked: {} → {} ({})", source_label(m.source), name, mode_label(m.mode)));
                self.anim.set(wid("ctl-linked", 0, 0), 1.0);
            }
            LearnEvent::TimedOut => self.toast(tr("No control moved: link cancelled")),
        }
    }

    /// The listening window of learn closes with time, not only with messages.
    pub(super) fn learn_tick(&mut self) {
        let now = self.ctl.now();
        if let Some(e) = self.ctl.learn.tick(now) {
            self.finish_learn(e);
        }
        self.ctl.waiting.retain(|(_, at)| now - at < 1.5);
    }

    // --- Incoming messages ---

    /// Controller messages: learn first, then the links; unlinked notes play.
    pub(super) fn handle_midi(&mut self, events: &mut Vec<MidiEvent>) {
        let mut label: Option<String> = None;
        for ev in events.drain(..) {
            let Some((source, value)) = ctl_event(ev) else { continue };
            let now = self.ctl.now();
            if self.ctl.learn.is_active() {
                if let Some(e) = self.ctl.learn.feed(source, value, now) {
                    self.finish_learn(e);
                }
                continue;
            }
            if let Some(name) = self.apply_controller(source, value, now) {
                if !name.is_empty() {
                    label = Some(name);
                }
                continue;
            }
            // Codes reserved for MIDI notes, outside the physical keys.
            if let CtlSource::Note { channel, key } = source {
                let code = 0x1000 + channel as u16 * 128 + key as u16;
                match value {
                    CtlValue::Note { on: true, velocity } => self.live_note_on(code, key, velocity as f32 / 127.0),
                    _ => self.live_note_off(code),
                }
            }
        }
        if let Some(name) = label {
            self.commit(&tr_fmt!("Controller: {}", name));
        }
    }

    /// Applies a message to every link of its source. Returns the name of a
    /// moved target if one was linked (the message is then consumed).
    fn apply_controller(&mut self, source: CtlSource, value: CtlValue, now: f64) -> Option<String> {
        let links: Vec<MidiMapping> =
            self.project.midi_map.iter().chain(&self.ctl.global).filter(|m| m.source == source).copied().collect();
        if links.is_empty() {
            return None;
        }
        let mut moved = None;
        for m in links {
            let Some(current) = self.ctl_norm(m.target) else { continue };
            let outcome = m.apply(self.ctl.state(&m), value, current);
            match outcome {
                Outcome::Set(n) => {
                    self.set_ctl_norm(m.target, n);
                    let actual = self.ctl_norm(m.target).unwrap_or(n);
                    self.ctl.state(&m).settle(actual);
                    self.ctl.waiting.retain(|(t, _)| *t != m.target);
                    moved = Some(self.ctl_name(m.target));
                }
                Outcome::Trigger => {
                    if let ControlTarget::Transport(a) = m.target {
                        self.transport_action(a);
                    }
                }
                Outcome::Waiting(_) => {
                    self.ctl.waiting.retain(|(t, _)| *t != m.target);
                    self.ctl.waiting.push((m.target, now));
                }
                Outcome::None => {}
            }
        }
        // Consumed in every case; a name only when something moved.
        Some(moved.unwrap_or_default())
    }

    fn transport_action(&mut self, a: TransportAction) {
        match a {
            TransportAction::Play => self.toggle_play(),
            TransportAction::Stop => self.stop(),
            TransportAction::Record => {
                self.record = !self.record;
                self.toast(if self.record { tr("Recording armed") } else { tr("Recording disarmed") });
            }
            TransportAction::Metronome => {
                self.metronome = !self.metronome;
                let m = self.metronome;
                self.send(ponzik_engine::Command::SetMetronome(m));
            }
        }
    }

    // --- Menus ---

    /// "Link to controller" (or relink) and "Unlink" for a target.
    pub(super) fn ctl_menu_items(&self, t: ControlTarget) -> Vec<MenuItem> {
        let mut v = Vec::new();
        match self.ctl_link(t) {
            Some((_, m)) => {
                v.push(
                    MenuItem::new(Icon::Midi, tr("Link to another control"), Action::Learn(t))
                        .detail(source_label(m.source))
                        .tip(tr("Move a control on your MIDI controller"))
                        .sep(),
                );
                v.push(MenuItem::new(Icon::Close, tr("Unlink the controller"), Action::Unlink(t)));
            }
            None => v.push(
                MenuItem::new(Icon::Midi, tr("Link to controller"), Action::Learn(t)).tip(tr("Move a control on your MIDI controller")).sep(),
            ),
        }
        v
    }

    // --- Drawing ---

    /// Marks a linked control: a small tag under a knob (radius `r` ≥ 14) or a
    /// dot beside a small one; a pulsing ring while it waits to be learned.
    pub(super) fn ctl_mark(&mut self, c: &mut Canvas, (cx, cy): (f32, f32), r: f32, t: ControlTarget) {
        let th = self.theme;
        if self.ctl.learn.target() == Some(t) {
            let k = 0.5 + 0.5 * (self.time * 6.0).sin();
            self.anim.set(wid("caret", 0, 0), self.time);
            c.stroke_circle(cx, cy, r + 4.0 + 2.0 * k, 2.0, th.accent.alpha(0.5 + 0.5 * k));
            c.fill_circle(cx, cy, r + 6.0, th.accent.alpha(0.08));
            return;
        }
        let Some((_, m)) = self.ctl_link(t) else { return };
        let waiting = self.ctl.waiting.iter().any(|(x, _)| *x == t);
        let col = if waiting { th.muted } else { th.accent };
        if r >= 14.0 {
            let tag = source_tag(m.source);
            let w = if tag.len() > 1 { 16.0 } else { 11.0 };
            let b = Rect::new(cx - w * 0.5, cy + r - 6.5, w, 9.0);
            c.fill_rrect(b, 3.0, col);
            self.text_center(c, self.f.mono_med, 7.5, cx, b.y + 4.5, tag, th.bg_deep);
        } else {
            let (dx, dy) = (cx + r * 0.85, cy - r * 0.85);
            c.fill_circle(dx, dy, 3.2, th.bg_deep);
            c.fill_circle(dx, dy, 2.3, col);
        }
    }

    /// Right-click on a small knob centred on (`cx`, `cy`): its parameter menu.
    pub(super) fn knob_menu(&mut self, input: &Input, (cx, cy): (f32, f32), r: f32, target: AutoTarget) {
        if input.right_pressed && input.inside && self.drag.is_none() && Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r).contains(input.x, input.y) {
            let Some((_, info)) = self.target_info(target) else { return };
            let value = self.auto_value(target).unwrap_or(info.default);
            self.open_popup(super::app::Popup::Param { target, value, default: info.default }, Rect::new(input.x, input.y, 0.0, 0.0));
        }
    }

    /// Right-click on a transport button: its controller menu.
    pub(super) fn transport_menu(&mut self, c: &mut Canvas, input: &Input, r: Rect, a: TransportAction) {
        let t = ControlTarget::Transport(a);
        if input.right_pressed && input.inside && r.contains(input.x, input.y) {
            self.open_popup(super::app::Popup::Control(t), Rect::new(r.x, r.bottom(), r.w, 0.0));
        }
        if self.ctl.learn.target() == Some(t) {
            self.ctl_mark(c, r.center(), r.h * 0.5, t);
        } else {
            self.ctl_mark(c, (r.right() - 4.0, r.y + 4.0), 1.0, t);
        }
    }

    /// [`Self::ctl_mark`] on a device knob cell.
    pub(super) fn param_mark(&mut self, c: &mut Canvas, cell: Rect, t: AutoTarget) {
        self.ctl_mark(c, (cell.x + cell.w * 0.5, cell.y + 34.0), 17.0, ControlTarget::Param(t));
    }

    /// Card shown while learning: what is waiting, and a way out.
    pub(super) fn learn_banner(&mut self, c: &mut Canvas, input: &Input, w: f32) {
        let Some(target) = self.ctl.learn.target() else { return };
        let t = self.theme;
        let a = self.anim.to(wid("learn-card", 0, 0), 1.0, T_LAYOUT);
        let r = Rect::new(((w - 360.0) * 0.5).round(), TOP_BAR + 12.0 + ((1.0 - a) * -8.0).round(), 360.0, 56.0);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (80.0 * a) as u8));
        c.fill_rrect(r, 14.0, t.raised_hi.mix(t.panel, 0.4).alpha(a));
        c.stroke_rrect(r, 14.0, 1.0, t.accent.alpha(0.6 * a));
        let cy = r.y + r.h * 0.5;
        let k = 0.5 + 0.5 * (self.time * 5.0).sin();
        self.anim.set(wid("caret", 0, 0), self.time);
        c.fill_circle(r.x + 28.0, cy, 16.0, t.accent.alpha((0.10 + 0.12 * k) * a));
        draw_icon(c, Icon::Midi, r.x + 28.0, cy, 18.0, t.accent.alpha(a));
        let listening = matches!(self.ctl.learn, Learn::Listening { .. });
        let line = if listening { tr("Listening…") } else { tr("Move a control on your MIDI controller") };
        self.text(c, self.f.sans_med, 14.0, r.x + 54.0, cy - 8.0, line, t.text.alpha(a));
        let detail = self.ctl_name(target);
        self.text_fit(c, self.f.sans, 11.0, r.x + 54.0, cy + 10.0, &detail, r.w - 54.0 - 50.0, t.muted.alpha(a));
        let no = Rect::new(r.right() - 44.0, cy - 17.0, 34.0, 34.0);
        if self.icon_button(c, input, wid("learn-cancel", 0, 0), no, Icon::Close, false, (tr("Cancel"), tr("Esc"))) {
            self.ctl.learn.cancel();
        }
    }

    /// Area of the learn card (it takes the mouse).
    pub(super) fn learn_rect(&self, w: f32) -> Option<Rect> {
        self.ctl.learn.is_active().then(|| Rect::new(((w - 360.0) * 0.5).round(), TOP_BAR + 12.0, 360.0, 56.0))
    }

    // --- Settings: the list of links ---

    fn link_chip(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, label: &str, on: bool) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let fill = if on { t.accent.alpha(0.18 + 0.1 * hv) } else { t.raised.mix(t.raised_hi, hv) };
        c.fill_rrect(r, r.h * 0.5, fill);
        if on {
            c.stroke_rrect(r, r.h * 0.5, 1.0, t.accent.alpha(0.55));
        }
        let ink = if on { t.accent } else { t.text2.mix(t.text, hv) };
        let label = self.fit(self.f.sans_med, 11.0, label, r.w - 12.0);
        self.text_center(c, self.f.sans_med, 11.0, r.x + r.w * 0.5, r.y + r.h * 0.5, &label, ink);
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Settings tab "MIDI": every link of the controller layout and of the
    /// project, editable in place.
    pub(super) fn midi_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let names: Vec<String> = self.midi_inputs.iter().map(|m| m.name().to_string()).collect();
        let inputs = if names.is_empty() { tr("No MIDI controller found").to_string() } else { names.join(" · ") };
        draw_icon(c, Icon::Midi, body.x + 9.0, body.y + 10.0, 16.0, if names.is_empty() { t.muted } else { t.accent });
        self.text_fit(c, self.f.sans, 12.0, body.x + 26.0, body.y + 10.0, &inputs, body.w - 26.0, t.text2);

        // (global, index) of every link, project first.
        let rows: Vec<(bool, usize)> =
            (0..self.project.midi_map.len()).map(|i| (false, i)).chain((0..self.ctl.global.len()).map(|i| (true, i))).collect();
        let head_y = body.y + 34.0;
        let cols = [("", 0.0), (tr("Control"), 92.0), (tr("Linked to"), 196.0), (tr("Mode"), body.w - 396.0), (tr("Range"), body.w - 270.0), (tr("Pickup"), body.w - 112.0)];
        for (label, x) in cols {
            self.text(c, self.f.sans_med, 11.0, body.x + x, head_y, label, t.muted);
        }
        c.fill_rect(Rect::new(body.x, head_y + 12.0, body.w, 1.0), t.hairline);
        if rows.is_empty() {
            let cy = body.y + body.h * 0.45;
            c.fill_circle(body.x + body.w * 0.5, cy - 34.0, 24.0, t.accent.alpha(0.1));
            draw_icon(c, Icon::Midi, body.x + body.w * 0.5, cy - 34.0, 26.0, t.accent);
            self.text_center(c, self.f.sans_med, 14.0, body.x + body.w * 0.5, cy + 6.0, tr("No controller links yet"), t.text);
            let how = tr("Right-click a knob, fader or transport button → Link to controller, then move a control");
            self.text_center(c, self.f.sans, 12.0, body.x + body.w * 0.5, cy + 28.0, how, t.muted);
            return;
        }
        const ROW: f32 = 40.0;
        let list = Rect::new(body.x, head_y + 18.0, body.w, body.bottom() - head_y - 18.0);
        let max_scroll = (rows.len() as f32 * ROW - list.h).max(0.0);
        if input.inside && list.contains(input.x, input.y) && input.wheel != 0.0 {
            self.ctl.scroll = (self.ctl.scroll - input.wheel * 40.0).clamp(0.0, max_scroll);
        }
        self.ctl.scroll = self.ctl.scroll.min(max_scroll);
        let prev = c.push_clip(list);
        let mut edit: Option<(bool, usize, Option<MidiMapping>)> = None;
        for (k, &(global, i)) in rows.iter().enumerate() {
            let y = list.y + k as f32 * ROW - self.ctl.scroll;
            if y + ROW < list.y || y > list.bottom() {
                continue;
            }
            let m = if global { self.ctl.global[i] } else { self.project.midi_map[i] };
            let row = Rect::new(list.x, y, list.w, ROW - 4.0);
            let key = k * 2 + usize::from(global);
            let hov = input.inside && row.contains(input.x, input.y);
            c.fill_rrect(row, 10.0, if hov { t.raised.alpha(0.7) } else { t.raised.alpha(0.35) });
            let cy = row.y + row.h * 0.5;
            // Where the link lives: click to move it.
            let scope = Rect::new(row.x + 8.0, cy - 11.0, 76.0, 22.0);
            let scope_label = if global { tr("Global") } else { tr("Project") };
            if self.link_chip(c, input, wid("ml-scope", key, 0), scope, scope_label, global) {
                edit = Some((global, i, None));
                // Moved to the other list below.
                self.move_link(global, i);
                break;
            }
            if self.hovered(input, wid("ml-scope-tip", key, 0), scope) {
                *hint = Some(tr("Global: part of the controller layout, in every project · Project: saved with this song").into());
            }
            self.text(c, self.f.mono_med, 12.0, row.x + 92.0, cy, &source_label(m.source), t.text);
            let name = self.ctl_name(m.target);
            let name_w = body.w - 396.0 - 196.0 - 12.0;
            self.text_fit(c, self.f.sans, 12.0, row.x + 196.0, cy, &name, name_w, t.text2);
            let mode_r = Rect::new(row.x + body.w - 396.0, cy - 11.0, 116.0, 22.0);
            let mut m2 = m;
            if !m.target.is_button() && self.link_chip(c, input, wid("ml-mode", key, 0), mode_r, mode_label(m.mode), false) {
                m2.mode = next_mode(m.mode);
            }
            if m.target.is_button() {
                self.text(c, self.f.sans, 11.0, mode_r.x + 8.0, cy, tr("Button"), t.muted);
            }
            if self.hovered(input, wid("ml-mode-tip", key, 0), mode_r) {
                *hint = Some(tr("Absolute: knobs and faders · Relative: endless encoders · Toggle: buttons. Click to change").into());
            }
            if !m.target.is_button() {
                for (j, v) in [m.min, m.max].into_iter().enumerate() {
                    let r = Rect::new(row.x + body.w - 270.0 + j as f32 * 78.0, cy - 13.0, 72.0, 26.0);
                    let id = wid("ml-range", key, j);
                    let hov = self.hovered(input, id, r);
                    let mut nv = None;
                    if hov && input.pressed {
                        self.begin_drag(id, input, v);
                    }
                    if let Some(d) = self.drag.filter(|d| d.id == id) {
                        nv = Some((d.v0 + (d.y0 - input.y) * if d.fine { 0.001 } else { 0.005 }).clamp(0.0, 1.0));
                    }
                    if hov && input.wheel != 0.0 {
                        nv = Some((v + input.wheel * 0.01).clamp(0.0, 1.0));
                    }
                    if hov || self.is_dragging(id) {
                        self.set_cursor(ponzik_window::Cursor::ResizeV);
                        *hint = Some(tr("Range of the parameter the control covers: drag or scroll · Min above Max inverts").into());
                    }
                    let hv = self.hover_anim(id, hov || self.is_dragging(id));
                    c.fill_rrect(r, 8.0, t.bg_deep.mix(t.raised, hv));
                    let shown = nv.unwrap_or(v);
                    c.fill_rrect(Rect::new(r.x + 3.0, r.bottom() - 5.0, (r.w - 6.0) * shown, 2.0), 1.0, t.accent.alpha(0.7));
                    let txt = format!("{}{:.0}%", if j == 0 { "↓ " } else { "↑ " }, shown * 100.0);
                    self.text_center(c, self.f.mono, 11.0, r.x + r.w * 0.5, cy - 1.0, &txt, t.text2.mix(t.text, hv));
                    if let Some(nv) = nv {
                        if j == 0 { m2.min = nv } else { m2.max = nv }
                    }
                }
                let pk = Rect::new(row.x + body.w - 112.0, cy - 11.0, 60.0, 22.0);
                if matches!(m.mode, CtlMode::Absolute) {
                    let label = if m.pickup { tr("On") } else { tr("Off") };
                    if self.link_chip(c, input, wid("ml-pickup", key, 0), pk, label, m.pickup) {
                        m2.pickup = !m.pickup;
                    }
                    if self.hovered(input, wid("ml-pk-tip", key, 0), pk) {
                        *hint = Some(tr("Pickup: the control acts only once it reaches the current value, no jumps").into());
                    }
                }
            }
            let del = Rect::new(row.right() - 40.0, cy - 15.0, 30.0, 30.0);
            if self.icon_button(c, input, wid("ml-del", key, 0), del, Icon::Trash, false, (tr("Remove the link"), "")) {
                edit = Some((global, i, None));
                self.remove_link(global, i);
                break;
            }
            if m2 != m {
                edit = Some((global, i, Some(m2)));
            }
        }
        c.set_clip(Some(prev));
        if let Some((global, i, Some(m))) = edit {
            if global {
                self.ctl.global[i] = m;
                self.ctl.dirty = true;
            } else {
                self.project.midi_map[i] = m;
                self.commit(tr("Edit a controller link"));
            }
        }
        if self.ctl.dirty && self.drag.is_none() {
            self.ctl.dirty = false;
            self.save_layout();
        }
    }

    fn move_link(&mut self, global: bool, i: usize) {
        if global {
            let m = self.ctl.global.remove(i);
            self.project.midi_map.retain(|x| x.target != m.target);
            self.project.midi_map.push(m);
            self.save_layout();
            self.commit(tr("Link to controller"));
        } else {
            let m = self.project.midi_map.remove(i);
            self.ctl.global.retain(|x| x.target != m.target);
            self.ctl.global.push(m);
            self.save_layout();
            self.commit(tr("Unlink a controller"));
        }
    }

    fn remove_link(&mut self, global: bool, i: usize) {
        if global {
            self.ctl.global.remove(i);
            self.save_layout();
        } else {
            self.project.midi_map.remove(i);
            self.commit(tr("Unlink a controller"));
        }
    }

    /// `PONZIK_LEARN=pending|linked|settings`: a learn in progress on the first
    /// knob of the focused device, links with every kind of control, or the
    /// settings list. Controller messages are simulated.
    pub fn screenshot_learn(&mut self, spec: &str) {
        let ch = self.selected_channel().unwrap_or(0);
        let kind = self.project.channels.get(ch).map(|c| c.instrument.kind.clone()).unwrap_or_default();
        let params = super::devices::params_of(&kind, true);
        let first = |k: usize| params.get(k).map(|p| ControlTarget::Param(AutoTarget::InstrumentParam { channel: ch, id: p.id }));
        if spec == "pending" {
            if let Some(t) = first(1) {
                self.start_learn(t);
                let now = self.ctl.now();
                self.ctl.learn.feed(CtlSource::Cc { channel: 0, cc: 21 }, CtlValue::Cc(40), now);
            }
            return;
        }
        let cc = |cc: u8| CtlSource::Cc { channel: 0, cc };
        let mut links = Vec::new();
        for (k, n) in [(1usize, 74u8), (2, 71), (3, 1)] {
            if let Some(t) = first(k) {
                links.push(MidiMapping::new(cc(n), t, CtlMode::Absolute));
            }
        }
        links.push(MidiMapping::new(CtlSource::PitchBend { channel: 0 }, ControlTarget::Param(AutoTarget::ChannelPan(ch)), CtlMode::Absolute));
        let track = self.project.channels.get(ch).map_or(1, |c| c.track);
        links.push(MidiMapping { min: 0.1, max: 0.85, ..MidiMapping::new(cc(7), ControlTarget::Param(AutoTarget::TrackGain(track)), CtlMode::Absolute) });
        links.push(MidiMapping::new(cc(16), ControlTarget::Param(AutoTarget::TrackPan(track)), CtlMode::Relative(Some(ponzik_project::midi_map::Encoding::BinaryOffset))));
        for m in links {
            self.add_link(m);
        }
        for (a, key) in [(TransportAction::Play, 41u8), (TransportAction::Stop, 42), (TransportAction::Record, 45)] {
            self.add_link(MidiMapping::new(CtlSource::Note { channel: 9, key }, ControlTarget::Transport(a), CtlMode::Toggle));
        }
        // A knob turned on the controller: the first link moves, the fader waits for pickup.
        let mut events = vec![
            MidiEvent::ControlChange { channel: 0, controller: 74, value: 100 },
            MidiEvent::ControlChange { channel: 0, controller: 7, value: 5 },
        ];
        // Pickup: the fader's control is far from the fader, it waits.
        let first_value = self.ctl_norm(first(1).unwrap_or(ControlTarget::Transport(TransportAction::Play))).unwrap_or(0.0);
        let near = (first_value * 127.0).round() as u8;
        events.insert(0, MidiEvent::ControlChange { channel: 0, controller: 74, value: near });
        self.handle_midi(&mut events);
        if spec == "settings" {
            self.open_popup(super::app::Popup::Settings { tab: 2 }, Rect::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_project::midi_map::Encoding;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    fn cc(controller: u8, value: u8) -> MidiEvent {
        MidiEvent::ControlChange { channel: 0, controller, value }
    }

    #[test]
    fn learn_links_a_knob_and_moves_it_with_pickup() {
        let mut app = app();
        let target = ControlTarget::Param(AutoTarget::TrackPan(2));
        app.project.tracks[2].pan = 0.0;
        app.start_learn(target);
        app.handle_midi(&mut vec![cc(10, 30), cc(10, 31), cc(10, 32)]);
        // Nothing moves while learning; the window closes with time.
        assert_eq!(app.project.tracks[2].pan, 0.0);
        let until = std::time::Instant::now() + std::time::Duration::from_millis(650);
        while std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.learn_tick();
        let (global, m) = app.ctl_link(target).expect("linked");
        assert!(!global);
        assert_eq!(m.mode, CtlMode::Absolute);
        assert_eq!(app.project.midi_map.len(), 1);
        // Pickup: far below the pan (centre), nothing; crossing it, it follows.
        app.handle_midi(&mut vec![cc(10, 10)]);
        assert_eq!(app.project.tracks[2].pan, 0.0);
        app.handle_midi(&mut vec![cc(10, 70)]);
        assert!((app.project.tracks[2].pan - (70.0 / 127.0 * 2.0 - 1.0)).abs() < 1e-4);
        // One undo step for the gesture, then the link survives undo of the move.
        app.undo();
        assert_eq!(app.project.tracks[2].pan, 0.0);
        assert_eq!(app.project.midi_map.len(), 1);
    }

    #[test]
    fn transport_links_go_to_the_layout_and_buttons_trigger() {
        let mut app = app();
        let rec = ControlTarget::Transport(TransportAction::Record);
        app.ctl.global.clear();
        app.ctl.global.push(MidiMapping::new(CtlSource::Note { channel: 9, key: 40 }, rec, CtlMode::Toggle));
        let enc = ControlTarget::Param(AutoTarget::TrackGain(1));
        app.project.midi_map.push(MidiMapping::new(CtlSource::Cc { channel: 1, cc: 20 }, enc, CtlMode::Relative(Some(Encoding::TwosComplement))));
        assert!(!app.record);
        app.handle_midi(&mut vec![MidiEvent::NoteOn { channel: 9, key: 40, velocity: 100 }, MidiEvent::NoteOff { channel: 9, key: 40, velocity: 0 }]);
        assert!(app.record);
        // A mapped pad does not play a note.
        assert!(app.held.is_empty());
        let before = app.ctl_norm(enc).unwrap();
        app.handle_midi(&mut vec![MidiEvent::ControlChange { channel: 1, controller: 20, value: 3 }]);
        assert!((app.ctl_norm(enc).unwrap() - (before + 3.0 / 128.0)).abs() < 1e-3);
        app.unlink(enc);
        assert!(app.project.midi_map.is_empty());
    }
}
