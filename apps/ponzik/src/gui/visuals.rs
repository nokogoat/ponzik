//! Visuels des appareils : réponse de l'EQ, courbes de transfert, enveloppes,
//! filtres. Tout se manipule directement à la souris.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ponzik_dsp::biquad::{BUTTERWORTH4_Q, BiquadCoefs};
use ponzik_engine::ParamInfo;
use ponzik_i18n::{num, tr, tr_fmt, tr_noop};
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;

use super::widgets::wid;

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;

/// Fréquence ↔ abscisse logarithmique dans `r`.
pub(super) fn x_of_freq(r: Rect, f: f32) -> f32 {
    r.x + (f.max(F_MIN) / F_MIN).ln() / (F_MAX / F_MIN).ln() * r.w
}

pub(super) fn freq_of_x(r: Rect, x: f32) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(((x - r.x) / r.w).clamp(0.0, 1.0))
}

fn db(mag: f32) -> f32 {
    20.0 * mag.max(1e-6).log10()
}

/// Grille fréquentielle : 100 Hz, 1 kHz, 10 kHz marquées, et lignes en dB.
pub(super) fn freq_grid(app: &mut App, c: &mut Canvas, r: Rect, db_range: f32, db_lines: &[f32]) {
    let t = app.theme;
    let mid = r.y + r.h * 0.5;
    for f in [50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0] {
        let x = x_of_freq(r, f).round();
        let strong = matches!(f as u32, 100 | 1_000 | 10_000);
        c.fill_rect(Rect::new(x, r.y, 1.0, r.h), t.hairline.alpha(if strong { 1.6 } else { 0.8 }));
        if strong {
            let label = if f >= 1_000.0 { format!("{}k", f as u32 / 1_000) } else { format!("{}", f as u32) };
            app.text(c, app.f.mono, 10.0, x + 4.0, r.bottom() - 9.0, &label, t.muted);
        }
    }
    for &d in db_lines {
        let y = (mid - d / db_range * (r.h * 0.5 - 6.0)).round();
        c.fill_rect(Rect::new(r.x, y, r.w, 1.0), t.hairline.alpha(if d == 0.0 { 2.0 } else { 0.8 }));
        if d != 0.0 {
            let label = if d > 0.0 { format!("+{d:.0}") } else { format!("−{:.0}", -d) };
            app.text(c, app.f.mono, 10.0, r.x + 4.0, y - 7.0, &label, t.muted);
        }
    }
}

/// Courbe remplie jusqu'à la ligne `base_y`.
pub(super) fn filled_curve(c: &mut Canvas, pts: &[(f32, f32)], base_y: f32, fill: ponzik_ui::Color, stroke: ponzik_ui::Color) {
    filled_curve_width(c, pts, base_y, fill, stroke, 2.0);
}

fn filled_curve_width(c: &mut Canvas, pts: &[(f32, f32)], base_y: f32, fill: ponzik_ui::Color, stroke: ponzik_ui::Color, width: f32) {
    if pts.len() < 2 {
        return;
    }
    let mut area: Vec<(f32, f32)> = pts.to_vec();
    area.push((pts[pts.len() - 1].0, base_y));
    area.push((pts[0].0, base_y));
    let mut p = Path::new();
    p.polygon(&area);
    c.fill_path(&p, fill);
    c.stroke_polyline(pts, width, stroke);
}

/// Une poignée d'EQ : bande, paramètres liés, libellé.
struct Handle {
    freq: u32,
    gain: Option<u32>,
    q: Option<u32>,
    on: Option<u32>,
    label: &'static str,
}

const EQ_HANDLES: [Handle; 7] = [
    Handle { freq: 1, gain: None, q: None, on: Some(0), label: tr_noop("LC") },
    Handle { freq: 3, gain: Some(4), q: None, on: None, label: tr_noop("L") },
    Handle { freq: 5, gain: Some(6), q: Some(7), on: None, label: "1" },
    Handle { freq: 8, gain: Some(9), q: Some(10), on: None, label: "2" },
    Handle { freq: 11, gain: Some(12), q: Some(13), on: None, label: "3" },
    Handle { freq: 14, gain: Some(15), q: None, on: None, label: tr_noop("H") },
    Handle { freq: 17, gain: None, q: None, on: Some(16), label: tr_noop("HC") },
];
const EQ_RANGE: f32 = 20.0;

/// Plage affichée du spectre du master (dB, pente comprise).
const SPECTRUM_DB: (f32, f32) = (-90.0, 50.0);

impl App {
    /// Spectre du master, discret, rempli jusqu'au bas de `r`.
    pub(super) fn draw_spectrum(&self, c: &mut Canvas, r: Rect, fill: ponzik_ui::Color, stroke: ponzik_ui::Color) {
        // Le flux du master est sous-échantillonné : rien au-dessus de sa moitié.
        let f_max = F_MAX.min(self.spectrum.bin_hz() * (self.spectrum.size() / 2) as f32);
        let n = ((x_of_freq(r, f_max) - r.x) / 3.0).clamp(16.0, 256.0) as usize;
        let mut bands = vec![0.0f32; n];
        self.spectrum.bins(n, F_MIN, f_max, &mut bands);
        let (lo, hi) = SPECTRUM_DB;
        let pts: Vec<(f32, f32)> = bands
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let f = ponzik_dsp::spectrum::SpectrumAnalyzer::band_center(i, n, F_MIN, f_max);
                (x_of_freq(r, f), r.bottom() - ((d - lo) / (hi - lo)).clamp(0.0, 1.0) * r.h)
            })
            .collect();
        if pts.iter().all(|p| p.1 >= r.bottom() - 0.5) {
            return;
        }
        filled_curve_width(c, &pts, r.bottom(), fill, stroke, 1.2);
    }

    fn info(params: &[ParamInfo], id: u32) -> Option<&ParamInfo> {
        params.iter().find(|p| p.id == id)
    }

    pub(super) fn value_of(&self, device: Device, params: &[ParamInfo], id: u32) -> f32 {
        Self::info(params, id).map_or(0.0, |p| self.param_value(device, p))
    }

    pub(super) fn set_value_of(&mut self, device: Device, params: &[ParamInfo], id: u32, v: f32) {
        if let Some(p) = Self::info(params, id) {
            self.set_param_value(device, p, v);
        }
    }

    /// Réponse totale de l'EQ en dB à la fréquence `f`.
    fn eq_response(&self, device: Device, params: &[ParamInfo], f: f32) -> f32 {
        let sr = self.sample_rate as f32;
        let v = |id| self.value_of(device, params, id);
        let mut total = 0.0;
        let mut cut = |on: u32, freq: u32, slope: u32, high: bool| {
            if v(on) < 0.5 {
                return;
            }
            let qs: &[f32] = if v(slope) > 0.5 { &BUTTERWORTH4_Q } else { &[0.707] };
            for &q in qs {
                let c = if high { BiquadCoefs::lowpass(v(freq), q, sr) } else { BiquadCoefs::highpass(v(freq), q, sr) };
                total += db(c.magnitude(f, sr));
            }
        };
        cut(0, 1, 2, false);
        cut(16, 17, 18, true);
        total += db(BiquadCoefs::low_shelf(v(3), 0.707, v(4), sr).magnitude(f, sr));
        for b in 0..3 {
            let id = 5 + 3 * b;
            total += db(BiquadCoefs::peaking(v(id), v(id + 2), v(id + 1), sr).magnitude(f, sr));
        }
        total += db(BiquadCoefs::high_shelf(v(14), 0.707, v(15), sr).magnitude(f, sr));
        total + v(19)
    }

    /// EQ façon Pro-Q : courbe de réponse, poignées par bande (glisser : fréquence
    /// et gain, molette : Q, double-clic : remise à zéro), réglages fins à droite.
    pub(super) fn eq_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let graph = Rect::new(body.x, body.y + 4.0, body.w - 160.0, body.h - 8.0);
        let side = Rect::new(graph.right() + 12.0, body.y, 148.0, body.h);
        c.fill_rrect(graph, 10.0, t.well);
        let prev = c.push_clip(graph);
        freq_grid(self, c, graph, EQ_RANGE, &[-12.0, -6.0, 0.0, 6.0, 12.0]);
        let mid = graph.y + graph.h * 0.5;
        let y_of = |d: f32| mid - d.clamp(-EQ_RANGE * 1.2, EQ_RANGE * 1.2) / EQ_RANGE * (graph.h * 0.5 - 6.0);

        let steps = graph.w as usize / 2;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let x = graph.x + i as f32 * graph.w / steps as f32;
                (x, y_of(self.eq_response(device, params, freq_of_x(graph, x))))
            })
            .collect();
        // EQ du master : le spectre de ce qui sort, en fond.
        if let Device::Effect { track: ponzik_engine::mixer::MASTER, .. } = device {
            self.draw_spectrum(c, graph, t.select.alpha(0.07), t.select.alpha(0.22));
        }
        filled_curve(c, &pts, mid, t.accent.alpha(0.14), t.accent);

        // Poignées.
        let key = wid("eq-dev", device_key(device), 0);
        let mut selected = self.anim.get(key).unwrap_or(2.0) as usize;
        for (i, h) in EQ_HANDLES.iter().enumerate() {
            let on = h.on.is_none_or(|id| self.value_of(device, params, id) > 0.5);
            let freq = self.value_of(device, params, h.freq);
            let gain = h.gain.map_or(0.0, |id| self.value_of(device, params, id));
            let (hx, hy) = (x_of_freq(graph, freq), y_of(gain));
            let id = wid("eq-h", device_key(device), i);
            let hit = Rect::new(hx - 11.0, hy - 11.0, 22.0, 22.0);
            let hov = self.hovered(input, id, hit);
            if hov && input.pressed {
                selected = i;
                if input.double {
                    if let Some(g) = h.gain {
                        self.set_value_of(device, params, g, 0.0);
                    } else if let Some(o) = h.on {
                        self.set_value_of(device, params, o, if on { 0.0 } else { 1.0 });
                    }
                } else {
                    self.begin_drag(id, input, 0.0);
                }
            }
            if self.is_dragging(id) {
                if let Some(o) = h.on
                    && !on
                {
                    self.set_value_of(device, params, o, 1.0);
                }
                self.set_value_of(device, params, h.freq, freq_of_x(graph, input.x));
                if let Some(g) = h.gain {
                    let d = (mid - input.y) / (graph.h * 0.5 - 6.0) * EQ_RANGE;
                    self.set_value_of(device, params, g, (d * 10.0).round() / 10.0);
                }
            }
            if hov && h.q.is_some() {
                self.claim_wheel();
            }
            if hov && input.wheel != 0.0
                && let Some(q) = h.q
            {
                let cur = self.value_of(device, params, q);
                self.set_value_of(device, params, q, cur * 1.15f32.powf(input.wheel));
            }
            let active = hov || self.is_dragging(id) || selected == i;
            let hv = self.hover_anim(id, active);
            let col = if !on {
                t.muted
            } else if selected == i {
                t.select
            } else {
                t.text2.mix(t.text, hv)
            };
            c.fill_circle(hx, hy, 9.0 + hv * 2.0, t.bg_deep.alpha(0.85));
            c.stroke_circle(hx, hy, 9.0 + hv * 2.0, 1.6, col);
            self.text_center(c, self.f.mono_med, 9.0, hx, hy, tr(h.label), col);
        }
        self.anim.set(key, selected as f32);
        c.set_clip(Some(prev));

        // Réglages fins de la bande choisie, et sortie.
        let h = &EQ_HANDLES[selected.min(EQ_HANDLES.len() - 1)];
        let group = params.iter().find(|p| p.id == h.freq).map_or("", |p| p.group);
        self.text(c, self.f.sans_med, 11.0, side.x + 4.0, side.y + 6.0, tr(group), t.muted);
        let mut ids: Vec<u32> = vec![h.freq];
        ids.extend(h.gain);
        ids.extend(h.q);
        ids.extend(h.on);
        if h.on.is_some() {
            ids.push(h.freq + 1);
        }
        ids.push(19);
        for (k, pid) in ids.into_iter().take(4).enumerate() {
            let Some(p) = Self::info(params, pid) else { continue };
            let cell = Rect::new(side.x + (k % 2) as f32 * 72.0, side.y + 16.0 + (k / 2) as f32 * 76.0, 72.0, 76.0);
            let value = self.param_value(device, p);
            let wid_k = wid("eq-k", device_key(device), pid as usize);
            let changed = if p.unit == ponzik_engine::Unit::Choice {
                self.choice(c, input, wid_k, cell, p, value)
            } else {
                self.knob(c, input, wid_k, cell, p, value)
            };
            if let Some(v) = changed {
                self.set_param_value(device, p, v);
            }
        }
    }

    /// Courbe de transfert des appareils de dynamique et de saturation.
    /// Glisser horizontalement règle le seuil (ou le drive), la molette le ratio.
    pub(super) fn transfer_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset(10.0);
        let v = |app: &App, id| app.value_of(device, params, id);
        let id = wid("xfer", device_key(device), 0);
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            let start = match kind {
                "saturator" => v(self, 1),
                "limiter" => v(self, 0),
                _ => v(self, 0),
            };
            self.begin_drag(id, input, start);
            self.drag = self.drag.map(|mut d| {
                d.y0 = input.x;
                d
            });
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let dx = (input.x - d.y0) / g.w;
            match kind {
                "saturator" => self.set_value_of(device, params, 1, d.v0 + dx * 36.0),
                "limiter" => self.set_value_of(device, params, 0, d.v0 + dx * 24.0),
                _ => self.set_value_of(device, params, 0, d.v0 + dx * 60.0),
            }
        }
        if hov && kind == "compressor" {
            self.claim_wheel();
        }
        if hov && input.wheel != 0.0 && kind == "compressor" {
            let ratio = v(self, 1);
            self.set_value_of(device, params, 1, ratio * 1.15f32.powf(input.wheel));
        }

        // Diagonale de référence.
        c.stroke_polyline(&[(g.x, g.bottom()), (g.right(), g.y)], 1.0, t.hairline.alpha(2.0));
        let n = 64;
        let pts: Vec<(f32, f32)> = if kind == "saturator" {
            let shape = v(self, 0).round() as u32;
            let drive = 10f32.powf(v(self, 1) / 20.0);
            let out = 10f32.powf(v(self, 3) / 20.0);
            let mix = v(self, 4);
            (0..=n)
                .map(|i| {
                    let x = -1.0 + 2.0 * i as f32 / n as f32;
                    let d = x * drive;
                    let s = match shape {
                        0 => d.tanh(),
                        1 => d.clamp(-1.0, 1.0),
                        2 => ((d + 0.25).tanh() - 0.25f32.tanh()) / (1.0 + 0.25f32.tanh()),
                        3 => 1.0 - ((d + 1.0).rem_euclid(4.0) - 2.0).abs(),
                        _ => d.tanh().abs(),
                    };
                    let y = (mix * s * out + (1.0 - mix) * x).clamp(-1.2, 1.2);
                    (g.x + (x + 1.0) * 0.5 * g.w, g.y + (1.0 - (y + 1.0) * 0.5) * g.h)
                })
                .collect()
        } else {
            // Entrée et sortie en dB, de −60 à 0.
            let (lo, hi) = (-60.0f32, 0.0f32);
            let (thr, ratio, knee, makeup) = match kind {
                "compressor" => (v(self, 0), v(self, 1), v(self, 2), v(self, 6)),
                "gate" => (v(self, 0), 1.0, 0.0, 0.0),
                _ => (v(self, 1) - v(self, 0), f32::INFINITY, 0.0, v(self, 0)),
            };
            let curve = |x: f32| -> f32 {
                match kind {
                    "gate" => {
                        if x >= thr {
                            x
                        } else {
                            x - v(self, 4)
                        }
                    }
                    "limiter" => (x + makeup).min(v(self, 1)),
                    _ => {
                        let over = x - thr;
                        let y = if 2.0 * over < -knee {
                            x
                        } else if 2.0 * over > knee {
                            thr + over / ratio
                        } else {
                            x + (1.0 / ratio - 1.0) * (over + knee / 2.0).powi(2) / (2.0 * knee.max(1e-3))
                        };
                        y + makeup
                    }
                }
            };
            let to_x = |d: f32| g.x + (d - lo) / (hi - lo) * g.w;
            let to_y = |d: f32| g.y + (1.0 - (d.clamp(lo - 6.0, hi + 6.0) - lo) / (hi - lo)) * g.h;
            let tx = to_x(thr.clamp(lo, hi)).round();
            c.fill_rect(Rect::new(tx, g.y, 1.0, g.h), t.select.alpha(0.5));
            (0..=n)
                .map(|i| {
                    let x = lo + (hi - lo) * i as f32 / n as f32;
                    (to_x(x), to_y(curve(x)))
                })
                .collect()
        };
        let prev = c.push_clip(r.inset(2.0));
        c.stroke_polyline(&pts, 2.2, t.accent);
        c.set_clip(Some(prev));
        let label = match kind {
            "saturator" => tr("Transfer\nDrag: drive"),
            "compressor" => tr("Transfer\nDrag: threshold · wheel: ratio"),
            "limiter" => tr("Transfer\nDrag: input gain"),
            _ => tr("Transfer\nDrag: threshold"),
        };
        if hov {
            self.set_tip(label, "");
        }
    }

    /// Enveloppe ADSR dessinée : poignées d'attaque, de déclin/maintien et de relâche.
    pub(super) fn envelope_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, adsr: [&'static ParamInfo; 4]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset_xy(10.0, 10.0);
        let seg = g.w * 0.28;
        let hold = g.w - 3.0 * seg;
        let [a, d, s, rl] = adsr;
        let (na, nd, sv, nr) = (
            a.to_norm(self.param_value(device, a)),
            d.to_norm(self.param_value(device, d)),
            self.param_value(device, s).clamp(0.0, 1.0),
            rl.to_norm(self.param_value(device, rl)),
        );
        let x_a = g.x + na * seg;
        let x_d = x_a + nd * seg;
        let x_h = x_d + hold;
        let x_r = x_h + nr * seg;
        let y_s = g.bottom() - sv * g.h;
        let pts = [(g.x, g.bottom()), (x_a, g.y), (x_d, y_s), (x_h, y_s), (x_r, g.bottom())];
        filled_curve(c, &pts, g.bottom(), t.accent.alpha(0.16), t.accent);

        let handles = [(x_a, g.y, 0usize), (x_d, y_s, 1), (x_r, g.bottom(), 2)];
        for (hx, hy, k) in handles {
            let id = wid("env-h", device_key(device), k * 1_000 + a.id as usize);
            let hit = Rect::new(hx - 10.0, hy - 10.0, 20.0, 20.0);
            let hov = self.hovered(input, id, hit);
            if hov && input.pressed {
                self.begin_drag(id, input, 0.0);
            }
            if self.is_dragging(id) {
                match k {
                    0 => self.set_param_value(device, a, a.from_norm((input.x - g.x) / seg)),
                    1 => {
                        self.set_param_value(device, d, d.from_norm((input.x - x_a) / seg));
                        self.set_param_value(device, s, ((g.bottom() - input.y) / g.h).clamp(0.0, 1.0));
                    }
                    _ => self.set_param_value(device, rl, rl.from_norm((input.x - x_h) / seg)),
                }
            }
            let hv = self.hover_anim(id, hov || self.is_dragging(id));
            c.fill_circle(hx, hy, 5.0 + hv * 2.0, t.bg_deep);
            c.stroke_circle(hx, hy, 5.0 + hv * 2.0, 1.6, t.text2.mix(t.select, hv));
        }
    }

    /// Réponse du filtre : poignée à la coupure, hauteur = résonance.
    pub(super) fn filter_graph(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        r: Rect,
        device: Device,
        cutoff: &'static ParamInfo,
        reso: Option<&'static ParamInfo>,
        highpass: bool,
    ) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset_xy(8.0, 8.0);
        let sr = self.sample_rate as f32;
        let fc = self.param_value(device, cutoff);
        let rv = reso.map(|p| self.param_value(device, p));
        // Résonance vers Q : en pourcentage (303) ou directement en Q.
        let q = match (reso, rv) {
            (Some(p), Some(v)) if p.max <= 1.0 => 0.7 + v * 9.0,
            (Some(_), Some(v)) => v,
            _ => 0.707,
        };
        let coefs = if highpass { BiquadCoefs::highpass(fc, q, sr) } else { BiquadCoefs::lowpass(fc, q, sr) };
        let top = 18.0;
        let range = 48.0;
        let y_of = |d: f32| g.y + (top - d.clamp(top - range, top)) / range * g.h;
        let steps = (g.w / 2.0) as usize;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let x = g.x + i as f32 * g.w / steps as f32;
                (x, y_of(db(coefs.magnitude(freq_of_x(g, x), sr))))
            })
            .collect();
        let zero = y_of(0.0);
        c.fill_rect(Rect::new(g.x, zero.round(), g.w, 1.0), t.hairline.alpha(1.6));
        let prev = c.push_clip(r);
        filled_curve(c, &pts, g.bottom(), t.accent.alpha(0.14), t.accent);
        c.set_clip(Some(prev));

        let hx = x_of_freq(g, fc);
        let hy = y_of(db(coefs.magnitude(fc, sr)));
        let id = wid("flt-h", device_key(device), cutoff.id as usize);
        let hov = self.hovered(input, id, Rect::new(hx - 12.0, hy - 12.0, 24.0, 24.0)) || self.hovered(input, id, r);
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
        }
        if self.is_dragging(id) {
            self.set_param_value(device, cutoff, freq_of_x(g, input.x));
            if let Some(p) = reso {
                let n = ((zero - input.y) / (zero - g.y)).clamp(0.0, 1.0);
                self.set_param_value(device, p, p.from_norm(n));
            }
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        c.fill_circle(hx, hy.clamp(g.y, g.bottom()), 5.0 + hv * 2.0, t.bg_deep);
        c.stroke_circle(hx, hy.clamp(g.y, g.bottom()), 5.0 + hv * 2.0, 1.6, t.text2.mix(t.select, hv));
    }
}

impl App {
    /// Forme d'onde de l'échantillon d'un instrument, avec les marqueurs de début et
    /// de fin (et de boucle) à faire glisser.
    pub(super) fn sample_view(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let Device::Instrument(ch) = device else { return };
        let Some(path) = self.project.channels.get(ch).and_then(|c| c.instrument.sample.clone()) else {
            c.stroke_rrect(r.inset(6.0), 8.0, 1.0, t.hairline.alpha(1.5));
            self.empty_state(c, r, super::widgets::Icon::Sample, tr("Drop a WAV here"));
            return;
        };
        if self.is_offline(&path) {
            // Offline: the file's name, why it is silent, and a way to find it.
            let col = t.negative;
            super::widgets::hatch(c, r.inset(2.0), 10.0, col.alpha(0.12));
            c.stroke_rrect(r, 10.0, 1.0, col.alpha(0.5));
            let cy = r.y + r.h * 0.5;
            super::widgets::draw_icon(c, super::widgets::Icon::Missing, r.x + 22.0, cy - 8.0, 16.0, col);
            self.text(c, self.f.sans_med, 13.0, r.x + 38.0, cy - 8.0, tr("Missing sample"), col.mix(t.text, 0.4));
            let file = ponzik_project::flp::file_name_of(&path);
            self.text_fit(c, self.f.sans, 11.0, r.x + 38.0, cy + 10.0, file, r.w - 84.0, t.text2);
            let find = Rect::new(r.right() - 34.0, r.bottom() - 32.0, 26.0, 26.0);
            if self.icon_button(c, input, wid("smp-locate", device_key(device), 0), find, super::widgets::Icon::Search, false, (tr("Locate missing files…"), "")) {
                let dir = self.locate_start_dir();
                self.open_popup(super::app::Popup::LocateMissing(dir), super::offline::REPORT_ANCHOR);
            }
            return;
        }
        let g = r.inset_xy(8.0, 10.0);
        let mid = g.y + g.h * 0.5;
        if let Some(peaks) = self.waveform(&path) {
            let n = peaks.len().max(1);
            for px in 0..g.w as usize {
                let a = px * n / g.w as usize;
                let b = ((px + 1) * n / g.w as usize).max(a + 1);
                let (lo, hi) = peaks[a.min(n - 1)..b.min(n)].iter().fold((0.0f32, 0.0f32), |(l, h), &(x, y)| (l.min(x), h.max(y)));
                c.fill_rect(Rect::new(g.x + px as f32, mid - hi * g.h * 0.5, 1.0, ((hi - lo) * g.h * 0.5).max(1.0)), t.accent.alpha(0.75));
            }
        }
        let name = std::path::Path::new(&path).file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        self.text(c, self.f.sans_med, 11.0, r.x + 10.0, r.y + 12.0, &name, t.text2);
        let open = Rect::new(r.right() - 34.0, r.bottom() - 32.0, 26.0, 26.0);
        if self.icon_button(c, input, wid("smp-lab", device_key(device), 0), open, super::widgets::Icon::Lab, false, (tr("Open in the Sample Lab: chop, trim, stretch"), "F8")) {
            self.open_channel_in_lab(ch);
            return;
        }

        // Marqueurs : début et fin (et début de boucle si présent), en pourcentage.
        let find = |n: &str| params.iter().find(|p| p.name == n);
        let markers = [(find("Start"), t.text), (find("End"), t.text), (find("Loop Start"), t.select)];
        for (k, (info, col)) in markers.into_iter().enumerate() {
            let Some(info) = info else { continue };
            let v = self.param_value(device, info);
            let x = g.x + info.to_norm(v) * g.w;
            let id = wid("smp-mk", device_key(device), k);
            let hit = Rect::new(x - 6.0, r.y, 12.0, r.h);
            let hov = self.hovered(input, id, hit);
            if hov && input.pressed {
                self.begin_drag(id, input, 0.0);
            }
            if self.is_dragging(id) {
                self.set_param_value(device, info, info.from_norm((input.x - g.x) / g.w));
            }
            let hv = self.hover_anim(id, hov || self.is_dragging(id));
            c.fill_rect(Rect::new(x.round() - 1.0, r.y + 4.0, 2.0, r.h - 8.0), col.alpha(0.6 + 0.4 * hv));
            let mut p = Path::new();
            p.polygon(&[(x - 5.0, r.y + 4.0), (x + 5.0, r.y + 4.0), (x, r.y + 11.0)]);
            c.fill_path(&p, col);
        }
    }
}

/// Signal de test d'une réponse d'effet, ou visuel dessiné sans simulation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Impulsion : queues de réverbe, échos.
    Impulse,
    /// Signal constant : gains rythmiques (pompe, tranche).
    Constant,
    /// Impulsion, canaux gauche et droit séparés, effet seul : échos d'un délai.
    Echoes,
    /// Impulsion, effet seul : queue d'une réverbe, avec pré-délai et gel.
    Tail,
    /// Visuel calculé directement à partir des réglages (et des VU-mètres).
    Drawn,
}

/// Effets dont on montre la réponse temporelle (ou un visuel dédié), avec le signal
/// et la durée (0 : une ou deux mesures au tempo du projet).
pub fn response_kind(kind: &str) -> Option<(Probe, f32)> {
    match kind {
        "reverb" | "shimmer" => Some((Probe::Impulse, 4.0)),
        "delay" => Some((Probe::Impulse, 2.5)),
        "pompe" | "tranche" => Some((Probe::Constant, 0.0)),
        "char_delay" => Some((Probe::Echoes, 0.0)),
        "vintage_verb" => Some((Probe::Tail, 5.0)),
        "bus_comp" | "master_limiter" | "char_saturation" | "lofi" => Some((Probe::Drawn, 0.0)),
        "rhodes" | "wurli" | "organ" | "mallets" => Some((Probe::Drawn, 0.0)),
        _ => None,
    }
}

const PROBE_SR: f32 = 16_000.0;

/// Plage des échos et des queues, en dB.
const TAIL_DB: f32 = 60.0;

/// Historique de réduction de gain par appareil (une valeur par image).
const GR_HISTORY: usize = 72;

thread_local! {
    static GR: RefCell<HashMap<usize, VecDeque<f32>>> = RefCell::new(HashMap::new());
}

/// « −1,0 » : une décimale, virgule et vrai signe moins.
pub(super) fn fmt_db1(d: f32) -> String {
    let s = num(d.abs() as f64, 1);
    if d < -0.05 { format!("−{s}") } else { s }
}

/// Réduction de gain (dB, positive) au niveau `x`, genou doux de largeur `knee`.
fn reduction(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if 2.0 * over <= -knee {
        0.0
    } else if knee > 0.0 && 2.0 * over.abs() <= knee {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        slope * over
    }
}

/// Courbes des caractères de la saturation (mêmes formules que le moteur).
fn saturation_shape(character: usize, x: f32) -> f32 {
    let cubic = |x: f32| {
        let u = x.clamp(-1.0, 1.0);
        1.5 * u - 0.5 * u * u * u
    };
    match character {
        0 => {
            let b = 0.3f32.tanh();
            ((x + 0.3).tanh() - b) / (1.0 + b)
        }
        1 => x / (1.0 + x * x).sqrt(),
        2 if x >= 0.0 => cubic(x),
        2 => 0.8 * cubic(x / 0.8),
        _ => {
            let t = x.tanh();
            if t >= 0.0 { t } else { 0.15 * t }
        }
    }
}

/// Bruit déterministe 0..1.
fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65_535.0
}

impl App {
    /// Enveloppe de la réponse de l'effet (crête par colonne), mise en cache par réglages.
    /// Pour [`Probe::Echoes`], les colonnes du canal gauche puis celles du droit.
    fn effect_response(&mut self, kind: &str, params: &'static [ParamInfo], device: Device, cols: usize) -> Option<Arc<Vec<f32>>> {
        let (probe, secs) = response_kind(kind)?;
        if probe == Probe::Drawn {
            return None;
        }
        // Les effets rythmiques sont montrés sur une mesure (deux pour les échos) au tempo du projet.
        let bar = 4.0 * 60.0 / self.project.bpm;
        let secs = match (probe, secs > 0.0) {
            (_, true) => secs,
            (Probe::Echoes, false) => 2.0 * bar,
            _ => bar,
        };
        let mut values: Vec<f32> = params.iter().map(|p| self.param_value(device, p)).collect();
        // Échos et queues : l'effet seul, sans le direct ni le ducking, gel coupé.
        if matches!(probe, Probe::Echoes | Probe::Tail) {
            for (p, v) in params.iter().zip(values.iter_mut()) {
                match p.name {
                    "Mix" => *v = 1.0,
                    "Ducking" | "Freeze" => *v = 0.0,
                    _ => {}
                }
            }
        }
        let key = format!("{kind}|{cols}|{}|{:?}", self.project.bpm, values);
        if let Some(r) = self.responses.get(&key) {
            return Some(r.clone());
        }
        let mut fx = ponzik_engine::registry::create_effect(kind)?;
        fx.set_sample_rate(PROBE_SR);
        fx.set_tempo(self.project.bpm);
        for (p, v) in params.iter().zip(&values) {
            fx.set_param(p.id, *v);
        }
        fx.reset();
        let n = (secs * PROBE_SR) as usize;
        let per_col = (n / cols.max(1)).max(1);
        let stereo = probe == Probe::Echoes;
        let mut env = vec![0.0f32; if stereo { 2 * cols } else { cols }];
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut done = 0;
        while done < n {
            let m = 256.min(n - done);
            for k in 0..m {
                let x = match probe {
                    Probe::Constant => 0.5,
                    _ => (done + k == 0) as u8 as f32,
                };
                l[k] = x;
                r[k] = x;
            }
            fx.process(&mut l[..m], &mut r[..m]);
            for k in 0..m {
                let col = ((done + k) / per_col).min(cols - 1);
                if stereo {
                    env[col] = env[col].max(l[k].abs());
                    env[cols + col] = env[cols + col].max(r[k].abs());
                } else {
                    env[col] = env[col].max(l[k].abs().max(r[k].abs()));
                }
            }
            done += m;
        }
        if probe == Probe::Constant {
            for v in &mut env {
                *v = (*v / 0.5).min(1.2);
            }
        }
        if self.responses.len() > 64 {
            self.responses.clear();
        }
        let env = Arc::new(env);
        self.responses.insert(key, env.clone());
        Some(env)
    }

    /// Visuel d'un effet temporel ou dédié : queue en dB pour une impulsion, gain
    /// linéaire pour un signal constant, échos, réverbe, dynamique, saturation, usure.
    pub(super) fn response_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let Some((probe, _)) = response_kind(kind) else { return };
        match (probe, kind) {
            (Probe::Drawn, "bus_comp") => return self.bus_comp_graph(c, input, r, device, params),
            (Probe::Drawn, "master_limiter") => return self.limiter_graph(c, input, r, device, params),
            (Probe::Drawn, "char_saturation") => return self.char_saturation_graph(c, input, r, device, params),
            (Probe::Drawn, "rhodes" | "wurli") => return self.ep_graph(c, input, r, device, params),
            (Probe::Drawn, "organ") => return self.organ_graph(c, input, r, device, params),
            (Probe::Drawn, "mallets") => return self.mallets_graph(c, input, r, device, params),
            (Probe::Drawn, _) => return self.lofi_graph(c, input, r, device, params),
            (Probe::Echoes, _) => return self.echoes_graph(c, input, r, device, kind, params),
            (Probe::Tail, _) => return self.tail_graph(c, input, r, device, kind, params),
            _ => {}
        }
        let g = r.inset_xy(10.0, 12.0);
        let cols = (g.w / 2.0) as usize;
        let Some(env) = self.effect_response(kind, params, device, cols) else { return };
        let y_of = |v: f32| match probe {
            Probe::Constant => g.bottom() - (v / 1.2).clamp(0.0, 1.0) * g.h,
            _ => {
                let db = 20.0 * v.max(1e-6).log10();
                g.bottom() - ((db + 60.0) / 60.0).clamp(0.0, 1.0) * g.h
            }
        };
        let pts: Vec<(f32, f32)> = env.iter().enumerate().map(|(i, &v)| (g.x + i as f32 * 2.0, y_of(v))).collect();
        filled_curve(c, &pts, g.bottom(), t.accent.alpha(0.14), t.accent);
    }

    /// Niveau crête de la piste de l'appareil, en dB.
    fn device_level_db(&self, device: Device) -> f32 {
        match device {
            Device::Effect { track, .. } | Device::RackUnit { track, .. } => {
                let m = self.track_meters.get(track).map_or(0.0, |m| m.level[0].max(m.level[1]));
                db(m)
            }
            Device::Instrument(_) => -120.0,
        }
    }

    /// Glisser horizontalement : `drag_id` de `range` unités sur toute la largeur ;
    /// molette : `wheel_id`, multiplié (`mul`) ou décalé de `step` par cran.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn graph_gestures(
        &mut self,
        input: &Input,
        id: u64,
        r: Rect,
        device: Device,
        params: &'static [ParamInfo],
        drag: (u32, f32),
        wheel: Option<(u32, f32, bool)>,
    ) -> bool {
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            let start = self.value_of(device, params, drag.0);
            self.begin_drag(id, input, start);
            self.drag = self.drag.map(|mut d| {
                d.y0 = input.x;
                d
            });
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let dx = (input.x - d.y0) / r.w.max(1.0);
            self.set_value_of(device, params, drag.0, d.v0 + dx * drag.1);
        }
        if hov && wheel.is_some() {
            self.claim_wheel();
        }
        if hov
            && input.wheel != 0.0
            && let Some((wid_p, step, mul)) = wheel
        {
            let cur = self.value_of(device, params, wid_p);
            let v = if mul { cur * step.powf(input.wheel) } else { cur + step * input.wheel };
            self.set_value_of(device, params, wid_p, v);
        }
        hov || self.is_dragging(id)
    }

    /// Compresseur de bus : courbe de transfert du caractère, point de
    /// fonctionnement, aiguille et historique de la réduction de gain.
    fn bus_comp_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let v = |app: &App, id| app.value_of(device, params, id);
        let (mode, thr, ratio, makeup) = (v(self, 0).round() as usize, v(self, 1), v(self, 2), v(self, 7));
        let knee = [6.0, 12.0, 2.0][mode.min(2)];
        let side = r.h.min(r.w - 70.0);
        let sq = Rect::new(r.x, r.y, side, r.h);
        let g = sq.inset(10.0);
        let id = wid("bc-xfer", device_key(device), 0);
        let hot = self.graph_gestures(input, id, sq, device, params, (1, 40.0), Some((2, 1.15, true)));

        let (lo, hi) = (-48.0f32, 0.0f32);
        let to_x = |d: f32| g.x + (d - lo) / (hi - lo) * g.w;
        let to_y = |d: f32| g.y + (1.0 - (d.clamp(lo - 6.0, hi + 6.0) - lo) / (hi - lo)) * g.h;
        c.stroke_polyline(&[(g.x, g.bottom()), (g.right(), g.y)], 1.0, t.hairline.alpha(2.0));
        c.fill_rect(Rect::new(to_x(thr).round(), g.y, 1.0, g.h), t.select.alpha(if hot { 0.8 } else { 0.5 }));
        let curve = |x: f32| x - reduction(x, thr, ratio, knee) + makeup;
        let pts: Vec<(f32, f32)> = (0..=64)
            .map(|i| {
                let x = lo + (hi - lo) * i as f32 / 64.0;
                (to_x(x), to_y(curve(x)))
            })
            .collect();
        let prev = c.push_clip(sq.inset(2.0));
        c.stroke_polyline(&pts, 2.2, t.accent);
        // Point de fonctionnement : le niveau de la piste sur la courbe.
        let level = self.device_level_db(device);
        let gr = reduction(level, thr, ratio, knee);
        if level > lo {
            c.fill_circle(to_x(level), to_y(curve(level)), 4.0, t.text);
        }
        c.set_clip(Some(prev));

        // Historique (à gauche, le plus récent contre l'aiguille) et aiguille.
        let key = device_key(device);
        let hist: Vec<f32> = GR.with(|h| {
            let mut h = h.borrow_mut();
            let q = h.entry(key).or_default();
            if self.playing || q.back().is_some_and(|&x| x > 0.01) {
                // Attaque immédiate, relâche lissée, comme une aiguille.
                let target = if self.playing { gr } else { 0.0 };
                let prev = q.back().copied().unwrap_or(0.0);
                q.push_back(if target > prev { target } else { prev + (target - prev) * 0.12 });
            }
            while q.len() > GR_HISTORY {
                q.pop_front();
            }
            q.iter().copied().collect()
        });
        const GR_RANGE: f32 = 18.0;
        let meter = Rect::new(r.right() - 22.0, r.y + 14.0, 8.0, r.h - 28.0);
        let lane = Rect::new(sq.right() + 4.0, meter.y, meter.x - sq.right() - 12.0, meter.h);
        let y_gr = |d: f32| meter.y + (d / GR_RANGE).clamp(0.0, 1.0) * meter.h;
        for d in [0.0, 6.0, 12.0, 18.0] {
            let y = y_gr(d).round();
            c.fill_rect(Rect::new(lane.x, y, lane.w, 1.0), t.hairline.alpha(if d == 0.0 { 1.6 } else { 0.7 }));
            if d > 0.0 {
                self.text(c, self.f.mono, 9.0, lane.x + 2.0, y - 6.0, &format!("−{d:.0}"), t.muted);
            }
        }
        if hist.len() > 1 && lane.w > 8.0 {
            let n = hist.len();
            let dx = lane.w / (GR_HISTORY - 1) as f32;
            let pts: Vec<(f32, f32)> =
                hist.iter().enumerate().map(|(i, &d)| (lane.right() - (n - 1 - i) as f32 * dx, y_gr(d))).collect();
            let mut area = vec![(pts[0].0, meter.y)];
            area.extend(&pts);
            area.push((lane.right(), meter.y));
            let mut p = Path::new();
            p.polygon(&area);
            c.fill_path(&p, t.accent.alpha(0.16));
            c.stroke_polyline(&pts, 1.4, t.accent.alpha(0.8));
        }
        c.fill_rrect(meter, 4.0, t.raised);
        let now = hist.last().copied().unwrap_or(0.0);
        if now > 0.05 {
            c.fill_rrect(Rect::new(meter.x, meter.y, meter.w, y_gr(now) - meter.y), 4.0, t.accent);
        }
        if hot {
            self.set_tip(tr("Transfer\nDrag: threshold · wheel: ratio"), "");
        } else if input.inside && Rect::new(lane.x, r.y, r.right() - lane.x, r.h).contains(input.x, input.y) {
            let tip = tr_fmt!("Gain reduction: {} dB\nEstimated from the track level", fmt_db1(-now));
            self.set_tip(&tip, "");
        }
    }

    /// Limiteur de master : transfert (gain d'entrée jusqu'au plafond), plafond
    /// marqué, crête de sortie de la piste à droite.
    fn limiter_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let v = |app: &App, id| app.value_of(device, params, id);
        let (algo, gain, ceiling, true_peak) = (v(self, 0).round() as usize, v(self, 1), v(self, 2), v(self, 5) > 0.5);
        let plot = Rect::new(r.x, r.y, r.w - 30.0, r.h);
        let g = plot.inset(10.0);
        let id = wid("lim-xfer", device_key(device), 0);
        let hot = self.graph_gestures(input, id, plot, device, params, (1, 24.0), Some((2, 0.5, false)));

        let (lo, hi) = (-30.0f32, 3.0f32);
        let to_x = |d: f32| g.x + (d - lo) / (hi - lo) * g.w;
        let to_y = |d: f32| g.y + (1.0 - (d.clamp(lo - 6.0, hi) - lo) / (hi - lo)) * g.h;
        for d in [-24.0f32, -12.0, 0.0] {
            let y = to_y(d).round();
            c.fill_rect(Rect::new(g.x, y, g.w, 1.0), t.hairline.alpha(if d == 0.0 { 1.8 } else { 0.8 }));
        }
        c.stroke_polyline(&[(to_x(lo), to_y(lo)), (to_x(hi), to_y(hi))], 1.0, t.hairline.alpha(2.0));
        let cl = 10f32.powf(ceiling / 20.0);
        let g_lin = 10f32.powf(gain / 20.0);
        let curve = |x: f32| -> f32 {
            let lin = 10f32.powf(x / 20.0) * g_lin;
            let y = if algo == 2 {
                // Agressif : écrêteur doux sous le plafond, comme le moteur.
                let knee = 0.7 * cl;
                if lin <= knee { lin } else { knee + (cl - knee) * ((lin - knee) / (cl - knee)).tanh() }
            } else {
                lin.min(cl)
            };
            db(y)
        };
        let pts: Vec<(f32, f32)> = (0..=96)
            .map(|i| {
                let x = lo + (hi - lo) * i as f32 / 96.0;
                (to_x(x), to_y(curve(x)))
            })
            .collect();
        let prev = c.push_clip(plot.inset(2.0));
        filled_curve(c, &pts, g.bottom(), t.accent.alpha(0.1), t.accent);
        // Plafond : la ligne qu'aucune crête ne franchit.
        let cy = to_y(ceiling).round();
        c.fill_rect(Rect::new(g.x, cy, g.w, 1.0), t.select.alpha(0.8));
        c.set_clip(Some(prev));
        let label = fmt_db1(ceiling);
        self.text(c, self.f.mono, 10.0, g.x + 2.0, cy - 8.0, &label, t.select);
        if true_peak {
            self.text_right(c, self.f.mono, 9.0, g.right(), g.bottom() - 6.0, "TP", t.muted);
        }

        // Crête de sortie de la piste, avec le plafond en repère.
        let bar = Rect::new(plot.right() + 4.0, g.y, 8.0, g.h);
        c.fill_rrect(bar, 4.0, t.raised);
        let level = self.device_level_db(device);
        if level > lo {
            let top = to_y(level);
            let col = if level > ceiling + 0.1 { t.negative } else { t.accent };
            c.fill_rrect(Rect::new(bar.x, top, bar.w, bar.bottom() - top), 4.0, col);
        }
        c.fill_rect(Rect::new(bar.x - 2.0, cy, bar.w + 4.0, 1.5), t.select);
        if hot {
            self.set_tip(tr("Transfer\nDrag: input gain · wheel: ceiling"), "");
        }
    }

    /// Saturation à caractère : courbe du caractère choisi (les autres en filigrane)
    /// et, à droite, les trois bandes avec leur drive et la bascule de ton.
    fn char_saturation_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let v = |app: &App, id| app.value_of(device, params, id);
        let character = v(self, 0).round() as usize;
        let drive = 10f32.powf(v(self, 1) / 20.0);
        let multiband = v(self, 2) > 0.5;
        let (out, mix) = (10f32.powf(v(self, 9) / 20.0), v(self, 10));
        let side = r.h.min(r.w * 0.6);
        let sq = Rect::new(r.x, r.y, side, r.h);
        let g = sq.inset(10.0);
        let id = wid("cs-xfer", device_key(device), 0);
        let hot = self.graph_gestures(input, id, sq, device, params, (1, 36.0), None);

        c.fill_rect(Rect::new(g.x, (g.y + g.h * 0.5).round(), g.w, 1.0), t.hairline.alpha(1.2));
        c.fill_rect(Rect::new((g.x + g.w * 0.5).round(), g.y, 1.0, g.h), t.hairline.alpha(1.2));
        let curve = |ch: usize, full: bool| -> Vec<(f32, f32)> {
            (0..=64)
                .map(|i| {
                    let x = -1.0 + 2.0 * i as f32 / 64.0;
                    let s = saturation_shape(ch, x * drive);
                    let y = if full { (mix * s * out + (1.0 - mix) * x).clamp(-1.2, 1.2) } else { s };
                    (g.x + (x + 1.0) * 0.5 * g.w, g.y + (1.0 - (y + 1.0) * 0.5) * g.h)
                })
                .collect()
        };
        let prev = c.push_clip(sq.inset(2.0));
        for ch in (0..4).filter(|&ch| ch != character) {
            c.stroke_polyline(&curve(ch, false), 1.0, t.text2.alpha(0.18));
        }
        c.stroke_polyline(&curve(character, true), 2.2, t.accent);
        c.set_clip(Some(prev));

        // Bandes : graves, médiums, aigus sur un axe logarithmique.
        let bands = Rect::new(sq.right() + 2.0, r.y + 10.0, r.right() - sq.right() - 12.0, r.h - 20.0);
        let mid = (bands.y + bands.h * 0.5).round();
        c.fill_rect(Rect::new(bands.x, mid, bands.w, 1.0), t.hairline.alpha(1.6));
        let (lo_split, hi_split) = (v(self, 3), v(self, 4));
        let xs = [bands.x, x_of_freq(bands, lo_split), x_of_freq(bands, hi_split), bands.right()];
        let y_of = |d: f32| mid - d.clamp(-12.0, 12.0) / 12.0 * (bands.h * 0.5 - 4.0);
        for b in 0..3 {
            let d = v(self, 5 + b as u32);
            let (x0, x1) = (xs[b] + 1.5, xs[b + 1] - 1.5);
            let y = y_of(d);
            let (top, h) = if d >= 0.0 { (y, mid - y) } else { (mid, y - mid) };
            if multiband {
                c.fill_rrect(Rect::new(x0, top, (x1 - x0).max(1.0), h.max(2.0)), 3.0, t.accent.alpha(0.55));
            } else {
                c.stroke_rrect(Rect::new(x0, mid - 8.0, (x1 - x0).max(1.0), 16.0), 3.0, 1.0, t.hairline.alpha(1.6));
            }
        }
        for &x in &xs[1..3] {
            c.fill_rect(Rect::new(x.round(), bands.y, 1.0, bands.h), t.select.alpha(if multiband { 0.6 } else { 0.25 }));
        }
        // Ton : bascule autour de 1 kHz.
        let tone = v(self, 8);
        let tilt = |f: f32| tone * 0.5 * (f / 1_000.0).log2().clamp(-4.0, 4.0) / 4.0;
        let pts: Vec<(f32, f32)> = (0..=16)
            .map(|i| {
                let x = bands.x + bands.w * i as f32 / 16.0;
                (x, y_of(tilt(freq_of_x(bands, x)) * 2.0))
            })
            .collect();
        c.stroke_polyline(&pts, 1.2, t.text2.alpha(0.6));
        if hot {
            self.set_tip(tr("Character transfer\nDrag: drive"), "");
        } else if input.inside && bands.contains(input.x, input.y) {
            let tip = if multiband { tr("Drive per band\nLine: tone") } else { tr("Multiband off\nLine: tone") };
            self.set_tip(tip, "");
        }
    }

    /// Bruits et texture : un vinyle qui tourne pendant la lecture (craquements,
    /// poussière d'âge) et une onde qui pleure, souffle et perd ses bits.
    fn lofi_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let v = |app: &App, id| app.value_of(device, params, id);
        let (crackle, hiss, wow) = (v(self, 0), v(self, 1), v(self, 2));
        let (flutter, bits, age) = (v(self, 3), v(self, 4), v(self, 5));
        let time = if self.playing { self.time } else { 0.0 };

        // Disque.
        let radius = (r.h * 0.5 - 12.0).min(r.w * 0.25);
        let (cx, cy) = (r.x + 12.0 + radius, r.y + r.h * 0.5);
        c.fill_circle(cx, cy, radius, t.bg_deep);
        let mut k = 0.42;
        while k < 0.97 {
            c.stroke_circle(cx, cy, radius * k, 1.0, t.text2.alpha(0.07 + 0.04 * (k * 31.0).sin().abs()));
            k += 0.075;
        }
        c.stroke_circle(cx, cy, radius, 1.0, t.hairline.alpha(2.0));
        // 33 tours par minute ; le reflet tourne avec le disque.
        let angle = time * std::f32::consts::TAU * 0.55;
        for (a, w) in [(angle, 0.18), (angle + std::f32::consts::PI, 0.1)] {
            c.stroke_arc(cx, cy, radius * 0.72, a - 0.35, a + 0.35, radius * 0.5, t.text.alpha(w * 0.25));
        }
        let label = t.accent.mix(t.muted, age * 0.6);
        c.fill_circle(cx, cy, radius * 0.3, label);
        let (dx, dy) = (angle.cos() * radius * 0.18, angle.sin() * radius * 0.18);
        c.fill_circle(cx + dx, cy + dy, radius * 0.04, t.bg_deep.alpha(0.6));
        c.fill_circle(cx, cy, 2.0, t.well);
        // Poussière fixe (âge) et craquements qui scintillent.
        for i in 0..(age * 24.0) as u32 {
            let (rr, th) = (radius * (0.35 + 0.6 * hash01(i * 2 + 1)), hash01(i * 2 + 2) * std::f32::consts::TAU + angle);
            c.fill_circle(cx + th.cos() * rr, cy + th.sin() * rr, 1.0, t.text2.alpha(0.35));
        }
        let frame = (time * 14.0) as u32;
        for i in 0..(crackle * 7.0).ceil() as u32 {
            let seed = frame.wrapping_mul(31).wrapping_add(i * 7);
            if hash01(seed) > crackle.max(0.05) {
                continue;
            }
            let (rr, th) = (radius * (0.35 + 0.6 * hash01(seed + 3)), hash01(seed + 5) * std::f32::consts::TAU);
            c.fill_circle(cx + th.cos() * rr, cy + th.sin() * rr, 1.6, t.text.alpha(0.9));
        }

        // Onde : pleurage et scintillement déforment le temps, le souffle ajoute du
        // bruit, la perte de bits l'escalier, l'âge éteint l'amplitude.
        let wave = Rect::new(cx + radius + 14.0, r.y + 14.0, r.right() - (cx + radius + 24.0), r.h - 28.0);
        let mid = wave.y + wave.h * 0.5;
        c.fill_rect(Rect::new(wave.x, mid.round(), wave.w, 1.0), t.hairline.alpha(1.2));
        let levels = 2f32.powf(16.0 - 12.0 * bits - 1.0);
        let amp = 0.85 * (1.0 - 0.45 * age);
        let n = (wave.w / 1.5) as usize;
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let u = i as f32 / n as f32;
                let warp = wow * 0.12 * (std::f32::consts::TAU * (0.45 * time + u * 0.6)).sin()
                    + flutter * 0.02 * (std::f32::consts::TAU * (2.0 * time + u * 9.0)).sin();
                let mut y = amp * (std::f32::consts::TAU * (2.0 * u + warp)).sin();
                y += hiss * 0.18 * (hash01(i as u32 + frame * 977) - 0.5);
                y = (y * levels).round() / levels;
                (wave.x + u * wave.w, mid - y.clamp(-1.0, 1.0) * wave.h * 0.5)
            })
            .collect();
        c.stroke_polyline(&pts, 1.6, t.accent);
        // Craquements : quelques pointes dans l'onde.
        for i in 0..(crackle * 4.0).ceil() as u32 {
            let seed = frame.wrapping_mul(17).wrapping_add(i * 13 + 1);
            if hash01(seed) > crackle {
                continue;
            }
            let x = wave.x + hash01(seed + 2) * wave.w;
            let h = wave.h * 0.5 * (0.4 + 0.5 * hash01(seed + 4));
            c.fill_rect(Rect::new(x.round(), mid - h, 1.0, 2.0 * h), t.text.alpha(0.8));
        }
        if input.inside && r.contains(input.x, input.y) {
            self.set_tip(tr("Vinyl: crackle and age\nWave: wow, hiss, bit loss"), "");
        }
    }

    /// Délai à caractère : chaque écho dans le temps, gauche en haut et droite en
    /// bas (le ping-pong alterne), sur deux mesures avec les temps marqués.
    fn echoes_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        let g = r.inset_xy(10.0, 10.0);
        let cols = (g.w / 2.0) as usize;
        let Some(env) = self.effect_response(kind, params, device, cols) else { return };
        let mid = (g.y + g.h * 0.5).round();
        // Temps et mesures.
        for beat in 0..8 {
            let x = (g.x + g.w * beat as f32 / 8.0).round();
            c.fill_rect(Rect::new(x, g.y, 1.0, g.h), t.hairline.alpha(if beat % 4 == 0 { 1.8 } else { 0.7 }));
        }
        c.fill_rect(Rect::new(g.x, mid, g.w, 1.0), t.hairline.alpha(1.6));
        let half = g.h * 0.5 - 2.0;
        let h_of = |v: f32| ((db(v) + TAIL_DB) / TAIL_DB).clamp(0.0, 1.0) * half;
        for i in 0..cols {
            let x = g.x + i as f32 * 2.0;
            let (hl, hr) = (h_of(env[i]), h_of(env[cols + i]));
            if hl > 0.5 {
                c.fill_rect(Rect::new(x, mid - hl, 1.6, hl), t.accent.alpha(0.35 + 0.6 * hl / half));
            }
            if hr > 0.5 {
                c.fill_rect(Rect::new(x, mid + 1.0, 1.6, hr), t.accent.alpha(0.35 + 0.6 * hr / half));
            }
        }
        self.text(c, self.f.mono, 9.0, g.right() - 8.0, g.y + 6.0, tr("L"), t.muted);
        self.text(c, self.f.mono, 9.0, g.right() - 8.0, g.bottom() - 6.0, tr("R"), t.muted);
        if input.inside && r.contains(input.x, input.y) {
            self.set_tip(tr("Echoes over two bars\nTop: left · bottom: right"), "");
        }
    }

    /// Réverbe : queue en dB, pré-délai et déclin marqués, gel en ligne continue.
    fn tail_graph(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        let g = r.inset_xy(10.0, 12.0);
        let cols = (g.w / 2.0) as usize;
        let Some(env) = self.effect_response(kind, params, device, cols) else { return };
        let secs = response_kind(kind).map_or(5.0, |k| k.1);
        let find = |n: &str| params.iter().find(|p| p.name == n);
        let get = |app: &App, n: &str| find(n).map_or(0.0, |p| app.param_value(device, p));
        let (predelay, decay, frozen) = (get(self, "Pre-delay"), get(self, "Decay"), get(self, "Freeze") > 0.5);
        let x_of = |s: f32| g.x + (s / secs).clamp(0.0, 1.0) * g.w;
        for s in 1..secs as u32 {
            c.fill_rect(Rect::new(x_of(s as f32).round(), g.y, 1.0, g.h), t.hairline.alpha(0.7));
        }
        // Relatif au début de la queue diffuse (les premières réflexions peuvent
        // dépasser) : la pente se lit alors comme le déclin à −60 dB.
        let start = (((predelay + 0.08) / secs) * cols as f32) as usize;
        let peak = env.iter().skip(start.min(cols.saturating_sub(1))).fold(1e-6f32, |m, &v| m.max(v));
        let y_of = |v: f32| g.bottom() - ((db(v / peak) + TAIL_DB) / TAIL_DB).clamp(0.0, 1.0) * g.h;
        let mut pts: Vec<(f32, f32)> = env.iter().enumerate().map(|(i, &v)| (g.x + i as f32 * 2.0, y_of(v))).collect();
        let col = if frozen { t.select } else { t.accent };
        if frozen {
            // Gel : la queue ne décroît plus, elle tient au niveau atteint juste après l'attaque.
            let hold_at = ((predelay + 0.15) / secs * cols as f32) as usize;
            let level = pts.get(hold_at.min(cols.saturating_sub(1))).map_or(g.bottom(), |p| p.1);
            for p in pts.iter_mut().skip(hold_at) {
                p.1 = p.1.min(level);
            }
        }
        filled_curve(c, &pts, g.bottom(), col.alpha(0.14), col);
        // Pré-délai.
        let px = x_of(predelay).round();
        c.fill_rect(Rect::new(px, g.y, 1.0, g.h), t.select.alpha(0.6));
        // Déclin (−60 dB), s'il tient dans la fenêtre.
        if !frozen && decay < secs {
            let dx = x_of(predelay + decay).round();
            c.fill_rect(Rect::new(dx, g.bottom() - 10.0, 1.0, 10.0), t.text2.alpha(0.7));
        }
        let label = if frozen { "∞".to_string() } else { format!("{} s", fmt_db1(decay).replace('−', "")) };
        self.text_right(c, self.f.mono, 10.0, g.right(), g.y + 4.0, &label, if frozen { t.select } else { t.muted });
        if input.inside && r.contains(input.x, input.y) {
            let tip = if frozen {
                tr("Frozen tail\nIt holds as long as Freeze is on")
            } else {
                tr("Tail over 5 s\nLine: pre-delay · mark: decay")
            };
            self.set_tip(tip, "");
        }
    }
}

/// Clé stable d'un appareil pour les identifiants de widgets.
pub fn device_key(d: Device) -> usize {
    match d {
        Device::Instrument(ch) => ch,
        Device::Effect { track, slot } => 10_000 + track * 16 + slot,
        Device::RackUnit { track, slot, unit } => 20_000 + (track * 16 + slot) * 64 + unit,
    }
}
