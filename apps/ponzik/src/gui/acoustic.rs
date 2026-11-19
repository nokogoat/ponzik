//! Visuals of the modeled acoustic instruments: a guitar whose strings
//! vibrate with the channel's level (pluck point and pickup marked), bowed
//! strings and their bow, a brass bell, a wind tube. Dragging across the
//! picture moves the pluck point, the bow or the brightness of the tone.

use std::f32::consts::{PI, TAU};

use ponzik_engine::ParamInfo;
use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::widgets::wid;

/// Width of the picture in the device card.
pub const ACOUSTIC_W: f32 = 300.0;

/// Instruments drawn by [`App::acoustic_visual`].
pub fn acoustic_kind(kind: &str) -> bool {
    matches!(kind, "guitar" | "strings" | "brass" | "winds")
}

fn find(params: &'static [ParamInfo], name: &str) -> Option<&'static ParamInfo> {
    params.iter().find(|p| p.name == name)
}

/// A string between two points, vibrating in its first modes.
fn string_points(a: (f32, f32), b: (f32, f32), amp: f32, phase: f32, pluck: f32) -> Vec<(f32, f32)> {
    let n = 48;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (nx, ny) = (-dy / len, dx / len);
    (0..=n)
        .map(|i| {
            let s = i as f32 / n as f32;
            // Sum of the first three modes weighted like a pluck at `pluck`.
            let mut y = 0.0;
            for k in 1..=3 {
                let kf = k as f32;
                let w = (kf * PI * pluck).sin() / (kf * kf);
                y += w * (kf * PI * s).sin() * (phase * kf).cos();
            }
            let y = y * amp;
            (a.0 + dx * s + nx * y, a.1 + dy * s + ny * y)
        })
        .collect()
}

impl App {
    fn channel_level(&self, device: Device) -> f32 {
        match device {
            Device::Instrument(ch) => self
                .project
                .channels
                .get(ch)
                .and_then(|c| self.track_meters.get(c.track))
                .map_or(0.0, |m| m.level[0].max(m.level[1])),
            _ => 0.0,
        }
    }

    pub(super) fn acoustic_visual(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let level = self.channel_level(device).clamp(0.0, 1.0).sqrt();
        let phase = self.time * TAU * 7.0;
        let id = wid("acoustic", super::visuals::device_key(device), 0);
        let get = |app: &App, n: &str| find(params, n).map_or(0.0, |p| app.param_value(device, p));
        // Dragging sideways moves the main "where" of each instrument.
        let drag = match kind {
            "guitar" => find(params, "Pluck Position"),
            "strings" => find(params, "Bow Position"),
            _ => find(params, "Brightness"),
        };
        if let Some(p) = drag {
            let hov = self.hovered(input, id, r);
            if hov && input.pressed {
                let v0 = self.param_value(device, p);
                self.begin_drag(id, input, v0);
                self.drag = self.drag.map(|mut d| {
                    d.y0 = input.x;
                    d
                });
            }
            if let Some(d) = self.drag.filter(|d| d.id == id) {
                let dx = (input.x - d.y0) / r.w.max(1.0);
                let sign = if kind == "guitar" || kind == "strings" { -1.0 } else { 1.0 };
                self.set_param_value(device, p, (d.v0 + sign * dx * (p.max - p.min)).clamp(p.min, p.max));
            }
            if hov {
                let tip = match kind {
                    "guitar" => tr("Drag sideways: pluck position\nNear the bridge is brighter"),
                    "strings" => tr("Drag sideways: bow position\nNear the bridge (sul ponticello) is brighter"),
                    _ => tr("Drag sideways: brightness"),
                };
                self.set_tip(tip, "");
            }
        }
        let g = r.inset_xy(12.0, 12.0);
        match kind {
            "guitar" => self.draw_guitar(c, g, level, phase, get(self, "Model").round() as usize, get(self, "Pluck Position"), get(self, "Pickup Position"), get(self, "Palm Mute")),
            "strings" => self.draw_bowed(c, g, level, phase, get(self, "Instrument").round() as usize, get(self, "Bow Position"), get(self, "Bow Pressure"), get(self, "Articulation").round() as usize),
            "brass" => self.draw_bell(c, g, level, phase, get(self, "Instrument").round() as usize, get(self, "Mute").round() as usize),
            _ => self.draw_tube(c, g, level, phase, get(self, "Instrument").round() as usize, get(self, "Breath")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_guitar(&mut self, c: &mut Canvas, g: Rect, level: f32, phase: f32, model: usize, pluck: f32, pickup: f32, mute: f32) {
        let t = self.theme;
        let electric = model >= 2;
        let strings = if model == 3 { 4 } else { 6 };
        // Body on the left, neck to the right.
        let cy = g.y + g.h * 0.5;
        let body_w = g.w * 0.48;
        let (lower, upper) = (g.h * 0.46, g.h * 0.36);
        let wood = if electric { t.accent_deep.alpha(0.55) } else { t.accent.alpha(0.18) };
        let x0 = g.x + lower;
        c.fill_circle(x0, cy, lower, wood);
        c.fill_circle(g.x + body_w - upper * 0.7, cy, upper, wood);
        c.fill_rect(Rect::new(x0, cy - upper * 0.78, body_w - upper * 0.7 - lower, upper * 1.56), wood);
        c.stroke_circle(x0, cy, lower, 1.5, t.accent.alpha(0.6));
        c.stroke_circle(g.x + body_w - upper * 0.7, cy, upper, 1.5, t.accent.alpha(0.6));
        // Neck and frets.
        let neck = Rect::new(g.x + body_w - upper * 0.4, cy - g.h * 0.09, g.right() - (g.x + body_w - upper * 0.4), g.h * 0.18);
        c.fill_rrect(neck, 3.0, t.panel);
        for k in 1..12 {
            let x = neck.right() - neck.w * (1.0 - 2f32.powf(-(k as f32) / 12.0)) * 1.6;
            if x > neck.x {
                c.fill_rect(Rect::new(x, neck.y, 1.0, neck.h), t.hairline);
            }
        }
        let bridge_x = g.x + lower * 0.55;
        let nut_x = neck.right() - 2.0;
        c.fill_rrect(Rect::new(bridge_x - 3.0, cy - g.h * 0.13, 6.0, g.h * 0.26), 2.0, t.text2.alpha(0.7));
        let span = nut_x - bridge_x;
        if electric {
            // Pickup at its position along the string.
            let px = bridge_x + span * pickup * 0.9;
            c.fill_rrect(Rect::new(px - 6.0, cy - g.h * 0.12, 12.0, g.h * 0.24), 3.0, t.text2.alpha(0.35));
        } else {
            let hx = x0 + lower * 0.55;
            c.fill_circle(hx, cy, g.h * 0.13, t.bg_deep);
            c.stroke_circle(hx, cy, g.h * 0.13, 1.5, t.accent.alpha(0.5));
        }
        // Strings: they ring with the channel's level, less when palm muted.
        let amp = level * g.h * 0.05 * (1.0 - 0.7 * mute);
        let spread = neck.h * 0.8;
        for s in 0..strings {
            let off = (s as f32 / (strings - 1) as f32 - 0.5) * spread;
            let width = if model == 3 { 2.0 } else { 0.8 + (strings - 1 - s) as f32 * 0.25 };
            let pts = string_points((bridge_x, cy + off), (nut_x, cy + off * 0.85), amp, phase * (1.0 + s as f32 * 0.21), pluck);
            c.stroke_polyline(&pts, width, t.text.alpha(0.85));
        }
        // Pluck point.
        let px = bridge_x + span * pluck.clamp(0.0, 1.0) * 0.9;
        c.fill_circle(px, cy - spread * 0.5 - 8.0, 3.5, t.accent_hot);
        c.fill_rect(Rect::new(px - 0.5, cy - spread * 0.5 - 4.0, 1.0, spread + 8.0), t.accent_hot.alpha(0.35));
        if mute > 0.05 {
            c.fill_rrect(Rect::new(bridge_x - 4.0, cy - spread * 0.6, 16.0, spread * 1.2), 6.0, t.select.alpha(0.25 + 0.4 * mute));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_bowed(&mut self, c: &mut Canvas, g: Rect, level: f32, phase: f32, instrument: usize, position: f32, pressure: f32, articulation: usize) {
        let t = self.theme;
        // Strings run top (scroll) to bottom (bridge).
        let cx = g.x + g.w * 0.5;
        let scale = [0.7, 0.78, 0.9, 1.0, 0.9][instrument.min(4)];
        let body = Rect::new(cx - g.w * 0.28 * scale, g.y + g.h * 0.32, g.w * 0.56 * scale, g.h * 0.66);
        c.fill_rrect(body, body.w * 0.35, t.accent.alpha(0.18));
        c.stroke_rrect(body, body.w * 0.35, 1.5, t.accent.alpha(0.55));
        // f-holes.
        for side in [-1.0, 1.0] {
            let fx = cx + side * body.w * 0.26;
            let pts: Vec<(f32, f32)> = (0..=12).map(|i| {
                let s = i as f32 / 12.0;
                (fx + side * 4.0 * (s * TAU).sin(), body.y + body.h * (0.35 + 0.3 * s))
            }).collect();
            c.stroke_polyline(&pts, 1.5, t.bg_deep);
        }
        let top = g.y + 2.0;
        let bridge = body.y + body.h * 0.7;
        let count = 4;
        let amp = level * g.w * 0.04;
        for s in 0..count {
            let x = cx + (s as f32 - 1.5) * 6.0;
            let pts = string_points((x, top), (x, bridge), amp, phase * (1.0 + s as f32 * 0.17), 0.12);
            c.stroke_polyline(&pts, 1.0 + (3 - s) as f32 * 0.3, t.text.alpha(0.85));
        }
        c.fill_rect(Rect::new(cx - 14.0, bridge - 1.0, 28.0, 3.0), t.text2.alpha(0.7));
        if articulation == 2 {
            // Pizzicato: a finger instead of the bow.
            let y = bridge - (bridge - top) * 0.35;
            c.fill_circle(cx + 14.0, y, 6.0, t.accent_hot.alpha(0.8));
            return;
        }
        // The bow crosses the strings at the bow position; thicker with more pressure.
        let y = bridge - (bridge - top) * position.clamp(0.0, 1.0) * 1.6;
        let tilt = if articulation == 3 { (phase * 0.5).sin() * 6.0 } else { 0.0 };
        let thick = 2.0 + pressure * 3.0;
        c.stroke_polyline(&[(g.x + 4.0, y + tilt), (g.right() - 4.0, y - tilt)], thick, t.accent_hot.alpha(0.85));
        c.stroke_polyline(&[(g.x + 4.0, y - 5.0 + tilt), (g.right() - 4.0, y - 5.0 - tilt)], 1.5, t.text2.alpha(0.6));
    }

    fn draw_bell(&mut self, c: &mut Canvas, g: Rect, level: f32, phase: f32, instrument: usize, mute: usize) {
        let t = self.theme;
        let cy = g.y + g.h * 0.5;
        let flare = [0.42, 0.4, 0.46, 0.48, 0.3][instrument.min(4)];
        // Bore from the mouthpiece (left) flaring into the bell (right).
        let n = 40;
        let mut upper = Vec::with_capacity(n + 1);
        let mut lower = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let s = i as f32 / n as f32;
            let radius = g.h * (0.04 + flare * s.powf(5.0));
            let x = g.x + 10.0 + s * (g.w * 0.62);
            upper.push((x, cy - radius));
            lower.push((x, cy + radius));
        }
        let mut path = Path::new();
        let mut poly = upper.clone();
        poly.extend(lower.iter().rev());
        path.polygon(&poly);
        c.fill_path(&path, t.accent.alpha(0.22));
        c.stroke_polyline(&upper, 1.5, t.accent);
        c.stroke_polyline(&lower, 1.5, t.accent);
        let bell_x = upper[n].0;
        // Mute in the bell.
        if mute > 0 {
            let w = [0.0, 10.0, 16.0, 12.0][mute.min(3)];
            c.fill_rrect(Rect::new(bell_x - w, cy - g.h * 0.16, w + 4.0, g.h * 0.32), 5.0, t.text2.alpha(0.55));
        }
        // Sound leaving the bell.
        let waves = (level * 4.0).ceil() as usize;
        for k in 0..waves.min(4) {
            let rad = ((phase / TAU * 0.5 + k as f32 * 0.25).fract()) * g.w * 0.35 + 6.0;
            let alpha = (1.0 - rad / (g.w * 0.4)).clamp(0.0, 1.0) * level;
            c.stroke_arc(bell_x, cy, rad, -0.9, 0.9, 1.5, t.accent_hot.alpha(alpha));
        }
    }

    fn draw_tube(&mut self, c: &mut Canvas, g: Rect, level: f32, phase: f32, instrument: usize, breath: f32) {
        let t = self.theme;
        let cy = g.y + g.h * 0.5;
        let tube = Rect::new(g.x + 8.0, cy - g.h * 0.08, g.w - 16.0, g.h * 0.16);
        c.fill_rrect(tube, tube.h * 0.5, t.accent.alpha(0.22));
        c.stroke_rrect(tube, tube.h * 0.5, 1.5, t.accent.alpha(0.7));
        let holes = [7, 9, 8][instrument.min(2)];
        for k in 0..holes {
            let x = tube.x + tube.w * (0.35 + 0.6 * k as f32 / holes as f32);
            c.fill_circle(x, cy, tube.h * 0.22, t.bg_deep);
        }
        // Standing wave inside the bore.
        let amp = level * tube.h * 0.4;
        let pts: Vec<(f32, f32)> = (0..=60)
            .map(|i| {
                let s = i as f32 / 60.0;
                (tube.x + s * tube.w, cy + amp * (s * PI * 3.0).sin() * phase.cos())
            })
            .collect();
        c.stroke_polyline(&pts, 1.5, t.accent_hot.alpha(0.8));
        // Breath at the mouthpiece.
        let dots = (breath * 10.0 * (0.3 + level)) as usize;
        for k in 0..dots {
            let a = (k as f32 * 2.4 + phase * 0.1).sin();
            let x = tube.x - 4.0 - (k as f32 * 3.7 + phase).rem_euclid(18.0);
            c.fill_circle(x, cy + a * tube.h * 0.6, 1.2, t.text2.alpha(0.5));
        }
    }
}
