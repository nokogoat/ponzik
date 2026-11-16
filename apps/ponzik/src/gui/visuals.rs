//! Visuels des appareils : réponse de l'EQ, courbes de transfert, enveloppes,
//! filtres. Tout se manipule directement à la souris.

use ponzik_dsp::biquad::{BUTTERWORTH4_Q, BiquadCoefs};
use ponzik_engine::ParamInfo;
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;

use super::widgets::wid;

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;

/// Fréquence ↔ abscisse logarithmique dans `r`.
fn x_of_freq(r: Rect, f: f32) -> f32 {
    r.x + (f.max(F_MIN) / F_MIN).ln() / (F_MAX / F_MIN).ln() * r.w
}

fn freq_of_x(r: Rect, x: f32) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(((x - r.x) / r.w).clamp(0.0, 1.0))
}

fn db(mag: f32) -> f32 {
    20.0 * mag.max(1e-6).log10()
}

/// Grille fréquentielle : 100 Hz, 1 kHz, 10 kHz marquées, et lignes en dB.
fn freq_grid(app: &mut App, c: &mut Canvas, r: Rect, db_range: f32, db_lines: &[f32]) {
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
fn filled_curve(c: &mut Canvas, pts: &[(f32, f32)], base_y: f32, fill: ponzik_ui::Color, stroke: ponzik_ui::Color) {
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
    Handle { freq: 1, gain: None, q: None, on: Some(0), label: "CB" },
    Handle { freq: 3, gain: Some(4), q: None, on: None, label: "G" },
    Handle { freq: 5, gain: Some(6), q: Some(7), on: None, label: "1" },
    Handle { freq: 8, gain: Some(9), q: Some(10), on: None, label: "2" },
    Handle { freq: 11, gain: Some(12), q: Some(13), on: None, label: "3" },
    Handle { freq: 14, gain: Some(15), q: None, on: None, label: "A" },
    Handle { freq: 17, gain: None, q: None, on: Some(16), label: "CH" },
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

    fn value_of(&self, device: Device, params: &[ParamInfo], id: u32) -> f32 {
        Self::info(params, id).map_or(0.0, |p| self.param_value(device, p))
    }

    fn set_value_of(&mut self, device: Device, params: &[ParamInfo], id: u32, v: f32) {
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
            self.text_center(c, self.f.mono_med, 9.0, hx, hy, h.label, col);
        }
        self.anim.set(key, selected as f32);
        c.set_clip(Some(prev));

        // Réglages fins de la bande choisie, et sortie.
        let h = &EQ_HANDLES[selected.min(EQ_HANDLES.len() - 1)];
        let group = params.iter().find(|p| p.id == h.freq).map_or("", |p| p.group);
        self.text(c, self.f.sans_med, 11.0, side.x + 4.0, side.y + 6.0, group, t.muted);
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
            "saturator" => "Transfert\nGlisser : drive",
            "compressor" => "Transfert\nGlisser : seuil · molette : ratio",
            "limiter" => "Transfert\nGlisser : gain d'entrée",
            _ => "Transfert\nGlisser : seuil",
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
            self.empty_state(c, r, super::widgets::Icon::Sample, "Glisse un WAV ici");
            return;
        };
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

        // Marqueurs : début et fin (et début de boucle si présent), en pourcentage.
        let find = |n: &str| params.iter().find(|p| p.name == n);
        let markers = [(find("Début"), t.text), (find("Fin"), t.text), (find("Début de boucle"), t.select)];
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

/// Signal de test d'une réponse d'effet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Impulsion : queues de réverbe, échos.
    Impulse,
    /// Signal constant : gains rythmiques (pompe, tranche).
    Constant,
}

/// Effets dont on montre la réponse temporelle, avec le signal et la durée.
pub fn response_kind(kind: &str) -> Option<(Probe, f32)> {
    match kind {
        "reverb" | "shimmer" => Some((Probe::Impulse, 4.0)),
        "delay" => Some((Probe::Impulse, 2.5)),
        "pompe" | "tranche" => Some((Probe::Constant, 0.0)),
        _ => None,
    }
}

const PROBE_SR: f32 = 16_000.0;

impl App {
    /// Enveloppe de la réponse de l'effet (crête par colonne), mise en cache par réglages.
    fn effect_response(&mut self, kind: &str, params: &'static [ParamInfo], device: Device, cols: usize) -> Option<std::sync::Arc<Vec<f32>>> {
        let (probe, secs) = response_kind(kind)?;
        // Les effets rythmiques sont montrés sur une mesure au tempo du projet.
        let secs = if secs > 0.0 { secs } else { 4.0 * 60.0 / self.project.bpm };
        let values: Vec<f32> = params.iter().map(|p| self.param_value(device, p)).collect();
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
        let mut env = vec![0.0f32; cols];
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut done = 0;
        while done < n {
            let m = 256.min(n - done);
            for k in 0..m {
                let x = match probe {
                    Probe::Impulse => (done + k == 0) as u8 as f32,
                    Probe::Constant => 0.5,
                };
                l[k] = x;
                r[k] = x;
            }
            fx.process(&mut l[..m], &mut r[..m]);
            for k in 0..m {
                let col = ((done + k) / per_col).min(cols - 1);
                env[col] = env[col].max(l[k].abs().max(r[k].abs()));
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
        let env = std::sync::Arc::new(env);
        self.responses.insert(key, env.clone());
        Some(env)
    }

    /// Réponse temporelle : queue en dB pour une impulsion, gain linéaire pour un signal constant.
    pub(super) fn response_graph(&mut self, c: &mut Canvas, r: Rect, device: Device, kind: &str, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset_xy(10.0, 12.0);
        let cols = (g.w / 2.0) as usize;
        let Some(env) = self.effect_response(kind, params, device, cols) else { return };
        let Some((probe, _)) = response_kind(kind) else { return };
        let y_of = |v: f32| match probe {
            Probe::Impulse => {
                let db = 20.0 * v.max(1e-6).log10();
                g.bottom() - ((db + 60.0) / 60.0).clamp(0.0, 1.0) * g.h
            }
            Probe::Constant => g.bottom() - (v / 1.2).clamp(0.0, 1.0) * g.h,
        };
        let pts: Vec<(f32, f32)> = env.iter().enumerate().map(|(i, &v)| (g.x + i as f32 * 2.0, y_of(v))).collect();
        filled_curve(c, &pts, g.bottom(), t.accent.alpha(0.14), t.accent);
    }
}

/// Clé stable d'un appareil pour les identifiants de widgets.
pub fn device_key(d: Device) -> usize {
    match d {
        Device::Instrument(ch) => ch,
        Device::Effect { track, slot } => 10_000 + track * 16 + slot,
    }
}
