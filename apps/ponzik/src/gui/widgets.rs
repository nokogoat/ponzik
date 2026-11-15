//! Widgets maison en mode immédiat : chaque appel dessine et gère son interaction.

use std::f32::consts::PI;

use ponzik_engine::{ParamInfo, Unit};
use ponzik_ui::{Canvas, Color, FontId, Path, Rect};

use super::Input;
use super::app::{App, Drag};
use super::theme::*;

/// Identifiant de widget stable : empreinte FNV d'un nom et de deux indices.
pub fn wid(tag: &str, a: usize, b: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tag.bytes().chain(a.to_le_bytes()).chain(b.to_le_bytes()) {
        h ^= byte as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    /// Action principale : fond clair.
    #[allow(dead_code)]
    Primary,
    /// Action secondaire : surface et filet.
    Secondary,
    /// Discret : texte seul, surface au survol.
    Ghost,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Plus,
    Close,
    Metronome,
    Undo,
    Redo,
    Save,
    Folder,
    Export,
    Sidebar,
    Panel,
    ChevronLeft,
    ChevronRight,
    #[allow(dead_code)]
    ChevronDown,
    Power,
    Keyboard,
}

/// Dessine une icône au trait, dans un carré de `size` centré sur (`cx`, `cy`).
pub fn draw_icon(c: &mut Canvas, icon: Icon, cx: f32, cy: f32, size: f32, col: Color) {
    let s = size / 16.0;
    let p = |x: f32, y: f32| (cx + x * s, cy + y * s);
    let stroke = 1.6 * s.max(0.9);
    match icon {
        Icon::Play => {
            let mut path = Path::new();
            path.polygon(&[p(-4.0, -6.0), p(6.5, 0.0), p(-4.0, 6.0)]);
            c.fill_path(&path, col);
        }
        Icon::Pause => {
            c.fill_rrect(Rect::new(cx - 5.0 * s, cy - 6.0 * s, 3.5 * s, 12.0 * s), 1.0, col);
            c.fill_rrect(Rect::new(cx + 1.5 * s, cy - 6.0 * s, 3.5 * s, 12.0 * s), 1.0, col);
        }
        Icon::Stop => c.fill_rrect(Rect::new(cx - 5.5 * s, cy - 5.5 * s, 11.0 * s, 11.0 * s), 2.0 * s, col),
        Icon::Plus => {
            c.fill_rrect(Rect::new(cx - 6.0 * s, cy - 0.8 * s, 12.0 * s, 1.6 * s), 0.8, col);
            c.fill_rrect(Rect::new(cx - 0.8 * s, cy - 6.0 * s, 1.6 * s, 12.0 * s), 0.8, col);
        }
        Icon::Close => {
            c.stroke_polyline(&[p(-5.0, -5.0), p(5.0, 5.0)], stroke, col);
            c.stroke_polyline(&[p(5.0, -5.0), p(-5.0, 5.0)], stroke, col);
        }
        Icon::Metronome => {
            c.stroke_polyline(&[p(-5.5, 6.5), p(-2.5, -6.5), p(2.5, -6.5), p(5.5, 6.5), p(-5.5, 6.5)], stroke, col);
            c.stroke_polyline(&[p(0.0, 3.0), p(4.5, -4.0)], stroke, col);
        }
        Icon::Undo | Icon::Redo => {
            // Demi-cercle ouvert d'un côté, flèche en haut qui repart vers l'arrière.
            let m = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let pts: Vec<(f32, f32)> = (0..=12)
                .map(|i| {
                    let a = -PI * 0.5 + PI * i as f32 / 12.0;
                    p(m * (a.cos() * 5.0 - 1.0), a.sin() * 5.0 + 1.0)
                })
                .collect();
            c.stroke_polyline(&pts, stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -1.0), p(-m, -4.0), p(m * 2.0, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -1.0), p(-m, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -7.0), p(-m, -4.0)], stroke, col);
        }
        Icon::Save => {
            c.stroke_rrect(Rect::new(cx - 6.0 * s, cy - 6.0 * s, 12.0 * s, 12.0 * s), 2.0 * s, stroke, col);
            c.fill_rect(Rect::new(cx - 3.0 * s, cy - 6.0 * s, 6.0 * s, 4.0 * s), col);
        }
        Icon::Folder => {
            c.stroke_polyline(
                &[p(-7.0, -5.0), p(-2.0, -5.0), p(0.0, -3.0), p(7.0, -3.0), p(7.0, 6.0), p(-7.0, 6.0), p(-7.0, -5.0)],
                stroke,
                col,
            );
        }
        Icon::Export => {
            c.stroke_polyline(&[p(0.0, 3.0), p(0.0, -7.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0, -3.0), p(0.0, -7.0), p(4.0, -3.0)], stroke, col);
            c.stroke_polyline(&[p(-6.0, 1.0), p(-6.0, 6.5), p(6.0, 6.5), p(6.0, 1.0)], stroke, col);
        }
        Icon::Sidebar | Icon::Panel => {
            c.stroke_rrect(Rect::new(cx - 7.0 * s, cy - 6.0 * s, 14.0 * s, 12.0 * s), 2.0 * s, stroke, col);
            if icon == Icon::Sidebar {
                c.fill_rect(Rect::new(cx - 2.5 * s, cy - 6.0 * s, stroke, 12.0 * s), col);
            } else {
                c.fill_rect(Rect::new(cx - 7.0 * s, cy + 1.5 * s, 14.0 * s, stroke), col);
            }
        }
        Icon::ChevronLeft => c.stroke_polyline(&[p(2.5, -5.0), p(-2.5, 0.0), p(2.5, 5.0)], stroke, col),
        Icon::ChevronRight => c.stroke_polyline(&[p(-2.5, -5.0), p(2.5, 0.0), p(-2.5, 5.0)], stroke, col),
        Icon::ChevronDown => c.stroke_polyline(&[p(-5.0, -2.5), p(0.0, 2.5), p(5.0, -2.5)], stroke, col),
        Icon::Keyboard => {
            c.stroke_rrect(Rect::new(cx - 7.5 * s, cy - 5.5 * s, 15.0 * s, 11.0 * s), 2.0 * s, stroke, col);
            for i in 0..3 {
                c.fill_rect(Rect::new(cx + (-3.5 + i as f32 * 3.5) * s - 0.75, cy - 5.5 * s, 1.5, 6.0 * s), col);
            }
        }
        Icon::Power => {
            c.stroke_arc(cx, cy + 0.5 * s, 5.5 * s, -PI * 0.5 + 0.7, PI * 1.5 - 0.7, stroke, col);
            c.stroke_polyline(&[p(0.0, -7.0), p(0.0, -1.0)], stroke, col);
        }
    }
}

impl App {
    // --- Texte ---

    pub(super) fn text(&mut self, c: &mut Canvas, f: FontId, size: f32, x: f32, cy: f32, s: &str, col: Color) -> f32 {
        let baseline = cy + self.fonts.cap_height(f, size) * 0.5;
        self.fonts.draw(c, f, size, x, baseline, s, col)
    }

    pub(super) fn text_right(&mut self, c: &mut Canvas, f: FontId, size: f32, right: f32, cy: f32, s: &str, col: Color) -> f32 {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, right - w, cy, s, col);
        right - w
    }

    pub(super) fn text_center(&mut self, c: &mut Canvas, f: FontId, size: f32, cx: f32, cy: f32, s: &str, col: Color) {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, cx - w * 0.5, cy, s, col);
    }

    /// Texte tronqué avec « … » pour tenir dans `max_w`.
    pub(super) fn fit(&self, f: FontId, size: f32, s: &str, max_w: f32) -> String {
        if self.fonts.measure(f, size, s) <= max_w {
            return s.to_string();
        }
        let mut out: String = s.to_string();
        while !out.is_empty() {
            out.pop();
            let candidate = format!("{}…", out.trim_end());
            if self.fonts.measure(f, size, &candidate) <= max_w {
                return candidate;
            }
        }
        "…".into()
    }

    pub(super) fn text_fit(&mut self, c: &mut Canvas, f: FontId, size: f32, x: f32, cy: f32, s: &str, max_w: f32, col: Color) {
        let s = self.fit(f, size, s, max_w);
        self.text(c, f, size, x, cy, &s, col);
    }

    // --- Interaction ---

    /// Survol : la souris est dessus et aucun autre widget n'est saisi.
    pub(super) fn hovered(&self, input: &Input, id: u64, r: Rect) -> bool {
        input.inside && r.contains(input.x, input.y) && self.drag.as_ref().is_none_or(|d| d.id == id)
    }

    pub(super) fn is_dragging(&self, id: u64) -> bool {
        self.drag.as_ref().is_some_and(|d| d.id == id)
    }

    pub(super) fn begin_drag(&mut self, id: u64, input: &Input, v0: f32) {
        self.drag = Some(Drag { id, y0: input.y, v0, fine: input.mods.shift });
    }

    /// Valeur 0..1 animée de survol pour `id`.
    pub(super) fn hover_anim(&mut self, id: u64, on: bool) -> f32 {
        self.anim.to(id ^ 0x5157_4552, on as u8 as f32, T_HOVER)
    }

    pub(super) fn button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, label: &str, style: Btn) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let clicked = hov && input.pressed;
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.97 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        let (fill, ink) = match style {
            Btn::Primary => (t.text.mix(WHITE, 0.15 * hv), t.bg_deep),
            Btn::Secondary => (t.raised.mix(t.raised_hi, hv), t.text),
            Btn::Ghost => (t.raised.alpha(hv), t.text2.mix(t.text, hv)),
        };
        c.fill_rrect(rr, 8.0, fill);
        if style == Btn::Secondary {
            c.stroke_rrect(rr, 8.0, 1.0, t.hairline);
        }
        let (cx, cy) = rr.center();
        self.text_center(c, self.f.sans_med, 13.0, cx, cy, label, ink);
        clicked
    }

    /// Bouton icône carré ; `on` le montre enfoncé (bascule active).
    pub(super) fn icon_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Icon, on: bool) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        let fill = t.raised.alpha(hv.max(onv)).mix(t.raised_hi, onv * 0.5);
        c.fill_rrect(r, 8.0, fill);
        let col = t.text2.mix(t.text, hv).mix(t.accent, onv);
        let (cx, cy) = r.center();
        draw_icon(c, icon, cx, cy, 16.0, col);
        hov && input.pressed
    }

    /// Sélecteur à segments ; le surlignage glisse vers l'option choisie.
    pub(super) fn segmented(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, opts: &[&str], sel: usize) -> Option<usize> {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.bg_deep);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let n = opts.len().max(1) as f32;
        let seg_w = (r.w - 6.0) / n;
        let x = self.anim.to(id, sel as f32, T_LAYOUT);
        let knob = Rect::new(r.x + 3.0 + x * seg_w, r.y + 3.0, seg_w, r.h - 6.0);
        c.fill_rrect(knob, 7.0, t.raised_hi);
        c.fill_rect(Rect::new(knob.x + 6.0, knob.y + 1.0, knob.w - 12.0, 1.0), WHITE.alpha(0.06));
        let mut picked = None;
        for (i, opt) in opts.iter().enumerate() {
            let seg = Rect::new(r.x + 3.0 + i as f32 * seg_w, r.y, seg_w, r.h);
            let hov = self.hovered(input, id ^ (i as u64 + 1), seg);
            if hov && input.pressed && i != sel {
                picked = Some(i);
            }
            let hv = self.hover_anim(id ^ (i as u64 + 1), hov);
            let selected = 1.0 - (x - i as f32).abs().min(1.0);
            let col = t.muted.mix(t.text2, hv).mix(t.text, selected);
            let (cx, cy) = seg.center();
            self.text_center(c, self.f.sans_med, 13.0, cx, cy, opt, col);
        }
        picked
    }

    /// Potard de paramètre dans une cellule : nom, potard, valeur.
    /// Glisser verticalement (Maj : fin), molette, double-clic : valeur par défaut.
    pub(super) fn knob(&mut self, c: &mut Canvas, input: &Input, id: u64, cell: Rect, info: &ParamInfo, value: f32) -> Option<f32> {
        let t = self.theme;
        let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
        let hit = Rect::new(cell.x, cell.y + 12.0, cell.w, 46.0);
        let hov = self.hovered(input, id, hit);
        let mut changed = None;
        let norm = info.to_norm(value);
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                changed = Some(info.default);
            } else {
                self.begin_drag(id, input, norm);
            }
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let per_px = if d.fine { 0.0012 } else { 0.006 };
            let n = (d.v0 + (d.y0 - input.y) * per_px).clamp(0.0, 1.0);
            let v = info.from_norm(n);
            if v != value {
                changed = Some(v);
            }
        }
        if hov && input.wheel != 0.0 {
            let step = if info.unit == Unit::Choice { 1.0 / (info.max - info.min).max(1.0) } else { 0.02 };
            changed = Some(info.from_norm(norm + input.wheel * step));
        }
        let active = hov || self.is_dragging(id);
        let hv = self.hover_anim(id, active);

        let name = self.fit(self.f.sans, 12.0, info.name, cell.w - 4.0);
        self.text_center(c, self.f.sans, 12.0, cx, cell.y + 6.0, &name, t.text2.mix(t.text, hv));

        let (a0, sweep) = (0.75 * PI, 1.5 * PI);
        let shown = changed.map_or(norm, |v| info.to_norm(v));
        let a = a0 + sweep * shown;
        c.stroke_arc(cx, cy, 17.0, a0, a0 + sweep, 3.0, t.raised_hi);
        let bipolar = info.min < 0.0 && info.max > 0.0;
        let from = if bipolar { a0 + sweep * info.to_norm(0.0) } else { a0 };
        if (a - from).abs() > 0.01 {
            c.stroke_arc(cx, cy, 17.0, from.min(a), from.max(a), 3.0, t.accent.mix(t.accent_hot, hv * 0.25));
        }
        c.fill_circle(cx, cy, 11.0, t.raised_hi.mix(Color::hex(0x34403e), hv));
        c.stroke_circle(cx, cy, 11.0, 1.0, t.hairline);
        let (dx, dy) = (a.cos(), a.sin());
        c.stroke_polyline(&[(cx + dx * 3.5, cy + dy * 3.5), (cx + dx * 9.5, cy + dy * 9.5)], 2.0, t.text);

        let label = info.format(changed.unwrap_or(value));
        let label = self.fit(self.f.mono, 11.0, &label, cell.w - 2.0);
        self.text_center(c, self.f.mono, 11.0, cx, cell.y + 62.0, &label, t.muted.mix(t.text, hv));
        changed
    }

    /// Paramètre à choix : nom, puis une pastille avec flèches de part et d'autre.
    pub(super) fn choice(&mut self, c: &mut Canvas, input: &Input, id: u64, cell: Rect, info: &ParamInfo, value: f32) -> Option<f32> {
        let t = self.theme;
        let name = self.fit(self.f.sans, 12.0, info.name, cell.w - 4.0);
        self.text_center(c, self.f.sans, 12.0, cell.x + cell.w * 0.5, cell.y + 6.0, &name, t.text2);
        let field = Rect::new(cell.x + 2.0, cell.y + 20.0, cell.w - 4.0, 30.0);
        let hov = self.hovered(input, id, field);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(field, 8.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(field, 8.0, 1.0, t.hairline);
        let count = (info.max - info.min).round() as i32 + 1;
        let cur = value.round() as i32;
        let mut changed = None;
        if hov && input.pressed {
            let dir = if input.x < field.x + field.w * 0.3 { -1 } else { 1 };
            changed = Some(((cur + dir).rem_euclid(count.max(1))) as f32);
        }
        if hov && input.wheel != 0.0 {
            changed = Some(((cur - input.wheel.signum() as i32).clamp(0, count - 1)) as f32);
        }
        let (fx, fy) = field.center();
        draw_icon(c, Icon::ChevronLeft, field.x + 10.0, fy, 12.0, t.muted.mix(t.text2, hv));
        draw_icon(c, Icon::ChevronRight, field.right() - 10.0, fy, 12.0, t.muted.mix(t.text2, hv));
        let label = info.format(changed.unwrap_or(value));
        let label = self.fit(self.f.sans_med, 12.0, &label, field.w - 34.0);
        self.text_center(c, self.f.sans_med, 12.0, fx, fy, &label, t.text);
        changed
    }

    /// Champ numérique réglé en glissant verticalement ou à la molette.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drag_field(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        id: u64,
        r: Rect,
        value: f32,
        per_px: f32,
        wheel_step: f32,
        text: &str,
        unit: &str,
    ) -> Option<f32> {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            self.begin_drag(id, input, value);
        }
        let mut changed = None;
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let k = if d.fine { 0.1 } else { 1.0 };
            changed = Some(d.v0 + (d.y0 - input.y) * per_px * k);
        }
        if hov && input.wheel != 0.0 {
            changed = Some(value + input.wheel * wheel_step * if input.mods.shift { 0.1 } else { 1.0 });
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        c.fill_rrect(r, 10.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (_, cy) = r.center();
        self.text(c, self.f.mono_med, 17.0, r.x + 12.0, cy, text, t.text);
        self.text_right(c, self.f.sans, 11.0, r.right() - 10.0, cy, unit, t.muted);
        changed
    }
}
