//! Vue Session, à la manière d'Ableton : une grille de clips lancés en direct,
//! quantifiés à la mesure, et des scènes qui lancent une rangée entière.

use ponzik_engine::{PlayMode, SessionSlot, TICKS_PER_BAR};
use ponzik_project::{SESSION_COLUMNS, SESSION_SCENES};
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::{App, Popup};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const SCENE_W: f32 = 72.0;
const SESSION_HELP: &str = "Session
Clic : lancer le clip à la prochaine mesure
Case vide : choisir un pattern
Clic droit maintenu : vider les cases balayées";
const HEAD_H: f32 = 30.0;
const STOP_H: f32 = 34.0;

/// Clip en cours dans une colonne : scène, début, et la case qui va le remplacer.
#[derive(Clone, Copy, Debug, Default)]
pub struct ColumnState {
    pub playing: Option<(usize, u32)>,
    pub queued: Option<(usize, u32)>,
}

impl App {
    /// Prochaine mesure (ou 0 si le transport est arrêté).
    fn next_bar(&self) -> u32 {
        if !self.playing {
            return 0;
        }
        let bar = TICKS_PER_BAR as f64;
        ((self.position / bar).floor() as u32 + 1) * TICKS_PER_BAR
    }

    /// Recalcule les clips de session envoyés au moteur, sans étape d'annulation.
    fn push_session(&mut self) {
        let grid = self.project.session_grid.clone();
        let mut live = Vec::new();
        for (col, st) in self.session_cols.iter().enumerate() {
            let next_start = st.queued.map_or(u32::MAX, |(_, s)| s);
            if let Some((row, start)) = st.playing
                && let Some(p) = grid[col][row]
            {
                live.push(SessionSlot { pattern: p, start, end: next_start });
            }
            if let Some((row, start)) = st.queued
                && let Some(p) = grid[col][row]
            {
                live.push(SessionSlot { pattern: p, start, end: u32::MAX });
            }
        }
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

    pub fn launch_clip(&mut self, col: usize, row: usize) {
        let start = self.next_bar();
        self.ensure_session_mode();
        self.session_cols[col].queued = Some((row, start));
        self.push_session();
    }

    pub fn launch_scene(&mut self, row: usize) {
        let start = self.next_bar();
        self.ensure_session_mode();
        for col in 0..SESSION_COLUMNS {
            if self.project.session_grid[col][row].is_some() {
                self.session_cols[col].queued = Some((row, start));
            } else {
                self.session_cols[col] = ColumnState::default();
            }
        }
        self.push_session();
    }

    pub fn stop_column(&mut self, col: usize) {
        self.session_cols[col] = ColumnState::default();
        self.push_session();
    }

    /// Les clips en attente deviennent « en cours » quand leur mesure arrive.
    fn advance_session(&mut self) {
        let pos = self.position as u32;
        let mut changed = false;
        for st in &mut self.session_cols {
            if let Some((row, start)) = st.queued
                && pos >= start
                && self.playing
            {
                st.playing = Some((row, start));
                st.queued = None;
                changed = true;
            }
        }
        if changed {
            self.push_session();
        }
    }

    pub(super) fn session_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        self.advance_session();

        let stop_all = Rect::new(header.right() - 36.0, header.y, 36.0, header.h);
        if self.icon_button(c, input, wid("ses-stop-all", 0, 0), stop_all, Icon::Stop, false, ("Tout arrêter", "")) {
            for col in 0..SESSION_COLUMNS {
                self.session_cols[col] = ColumnState::default();
            }
            self.push_session();
        }
        let help = Rect::new(stop_all.x - 44.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("ses-help", 0, 0), help, Icon::Info, false, (SESSION_HELP, ""));

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(14.0);
        let grid = Rect::new(inner.x + SCENE_W + 10.0, inner.y, inner.w - SCENE_W - 10.0, inner.h);
        let col_w = ((grid.w - (SESSION_COLUMNS - 1) as f32 * 8.0) / SESSION_COLUMNS as f32).floor();
        let row_h = ((grid.h - HEAD_H - STOP_H - 8.0) / SESSION_SCENES as f32).floor().clamp(32.0, 64.0);
        let col_x = |k: usize| grid.x + k as f32 * (col_w + 8.0);
        let row_y = |r: usize| grid.y + HEAD_H + r as f32 * row_h;
        let bar = TICKS_PER_BAR as f64;
        let pulse = 0.5 + 0.5 * (self.time * 8.0).sin();

        // En-têtes de colonnes.
        for k in 0..SESSION_COLUMNS {
            let label = (k + 1).to_string();
            let active = self.session_cols[k].playing.is_some();
            self.text_center(c, self.f.mono_med, 12.0, col_x(k) + col_w * 0.5, grid.y + 12.0, &label, if active { t.text } else { t.muted });
        }

        // Lanceurs de scène.
        for r in 0..SESSION_SCENES {
            let cell = Rect::new(inner.x, row_y(r) + 3.0, SCENE_W, row_h - 6.0);
            let id = wid("ses-scene", r, 0);
            let hov = self.hovered(input, id, cell);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(cell, 10.0, t.raised.mix(t.raised_hi, hv));
            draw_icon(c, Icon::Play, cell.x + 20.0, cell.y + cell.h * 0.5, 12.0, t.text2.mix(t.accent, hv));
            self.text_right(c, self.f.mono_med, 12.0, cell.right() - 14.0, cell.y + cell.h * 0.5, &(r + 1).to_string(), t.muted.mix(t.text2, hv));
            if hov {
                self.set_tip(&format!("Lancer la scène {}", r + 1), "");
            }
            if hov && input.pressed {
                self.launch_scene(r);
            }
        }

        // Cases.
        for k in 0..SESSION_COLUMNS {
            for r in 0..SESSION_SCENES {
                let cell = Rect::new(col_x(k), row_y(r) + 3.0, col_w, row_h - 6.0);
                let id = wid("ses-cell", k, r);
                let hov = self.hovered(input, id, cell);
                let hv = self.hover_anim(id, hov);
                let pattern = self.project.session_grid[k][r];
                let st = self.session_cols[k];
                let playing = st.playing.is_some_and(|(row, _)| row == r);
                let queued = st.queued.is_some_and(|(row, _)| row == r);
                match pattern {
                    None => {
                        c.stroke_rrect(cell, 10.0, 1.0, t.hairline.alpha(0.8 + hv * 2.0));
                        if hv > 0.01 {
                            draw_icon(c, Icon::Plus, cell.x + cell.w * 0.5, cell.y + cell.h * 0.5, 14.0, t.muted.alpha(hv));
                        }
                        if hov {
                            self.set_tip("Choisir un pattern", "");
                        }
                        if hov && input.pressed {
                            self.open_popup(Popup::SessionAssign { col: k, row: r }, cell);
                        }
                    }
                    Some(p) => {
                        let col = self.pattern_tint(p);
                        let on = self.anim.to(wid("ses-on", k, r), playing as u8 as f32, T_HOVER);
                        c.fill_rrect(cell, 10.0, col.mix(t.panel, 0.7 - on * 0.35 - hv * 0.08));
                        if queued {
                            c.stroke_rrect(cell, 10.0, 2.0, col.alpha(0.4 + 0.6 * pulse));
                        }
                        // Triangle de lecture, plein quand le clip joue.
                        let (tx, ty) = (cell.x + 16.0, cell.y + cell.h * 0.5);
                        let mut tri = Path::new();
                        tri.polygon(&[(tx - 4.0, ty - 6.0), (tx + 6.0, ty), (tx - 4.0, ty + 6.0)]);
                        c.fill_path(&tri, if playing { t.text } else { t.text2.alpha(0.7) });
                        let name = self.project.patterns.get(p).map_or(String::new(), |pt| pt.name.clone());
                        self.text_fit(c, self.f.sans_med, 12.0, cell.x + 30.0, ty, &name, cell.w - 38.0, t.text);
                        // Progression dans la boucle du pattern.
                        if let Some((_, start)) = st.playing.filter(|_| playing) {
                            let len = self.project.patterns.get(p).map_or(bar, |pt| pt.len.max(1) as f64);
                            let f = ((self.position - start as f64).max(0.0) % len / len) as f32;
                            c.fill_rrect(Rect::new(cell.x + 6.0, cell.bottom() - 6.0, (cell.w - 12.0) * f, 3.0), 1.5, t.text);
                        }
                        if hov && input.pressed {
                            self.launch_clip(k, r);
                        }
                        if hov && input.right_down && self.drag.is_none() {
                            self.project.session_grid[k][r] = None;
                            if playing || queued {
                                self.stop_column(k);
                            }
                            self.commit("Vider une case");
                        }
                    }
                }
            }
            // Arrêt de la colonne.
            let stop = Rect::new(col_x(k), row_y(SESSION_SCENES) + 6.0, col_w, STOP_H - 6.0);
            let id = wid("ses-stop", k, 0);
            let hov = self.hovered(input, id, stop);
            let hv = self.hover_anim(id, hov);
            let active = self.session_cols[k].playing.is_some() || self.session_cols[k].queued.is_some();
            c.fill_rrect(stop, 8.0, t.raised.alpha(0.5 + hv * 0.5));
            draw_icon(c, Icon::Stop, stop.x + stop.w * 0.5, stop.y + stop.h * 0.5, 11.0, if active { t.text } else { t.muted });
            if hov {
                self.set_tip("Arrêter la colonne", "");
            }
            if hov && input.pressed {
                self.stop_column(k);
            }
        }
    }
}
