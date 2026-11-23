//! Visuals of the modeled keyboards: the tine (or reed) over its pickup with
//! the output it gives soft and hard, the organ's drawbars and spinning
//! rotary speaker, the mallet instrument's bars and mode spectrum.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::TAU;

use ponzik_engine::ParamInfo;
use ponzik_engine::instruments::{EpModel, MALLET_MODELS, pickup_cycle};
use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::visuals::device_key;
use super::widgets::wid;

/// Rotor speeds (turns per second) and 90 % ramp times of the rotary
/// speaker, as in the engine: the drawing spins up and down like the sound.
const HORN: (f32, f32) = (0.8, 6.67);
const DRUM: (f32, f32) = (0.67, 5.67);
const HORN_RAMP: (f32, f32) = (1.0, 1.6);
const DRUM_RAMP: (f32, f32) = (4.5, 5.5);

/// Rotary drawing state per device: horn and drum angle and speed, last time.
#[derive(Clone, Copy, Default)]
struct Spin {
    horn: f32,
    drum: f32,
    horn_speed: f32,
    drum_speed: f32,
    time: f32,
}

thread_local! {
    static SPINS: RefCell<HashMap<usize, Spin>> = RefCell::new(HashMap::new());
}

/// Hammond drawbar colours: brown subharmonics, white octaves, black others.
fn drawbar_color(i: usize) -> Color {
    match i {
        0 | 1 => Color::hex(0x8a_5a_3c),
        2 | 3 | 5 | 8 => Color::hex(0xe8_e4_da),
        _ => Color::hex(0x2a_2a_2a),
    }
}

impl App {
    /// Tine piano or reed piano: two cycles of the pickup's output for a soft
    /// and a hard strike, as the sound comes out of it. Drag: offset or gap;
    /// wheel: distance or grit.
    pub(super) fn ep_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let model = if params.iter().any(|p| p.name == "Pickup Offset") { EpModel::Tine } else { EpModel::Reed };
        let v = |app: &App, id| app.value_of(device, params, id);
        let (a, b) = (v(self, 4), v(self, 5));
        let id = wid("ep-pickup", device_key(device), 0);
        let range = if model == EpModel::Tine { 2.0 } else { 1.0 };
        let hot = self.graph_gestures(input, id, r, device, params, (4, range), Some((5, 0.05, false)));

        let g = r.inset_xy(12.0, 14.0);
        let mid = (g.y + g.h * 0.5).round();
        c.fill_rect(Rect::new(g.x, mid, g.w, 1.0), t.hairline.alpha(1.4));
        c.fill_rect(Rect::new((g.x + g.w * 0.5).round(), g.y, 1.0, g.h), t.hairline.alpha(0.8));
        // Both on the hard strike's scale, without their offset (the amp
        // blocks it): the soft one is smaller and rounder.
        let cycle_of = |vel: f32| {
            let mut cycle = [0.0f32; 96];
            pickup_cycle(model, a, b, vel, &mut cycle);
            let mean = cycle.iter().sum::<f32>() / cycle.len() as f32;
            cycle.iter_mut().for_each(|s| *s -= mean);
            cycle
        };
        let hard = cycle_of(1.0);
        let peak = hard.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-3);
        for (cycle, color, width) in [(cycle_of(0.3), t.text2.alpha(0.6), 1.2), (hard, t.accent, 2.0)] {
            let pts: Vec<(f32, f32)> = cycle
                .iter()
                .chain(cycle.iter())
                .enumerate()
                .map(|(i, &s)| (g.x + i as f32 / 191.0 * g.w, mid - s / peak * g.h * 0.5))
                .collect();
            c.stroke_polyline(&pts, width, color);
        }
        if hot {
            let tip = if model == EpModel::Tine {
                tr("Pickup: output for a soft and a hard strike\nDrag: offset · wheel: distance")
            } else {
                tr("Pickup: output for a soft and a hard strike\nDrag: gap · wheel: grit")
            };
            self.set_tip(tip, "");
        }
    }

    /// Organ: the nine drawbars (drag them in and out) and the rotary
    /// speaker seen from above, horn and drum spinning with their inertia.
    pub(super) fn organ_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let key = device_key(device);
        let bars = Rect::new(r.x + 10.0, r.y + 10.0, r.w * 0.56, r.h - 20.0);
        let w = bars.w / 9.0;
        let slot_h = bars.h - 16.0;
        let mut hovered_bar = None;
        for i in 0..9 {
            let id = wid("organ-bar", key, i);
            let cell = Rect::new(bars.x + i as f32 * w, bars.y, w, bars.h);
            if self.hovered(input, id, cell) {
                hovered_bar = Some(i);
                if input.pressed {
                    self.begin_drag(id, input, 0.0);
                }
            }
            if self.is_dragging(id) {
                let v = ((input.y - bars.y) / slot_h * 8.0).clamp(0.0, 8.0).round();
                self.set_value_of(device, params, i as u32, v);
            }
            let value = self.value_of(device, params, i as u32);
            // Slot and the bar pulled out of it: longer is louder.
            let x = cell.x + w * 0.5;
            c.fill_rrect(Rect::new(x - 2.0, bars.y, 4.0, slot_h), 2.0, t.bg_deep);
            let len = 6.0 + value / 8.0 * (slot_h - 6.0);
            let body = Rect::new(x - w * 0.32, bars.y, w * 0.64, len);
            c.fill_rrect(body, 3.0, drawbar_color(i).alpha(0.9));
            c.fill_rrect(Rect::new(body.x, body.bottom() - 10.0, body.w, 10.0), 3.0, drawbar_color(i));
            self.text_right(c, self.f.mono, 9.0, x + 3.0, bars.bottom() - 4.0, &format!("{}", value.round() as i32), t.muted);
        }
        if let Some(i) = hovered_bar {
            let name = params.get(i).map_or("", |p| tr(p.name));
            self.set_tip(&format!("{name}\n{}", tr("Drag: pull the drawbar out (louder) or push it in")), "");
        }

        // Rotary speaker, top view.
        let speed_param = self.value_of(device, params, 17).round() as usize;
        let mix = self.value_of(device, params, 18);
        let inertia = self.value_of(device, params, 19).max(0.25);
        let now = self.time;
        let spin = SPINS.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.entry(key).or_insert(Spin { horn_speed: HORN.0, drum_speed: DRUM.0, time: now, ..Spin::default() });
            let dt = (now - st.time).clamp(0.0, 0.1);
            st.time = now;
            let (ht, dtg) = match (self.playing, speed_param) {
                (false, _) | (_, 0) => (0.0, 0.0),
                (_, 1) => (HORN.0, DRUM.0),
                _ => (HORN.1, DRUM.1),
            };
            let step = |speed: &mut f32, target: f32, ramp: (f32, f32)| {
                let time = if target > *speed { ramp.0 } else { ramp.1 } * inertia;
                let k = 1.0 - (-std::f32::consts::LN_10 * dt / time).exp();
                *speed += (target - *speed) * k;
            };
            step(&mut st.horn_speed, ht, HORN_RAMP);
            step(&mut st.drum_speed, dtg, DRUM_RAMP);
            st.horn = (st.horn + st.horn_speed * dt).fract();
            st.drum = (st.drum + st.drum_speed * dt).fract();
            *st
        });
        let radius = (r.h * 0.5 - 14.0).min(r.right() - bars.right() - 28.0) * 0.5 + 10.0;
        let (cx, cy) = (bars.right() + 14.0 + (r.right() - bars.right() - 14.0) * 0.5, r.y + r.h * 0.5);
        let dim = 0.35 + 0.65 * mix;
        c.fill_circle(cx, cy, radius * 1.15, t.bg_deep);
        c.stroke_circle(cx, cy, radius * 1.15, 1.0, t.hairline.alpha(2.0));
        // Drum: a rotating baffle.
        let da = spin.drum * TAU;
        c.stroke_arc(cx, cy, radius * 0.7, da, da + std::f32::consts::PI, radius * 0.25, t.text2.alpha(0.25 * dim));
        // Horn: two mouths on one rotor (one is a dummy).
        let ha = spin.horn * TAU;
        let (dx, dy) = (ha.cos() * radius, ha.sin() * radius);
        c.stroke_polyline(&[(cx - dx * 0.8, cy - dy * 0.8), (cx + dx, cy + dy)], 3.0, t.text2.alpha(0.7 * dim));
        let mouth = Rect::new(cx + dx - 5.0, cy + dy - 5.0, 10.0, 10.0);
        c.fill_rrect(mouth, 5.0, t.accent.alpha(dim));
        c.fill_circle(cx, cy, 3.0, t.text);
        let rpm = spin.horn_speed * 60.0;
        self.text_right(c, self.f.mono, 9.0, r.right() - 6.0, r.bottom() - 8.0, &format!("{rpm:.0} rpm"), t.muted);
        if input.inside && Rect::new(bars.right(), r.y, r.right() - bars.right(), r.h).contains(input.x, input.y) {
            self.set_tip(tr("Rotary speaker: horn and drum\nMod wheel or Speed: slow, fast, stop"), "");
        }
    }

    /// Mallets: the instrument's bars (tines, tubes or pan) and the mode
    /// spectrum a strike gives with the current mallet. Drag: hardness;
    /// wheel: instrument.
    pub(super) fn mallets_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let v = |app: &App, id| app.value_of(device, params, id);
        let model = (v(self, 0).round() as usize).min(MALLET_MODELS.len() - 1);
        let (hard, pos) = (v(self, 1), v(self, 2));
        let id = wid("mallets", device_key(device), 0);
        let hot = self.graph_gestures(input, id, r, device, params, (1, 1.0), Some((0, 1.0, false)));
        let m = &MALLET_MODELS[model];

        // Instrument, top.
        let top = Rect::new(r.x + 10.0, r.y + 10.0, r.w - 20.0, r.h * 0.48);
        let n = 8;
        let wbar = top.w / n as f32;
        for i in 0..n {
            let x = top.x + i as f32 * wbar;
            let k = 1.0 - i as f32 / n as f32 * 0.5;
            match model {
                // Tubular bells: hanging tubes.
                6 => {
                    let h = top.h * k;
                    c.fill_rrect(Rect::new(x + wbar * 0.3, top.y, wbar * 0.4, h), wbar * 0.2, t.text2.alpha(0.55));
                    c.fill_rect(Rect::new(x + wbar * 0.25, top.y + 2.0, wbar * 0.5, 3.0), t.text2.alpha(0.8));
                }
                // Kalimba: tines fanning out over a box.
                5 => {
                    let h = top.h * (0.5 + 0.5 * (1.0 - ((i as f32 - 3.5).abs() / 4.0)));
                    c.fill_rrect(Rect::new(x + wbar * 0.35, top.bottom() - h, wbar * 0.3, h), 2.0, t.text2.alpha(0.7));
                }
                // Steel drum: notes as dimples on the pan.
                7 => {
                    if i == 0 {
                        let rr = top.h * 0.5;
                        c.fill_circle(top.x + top.w * 0.5, top.y + rr, rr, t.text2.alpha(0.2));
                        for j in 0..8 {
                            let a = j as f32 / 8.0 * TAU;
                            c.fill_circle(
                                top.x + top.w * 0.5 + a.cos() * rr * 0.65,
                                top.y + rr + a.sin() * rr * 0.65,
                                rr * 0.18,
                                t.text2.alpha(0.45),
                            );
                        }
                    }
                }
                // Bars, with tubes under them where the instrument has them.
                _ => {
                    let h = top.h * (if m.resonator { 0.55 } else { 0.8 }) * k;
                    let bar = Rect::new(x + wbar * 0.12, top.y, wbar * 0.76, h);
                    let wood = if model == 0 || model == 2 { Color::hex(0x9a_5b_3a) } else { t.text2.alpha(0.7) };
                    c.fill_rrect(bar, 3.0, wood);
                    // Where the mallet hits.
                    let hy = bar.y + bar.h * (0.5 - 0.42 * pos);
                    c.fill_circle(bar.x + bar.w * 0.5, hy, 2.0, t.accent);
                    if m.resonator {
                        let tube = top.h * 0.4 * k;
                        c.fill_rrect(Rect::new(x + wbar * 0.3, bar.bottom() + 3.0, wbar * 0.4, tube), wbar * 0.2, t.bg_deep);
                    }
                }
            }
        }

        // Mode spectrum, bottom: ratios on a log axis, amplitudes after the
        // mallet's contact filter.
        let g = Rect::new(r.x + 10.0, r.y + r.h * 0.6, r.w - 20.0, r.h * 0.4 - 12.0);
        c.fill_rect(Rect::new(g.x, g.bottom().round(), g.w, 1.0), t.hairline.alpha(1.4));
        let x_of = |ratio: f32| g.x + (ratio.max(0.1).log2() + 3.0) / 8.0 * g.w;
        let contact = m.contact.0 * (m.contact.1 / m.contact.0).powf(hard);
        let f0 = 440.0;
        let reference = m.gains.iter().cloned().fold(0.0f32, f32::max).max(1e-3);
        for k in 0..m.ratios.len() {
            if m.gains[k] <= 0.0 {
                continue;
            }
            let x = (f0 * m.ratios[k] * contact).max(1e-4);
            let filt = if (x - 1.0).abs() < 1e-3 {
                0.5
            } else {
                let s = (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x);
                (s / (1.0 - x * x)).abs()
            };
            let h = (m.gains[k] / reference * filt.min(1.0)).sqrt() * g.h;
            let xx = x_of(m.ratios[k]).round();
            c.fill_rect(Rect::new(xx - 1.0, g.bottom() - h, 2.0, h), t.accent);
            c.fill_circle(xx, g.bottom() - h, 2.5, t.accent);
        }
        self.text(c, self.f.sans_med, 11.0, g.x, r.y + r.h * 0.52 + 4.0, tr(ponzik_engine::instruments::MALLET_NAMES[model]), t.text2);
        if hot {
            self.set_tip(tr("Modes of the bar after the mallet\nDrag: hardness · wheel: instrument"), "");
        }
    }
}
