//! Visual of the plucked and bowed strings: the string from the bridge to
//! the nut with the pluck (or bow) point, and the harmonics that point lets
//! ring. Exciting a string at a fraction `p` of its length from the bridge
//! cancels the harmonics `n` where `n·p` is a whole number: the farther from
//! the bridge, the fewer upper harmonics, the rounder the tone. An electric
//! guitar's pickup reads the string at a point too: the same comb, again.
//! Dragging sideways moves the point.

use std::f32::consts::PI;

use ponzik_engine::ParamInfo;
use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::widgets::wid;

/// Width of the picture in the device card.
pub const ACOUSTIC_W: f32 = 300.0;

/// Harmonics drawn.
const HARMONICS: usize = 16;

/// Instruments drawn by [`App::acoustic_visual`].
pub fn acoustic_kind(kind: &str) -> bool {
    matches!(kind, "guitar" | "strings")
}

fn find(params: &'static [ParamInfo], name: &str) -> Option<&'static ParamInfo> {
    params.iter().find(|p| p.name == name)
}

/// Level (0 to 1) of harmonic `n` of a string excited, or read, at a
/// fraction `p` of its length from the bridge.
fn harmonic_gain(p: f32, n: usize) -> f32 {
    (PI * n as f32 * p).sin().abs()
}

impl App {
    pub(super) fn acoustic_visual(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let guitar = kind == "guitar";
        let Some(point) = find(params, if guitar { "Pluck Position" } else { "Bow Position" }) else { return };
        let get = |app: &App, n: &str| find(params, n).map_or(0.0, |p| app.param_value(device, p));
        let g = r.inset_xy(14.0, 14.0);
        // The whole string spans the width: the point follows the mouse.
        let id = wid("acoustic", super::visuals::device_key(device), 0);
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            let v0 = self.param_value(device, point);
            self.begin_drag(id, input, v0);
            self.drag = self.drag.map(|mut d| {
                d.y0 = input.x;
                d
            });
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let v = d.v0 + (input.x - d.y0) / g.w.max(1.0);
            self.set_param_value(device, point, v.clamp(point.min, point.max));
        }
        if hov {
            let tip = if guitar {
                tr("Harmonics the pluck point lets ring\nDrag sideways: pluck position, brighter near the bridge")
            } else {
                tr("Harmonics the bow point lets ring\nDrag sideways: bow position, brighter near the bridge")
            };
            self.set_tip(tip, "");
        }

        // The engine's own limits: the bow only keeps the Helmholtz motion
        // between 8 and 25 % of the string.
        let p = if guitar { self.param_value(device, point).clamp(0.02, 0.5) } else { self.param_value(device, point).clamp(0.08, 0.25) };
        let pickup = (guitar && get(self, "Model").round() >= 2.0).then(|| get(self, "Pickup Position").clamp(0.02, 0.5));

        // String: bridge on the left, nut on the right.
        let sy = (g.y + 6.0).round();
        c.fill_rect(Rect::new(g.x, sy, g.w, 1.5), t.text2.alpha(0.7));
        for x in [g.x, g.right() - 2.0] {
            c.fill_rect(Rect::new(x, sy - 6.0, 2.0, 13.5), t.text2);
        }
        if let Some(q) = pickup {
            let x = g.x + q * g.w;
            c.fill_rrect(Rect::new(x - 5.0, sy - 5.0, 10.0, 11.5), 2.0, t.select.alpha(0.7));
        }
        let px = g.x + p * g.w;
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        c.fill_circle(px, sy + 0.75, 5.0 + hv, t.accent);

        // Harmonics.
        let bars = Rect::new(g.x, sy + 18.0, g.w, g.bottom() - (sy + 18.0));
        c.fill_rect(Rect::new(bars.x, bars.bottom().round(), bars.w, 1.0), t.hairline.alpha(1.6));
        let step = bars.w / HARMONICS as f32;
        for n in 1..=HARMONICS {
            let level = harmonic_gain(p, n) * pickup.map_or(1.0, |q| harmonic_gain(q, n));
            let h = (level * bars.h).max(1.0);
            let x = bars.x + (n - 1) as f32 * step;
            c.fill_rect(Rect::new((x + 1.0).round(), bars.bottom() - h, (step - 3.0).max(1.0).round(), h), t.accent.alpha(0.75));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_point_cancels_its_harmonics() {
        // A fifth of the way from the bridge: the 5th and 10th harmonics vanish.
        assert!(harmonic_gain(0.2, 5) < 1e-5 && harmonic_gain(0.2, 10) < 1e-5);
        assert!(harmonic_gain(0.2, 1) > 0.5 && harmonic_gain(0.2, 3) > 0.5);
        // In the middle: only odd harmonics.
        assert!(harmonic_gain(0.5, 2) < 1e-5 && (harmonic_gain(0.5, 3) - 1.0).abs() < 1e-5);
        // Nearer the bridge: more of the upper harmonics.
        let upper = |p: f32| (8..=16).map(|n| harmonic_gain(p, n)).sum::<f32>();
        assert!(upper(0.04) > upper(0.4));
    }
}
