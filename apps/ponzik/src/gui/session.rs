//! Vue Session, à la manière d'Ableton : une grille de clips lancés en direct,
//! quantifiés, et des scènes qui lancent une rangée entière.
//!
//! The player ([`ponzik_project::session::Player`]) plans every launch, gate
//! release and follow action ahead of time; this view draws it, feeds it the
//! mouse, the number keys and MIDI, and edits the launch settings of the
//! selected slot or scene in an inspector on the right.

use ponzik_engine::{PlayMode, TICKS_PER_BAR};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::midi_map::ControlTarget;
use ponzik_project::session::{Follow, FollowAction, LaunchMode, Quant};
use ponzik_project::{AudioClipDef, SESSION_COLUMNS, SESSION_SCENES};
use ponzik_ui::{Canvas, Color, Path, Rect};

use super::Input;
use super::app::{App, Popup};
use super::dnd::DropTarget;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const SCENE_W: f32 = 72.0;
const INSPECT_W: f32 = 262.0;
const SESSION_HELP: &str = tr_noop("Session
Click: launch the clip on the next boundary
Empty slot: pick a pattern, or drop an audio file
Keys 1 to 8: launch a scene
Hold right-click: clear the swept slots");
const HEAD_H: f32 = 30.0;
const STOP_H: f32 = 34.0;
/// Follow action waits offered, in bars (0: off).
const FOLLOW_BARS: [u16; 10] = [0, 1, 2, 4, 8, 12, 16, 24, 32, 64];

/// What the inspector shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionSel {
    /// The session's own settings.
    #[default]
    None,
    Slot(usize, usize),
    Scene(usize),
}

/// Session view state of the interface (not saved).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUi {
    pub sel: SessionSel,
    /// Slot whose button the mouse holds.
    pub mouse: Option<(usize, usize)>,
    /// Number keys held, by scene.
    pub keys: [bool; SESSION_SCENES],
}

impl SessionUi {
    pub fn release_all(&mut self) {
        self.mouse = None;
        self.keys = [false; SESSION_SCENES];
    }
}

/// Seed of the follow actions' chances: a new performance each launch of Ponzik.
pub fn session_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x5e55_1011, |d| d.as_nanos() as u64)
}

/// Scene of a number key (physical codes of 1 to 8 on the main row).
pub fn scene_key(code: u16) -> Option<usize> {
    (2..2 + SESSION_SCENES as u16).contains(&code).then(|| (code - 2) as usize)
}

/// Short label of a quantization on a segmented control.
fn quant_short(q: Quant) -> &'static str {
    if q == Quant::None { tr("Off") } else { q.id() }
}

/// Short label of a follow wait.
fn bars_label(bars: u16) -> String {
    match bars {
        0 => tr("Off").into(),
        1 => tr("1 bar").into(),
        n => tr_fmt!("{} bars", n),
    }
}

impl App {
    /// Clips of the player for the engine, sent when they changed (no undo step).
    fn push_session(&mut self) {
        let live = self.session.live(&self.project);
        if self.project.session_live != live {
            self.project.session_live = live;
            self.sync_session();
        }
    }

    fn ensure_session_mode(&mut self) {
        if self.project.mode != PlayMode::Session {
            self.set_mode(PlayMode::Session);
        }
        if !self.playing {
            self.start_marker = 0.0;
            self.play_stop();
        }
    }

    /// A slot's button goes down (mouse, MIDI).
    pub fn launch_clip(&mut self, col: usize, row: usize) {
        if !self.project.session_filled(col, row) {
            return;
        }
        let (pos, playing) = (self.position, self.playing && self.project.mode == PlayMode::Session);
        self.session.launch(&self.project, col, row, pos, playing);
        self.ensure_session_mode();
        self.push_session();
    }

    /// A slot's button comes up: gate and repeat slots let go.
    pub fn release_clip(&mut self, col: usize, row: usize) {
        self.session.release(&self.project, col, row, self.position, self.playing);
        self.push_session();
    }

    pub fn launch_scene(&mut self, row: usize) {
        let (pos, playing) = (self.position, self.playing && self.project.mode == PlayMode::Session);
        self.session.launch_scene(&self.project, row, pos, playing);
        // Gate and repeat slots of the scene play while its button is held.
        for col in 0..SESSION_COLUMNS {
            if self.project.session_filled(col, row) && self.project.session_slot(col, row).launch.held() {
                self.session.cols[col].held = Some(row);
            }
        }
        self.ensure_session_mode();
        self.push_session();
    }

    pub fn release_scene(&mut self, row: usize) {
        self.session.release_scene(&self.project, row, self.position, self.playing);
        self.push_session();
    }

    pub fn stop_column(&mut self, col: usize) {
        self.session.stop_column(&self.project, col, self.position, self.playing);
        self.push_session();
    }

    /// Planned steps whose tick has come happen; their follow actions are
    /// drawn. Every frame, whatever the view.
    pub(super) fn advance_session(&mut self) {
        if self.project.mode != PlayMode::Session {
            return;
        }
        if self.playing {
            self.session.advance(&self.project, self.position);
        }
        self.push_session();
    }

    /// Number keys: press launches a scene, release lets its gate slots go.
    pub(super) fn session_keys(&mut self, input: &Input) {
        if self.view != super::app::View::Session || input.mods.ctrl || input.mods.alt {
            return;
        }
        for &(code, down) in &input.physical {
            let Some(row) = scene_key(code) else { continue };
            match (down, self.session_ui.keys[row]) {
                (true, false) => {
                    self.session_ui.keys[row] = true;
                    self.session_ui.sel = SessionSel::Scene(row);
                    self.launch_scene(row);
                }
                (false, true) => {
                    self.session_ui.keys[row] = false;
                    self.release_scene(row);
                }
                // Auto-repeat.
                _ => {}
            }
        }
    }

    /// Starts or ends the arrangement record.
    pub fn toggle_arrangement_record(&mut self) {
        if self.session.recorder.is_some() {
            self.finish_arrangement_record();
        } else {
            let from = if self.playing && self.project.mode == PlayMode::Session { self.position } else { 0.0 };
            self.session.start_recording(from);
            self.toast(tr("Arrangement record: the launched clips go into the playlist"));
        }
    }

    /// Writes the recorded performance into the playlist (one undo step).
    pub(super) fn finish_arrangement_record(&mut self) {
        if self.session.recorder.is_none() {
            return;
        }
        let takes = self.session.finish_recording(self.position);
        let n = self.project.write_takes(&takes);
        if n > 0 {
            self.commit(tr("Arrangement record"));
            self.toast(&tr_fmt!("{} clips written to the playlist", n));
        } else {
            self.toast(tr("Arrangement record: nothing was played"));
        }
    }

    /// An audio file dropped on a slot: a loop of whole bars when it is one
    /// (warped), else as long as the file.
    pub fn add_session_audio(&mut self, path: &str, col: usize, row: usize) {
        if !self.ensure_sample(path) {
            return;
        }
        let sample = self.samples[path].clone();
        let secs = sample.duration() as f64;
        let bpm = ponzik_project::warp::detect_tempo(&sample);
        let warp = bpm.is_some_and(|b| ponzik_project::warp::is_loop(&sample, b));
        // The column's other audio clips set its track; else a free one.
        let used: Vec<usize> = self.project.channels.iter().map(|c| c.track).collect();
        let track = (0..SESSION_SCENES)
            .find_map(|r| self.project.session_slot(col, r).audio.as_ref().map(|a| a.track))
            .unwrap_or_else(|| (1..self.project.tracks.len()).find(|t| !used.contains(t) && !self.project.track_in_use(*t)).unwrap_or(1));
        let mut clip = AudioClipDef { path: path.to_string(), track, bpm: bpm.unwrap_or(0.0), warp, ..Default::default() };
        let tempo = self.project.session_tempo();
        let natural = clip.natural_len(secs, &tempo);
        clip.len = if warp { natural.div_ceil(TICKS_PER_BAR / 4).max(1) * (TICKS_PER_BAR / 4) } else { natural };
        let name = std::path::Path::new(path).file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        if !self.project.track_in_use(track) {
            self.project.tracks[track].name = name.clone();
        }
        self.project.set_session_audio(col, row, clip);
        self.session_ui.sel = SessionSel::Slot(col, row);
        self.commit(tr("Fill a slot"));
        self.toast(&tr_fmt!("{} in slot {} · {}", name, col + 1, row + 1));
    }

    /// Color and name of a slot's clip.
    fn slot_look(&self, col: usize, row: usize) -> (Color, String) {
        let slot = self.project.session_slot(col, row);
        let (base, name) = match (self.project.session_pattern(col, row), &slot.audio) {
            (Some(p), _) => (self.pattern_tint(p), self.project.patterns[p].name.clone()),
            (None, Some(a)) => (
                super::clip::audio_color(a, &self.theme),
                std::path::Path::new(&a.path).file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
            ),
            (None, None) => (self.theme.muted, String::new()),
        };
        (if slot.color != 0 { Color::hex(slot.color) } else { base }, name)
    }

    pub(super) fn session_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        // The mouse let go of a held slot.
        if let Some((col, row)) = self.session_ui.mouse
            && !input.down
        {
            self.session_ui.mouse = None;
            self.release_clip(col, row);
        }

        let stop_all = Rect::new(header.right() - 36.0, header.y, 36.0, header.h);
        if self.icon_button(c, input, wid("ses-stop-all", 0, 0), stop_all, Icon::Stop, false, (tr("Stop all"), "")) {
            self.session.stop_all(&self.project, self.position, self.playing);
            self.push_session();
        }
        let help = Rect::new(stop_all.x - 44.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("ses-help", 0, 0), help, Icon::Info, false, (tr(SESSION_HELP), ""));
        // Arrangement record: a red dot that pulses while it records.
        let rec = Rect::new(help.x - 44.0, header.y, 36.0, header.h);
        let recording = self.session.recorder.is_some();
        let rec_tip = if recording {
            tr("Stop the arrangement record\nThe launched clips are written into the playlist")
        } else {
            tr("Arrangement record\nWrites the clips you launch into the playlist, where they played")
        };
        if self.icon_button(c, input, wid("ses-rec", 0, 0), rec, Icon::Record, recording, (rec_tip, "")) {
            self.toggle_arrangement_record();
        }
        if recording {
            let k = 0.5 + 0.5 * (self.time * 5.0).sin();
            let (cx, cy) = rec.center();
            c.stroke_circle(cx, cy, 13.0, 2.0, t.negative.alpha(0.4 + 0.5 * k));
        }
        // Quantization of the session.
        let q_r = Rect::new(rec.x - 8.0 - 250.0, header.y + (header.h - 28.0) * 0.5, 250.0, 28.0);
        let labels: Vec<&str> = Quant::ALL.iter().map(|q| quant_short(*q)).collect();
        let tips: Vec<(String, &str)> =
            Quant::ALL.iter().map(|q| (tr_fmt!("Launch quantization: {}", tr(q.label())), "")).collect();
        let tips: Vec<(&str, &str)> = tips.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        let sel = Quant::ALL.iter().position(|q| *q == self.project.session_quant).unwrap_or(3);
        if let Some(i) = self.segmented_tips(c, input, wid("ses-quant", 0, 0), q_r, &labels, &tips, sel) {
            self.project.session_quant = Quant::ALL[i];
            self.commit(tr("Launch quantization"));
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(14.0);
        let inspect = Rect::new(inner.right() - INSPECT_W, inner.y, INSPECT_W, inner.h);
        let grid = Rect::new(inner.x + SCENE_W + 10.0, inner.y, inspect.x - 14.0 - (inner.x + SCENE_W + 10.0), inner.h);
        let col_w = ((grid.w - (SESSION_COLUMNS - 1) as f32 * 8.0) / SESSION_COLUMNS as f32).floor();
        let row_h = ((grid.h - HEAD_H - STOP_H - 8.0) / SESSION_SCENES as f32).floor().clamp(32.0, 64.0);
        let col_x = |k: usize| grid.x + k as f32 * (col_w + 8.0);
        let row_y = |r: usize| grid.y + HEAD_H + r as f32 * row_h;
        let pulse = 0.5 + 0.5 * (self.time * 8.0).sin();

        // En-têtes de colonnes.
        for k in 0..SESSION_COLUMNS {
            let label = (k + 1).to_string();
            let active = self.session.cols[k].playing.is_some();
            self.text_center(c, self.f.mono_med, 12.0, col_x(k) + col_w * 0.5, grid.y + 12.0, &label, if active { t.text } else { t.muted });
        }

        // Lanceurs de scène.
        let filled_scenes = self.project.session_filled_scenes();
        for (r, &filled) in filled_scenes.iter().enumerate() {
            let cell = Rect::new(inner.x, row_y(r) + 3.0, SCENE_W, row_h - 6.0);
            let id = wid("ses-scene", r, 0);
            let hov = self.hovered(input, id, cell);
            let hv = self.hover_anim(id, hov);
            let current = self.session.scene.is_some_and(|(s, _)| s == r);
            let queued = self.session.scene_queued.is_some_and(|(s, _)| s == r);
            let selected = self.session_ui.sel == SessionSel::Scene(r);
            c.fill_rrect(cell, 10.0, t.raised.mix(t.raised_hi, hv).mix(t.accent_deep, if current { 0.35 } else { 0.0 }));
            if selected {
                c.stroke_rrect(cell, 10.0, 1.5, t.accent.alpha(0.8));
            } else if queued {
                c.stroke_rrect(cell, 10.0, 1.5, t.accent.alpha(0.3 + 0.6 * pulse));
            }
            let icon_col = if current { t.accent } else { t.text2.mix(t.accent, hv) };
            draw_icon(c, Icon::Play, cell.x + 18.0, cell.y + cell.h * 0.5, 12.0, if filled { icon_col } else { t.muted });
            self.text_right(c, self.f.mono_med, 12.0, cell.right() - 12.0, cell.y + cell.h * 0.5, &(r + 1).to_string(), t.muted.mix(t.text2, hv));
            if self.project.session_scenes[r].follow.is_on() {
                draw_icon(c, Icon::ChevronRight, cell.right() - 30.0, cell.y + cell.h * 0.5, 10.0, t.select);
            }
            self.ctl_mark(c, (cell.right() - 6.0, cell.y + 6.0), 1.0, ControlTarget::Scene(r));
            if hov {
                self.set_tip(&tr_fmt!("Launch scene {}", r + 1), &(r + 1).to_string());
            }
            if hov && input.pressed {
                self.session_ui.sel = SessionSel::Scene(r);
                self.launch_scene(r);
                self.session_ui.mouse = None;
                // Held by the mouse: gate slots of the scene let go on release.
                if (0..SESSION_COLUMNS).any(|k| self.session.cols[k].held == Some(r)) {
                    self.session_ui.keys[r] = true;
                }
            }
            if self.session_ui.keys[r] && !input.down && hov && input.released {
                self.session_ui.keys[r] = false;
                self.release_scene(r);
            }
            if hov && input.right_pressed {
                self.open_popup(Popup::Control(ControlTarget::Scene(r)), cell);
            }
        }

        // Cases.
        for k in 0..SESSION_COLUMNS {
            let col_state = self.session.cols[k].clone();
            let live = self.project.session_live.iter().filter(|l| l.col == k).copied().collect::<Vec<_>>();
            for r in 0..SESSION_SCENES {
                let cell = Rect::new(col_x(k), row_y(r) + 3.0, col_w, row_h - 6.0);
                let id = wid("ses-cell", k, r);
                let hov = self.hovered(input, id, cell);
                let hv = self.hover_anim(id, hov);
                let filled = self.project.session_filled(k, r);
                self.drop_zone(input, cell, DropTarget::SessionSlot { col: k, row: r });
                let selected = self.session_ui.sel == SessionSel::Slot(k, r);
                if !filled {
                    c.stroke_rrect(cell, 10.0, 1.0, t.hairline.alpha(0.8 + hv * 2.0));
                    if hv > 0.01 {
                        draw_icon(c, Icon::Plus, cell.x + cell.w * 0.5, cell.y + cell.h * 0.5, 14.0, t.muted.alpha(hv));
                    }
                    if hov {
                        self.set_tip(tr("Pick a pattern\nor drop an audio file"), "");
                    }
                    if hov && input.pressed {
                        self.open_popup(Popup::SessionAssign { col: k, row: r }, cell);
                    }
                    continue;
                }
                let slot = self.project.session_slot(k, r).clone();
                let (col, name) = self.slot_look(k, r);
                let playing_now = col_state.playing.filter(|p| p.row == r);
                let queued = col_state.queued().is_some_and(|(row, _)| row == r);
                // Planned by a follow action: a faint outline.
                let planned = col_state.pending.iter().any(|x| x.follow && matches!(x.step, ponzik_project::session::Step::Play { row, .. } if row == r));
                let on = self.anim.to(wid("ses-on", k, r), playing_now.is_some() as u8 as f32, T_HOVER);
                c.fill_rrect(cell, 10.0, col.mix(t.panel, 0.7 - on * 0.35 - hv * 0.08));
                if queued {
                    c.stroke_rrect(cell, 10.0, 2.0, col.alpha(0.4 + 0.6 * pulse));
                } else if planned {
                    c.stroke_rrect(cell, 10.0, 1.0, col.alpha(0.55));
                }
                if selected {
                    c.stroke_rrect(cell.inset(-2.0), 12.0, 1.5, t.text.alpha(0.85));
                }
                // Triangle de lecture, plein quand le clip joue.
                let (tx, ty) = (cell.x + 16.0, cell.y + cell.h * 0.5);
                if slot.audio.is_some() {
                    draw_icon(c, Icon::Sample, tx + 1.0, ty, 12.0, if playing_now.is_some() { t.text } else { t.text2.alpha(0.8) });
                } else {
                    let mut tri = Path::new();
                    tri.polygon(&[(tx - 4.0, ty - 6.0), (tx + 6.0, ty), (tx - 4.0, ty + 6.0)]);
                    c.fill_path(&tri, if playing_now.is_some() { t.text } else { t.text2.alpha(0.7) });
                }
                // Badges: launch mode, legato, follow action.
                let mut badges = String::new();
                match slot.launch {
                    LaunchMode::Trigger => {}
                    LaunchMode::Gate => badges.push('G'),
                    LaunchMode::Toggle => badges.push('T'),
                    LaunchMode::Repeat => badges.push('R'),
                }
                if slot.legato {
                    badges.push('L');
                }
                let mut right = cell.right() - 8.0;
                if slot.follow.is_on() {
                    draw_icon(c, Icon::ChevronRight, right - 5.0, ty, 10.0, t.text);
                    right -= 14.0;
                }
                if !badges.is_empty() {
                    right = self.text_right(c, self.f.mono_med, 10.0, right, ty, &badges, t.text.alpha(0.8)) - 4.0;
                }
                self.text_fit(c, self.f.sans_med, 12.0, cell.x + 30.0, ty, &name, (right - cell.x - 32.0).max(10.0), t.text);
                // Progression dans la boucle du clip.
                if let Some(l) = live.iter().find(|l| l.row == r && (l.start as f64) <= self.position && self.position < l.end as f64) {
                    let len = self.project.session_len(k, r).max(1) as f64;
                    let slot_pos = ponzik_engine::SessionSlot { pattern: 0, start: l.start, end: l.end, offset: l.offset, period: l.period };
                    let f = (slot_pos.local(self.position).max(0.0) % len / len) as f32;
                    c.fill_rrect(Rect::new(cell.x + 6.0, cell.bottom() - 6.0, (cell.w - 12.0) * f, 3.0), 1.5, t.text);
                }
                self.ctl_mark(c, (cell.right() - 5.0, cell.y + 5.0), 1.0, ControlTarget::Slot { col: k, row: r });
                if hov && input.pressed {
                    self.session_ui.sel = SessionSel::Slot(k, r);
                    self.launch_clip(k, r);
                    if slot.launch.held() {
                        self.session_ui.mouse = Some((k, r));
                    }
                }
                if hov && input.right_down && self.drag.is_none() {
                    let active = playing_now.is_some() || queued;
                    self.project.clear_session_slot(k, r);
                    if active {
                        self.stop_column(k);
                    }
                    self.commit(tr("Clear slot"));
                }
            }
            // Arrêt de la colonne.
            let stop = Rect::new(col_x(k), row_y(SESSION_SCENES) + 6.0, col_w, STOP_H - 6.0);
            let id = wid("ses-stop", k, 0);
            let hov = self.hovered(input, id, stop);
            let hv = self.hover_anim(id, hov);
            let active = col_state.playing.is_some() || !col_state.pending.is_empty();
            let stopping = col_state.stopping();
            c.fill_rrect(stop, 8.0, t.raised.alpha(0.5 + hv * 0.5));
            let col = if stopping { t.text.alpha(0.4 + 0.6 * pulse) } else if active { t.text } else { t.muted };
            draw_icon(c, Icon::Stop, stop.x + stop.w * 0.5, stop.y + stop.h * 0.5, 11.0, col);
            if hov {
                self.set_tip(tr("Stop column"), "");
            }
            if hov && input.pressed {
                self.stop_column(k);
            }
        }

        // Empty grid: one sentence on how to start, over the slots.
        if self.project.session_grid.iter().all(|col| col.iter().all(Option::is_none)) && !filled_scenes.iter().any(|&f| f) {
            let line = tr("Click a slot to put a pattern in it, then click the clip to launch it on the next bar");
            let w = (self.fonts.measure(self.f.sans, 13.0, line) + 64.0).min(grid.w - 24.0);
            let (cx, cy) = (grid.x + grid.w * 0.5, row_y(SESSION_SCENES / 2) - 3.0);
            let card = Rect::new((cx - w * 0.5).round(), cy - 22.0, w.round(), 44.0);
            c.fill_rrect(card, 12.0, t.raised);
            c.stroke_rrect(card, 12.0, 1.0, t.hairline);
            draw_icon(c, Icon::Plus, card.x + 22.0, cy, 14.0, t.accent);
            let line = self.fit(self.f.sans, 13.0, line, card.w - 52.0);
            self.text(c, self.f.sans, 13.0, card.x + 40.0, cy, &line, t.text2);
        }

        self.session_inspector(c, input, inspect);
    }

    // --- Inspector ---

    fn session_inspector(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 12.0, t.bg_deep.mix(t.panel, 0.4));
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
        let r = r.inset(12.0);
        match self.session_ui.sel {
            SessionSel::Slot(col, row) if self.project.session_filled(col, row) => self.slot_inspector(c, input, r, col, row),
            SessionSel::Scene(row) => self.scene_inspector(c, input, r, row),
            _ => {
                self.text(c, self.f.sans_med, 14.0, r.x, r.y + 10.0, tr("Session"), t.text);
                let lines = [
                    tr("Select a clip or a scene to set how it launches."),
                    tr("Keys 1 to 8 launch the scenes."),
                    tr("The red dot records what you launch into the playlist."),
                ];
                let mut y = r.y + 40.0;
                for l in lines {
                    for part in self.wrap(self.f.sans, 12.0, l, r.w) {
                        self.text(c, self.f.sans, 12.0, r.x, y, &part, t.text2);
                        y += 18.0;
                    }
                    y += 8.0;
                }
            }
        }
    }

    /// Section label of the inspector; returns the y below it.
    fn inspector_label(&mut self, c: &mut Canvas, x: f32, y: f32, label: &str) -> f32 {
        self.text(c, self.f.sans_med, 11.0, x, y + 7.0, label, self.theme.muted);
        y + 18.0
    }

    /// `[<] label [>]`: −1, +1 or 0.
    fn stepper(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, label: &str, tip: &str) -> i32 {
        let t = self.theme;
        c.fill_rrect(r, 8.0, t.bg_deep);
        c.stroke_rrect(r, 8.0, 1.0, t.hairline);
        let left = Rect::new(r.x, r.y, 26.0, r.h);
        let right = Rect::new(r.right() - 26.0, r.y, 26.0, r.h);
        let mut step = 0;
        for (i, (b, icon, d)) in [(left, Icon::ChevronLeft, -1), (right, Icon::ChevronRight, 1)].into_iter().enumerate() {
            let bid = id ^ ((i as u64 + 1) * 0x9e37);
            let hov = self.hovered(input, bid, b);
            let hv = self.hover_anim(bid, hov);
            draw_icon(c, icon, b.x + b.w * 0.5, b.y + b.h * 0.5, 12.0, t.muted.mix(t.text, hv));
            if hov {
                self.set_tip(tip, "");
                if input.pressed {
                    step = d;
                }
            }
        }
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 && step == 0 {
            step = if input.wheel > 0.0 { 1 } else { -1 };
        }
        self.text_center(c, self.f.sans_med, 12.0, r.x + r.w * 0.5, r.y + r.h * 0.5, label, t.text);
        step
    }

    /// Follow action editor (wait, A, B, chance); the new settings if changed.
    fn follow_editor(&mut self, c: &mut Canvas, input: &Input, tag: &str, x: f32, y: f32, w: f32, f: Follow, scene: bool) -> (Option<Follow>, f32) {
        let t = self.theme;
        let mut out = f;
        let mut y = self.inspector_label(c, x, y, tr("Follow action"));
        let wait = Rect::new(x, y, w, 26.0);
        let i = FOLLOW_BARS.iter().position(|&b| b >= f.bars).unwrap_or(0);
        let label = if f.bars == 0 { tr("Off").to_string() } else { tr_fmt!("After {}", bars_label(f.bars)) };
        let tip = if scene { tr("When the scene's follow action fires") } else { tr("When the clip's follow action fires") };
        let d = self.stepper(c, input, wid(tag, 0, 0), wait, &label, tip);
        if d != 0 {
            out.bars = FOLLOW_BARS[(i as i32 + d).clamp(0, FOLLOW_BARS.len() as i32 - 1) as usize];
        }
        y += 32.0;
        let dim = if f.is_on() { 1.0 } else { 0.45 };
        for (k, letter) in ["A", "B"].into_iter().enumerate() {
            let action = if k == 0 { f.a } else { f.b };
            self.text(c, self.f.mono_med, 12.0, x + 4.0, y + 13.0, letter, t.accent.alpha(dim));
            let r = Rect::new(x + 22.0, y, w - 22.0, 26.0);
            let label = match (action, scene) {
                (FollowAction::Next, true) => tr("Next scene").to_string(),
                (FollowAction::Previous, true) => tr("Previous scene").to_string(),
                (a, _) => tr(a.label()).to_string(),
            };
            let d = self.stepper(c, input, wid(tag, 1 + k, 0), r, &label, tr("What happens then"));
            if d != 0 {
                let i = FollowAction::ALL.iter().position(|a| *a == action).unwrap_or(0) as i32;
                let next = FollowAction::ALL[(i + d).rem_euclid(FollowAction::ALL.len() as i32) as usize];
                if k == 0 { out.a = next } else { out.b = next }
            }
            y += 30.0;
        }
        // Chance of A against B: drag along the bar.
        let bar = Rect::new(x, y + 4.0, w, 22.0);
        let id = wid(tag, 9, 0);
        let hov = self.hovered(input, id, bar);
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
        }
        if (self.is_dragging(id) && input.down) || (hov && input.pressed) {
            out.chance_a = ((((input.x - bar.x) / bar.w).clamp(0.0, 1.0) * 20.0).round() * 5.0) as u8;
        }
        let a = f.chance_a.min(100) as f32 / 100.0;
        c.fill_rrect(bar, 7.0, t.select.alpha(0.25 * dim));
        if a > 0.0 {
            c.fill_rrect(Rect::new(bar.x, bar.y, (bar.w * a).max(14.0), bar.h), 7.0, t.accent.alpha(0.55 * dim));
        }
        self.text(c, self.f.mono_med, 11.0, bar.x + 8.0, bar.y + bar.h * 0.5, &format!("A {}%", f.chance_a), t.text.alpha(dim));
        self.text_right(c, self.f.mono_med, 11.0, bar.right() - 8.0, bar.y + bar.h * 0.5, &format!("{}% B", 100 - f.chance_a.min(100)), t.text.alpha(dim));
        if hov {
            self.set_tip(tr("Chance of A, against B\nDrag along the bar"), "");
        }
        y = bar.bottom() + 10.0;
        ((out != f).then_some(out), y)
    }

    fn slot_inspector(&mut self, c: &mut Canvas, input: &Input, r: Rect, col: usize, row: usize) {
        let t = self.theme;
        let slot = self.project.session_slot(col, row).clone();
        let (color, name) = self.slot_look(col, row);
        c.fill_rrect(Rect::new(r.x, r.y + 3.0, 4.0, 16.0), 2.0, color);
        self.text_fit(c, self.f.sans_med, 14.0, r.x + 12.0, r.y + 11.0, &name, r.w - 52.0, t.text);
        self.text(c, self.f.mono, 10.0, r.x + 12.0, r.y + 28.0, &tr_fmt!("Track {} · scene {}", col + 1, row + 1), t.muted);
        // MIDI: link the slot to a pad.
        let midi = Rect::new(r.right() - 28.0, r.y, 28.0, 28.0);
        let target = ControlTarget::Slot { col, row };
        if self.icon_button(c, input, wid("ses-midi", col, row), midi, Icon::Midi, self.ctl_link(target).is_some(), (tr("Link to a MIDI pad"), "")) {
            self.open_popup(Popup::Control(target), midi);
        }
        self.ctl_mark(c, (midi.right() - 4.0, midi.y + 4.0), 1.0, target);
        let mut y = r.y + 44.0;
        let mut edited = None;

        y = self.inspector_label(c, r.x, y, tr("Launch"));
        let labels: Vec<&str> = LaunchMode::ALL.iter().map(|m| tr(m.label())).collect();
        let tips = [
            (tr("Trigger: a press starts the clip"), ""),
            (tr("Gate: the clip plays while held (mouse, key or pad)"), ""),
            (tr("Toggle: a press starts the clip, the next one stops it"), ""),
            (tr("Repeat: while held, the clip starts over on each quantization step"), ""),
        ];
        let sel = LaunchMode::ALL.iter().position(|m| *m == slot.launch).unwrap_or(0);
        if let Some(i) = self.segmented_tips(c, input, wid("ses-launch", col, row), Rect::new(r.x, y, r.w, 28.0), &labels, &tips, sel) {
            edited = Some((tr("Launch mode"), ponzik_project::SlotDef { launch: LaunchMode::ALL[i], ..slot.clone() }));
        }
        y += 38.0;

        y = self.inspector_label(c, r.x, y, tr("Quantization"));
        // The session's, or one of its own.
        let sel = slot.quant.map_or(0, |q| 1 + Quant::ALL.iter().position(|x| *x == q).unwrap_or(0));
        let label = match slot.quant {
            None => tr_fmt!("Session ({})", tr(self.project.session_quant.label())),
            Some(q) => tr(q.label()).to_string(),
        };
        let d = self.stepper(c, input, wid("ses-squant", col, row), Rect::new(r.x, y, r.w, 26.0), &label, tr("Where the clip's launches land"));
        if d != 0 {
            let i = (sel as i32 + d).clamp(0, Quant::ALL.len() as i32) as usize;
            let quant = (i > 0).then(|| Quant::ALL[i - 1]);
            edited = Some((tr("Launch quantization"), ponzik_project::SlotDef { quant, ..slot.clone() }));
        }
        y += 36.0;

        let legato = Rect::new(r.x, y, 110.0, 26.0);
        if self.chip(c, input, wid("ses-legato", col, row), legato, tr("Legato"), slot.legato, tr("Legato: the clip takes over where the column's clip is")) {
            edited = Some((tr("Legato"), ponzik_project::SlotDef { legato: !slot.legato, ..slot.clone() }));
        }
        if let Some(a) = &slot.audio {
            let warp = Rect::new(legato.right() + 8.0, y, 110.0, 26.0);
            if self.chip(c, input, wid("ses-warp", col, row), warp, tr("Warp"), a.warp, tr("Warp: the loop follows the tempo")) {
                let audio = AudioClipDef { warp: !a.warp, ..a.clone() };
                edited = Some((tr("Warp"), ponzik_project::SlotDef { audio: Some(audio), ..slot.clone() }));
            }
        }
        y += 36.0;

        if let Some(a) = &slot.audio {
            y = self.inspector_label(c, r.x, y, tr("Loop length"));
            let beats = a.len as f32 / (TICKS_PER_BAR / 4) as f32;
            let label = tr_fmt!("{} beats", ponzik_i18n::num(beats as f64, 2));
            let d = self.stepper(c, input, wid("ses-len", col, row), Rect::new(r.x, y, r.w, 26.0), &label, tr("Length of the loop, in beats"));
            if d != 0 {
                let len = (a.len as i64 + d as i64 * (TICKS_PER_BAR / 4) as i64).max((TICKS_PER_BAR / 4) as i64) as u32;
                let audio = AudioClipDef { len, ..a.clone() };
                edited = Some((tr("Loop length"), ponzik_project::SlotDef { audio: Some(audio), ..slot.clone() }));
            }
            y += 36.0;
        }

        let (follow, ny) = self.follow_editor(c, input, "ses-follow", r.x, y, r.w, slot.follow, false);
        if let Some(follow) = follow {
            edited = Some((tr("Follow action"), ponzik_project::SlotDef { follow, ..slot.clone() }));
        }
        y = ny;

        y = self.inspector_label(c, r.x, y, tr("Color"));
        if let Some(hex) = self.swatches(c, input, "ses-color", r.x, y + 8.0, slot.color) {
            edited = Some((tr("Clip color"), ponzik_project::SlotDef { color: hex, ..slot.clone() }));
        }

        if let Some((label, new)) = edited
            && let Some(s) = self.project.session_slot_mut(col, row)
        {
            *s = new;
            self.commit(label);
        }
    }

    fn scene_inspector(&mut self, c: &mut Canvas, input: &Input, r: Rect, row: usize) {
        let t = self.theme;
        self.text(c, self.f.sans_med, 14.0, r.x, r.y + 11.0, &tr_fmt!("Scene {}", row + 1), t.text);
        let n = (0..SESSION_COLUMNS).filter(|&k| self.project.session_filled(k, row)).count();
        self.text(c, self.f.mono, 10.0, r.x, r.y + 28.0, &tr_fmt!("{} clips · launched with key {}", n, row + 1), t.muted);
        let midi = Rect::new(r.right() - 28.0, r.y, 28.0, 28.0);
        let target = ControlTarget::Scene(row);
        if self.icon_button(c, input, wid("ses-midi-scene", row, 0), midi, Icon::Midi, self.ctl_link(target).is_some(), (tr("Link to a MIDI pad"), "")) {
            self.open_popup(Popup::Control(target), midi);
        }
        self.ctl_mark(c, (midi.right() - 4.0, midi.y + 4.0), 1.0, target);
        let follow = self.project.session_scenes[row].follow;
        let (edited, y) = self.follow_editor(c, input, "ses-sfollow", r.x, r.y + 44.0, r.w, follow, true);
        if let Some(f) = edited {
            self.project.session_scenes[row].follow = f;
            self.commit(tr("Scene follow action"));
        }
        let line = tr("The scene's clips launch together, on the session's quantization; empty slots stop their column.");
        let mut y = y + 6.0;
        for part in self.wrap(self.f.sans, 12.0, line, r.w) {
            self.text(c, self.f.sans, 12.0, r.x, y, &part, t.text2);
            y += 18.0;
        }
    }

    /// Screenshot setup (`PONZIK_SESSION=slot:c,r|scene:r|rec|follow|audio`,
    /// `;`-separated): launch settings and a performance in progress.
    pub fn screenshot_session(&mut self, spec: &str) {
        for part in spec.split(';').map(str::trim) {
            let (key, arg) = part.split_once(':').unwrap_or((part, ""));
            let nums: Vec<usize> = arg.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            match key {
                "slot" if nums.len() == 2 => self.session_ui.sel = SessionSel::Slot(nums[0], nums[1]),
                "scene" if !nums.is_empty() => self.session_ui.sel = SessionSel::Scene(nums[0]),
                "follow" => {
                    for (col, row, a, b, chance) in [(0, 0, FollowAction::Next, FollowAction::Again, 70), (6, 0, FollowAction::Any, FollowAction::Stop, 85)] {
                        if let Some(s) = self.project.session_slot_mut(col, row) {
                            s.follow = Follow { bars: 4, a, b, chance_a: chance };
                        }
                    }
                    if let Some(s) = self.project.session_slot_mut(0, 1) {
                        (s.launch, s.legato, s.color) = (LaunchMode::Gate, true, 0xf0b44a);
                    }
                    if let Some(s) = self.project.session_slot_mut(3, 1) {
                        (s.launch, s.quant) = (LaunchMode::Repeat, Some(Quant::Beat));
                    }
                    self.project.session_scenes[1].follow = Follow { bars: 8, a: FollowAction::Next, b: FollowAction::First, chance_a: 60 };
                    self.commit(tr("Follow action"));
                }
                "audio" => {
                    let path = std::env::temp_dir().join("ponzik-session-loop.wav");
                    let sr = 48_000u32;
                    let n = sr as usize * 2;
                    // Two seconds of decaying blips, interleaved stereo.
                    let tone: Vec<f32> = (0..2 * n).map(|i| 0.3 * (i as f32 * 0.025).sin() * (1.0 - (i / 2 % 12_000) as f32 / 12_000.0)).collect();
                    if ponzik_project::write_wav(&path, &tone, sr, ponzik_project::BitDepth::Pcm16).is_ok() {
                        self.add_session_audio(&path.to_string_lossy(), 2, 5);
                    }
                }
                "rec" => {
                    self.session.start_recording(0.0);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_midi::MidiEvent;
    use ponzik_project::LiveClip;
    use ponzik_project::midi_map::{CtlMode, CtlSource, MidiMapping};

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        let mut app = App::new(handle, project, 48_000);
        app.view = super::super::app::View::Session;
        app.project.set_session_pattern(0, 0, 0);
        app.project.set_session_pattern(1, 0, 1);
        app.project.session_slot_mut(1, 0).unwrap().launch = LaunchMode::Gate;
        app.commit("setup");
        app
    }

    fn keys(app: &mut App, physical: Vec<(u16, bool)>) {
        let input = Input { physical, ..Input::default() };
        app.session_keys(&input);
    }

    #[test]
    fn number_keys_launch_scenes_and_release_their_gates() {
        let mut app = app();
        keys(&mut app, vec![(2, true)]);
        assert!(app.playing);
        assert_eq!(app.project.mode, PlayMode::Session);
        assert_eq!(app.session_ui.sel, SessionSel::Scene(0));
        assert_eq!(app.project.session_live.len(), 2);
        app.position = 0.0;
        app.advance_session();
        // Auto-repeat does nothing; the release lets the gate column go on the next bar.
        keys(&mut app, vec![(2, false), (2, true)]);
        app.position = 100.0;
        keys(&mut app, vec![(2, false)]);
        let gate = app.project.session_live.iter().find(|l| l.col == 1).copied();
        assert_eq!(gate, Some(LiveClip { col: 1, row: 0, start: 0, end: TICKS_PER_BAR, offset: 0, period: 0 }));
        assert!(app.project.session_live.iter().any(|l| l.col == 0 && l.end == u32::MAX));
    }

    #[test]
    fn midi_pads_hold_gate_slots() {
        let mut app = app();
        let pad = CtlSource::Note { channel: 9, key: 36 };
        app.ctl.global.push(MidiMapping::new(pad, ControlTarget::Slot { col: 1, row: 0 }, CtlMode::Toggle));
        app.handle_midi(&mut vec![MidiEvent::NoteOn { channel: 9, key: 36, velocity: 100 }]);
        assert_eq!(app.session.cols[1].held, Some(0));
        app.position = 0.0;
        app.advance_session();
        app.position = 500.0;
        app.handle_midi(&mut vec![MidiEvent::NoteOff { channel: 9, key: 36, velocity: 0 }]);
        assert_eq!(app.project.session_live, [LiveClip { col: 1, row: 0, start: 0, end: 2 * TICKS_PER_BAR, offset: 0, period: 0 }]);
        assert!(app.held.is_empty(), "a linked pad plays no note");
    }

    #[test]
    fn arrangement_record_writes_clips_in_one_undo_step() {
        let mut app = app();
        app.launch_clip(0, 0);
        app.toggle_arrangement_record();
        app.position = 0.0;
        app.advance_session();
        app.position = 700.0;
        let clips = app.project.clips.len();
        app.toggle_arrangement_record();
        assert!(app.session.recorder.is_none());
        let pattern = app.project.session_pattern(0, 0).unwrap();
        assert_eq!(app.project.clips.len(), clips + 1);
        assert_eq!(app.project.clips[clips], ponzik_project::ClipDef { pattern, lane: 0, start: 0, len: 700, offset: 0, muted: false });
        app.undo();
        assert_eq!(app.project.clips.len(), clips);
        // Stopping the transport ends a recording too.
        app.toggle_arrangement_record();
        app.position = 384.0;
        app.stop();
        assert!(app.session.recorder.is_none());
        assert!(app.session.is_idle());
    }
}
