//! The top bar, in three calm zones: the browser and the project on the left,
//! the transport as one capsule in the middle (play, stop, record, tempo,
//! position), undo, search and the menu on the right. Everything else lives
//! where it belongs (the record menu, the views, the main menu) and comes back
//! to the bar from Settings > Workspace.

use ponzik_engine::{Command, PlayMode, TICKS_PER_STEP};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::ScaleKind;
use ponzik_project::midi_map::TransportAction as TA;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App, MenuItem, Popup, Rename, key_name, scale_label};
use super::keymap::Shortcut as S;
use super::lessons::Spot;
use super::settings::Bar;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Height of the controls of the bar.
const H: f32 = 36.0;
/// Inner margin of the transport capsule.
const CAP_PAD: f32 = 3.0;
/// Space between two buttons of the right zone.
const STEP: f32 = 36.0;

/// What the transport capsule holds this frame, from left to right.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Mode,
    Play,
    Stop,
    Record,
    Mic,
    Voice,
    AutoArm,
    Keyboard,
    Divider,
    Tempo,
    Tap,
    Metronome,
    Position,
}

impl Slot {
    fn width(self, mode_w: f32) -> f32 {
        match self {
            Slot::Mode => mode_w,
            Slot::Play => 44.0,
            Slot::Stop => 40.0,
            Slot::Record => 58.0,
            Slot::Mic | Slot::Voice | Slot::AutoArm | Slot::Keyboard | Slot::Metronome => 40.0,
            Slot::Divider => 13.0,
            Slot::Tempo => 118.0,
            Slot::Tap => 44.0,
            Slot::Position => 134.0,
        }
    }

    /// The item of Settings > Workspace that shows it; `None`: always there.
    fn bar(self) -> Option<Bar> {
        match self {
            Slot::Mode => Some(Bar::Mode),
            Slot::Mic => Some(Bar::Mic),
            Slot::Voice => Some(Bar::Voice),
            Slot::AutoArm => Some(Bar::AutoArm),
            Slot::Keyboard => Some(Bar::Keyboard),
            Slot::Tap => Some(Bar::Tap),
            Slot::Metronome => Some(Bar::Metronome),
            Slot::Position => Some(Bar::Position),
            Slot::Record => Some(Bar::Record),
            Slot::Play | Slot::Stop | Slot::Divider | Slot::Tempo => None,
        }
    }
}

/// Order of the capsule.
const SLOTS: [Slot; 14] = [
    Slot::Mode,
    Slot::Play,
    Slot::Stop,
    Slot::Record,
    Slot::Mic,
    Slot::Voice,
    Slot::AutoArm,
    Slot::Keyboard,
    Slot::Divider,
    Slot::Tempo,
    Slot::Tap,
    Slot::Metronome,
    Slot::Divider,
    Slot::Position,
];

/// When the window is narrow, what leaves the capsule first.
const DROP: [Slot; 7] = [Slot::Keyboard, Slot::AutoArm, Slot::Voice, Slot::Mic, Slot::Tap, Slot::Position, Slot::Metronome];

impl App {
    pub(super) fn top_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        let cy = r.y + r.h * 0.5;

        // Left: the browser.
        let k_browser = self.key_label(S::ToggleBrowser);
        let sidebar = Rect::new(12.0, cy - 16.0, 32.0, 32.0);
        self.spot(Spot::Browser, sidebar);
        if self.icon_button(c, input, wid("tb-sidebar", 0, 0), sidebar, Icon::Sidebar, self.sidebar, (tr("Browser"), &k_browser)) {
            self.sidebar = !self.sidebar;
        }
        let name_x = 58.0;

        // Right first: it never leaves the bar.
        let right_x = self.top_bar_right(c, input, r, cy);

        // Centre: the transport capsule, items dropped when the room runs out.
        let mode_w = self.mode_width();
        let mut slots: Vec<Slot> = SLOTS.iter().copied().filter(|s| s.bar().is_none_or(|b| self.bar_shown(b))).collect();
        let extras = |s: &App| -> f32 {
            let mut w = 0.0;
            if s.bar_shown(Bar::Pattern) {
                w += 198.0;
            }
            if s.bar_shown(Bar::Scale) {
                w += 136.0;
            }
            w
        };
        let width = |slots: &[Slot]| -> f32 {
            let mut v: Vec<Slot> = slots.to_vec();
            v.dedup();
            // A divider at an end of the capsule or next to another one: none.
            while v.last() == Some(&Slot::Divider) {
                v.pop();
            }
            v.iter().map(|s| s.width(mode_w)).sum::<f32>() + 2.0 * CAP_PAD
        };
        let min_left = name_x + 72.0;
        let room = right_x - 16.0 - min_left;
        let mut show_extras = extras(self);
        let mut drop = DROP.iter();
        while width(&slots) + show_extras > room {
            if show_extras > 0.0 {
                show_extras = 0.0;
                continue;
            }
            match drop.next() {
                Some(d) => slots.retain(|s| s != d),
                None => break,
            }
        }
        slots.dedup();
        while slots.last() == Some(&Slot::Divider) {
            slots.pop();
        }
        let cap_w = width(&slots);
        let total = cap_w + show_extras;
        let x0 = ((r.w - total) * 0.5).round().min(right_x - 16.0 - total).max(min_left);
        let cap = Rect::new(x0, cy - H * 0.5 - CAP_PAD, cap_w, H + 2.0 * CAP_PAD);
        c.fill_rrect(cap, 14.0, t.raised.alpha(0.45));
        c.stroke_rrect(cap, 14.0, 1.0, t.hairline);
        let mut x = cap.x + CAP_PAD;
        for s in slots {
            let w = s.width(mode_w);
            let cell = Rect::new(x, cy - H * 0.5, w, H);
            self.transport_slot(c, input, s, cell);
            x += w;
        }
        // Pattern and scale, when the workspace keeps them in the bar.
        let mut x = cap.right() + 10.0;
        if show_extras > 0.0 && self.bar_shown(Bar::Pattern) {
            self.pattern_selector(c, input, Rect::new(x, cy - 18.0, 188.0, H));
            x += 198.0;
        }
        if show_extras > 0.0 && self.bar_shown(Bar::Scale) {
            self.spot(Spot::Scale, Rect::new(x, cy - 18.0, 128.0, H));
            self.scale_chip(c, input, Rect::new(x, cy - 18.0, 128.0, H));
        }

        // The project's name, between the browser and the transport.
        let room = cap.x - name_x - 20.0;
        if room > 48.0 {
            self.project_name(c, input, name_x, cy, room);
        }
    }

    /// Undo, redo, search and the menu (and what the workspace adds), from the
    /// right edge; returns the left edge of the zone.
    fn top_bar_right(&mut self, c: &mut Canvas, input: &Input, r: Rect, cy: f32) -> f32 {
        let t = self.theme;
        let (k_pal, k_save, k_redo, k_undo, k_panel, k_hist) =
            (self.key_label(S::Palette), self.key_label(S::Save), self.key_label(S::Redo), self.key_label(S::Undo), self.key_label(S::TogglePanel), self.key_label(S::History));
        let learn_in_bar = self.bar_shown(Bar::Learn);
        let mut buttons = vec![(Icon::Menu, "tb-menu"), (Icon::Search, "tb-search")];
        if self.bar_shown(Bar::Undo) {
            buttons.extend([(Icon::Redo, "tb-redo"), (Icon::Undo, "tb-undo")]);
        }
        for (b, icon, name) in [(Bar::History, Icon::History, "tb-history"), (Bar::Save, Icon::Save, "tb-save"), (Bar::Panel, Icon::Panel, "tb-panel"), (Bar::Learn, Icon::Learn, "tb-learn")] {
            if self.bar_shown(b) {
                buttons.push((icon, name));
            }
        }
        let mut rx = r.w - 12.0 - 32.0;
        for (k, (icon, name)) in buttons.into_iter().enumerate() {
            // A little air between the search and the edit buttons.
            if k == 2 {
                rx -= 8.0;
            }
            let b = Rect::new(rx, cy - 16.0, 32.0, 32.0);
            let id = wid(name, 0, 0);
            let tip = match icon {
                Icon::Menu => (tr("Menu"), ""),
                Icon::Search => (tr("Find anything"), k_pal.as_str()),
                Icon::Redo => (tr("Redo"), k_redo.as_str()),
                Icon::Undo => (tr("Undo\nRight-click: history"), k_undo.as_str()),
                Icon::History => (tr("History"), k_hist.as_str()),
                Icon::Save => (tr("Save"), k_save.as_str()),
                Icon::Panel => (tr("Devices"), k_panel.as_str()),
                _ => (tr("Learn: short hands-on lessons"), ""),
            };
            let on = (icon == Icon::Panel && self.panel) || (icon == Icon::Learn && self.lessons.active());
            if icon == Icon::Menu {
                self.tour_anchor(2, b);
                self.spot(Spot::Menu, b);
            }
            // A lesson left halfway: a small dot on Learn, or on the menu that holds it.
            if self.lessons.unfinished() && ((icon == Icon::Learn) || (icon == Icon::Menu && !learn_in_bar)) {
                c.fill_circle(rx + 26.0, cy - 10.0, 3.5, t.accent);
            }
            if self.icon_button(c, input, id, b, icon, on, tip) {
                match icon {
                    Icon::Menu => self.open_popup(Popup::MainMenu, b),
                    Icon::Search => self.open_palette(),
                    Icon::Learn => self.open_learn(),
                    Icon::Panel => self.panel = !self.panel,
                    Icon::Save => self.save(),
                    Icon::Redo => self.redo(),
                    Icon::Undo => self.undo(),
                    Icon::History => self.open_popup(Popup::History, b),
                    _ => {}
                }
            }
            if icon == Icon::Undo && self.hovered(input, id, b) && input.right_pressed {
                self.open_popup(Popup::History, b);
            }
            rx -= STEP;
        }
        let mut left = rx + STEP;
        // DSP load: a discreet gauge, the value in its tooltip.
        if self.bar_shown(Bar::Load) {
            let gauge = Rect::new(left - 40.0, cy - 3.0, 26.0, 6.0);
            let load = self.load.clamp(0.0, 1.0);
            c.fill_rrect(gauge, 3.0, t.raised);
            if load > 0.005 {
                let col = if load > 0.85 { t.negative } else { t.muted.mix(t.text2, load) };
                c.fill_rrect(Rect::new(gauge.x, gauge.y, (gauge.w * load).max(6.0), gauge.h), 3.0, col);
            }
            let tip = format!("{}\n{}", tr_fmt!("DSP load: {:.0}%", self.load * 100.0), self.audio_status());
            self.tip_zone(input, gauge.inset(-8.0), &tip, tr("Click: performance settings"));
            if input.pressed && input.inside && gauge.inset(-8.0).contains(input.x, input.y) {
                self.open_popup(Popup::Settings { tab: 4 }, gauge);
            }
            left = gauge.x - 8.0;
        }
        left
    }

    /// Width of the mode pill: its longest label, so the capsule never jumps.
    fn mode_width(&self) -> f32 {
        let widest = [PlayMode::Pattern(0), PlayMode::Song, PlayMode::Session]
            .into_iter()
            .map(|m| self.fonts.measure(self.f.sans_med, 12.0, mode_label(m)))
            .fold(0.0, f32::max);
        (widest + 42.0).round()
    }

    fn transport_slot(&mut self, c: &mut Canvas, input: &Input, s: Slot, cell: Rect) {
        let t = self.theme;
        let (_, cy) = cell.center();
        match s {
            Slot::Mode => self.mode_pill(c, input, Rect::new(cell.x, cell.y + 4.0, cell.w - 10.0, cell.h - 8.0)),
            Slot::Play => {
                let play = Rect::new(cell.x, cell.y, 40.0, H);
                let k_play = self.key_label(S::PlayStop);
                let id = wid("tb-play", 0, 0);
                self.spot(Spot::Play, play);
                let hov = self.hovered(input, id, play);
                let kb = self.kb_control(c, input, id, play) && self.kb_enter(id);
                if (hov && input.pressed) || kb {
                    self.toggle_play();
                }
                if hov {
                    self.set_tip(if self.playing { tr("Pause") } else { tr("Play") }, &k_play);
                }
                let hv = self.hover_anim(id, hov);
                let on = self.anim.to(wid("tb-play-on", 0, 0), self.playing as u8 as f32, T_HOVER);
                let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.94 } else { 1.0 }, T_PRESS);
                let body = play.scale(press);
                c.fill_rrect(body, 10.0, t.text.mix(t.accent, on).mix(t.highlight, 0.12 * hv));
                c.fill_rect(Rect::new(body.x + 8.0, body.y + 1.0, body.w - 16.0, 1.0), t.highlight.alpha(0.3));
                let (px, py) = play.center();
                draw_icon(c, if self.playing { Icon::Pause } else { Icon::Play }, px, py, 16.0 * press, t.bg_deep.mix(t.on_accent, on));
                self.transport_menu(c, input, play, TA::Play);
            }
            Slot::Stop => {
                let stop = Rect::new(cell.x, cell.y, 36.0, H);
                if self.icon_button(c, input, wid("tb-stop", 0, 0), stop, Icon::Stop, false, (tr("Stop"), "")) {
                    self.stop();
                }
                self.transport_menu(c, input, stop, TA::Stop);
            }
            Slot::Record => {
                let k_rec = self.key_label(S::Record);
                let rec = Rect::new(cell.x, cell.y, 36.0, H);
                self.spot(Spot::Record, rec);
                self.record_button(c, input, rec, &k_rec);
                // The record menu: count-in, metronome, input, voice, automation, typing.
                let more = Rect::new(rec.right(), cell.y + 6.0, 16.0, H - 12.0);
                let anchor = (more.x + more.w * 0.5, cell.bottom() + 10.0);
                if !self.bar_shown(Bar::Mic) {
                    self.spot(Spot::Mic, more.inset(-2.0));
                    self.rec.panel_anchor = anchor;
                }
                if !self.bar_shown(Bar::Voice) {
                    self.voice.panel_anchor = anchor;
                }
                let id = wid("tb-rec-more", 0, 0);
                let hov = self.hovered(input, id, more);
                let hv = self.hover_anim(id, hov);
                let open = matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::RecordMenu));
                let on = self.anim.to(id ^ 0x4f4e, open as u8 as f32, T_HOVER);
                if hv.max(on) > 0.01 {
                    c.fill_rrect(more, 6.0, t.raised.alpha(hv.max(on)));
                }
                // A light when something of the menu is on.
                let lit = self.rec.panel || self.voice.enabled || self.typing_keyboard || self.autorec.mode != ponzik_project::auto_rec::AutoMode::Read;
                draw_icon(c, Icon::ChevronDown, more.x + more.w * 0.5, cy, 10.0, t.muted.mix(t.text, hv.max(on)).mix(t.accent, lit as u8 as f32));
                if hov {
                    self.set_tip(tr("Recording options\nCount-in, metronome, input, voice, automation, typing"), "");
                }
                let kb = self.kb_control(c, input, id, more) && self.kb_enter(id);
                if (hov && input.pressed) || kb {
                    self.open_popup(Popup::RecordMenu, Rect::new(rec.x, cell.bottom() + 4.0, rec.w + more.w, 0.0));
                }
                // Input level under the dot while a track is armed.
                if !self.rec.armed.is_empty() && !self.bar_shown(Bar::Mic) {
                    let lv = self.rec.input_level();
                    let col = if self.rec.clipped { t.negative } else { t.accent };
                    c.fill_rrect(Rect::new(rec.x + 8.0, rec.bottom() - 3.0, (rec.w - 16.0) * lv, 2.0), 1.0, col);
                }
            }
            Slot::Mic => {
                let mic = Rect::new(cell.x, cell.y, 36.0, H);
                self.spot(Spot::Mic, mic);
                if self.icon_button(c, input, wid("tb-input", 0, 0), mic, Icon::Mic, self.rec.panel, (tr("Recording panel: input, level, monitoring, latency"), "")) {
                    self.toggle_record_panel((mic.x + mic.w * 0.5, mic.bottom() + 10.0));
                }
                if !self.rec.armed.is_empty() {
                    let lv = self.rec.input_level();
                    let col = if self.rec.clipped { t.negative } else { t.accent };
                    c.fill_rrect(Rect::new(mic.x + 8.0, mic.bottom() - 5.0, (mic.w - 16.0) * lv, 2.0), 1.0, col);
                }
            }
            Slot::Voice => self.voice_button(c, input, Rect::new(cell.x, cell.y, 36.0, H)),
            Slot::AutoArm => self.auto_arm_button(c, input, Rect::new(cell.x, cell.y, 36.0, H)),
            Slot::Keyboard => {
                let k_keys = self.key_label(S::TypingKeyboard);
                if self.icon_button(c, input, wid("tb-keys", 0, 0), Rect::new(cell.x, cell.y, 36.0, H), Icon::Keyboard, self.typing_keyboard, (tr("Musical typing"), &k_keys)) {
                    self.toggle_typing_keyboard();
                }
            }
            Slot::Divider => c.fill_rect(Rect::new(cell.x + 6.0, cy - 11.0, 1.0, 22.0), t.hairline.alpha(1.6)),
            Slot::Tempo => self.tempo_field(c, input, Rect::new(cell.x, cell.y, cell.w - 4.0, H)),
            Slot::Tap => {
                let k_tap = self.key_label(S::TapTempo);
                self.tap_button(c, input, Rect::new(cell.x, cell.y, 40.0, H), &k_tap);
            }
            Slot::Metronome => {
                let k_metro = self.key_label(S::Metronome);
                let metro = Rect::new(cell.x, cell.y, 36.0, H);
                if self.icon_button(c, input, wid("tb-metro", 0, 0), metro, Icon::Metronome, self.metronome, (tr("Metronome"), &k_metro)) {
                    self.metronome = !self.metronome;
                    let m = self.metronome;
                    self.send(Command::SetMetronome(m));
                }
                self.transport_menu(c, input, metro, TA::Metronome);
            }
            Slot::Position => {
                let pos_r = Rect::new(cell.x + 2.0, cell.y, cell.w - 2.0, H);
                self.position_field(c, pos_r);
                self.tip_zone(input, pos_r, tr("Position\nbar . beat . step"), "");
            }
        }
    }

    /// What plays: the pattern, the song or the session. Click: pattern or
    /// song; right-click: the three.
    fn mode_pill(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let id = wid("tb-mode", 0, 0);
        self.spot(Spot::Mode, r);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let kb = self.kb_control(c, input, id, r) && self.kb_enter(id);
        let mode = self.project.mode;
        let song = self.anim.to(id ^ 0x534f, (mode != PlayMode::Pattern(self.project.current_pattern)) as u8 as f32, T_HOVER);
        c.fill_rrect(r, r.h * 0.5, t.raised.alpha(0.5 + 0.5 * hv).mix(t.accent.alpha(0.16), song));
        // A dot in the colour of what plays: the pattern's, or the accent for the song.
        let dot = match mode {
            PlayMode::Pattern(p) => self.pattern_tint(p),
            _ => t.accent,
        };
        let cy = r.y + r.h * 0.5;
        c.fill_circle(r.x + 12.0, cy, 3.5, dot);
        let label = mode_label(mode);
        self.text(c, self.f.sans_med, 12.0, r.x + 21.0, cy, label, t.text2.mix(t.text, hv.max(song)));
        if hov {
            let k_mode = self.key_label(S::ToggleMode);
            let tip = match mode {
                PlayMode::Pattern(_) => tr("Plays the pattern\nClick: the song · right-click: every mode"),
                PlayMode::Song => tr("Plays the song\nClick: the pattern · right-click: every mode"),
                PlayMode::Session => tr("Plays the session\nClick: the pattern · right-click: every mode"),
            };
            self.set_tip(tip, &k_mode);
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        if (hov && input.pressed) || kb {
            self.run_action(Action::ToggleMode, r);
        }
        if hov && input.right_pressed {
            self.open_popup(Popup::ModeMenu, Rect::new(r.x, r.bottom() + 8.0, r.w, 0.0));
        }
    }

    /// The tempo: drag, scroll or type it; hovering it shows a tap pad on its
    /// right, unless the workspace keeps a TAP button in the bar.
    fn tempo_field(&mut self, c: &mut Canvas, input: &Input, group: Rect) {
        let id = wid("tb-bpm", 0, 0);
        self.spot(Spot::Tempo, group);
        let inline_tap = !self.bar_shown(Bar::Tap);
        let fresh = self.taps.last().is_some_and(|&last| self.time - last <= 2.0);
        let over = input.inside && group.contains(input.x, input.y) && !self.is_dragging(id);
        let tv = self.anim.to(wid("tb-bpm-tap", 0, 0), (inline_tap && (over || fresh)) as u8 as f32, T_HOVER);
        let field = Rect::new(group.x, group.y, (group.w - 40.0 * tv).round(), group.h);
        // While the song plays, the tempo heard (ramps included); otherwise the one edited.
        let heard = if self.playing && self.project.mode == PlayMode::Song {
            self.project.tempo_map().bpm_at(self.position)
        } else {
            self.tempo_here() as f64
        };
        let bpm_text = ponzik_i18n::num(heard, 1);
        let unit = if tv > 0.3 { "" } else { "BPM" };
        if let Some(bpm) = self.drag_field(c, input, id, field, self.tempo_here(), 0.25, 1.0, &bpm_text, unit) {
            let bpm = ((bpm * 10.0).round() / 10.0).clamp(40.0, 300.0);
            if self.set_tempo_here(bpm) {
                self.commit(tr("Tempo"));
            }
        }
        // Double-click or Enter: type the tempo; right-click: its menu (type, automate, controller).
        let typed = (self.hovered(input, id, field) && input.pressed && input.double) || self.kb_enter(id);
        if typed {
            self.drag = None;
            self.open_popup(Popup::Rename { target: Rename::Value(ponzik_engine::AutoTarget::Tempo), text: String::new() }, field);
        }
        if input.right_pressed && input.inside && field.contains(input.x, input.y) {
            let value = self.tempo_here();
            self.open_popup(Popup::Param { target: ponzik_engine::AutoTarget::Tempo, value, default: 120.0 }, Rect::new(field.x, field.bottom(), 0.0, 0.0));
        }
        let tip = if inline_tap {
            tr("Tempo at the playhead\nDrag or scroll · Shift: fine · double-click: type it · TAP on its right")
        } else {
            tr("Tempo at the playhead\nDrag or scroll · Shift: fine · double-click: type it")
        };
        self.tip_zone(input, field, tip, "");
        if tv > 0.02 {
            let pad = Rect::new(field.right() + 4.0, group.y, 36.0, group.h);
            let prev = c.push_clip(Rect::new(pad.x, group.y - 2.0, (group.right() - pad.x).max(0.0), group.h + 4.0));
            let k_tap = self.key_label(S::TapTempo);
            if tv > 0.5 {
                self.tap_button(c, input, pad, &k_tap);
            } else {
                self.tap_button(c, &Input { inside: false, pressed: false, ..input.clone() }, pad, &k_tap);
            }
            c.set_clip(Some(prev));
        }
    }

    /// The project's name; a click opens its menu (rename, save, open, export).
    fn project_name(&mut self, c: &mut Canvas, input: &Input, x: f32, cy: f32, room: f32) {
        let t = self.theme;
        let name = self.project.name.clone();
        let label = if self.modified { format!("{name} ·") } else { name.clone() };
        let w = (self.fonts.measure(self.f.sans, 13.0, &label) + 18.0).min(room);
        let area = Rect::new(x - 6.0, cy - 14.0, w + 12.0, 28.0);
        let id = wid("tb-project", 0, 0);
        let hov = self.hovered(input, id, area);
        let hv = self.hover_anim(id, hov);
        let open = matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::ProjectMenu));
        let on = self.anim.to(id ^ 0x4f4e, open as u8 as f32, T_HOVER);
        if hv.max(on) > 0.01 {
            c.fill_rrect(area, 8.0, t.raised.alpha(hv.max(on)));
        }
        if hov {
            self.set_tip(tr("Project\nClick: rename, save, open, export · double-click: rename"), "");
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        let kb = self.kb_control(c, input, id, area) && self.kb_enter(id);
        if hov && input.pressed && input.double {
            self.open_popup(Popup::Rename { target: Rename::Project, text: name }, area);
        } else if (hov && input.pressed) || kb {
            self.open_popup(Popup::ProjectMenu, Rect::new(area.x, area.bottom() + 6.0, area.w, 0.0));
        }
        self.text_fit(c, self.f.sans, 13.0, x, cy, &label, w - 18.0, t.text2.mix(t.text, hv.max(on)));
        let chev = (x + w - 8.0).min(x + room);
        draw_icon(c, Icon::ChevronDown, chev, cy + 1.0, 9.0, t.muted.alpha(0.4 + 0.6 * hv.max(on)));
    }

    // --- Menus ---

    /// The record button's menu: what goes with recording.
    pub(super) fn record_menu(&self) -> (String, Vec<MenuItem>) {
        let key = |s: S| self.key_label(s);
        let armed = self.autorec.mode != ponzik_project::auto_rec::AutoMode::Read;
        let v = vec![
            MenuItem::new(Icon::Mic, tr("Recording panel"), Action::ToggleRecordPanel).on(self.rec.panel).tip(tr("Input, level, monitoring, latency")),
            MenuItem::new(Icon::Record, tr("Count-in"), Action::ToggleCountIn).on(self.count_in).tip(tr("One bar of metronome before recording")),
            MenuItem::new(Icon::Metronome, tr("Metronome"), Action::ToggleMetronome).on(self.metronome).detail(key(S::Metronome)),
            MenuItem::new(Icon::Voice, tr("Voice to Notes"), Action::ToggleVoice).on(self.voice.enabled).tip(tr("Sing or hum to play the selected instrument")).sep(),
            MenuItem::new(Icon::Keyboard, tr("Musical typing"), Action::ToggleTyping).on(self.typing_keyboard).detail(key(S::TypingKeyboard)),
            MenuItem::new(Icon::Midi, tr("Capture MIDI"), Action::CaptureMidi).detail(key(S::Capture)).tip(tr("The last phrase played becomes a pattern")),
            MenuItem::new(Icon::Automation, tr("Record automation"), Action::ToggleAutoArm).on(armed).tip(tr("Moves are recorded while the song plays")).sep(),
            MenuItem::new(Icon::Automation, tr("Automation mode"), Action::Popup(Popup::AutoMode)).tip(tr("Touch, Latch, Write")),
        ];
        (tr("Recording").into(), v)
    }

    /// The project's menu, under its name.
    pub(super) fn project_menu(&self) -> (String, Vec<MenuItem>) {
        let key = |s: S| self.key_label(s);
        let rename = Popup::Rename { target: Rename::Project, text: self.project.name.clone() };
        let v = vec![
            MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)).detail(tr("double-click").into()),
            MenuItem::new(Icon::Save, tr("Save"), Action::Save).detail(key(S::Save)).sep(),
            MenuItem::new(Icon::Duplicate, tr("Save as"), Action::SaveAs).detail(key(S::SaveAs)),
            MenuItem::new(Icon::Folder, tr("Open"), Action::OpenMenu).detail(key(S::Open)),
            MenuItem::sub_of(Icon::History, tr("Open recent"), Popup::Recent).id("recent"),
            MenuItem::new(Icon::Plus, tr("New"), Action::NewProject).detail(key(S::NewProject)),
            MenuItem::sub(Icon::Export, tr("Export"), self.export_items()).id("export").sep(),
        ];
        (self.project.name.clone(), v)
    }

    /// The three playback modes.
    pub(super) fn mode_menu(&self) -> (String, Vec<MenuItem>) {
        let mode = self.project.mode;
        let k_mode = self.key_label(S::ToggleMode);
        let pat = PlayMode::Pattern(self.project.current_pattern);
        let v = vec![
            MenuItem::new(Icon::Rack, tr("Play the pattern"), Action::SetPlayMode(pat)).on(matches!(mode, PlayMode::Pattern(_))).detail(k_mode.clone()),
            MenuItem::new(Icon::Playlist, tr("Play the song"), Action::SetPlayMode(PlayMode::Song)).on(mode == PlayMode::Song).detail(k_mode),
            MenuItem::new(Icon::Session, tr("Play the session"), Action::SetPlayMode(PlayMode::Session)).on(mode == PlayMode::Session),
        ];
        (tr("Playback mode").into(), v)
    }
}

/// The word of the mode pill.
fn mode_label(mode: PlayMode) -> &'static str {
    match mode {
        PlayMode::Pattern(_) => tr("Pattern"),
        PlayMode::Song => tr("Song"),
        PlayMode::Session => tr("Session"),
    }
}

impl App {
    pub(super) fn pattern_selector(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.raised);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let n = self.project.patterns.len();
        let cur = self.project.current_pattern;
        let prev = Rect::new(r.x + 2.0, r.y + 2.0, 28.0, r.h - 4.0);
        let next = Rect::new(r.right() - 30.0, r.y + 2.0, 28.0, r.h - 4.0);
        use super::keymap::Shortcut as S;
        let (k_prev, k_next, k_new) = (self.key_label(S::PrevPattern), self.key_label(S::NextPattern), self.key_label(S::NewPattern));
        if self.icon_button(c, input, wid("pat-prev", 0, 0), prev, Icon::ChevronLeft, false, (tr("Previous pattern"), &k_prev)) && cur > 0 {
            self.select_pattern(cur - 1);
        }
        let next_tip = if cur + 1 < n { (tr("Next pattern"), k_next.as_str()) } else { (tr("New pattern"), k_new.as_str()) };
        if self.icon_button(c, input, wid("pat-next", 0, 0), next, Icon::ChevronRight, false, next_tip) {
            if cur + 1 < n {
                self.select_pattern(cur + 1);
            } else {
                let p = self.project.add_pattern();
                self.select_pattern(p);
                self.toast(tr("New pattern"));
            }
        }
        let mid = Rect::new(prev.right(), r.y, next.x - prev.right(), r.h);
        let id = wid("pat-name", 0, 0);
        let hov = self.hovered(input, id, mid);
        let hv = self.hover_anim(id, hov);
        if hov && (input.pressed || input.right_pressed) {
            self.open_popup(Popup::Patterns, r);
        }
        if hov {
            self.set_tip(tr("Patterns"), "");
        }
        if hov && input.wheel != 0.0 {
            let p = (cur as i32 - input.wheel.signum() as i32).clamp(0, n as i32 - 1) as usize;
            if p != cur {
                self.select_pattern(p);
            }
        }
        let name = self.project.patterns[cur].name.clone();
        let (cx, cy) = mid.center();
        c.fill_rrect(Rect::new(mid.x + 6.0, cy - 6.0, 3.0, 12.0), 1.5, self.pattern_tint(cur));
        let label = self.fit(self.f.sans_med, 13.0, &name, mid.w - 22.0);
        self.text_center(c, self.f.sans_med, 13.0, cx + 4.0, cy, &label, t.text2.mix(t.text, 0.6 + 0.4 * hv));
    }

    /// Pastille de la gamme du projet : clic pour la choisir, molette pour la tonalité.
    pub(super) fn scale_chip(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        self.scale_anchor = r;
        let id = wid("tb-scale", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 10.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (key, scale) = (self.project.key, self.project.scale);
        let chromatic = scale == ScaleKind::Chromatic;
        let cy = r.y + r.h * 0.5;
        draw_icon(c, Icon::Scale, r.x + 18.0, cy, 15.0, if chromatic { t.muted } else { t.accent });
        let label = scale_label(key, scale);
        self.text_fit(c, self.f.sans_med, 13.0, r.x + 34.0, cy, &label, r.w - 42.0, if chromatic { t.text2 } else { t.text });
        if hov {
            let tip = if chromatic {
                tr("Project scale\nNone: every note").to_string()
            } else {
                tr_fmt!("Project scale\n{} {} · scroll: key", key_name(key), tr(scale.name()).to_lowercase())
            };
            self.set_tip(&tip, "");
        }
        if hov && input.pressed {
            self.open_popup(Popup::Scale, r);
        }
        if hov && input.wheel != 0.0 && !chromatic {
            self.project.key = ((key as i32 + input.wheel.signum() as i32).rem_euclid(12)) as u8;
            self.commit(tr("Key"));
        }
    }


    fn position_field(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.raised);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (_, cy) = r.center();
        // Pendant le pré-compte : la mesure « -1 », comptée en rouge comme l'enregistrement.
        let counting = self.counting > 0.0;
        let lit_col = if counting { t.negative } else { t.accent };
        // Bars and beats of the signature map (song) or of the first signature.
        let map = self.project.tempo_map();
        let view = if self.project.mode == PlayMode::Song { map.view() } else { map.head() };
        let at = if counting {
            let sig = self.sig_here();
            let pos = (self.count_bar - self.counting).max(0.0);
            let beat = (pos / sig.beat_len() as f64).floor();
            ponzik_engine::BarBeat { bar: 0, beat: beat as u32, tick_in_beat: pos - beat * sig.beat_len() as f64, sig }
        } else {
            view.bar_beat(self.cycle_position())
        };
        let leds = (at.sig.num as usize).min(8);
        let gap = if leds <= 4 { 11.0 } else { 44.0 / leds as f32 * 0.98 + 1.0 };
        for b in 0..leds {
            let lit = self.playing && b == at.beat as usize;
            let v = self.anim.to_ambient(wid("beat-led", b, 0), lit as u8 as f32, T_HEAD);
            let rad = if leds <= 4 { 3.0 } else { 2.3 };
            c.fill_circle(r.x + 14.0 + b as f32 * gap, cy, rad, t.raised_hi.mix(lit_col, v));
        }
        let bar = if counting { "-1".to_string() } else { (at.bar + 1).to_string() };
        let beat = at.beat + 1;
        let step = (at.tick_in_beat / TICKS_PER_STEP as f64).floor() as u32 + 1;
        let text = format!("{bar}.{beat}.{step}");
        let ink = if counting { t.negative } else if self.playing { t.text } else { t.text2 };
        self.text_right(c, self.f.mono_med, 15.0, r.right() - 12.0, cy, &text, ink);
    }

    /// Bouton TAP : chaque appui cale le tempo sur la moyenne des 4 derniers ;
    /// les points dessous comptent les appuis pris en compte.
    fn tap_button(&mut self, c: &mut Canvas, input: &Input, r: Rect, key: &str) {
        let t = self.theme;
        let id = wid("tb-tap", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let kb = self.kb_control(c, input, id, r) && self.kb_enter(id);
        if (hov && input.pressed) || kb {
            self.tap_tempo();
        }
        let flash = self.anim.to(wid("tb-tap-flash", 0, 0), 0.0, T_LAYOUT);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.92 } else { 1.0 }, T_PRESS);
        let body = r.scale(press);
        c.fill_rrect(body, 10.0, t.raised.mix(t.raised_hi, hv).mix(t.accent, flash * 0.35));
        c.stroke_rrect(body, 10.0, 1.0, t.hairline.mix(t.accent, flash));
        let (cx, cy) = r.center();
        self.text_center(c, self.f.mono_med, 11.0, cx, cy - 3.0, tr("TAP"), t.text2.mix(t.text, hv).mix(t.accent, flash));
        let fresh = self.taps.last().is_some_and(|&last| self.time - last <= 2.0);
        let n = if fresh { self.taps.len() } else { 0 };
        for k in 0..4 {
            let on = self.anim.to(wid("tb-tap-dot", k, 0), (k < n) as u8 as f32, T_HOVER);
            c.fill_circle(cx - 7.5 + k as f32 * 5.0, cy + 8.0, 1.3, t.raised_hi.mix(t.accent, on));
        }
        if hov {
            self.set_tip(tr("Tap tempo\nTap the beat: average of the last 4 taps"), key);
        }
    }

    /// Bouton d'enregistrement, avec l'anneau du pré-compte.
    fn record_button(&mut self, c: &mut Canvas, input: &Input, rec: Rect, key: &str) {
        let t = self.theme;
        let rid = wid("tb-rec", 0, 0);
        let rhov = self.hovered(input, rid, rec);
        if self.kb_control(c, input, rid, rec) && self.kb_enter(rid) {
            self.record = !self.record;
        }
        let rhv = self.hover_anim(rid, rhov);
        let on = self.anim.to(wid("tb-rec-on", 0, 0), self.record as u8 as f32, T_HOVER);
        c.fill_rrect(rec, 8.0, t.raised.alpha(rhv.max(on * 0.6)));
        let (rx0, ry0) = rec.center();
        let pulse = if self.record && self.playing { 0.75 + 0.25 * (self.time * 6.0).sin().abs() } else { 1.0 };
        // Lueur : armé, elle respire ; en prise, elle bat.
        if on > 0.01 {
            let glow = on * if self.playing { pulse } else { 0.55 + 0.15 * (self.time * 2.0).sin() };
            for k in 1..=3 {
                c.fill_circle(rx0, ry0, 6.0 + 4.0 * k as f32, t.negative.alpha(0.07 * glow));
            }
        }
        c.fill_circle(rx0, ry0, 6.0, t.muted.mix(t.text2, rhv).mix(t.negative, on).alpha(pulse));
        // Anneau : pré-compte d'une mesure ; il se remplit pendant le décompte.
        let ci = self.anim.to(wid("tb-rec-ci", 0, 0), self.count_in as u8 as f32, T_HOVER);
        if ci > 0.01 {
            let ring = t.muted.mix(t.negative, on).alpha(0.55 * ci);
            c.stroke_circle(rx0, ry0, 10.5, 1.2, ring);
            if self.counting > 0.0 {
                let done = (1.0 - self.counting / self.count_bar.max(1.0)).clamp(0.0, 1.0) as f32;
                let a0 = -std::f32::consts::FRAC_PI_2;
                let pts: Vec<(f32, f32)> = (0..=32)
                    .map(|k| a0 + done * std::f32::consts::TAU * k as f32 / 32.0)
                    .map(|a| (rx0 + 10.5 * a.cos(), ry0 + 10.5 * a.sin()))
                    .collect();
                c.stroke_polyline(&pts, 2.0, t.negative);
            }
        }
        if rhov {
            let tip = if self.count_in {
                tr("Record notes and audio\nOne-bar count-in · right-click: count-in, controller")
            } else {
                tr("Record notes and audio\nNo count-in · right-click: count-in, controller")
            };
            self.set_tip(tip, key);
        }
        if rhov && input.pressed {
            self.record = !self.record;
            self.toast(if self.record { tr("Recording armed") } else { tr("Recording disarmed") });
        }
        self.transport_menu(c, input, rec, ponzik_project::midi_map::TransportAction::Record);
    }

    /// The pattern and the scale at the left of a view's toolbar, when the top
    /// bar does not show them; nothing past `limit`.
    pub(super) fn view_context(&mut self, c: &mut Canvas, input: &Input, header: Rect, limit: f32) {
        let mut x = header.x;
        if !self.bar_shown(Bar::Pattern) && x + 188.0 <= limit {
            self.pattern_selector(c, input, Rect::new(x, header.y, 188.0, header.h));
            x += 198.0;
        }
        if !self.bar_shown(Bar::Scale) && x + 128.0 <= limit {
            let r = Rect::new(x, header.y, 128.0, header.h);
            self.spot(Spot::Scale, r);
            self.scale_chip(c, input, r);
        }
    }

    /// The piano roll's scale button: a click on the icon snaps notes to the
    /// scale, a click on the name picks the key and the scale. Returns its width.
    pub(super) fn piano_scale_button(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        let scaled = self.project.scale != ScaleKind::Chromatic;
        let snap = Rect::new(r.x, r.y, 36.0, r.h);
        let tip = if scaled { (tr("Snap to scale"), "") } else { (tr("Snap to scale\nPick a scale first, on its right"), "") };
        self.spot(Spot::PianoSnap, snap);
        if self.bar_shown(Bar::Scale) {
            let tip = if scaled { tip } else { (tr("Snap to scale\nPick a scale first, at the top"), "") };
            if self.icon_button(c, input, wid("pr-scale", 0, 0), snap, Icon::Scale, self.piano.scale_snap, tip) {
                self.piano.scale_snap = !self.piano.scale_snap;
            }
            return snap.w;
        }
        let (key, scale) = (self.project.key, self.project.scale);
        let label = if scaled { scale_label(key, scale) } else { tr("Scale").to_string() };
        let lw = (self.fonts.measure(self.f.sans_med, 12.0, &label) + 32.0).min(132.0).round();
        let chip = Rect::new(r.x, r.y, snap.w + lw, r.h);
        let name = Rect::new(snap.right(), r.y + 4.0, lw - 4.0, r.h - 8.0);
        c.fill_rrect(chip, 10.0, t.bg_deep);
        c.stroke_rrect(chip, 10.0, 1.0, t.hairline);
        if self.icon_button(c, input, wid("pr-scale", 0, 0), snap.inset(2.0), Icon::Scale, self.piano.scale_snap, tip) {
            self.piano.scale_snap = !self.piano.scale_snap;
        }
        self.scale_anchor = chip;
        self.spot(Spot::Scale, name);
        let id = wid("pr-scale-name", 0, 0);
        let hov = self.hovered(input, id, name);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(name, 8.0, t.raised.alpha(0.6 * hv));
        let cy = name.y + name.h * 0.5;
        self.text_fit(c, self.f.sans_med, 12.0, name.x + 6.0, cy, &label, name.w - 22.0, if scaled { t.text2.mix(t.text, hv) } else { t.muted.mix(t.text2, hv) });
        draw_icon(c, Icon::ChevronDown, name.right() - 9.0, cy, 9.0, t.muted);
        if hov {
            let tip = if scaled {
                tr_fmt!("Project scale\n{} {} · scroll: key", key_name(key), tr(scale.name()).to_lowercase())
            } else {
                tr("Project scale\nNone: every note").to_string()
            };
            self.set_tip(&tip, "");
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed {
                self.open_popup(Popup::Scale, chip);
            }
            if input.wheel != 0.0 && scaled {
                self.project.key = ((key as i32 + input.wheel.signum() as i32).rem_euclid(12)) as u8;
                self.commit(tr("Key"));
            }
        }
        chip.w
    }

    /// The pattern being edited, in front of the piano roll's channel; a click
    /// lists the patterns, the wheel walks through them. Returns its width.
    pub(super) fn piano_pattern_tag(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        // A narrow piano roll keeps its room for the notes' tools.
        if self.bar_shown(Bar::Pattern) || r.w < 900.0 {
            return 0.0;
        }
        let t = self.theme;
        let cur = self.project.current_pattern;
        let name = self.project.patterns[cur].name.clone();
        let w = (self.fonts.measure(self.f.sans_med, 12.0, &name) + 40.0).clamp(64.0, 132.0).round();
        let tag = Rect::new(r.x, r.y, w, r.h);
        let id = wid("pr-pattern", 0, 0);
        let hov = self.hovered(input, id, tag);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(tag, 10.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(tag, 10.0, 1.0, t.hairline);
        let cy = tag.y + tag.h * 0.5;
        c.fill_rrect(Rect::new(tag.x + 10.0, cy - 7.0, 3.0, 14.0), 1.5, self.pattern_tint(cur));
        self.text_fit(c, self.f.sans_med, 12.0, tag.x + 20.0, cy, &name, w - 32.0, t.text2.mix(t.text, hv));
        draw_icon(c, Icon::ChevronDown, tag.right() - 11.0, cy, 9.0, t.muted);
        if hov {
            let (k_prev, k_next) = (self.key_label(S::PrevPattern), self.key_label(S::NextPattern));
            self.set_tip(&tr_fmt!("Pattern\nClick: every pattern · scroll or {} / {}: the next one", k_prev, k_next), "");
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed || input.right_pressed {
                self.open_popup(Popup::Patterns, tag);
            }
            let n = self.project.patterns.len();
            if input.wheel != 0.0 {
                let p = (cur as i32 - input.wheel.signum() as i32).clamp(0, n as i32 - 1) as usize;
                if p != cur {
                    self.select_pattern(p);
                }
            }
        }
        w
    }

    /// While the devices panel is hidden: a small tab at the bottom of the
    /// main area brings it back.
    pub(super) fn panel_tab(&mut self, c: &mut Canvas, input: &Input, main: Rect) {
        let t = self.theme;
        let id = wid("panel-tab", 0, 0);
        let zone = Rect::new(main.x + main.w * 0.5 - 40.0, main.bottom() - 16.0, 80.0, 16.0);
        let hov = self.hovered(input, id, zone);
        let hv = self.hover_anim(id, hov);
        let tab = Rect::new(zone.x + 12.0 - 6.0 * hv, main.bottom() - 13.0 - 3.0 * hv, zone.w - 24.0 + 12.0 * hv, 26.0);
        c.fill_rrect(tab, 8.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(tab, 8.0, 1.0, t.hairline);
        let (cx, _) = tab.center();
        // An upward chevron: the panel comes up from here.
        let (s, y) = (4.0 + hv, tab.y + 7.5);
        c.stroke_polyline(&[(cx - s, y + s * 0.5), (cx, y - s * 0.5), (cx + s, y + s * 0.5)], 1.6, t.muted.mix(t.accent, hv));
        if hov {
            let k_panel = self.key_label(S::TogglePanel);
            self.set_tip(tr("Show the devices"), &k_panel);
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed {
                self.panel = true;
            }
        }
    }
}
