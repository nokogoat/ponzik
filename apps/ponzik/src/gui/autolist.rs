//! The automation list: every curve of the project, where it lives (the song
//! or a pattern), what it moves, its shape; click to jump to it, mute or
//! delete it. Opened from the main menu, the palette (Ctrl+K) or the lane chips.

use ponzik_engine::{AutoScope, AutoTarget};
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::App;
use super::theme::T_LAYOUT;
use super::widgets::{Icon, draw_icon, wid};

const ROW_H: f32 = 56.0;
const SPARK_W: f32 = 168.0;

/// A row of the list: a curve, by its place in `Project::automation`.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveRow {
    pub index: usize,
    pub target: AutoTarget,
    pub scope: AutoScope,
    pub points: usize,
    pub muted: bool,
}

impl App {
    /// The project's curves for the list: the song's first, then each
    /// pattern's in order; empty lanes are left out.
    pub fn curve_rows(&self) -> Vec<CurveRow> {
        let mut rows: Vec<CurveRow> = self
            .project
            .automation
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.points.is_empty())
            .map(|(index, a)| CurveRow { index, target: a.target, scope: a.scope, points: a.points.len(), muted: a.muted })
            .collect();
        let order = |s: AutoScope| match s {
            AutoScope::Song => 0,
            AutoScope::Pattern(p) => p + 1,
        };
        rows.sort_by_key(|r| (order(r.scope), r.index));
        rows
    }

    /// The list, as a sheet in the middle of the screen; false once closed.
    pub(super) fn automation_list_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, super::theme::T_HOVER);
        let rows = self.curve_rows();
        c.fill_rect(screen, t.shade(110.0 * open));
        let w = 720.0f32.min(screen.w - 40.0);
        let list_h = (rows.len().max(2) as f32 * ROW_H).min(screen.h - 220.0);
        let h = 96.0 + list_h + 24.0;
        let r = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.42 + (1.0 - open) * 10.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 10.0).inset(-4.0), 22.0, t.shade(90.0 * open));
        c.fill_rrect(r, 18.0, t.panel);
        c.stroke_rrect(r, 18.0, 1.0, t.hairline.alpha(1.8));

        // Header.
        c.fill_circle(r.x + 40.0, r.y + 44.0, 18.0, t.select.alpha(0.16));
        draw_icon(c, Icon::Automation, r.x + 40.0, r.y + 44.0, 18.0, t.select);
        self.text(c, self.f.sans_med, 18.0, r.x + 68.0, r.y + 34.0, tr("Automation"), t.text);
        let muted = rows.iter().filter(|r| r.muted).count();
        let sub = if rows.is_empty() {
            tr("No curve yet").to_string()
        } else if muted > 0 {
            tr_fmt!("{} · {} muted", tr_fmt_n!("{} curve", "{} curves", rows.len(), rows.len()), muted)
        } else {
            tr_fmt_n!("{} curve", "{} curves", rows.len(), rows.len())
        };
        self.text(c, self.f.sans, 12.0, r.x + 68.0, r.y + 56.0, &sub, t.text2);
        let close = Rect::new(r.right() - 52.0, r.y + 18.0, 34.0, 34.0);
        let mut keep = !self.icon_button(c, input, wid("al-close", 0, 0), close, Icon::Close, false, (tr("Close"), tr("Esc")));

        let body = Rect::new(r.x + 12.0, r.y + 84.0, r.w - 24.0, list_h);
        if rows.is_empty() {
            self.empty_state(c, body, Icon::Automation, tr("Right-click a knob > Automate, or draw in a piano roll lane"));
        }
        let total = rows.len() as f32 * ROW_H;
        let max_scroll = (total - body.h).max(0.0);
        if input.inside && body.contains(input.x, input.y) && input.wheel != 0.0 {
            self.auto_ui.list_scroll -= input.wheel * ROW_H;
        }
        self.auto_ui.list_scroll = self.auto_ui.list_scroll.clamp(0.0, max_scroll);
        let scroll = self.anim.to(wid("al-scroll", 0, 0), self.auto_ui.list_scroll, T_LAYOUT);
        let prev = c.push_clip(body);
        let mut y = body.y - scroll;
        for (i, row) in rows.iter().enumerate() {
            let rr = Rect::new(body.x, y, body.w, ROW_H - 4.0);
            y += ROW_H;
            if rr.bottom() < body.y || rr.y > body.bottom() {
                continue;
            }
            if self.curve_row(c, input, rr, i, row, body) {
                keep = false;
            }
        }
        c.set_clip(Some(prev));
        if input.pressed && !r.contains(input.x, input.y) {
            keep = false;
        }
        keep
    }

    /// One curve of the list; true when it was jumped to (the list closes).
    fn curve_row(&mut self, c: &mut Canvas, input: &Input, rr: Rect, i: usize, row: &CurveRow, clip: Rect) -> bool {
        let t = self.theme;
        let Some((name, info)) = self.target_info(row.target) else { return false };
        let inside = input.inside && clip.contains(input.x, input.y);
        let id = wid("al-row", i, 0);
        let hov = inside && self.hovered(input, id, rr);
        let hv = self.hover_anim(id, hov);
        let col = if row.muted { t.muted } else { self.scope_color(row.scope) };
        c.fill_rrect(rr, 12.0, t.raised.alpha(0.35 + 0.5 * hv));
        c.fill_rrect(Rect::new(rr.x + 8.0, rr.y + 10.0, 4.0, rr.h - 20.0), 2.0, col);
        let cy = rr.y + rr.h * 0.5;
        let text_w = rr.w - SPARK_W - 3.0 * 40.0 - 60.0;
        let ink = if row.muted { t.muted } else { t.text };
        self.text_fit(c, self.f.sans_med, 13.0, rr.x + 24.0, cy - 9.0, &name, text_w, ink);
        let points = tr_fmt_n!("{} point", "{} points", row.points, row.points);
        let place = match row.scope {
            AutoScope::Song => tr_fmt!("Song · {}", points),
            AutoScope::Pattern(_) => tr_fmt!("Pattern {} · {}", self.scope_label(row.scope), points),
        };
        let place = if row.muted { format!("{place} · {}", tr("muted")) } else { place };
        self.text_fit(c, self.f.sans, 11.0, rr.x + 24.0, cy + 10.0, &place, text_w, t.text2);

        let spark = Rect::new(rr.right() - 3.0 * 40.0 - 12.0 - SPARK_W, rr.y + 9.0, SPARK_W, rr.h - 18.0);
        c.fill_rrect(spark.inset(-3.0), 6.0, t.well);
        if let Some(points) = self.project.automation.get(row.index).map(|a| a.points.clone()) {
            let now = self.live_tick(row.scope);
            self.sparkline(c, spark, &points, &info, self.scope_len(row.scope), col, now);
        }

        // Mute, show, delete.
        let mut bx = rr.right() - 3.0 * 40.0 - 4.0;
        let mut jumped = false;
        let mut on_button = false;
        let buttons = [
            (Icon::Mute, row.muted, if row.muted { tr("Play the curve again") } else { tr("Mute the curve\nKeeps its points; the knob's value plays") }),
            (Icon::ChevronRight, false, tr("Show the curve")),
            (Icon::Trash, false, tr("Delete the curve")),
        ];
        for (k, (icon, on, tip)) in buttons.into_iter().enumerate() {
            let b = Rect::new(bx, cy - 17.0, 34.0, 34.0);
            bx += 40.0;
            on_button |= inside && b.contains(input.x, input.y);
            if inside && self.icon_button(c, input, wid("al-btn", i, k), b, icon, on, (tip, "")) {
                match k {
                    0 => self.mute_curve(row.target, Some(row.scope), !row.muted),
                    1 => jumped = true,
                    _ => self.delete_curve(row.target, Some(row.scope)),
                }
            } else if !inside {
                draw_icon(c, icon, b.x + 17.0, b.y + 17.0, 15.0, t.text2);
            }
        }
        if hov && !on_button {
            self.set_cursor(Cursor::Hand);
            let tip = match row.scope {
                AutoScope::Song => tr("Click: show it in the playlist"),
                AutoScope::Pattern(_) => tr("Click: show it in the piano roll"),
            };
            self.set_tip(tip, "");
            if input.pressed {
                jumped = true;
            }
        }
        if jumped {
            self.show_curve(row.target, row.scope);
        }
        jumped
    }
}
