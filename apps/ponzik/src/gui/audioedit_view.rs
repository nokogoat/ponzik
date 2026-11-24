//! Drawing and gestures of the audio editor ([`super::audioedit`]).
//!
//! Top: the toolbar (transport, edits, snapping, view). Middle: the overview
//! of the whole sound, the time ruler with markers and regions, the slice
//! lane and the big waveform (or spectrogram). Right: the tool panel, one
//! tab per family (volume, cleaning, time, slices) and the ways out. Bottom:
//! the status bar (cursor, selection, levels, format).

use std::sync::Arc;

use ponzik_dsp::edit::{self, FadeShape};
use ponzik_dsp::spectrogram;
use ponzik_dsp::warp::WarpAlgo;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Color, Path as Shape, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::App;
use super::audioedit::{BUCKET, Grab, NormMode, REC_SECONDS, SpecImage, Tab, fmt_clock, fmt_secs, fr, signed};
use super::dnd::DropTarget;
use super::samplelab::Source;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Width of the tool panel.
const PANEL_W: f32 = 324.0;
const TOOLBAR_H: f32 = 52.0;
const STATUS_H: f32 = 30.0;
/// Slice handles strip, at the top of the waveform.
const LANE_H: f32 = 20.0;
/// Rows of the spectrogram picture (interpolated to its height).
const SPEC_ROWS: usize = 192;

/// Drawings of the editor's own buttons, beside the shared icons.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum G {
    I(Icon),
    Scissors,
    Copy,
    Paste,
    Crop,
    Silence,
    ZeroX,
    Wave,
    Spectrum,
    ZoomSel,
    Marker,
    Region,
    FadeIn,
    FadeOut,
    Dc,
    Noise,
    Declick,
    Declip,
    Trim,
    Pitch,
    Resample,
    Mono,
    Pads,
    Drums,
    Gain,
}

/// Draws `g` in a box of `s` centred on (`cx`, `cy`).
pub(super) fn glyph(c: &mut Canvas, g: G, cx: f32, cy: f32, s: f32, col: Color) {
    let h = s * 0.5;
    let p = |x: f32, y: f32| (cx + x * h, cy + y * h);
    let w = (s / 10.0).clamp(1.3, 2.2);
    let line = |c: &mut Canvas, pts: &[(f32, f32)]| {
        let v: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| p(x, y)).collect();
        c.stroke_polyline(&v, w, col);
    };
    let wave = |c: &mut Canvas, x0: f32, x1: f32, y: f32, amp: f32, cycles: f32| {
        let v: Vec<(f32, f32)> = (0..=24).map(|i| {
            let t = i as f32 / 24.0;
            p(x0 + (x1 - x0) * t, y - amp * (t * cycles * std::f32::consts::TAU).sin())
        }).collect();
        c.stroke_polyline(&v, w, col);
    };
    match g {
        G::I(icon) => draw_icon(c, icon, cx, cy, s, col),
        G::Scissors => {
            c.stroke_circle(p(-0.5, 0.0).0, p(0.0, 0.55).1, h * 0.28, w, col);
            c.stroke_circle(p(0.5, 0.0).0, p(0.0, 0.55).1, h * 0.28, w, col);
            line(c, &[(-0.32, 0.36), (0.55, -0.9)]);
            line(c, &[(0.32, 0.36), (-0.55, -0.9)]);
        }
        G::Copy => {
            let (a, b) = (p(-0.85, -0.45), p(0.35, 0.85));
            c.stroke_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), h * 0.15, w, col);
            let (a, b) = (p(-0.35, -0.9), p(0.85, 0.4));
            c.fill_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), h * 0.15, col.alpha(0.35));
            c.stroke_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), h * 0.15, w, col);
        }
        G::Paste => {
            let (a, b) = (p(-0.75, -0.65), p(0.75, 0.95));
            c.stroke_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), h * 0.15, w, col);
            let (a, b) = (p(-0.35, -0.95), p(0.35, -0.45));
            c.fill_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), h * 0.1, col);
            line(c, &[(-0.4, 0.1), (0.4, 0.1)]);
            line(c, &[(-0.4, 0.5), (0.2, 0.5)]);
        }
        G::Crop => {
            line(c, &[(-0.55, -1.0), (-0.55, 0.55), (1.0, 0.55)]);
            line(c, &[(-1.0, -0.55), (0.55, -0.55), (0.55, 1.0)]);
        }
        G::Silence => {
            wave(c, -1.0, -0.45, 0.0, 0.6, 1.0);
            line(c, &[(-0.45, 0.0), (0.45, 0.0)]);
            wave(c, 0.45, 1.0, 0.0, 0.6, 1.0);
        }
        G::ZeroX => {
            c.fill_rect(Rect::new(p(-1.0, 0.0).0, cy - 0.5, s, 1.0), col.alpha(0.5));
            wave(c, -1.0, 1.0, 0.0, 0.75, 1.0);
            c.fill_circle(cx, cy, h * 0.24, col);
        }
        G::Wave => {
            for (i, a) in [0.3f32, 0.75, 1.0, 0.55, 0.85, 0.4, 0.2].iter().enumerate() {
                let x = p(-0.9 + i as f32 * 0.3, 0.0).0;
                c.fill_rrect(Rect::new(x - w * 0.5, cy - a * h * 0.9, w, a * h * 1.8), w * 0.5, col);
            }
        }
        G::Spectrum => {
            for row in 0..4 {
                for col_i in 0..5 {
                    let v = [[0.9f32, 0.7, 0.8, 0.6, 0.9], [0.5, 0.8, 0.4, 0.7, 0.5], [0.25, 0.4, 0.6, 0.3, 0.45], [0.1, 0.2, 0.3, 0.15, 0.25]][3 - row][col_i];
                    let (x, y) = p(-0.95 + col_i as f32 * 0.4, 0.55 - row as f32 * 0.5);
                    c.fill_rect(Rect::new(x, y - h * 0.2, h * 0.36, h * 0.4), col.alpha(v));
                }
            }
        }
        G::ZoomSel => {
            let (x, y) = p(-0.2, -0.2);
            c.stroke_circle(x, y, h * 0.6, w, col);
            line(c, &[(0.25, 0.25), (0.9, 0.9)]);
            line(c, &[(-0.5, -0.2), (0.1, -0.2)]);
            line(c, &[(-0.2, -0.5), (-0.2, 0.1)]);
        }
        G::Marker => {
            line(c, &[(-0.5, -0.95), (-0.5, 0.95)]);
            let mut sh = Shape::new();
            sh.polygon(&[p(-0.5, -0.95), p(0.75, -0.55), p(-0.5, -0.15)]);
            c.fill_path(&sh, col);
        }
        G::Region => {
            line(c, &[(-0.75, -0.8), (-0.95, -0.8), (-0.95, 0.8), (-0.75, 0.8)]);
            line(c, &[(0.75, -0.8), (0.95, -0.8), (0.95, 0.8), (0.75, 0.8)]);
            let (a, b) = (p(-0.6, -0.3), p(0.6, 0.3));
            c.fill_rrect(Rect::new(a.0, a.1, b.0 - a.0, b.1 - a.1), 2.0, col);
        }
        G::FadeIn | G::FadeOut => {
            let mut pts: Vec<(f32, f32)> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0;
                    (-0.95 + 1.9 * t, 0.8 - 1.6 * (t * std::f32::consts::FRAC_PI_2).sin())
                })
                .collect();
            if g == G::FadeOut {
                pts.iter_mut().for_each(|q| q.0 = -q.0);
            }
            let mut fill: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| p(x, y)).collect();
            fill.push(p(if g == G::FadeOut { -0.95 } else { 0.95 }, 0.8));
            let mut sh = Shape::new();
            sh.polygon(&fill);
            c.fill_path(&sh, col.alpha(0.3));
            line(c, &pts);
            line(c, &[(-0.95, 0.8), (0.95, 0.8)]);
        }
        G::Dc => {
            for i in 0..5 {
                let x = p(-1.0 + i as f32 * 0.45, 0.0).0;
                c.fill_rect(Rect::new(x, cy - 0.5, h * 0.25, 1.0), col.alpha(0.6));
            }
            wave(c, -1.0, 1.0, -0.4, 0.4, 1.5);
            line(c, &[(0.0, 0.25), (0.0, 0.95)]);
            line(c, &[(-0.25, 0.7), (0.0, 0.95), (0.25, 0.7)]);
        }
        G::Noise => {
            let jag = [0.0f32, 0.6, -0.5, 0.8, -0.7, 0.4, -0.6, 0.5, -0.3];
            let pts: Vec<(f32, f32)> = jag.iter().enumerate().map(|(i, &v)| (-1.0 + i as f32 * 0.12, v * 0.7)).collect();
            line(c, &pts);
            wave(c, -0.04, 1.0, 0.0, 0.25, 1.0);
        }
        G::Declick => {
            line(c, &[(-1.0, 0.2), (-0.25, 0.2), (-0.1, -0.95), (0.05, 0.2), (1.0, 0.2)]);
            c.fill_circle(p(0.6, -0.55).0, p(0.6, -0.55).1, h * 0.18, col);
        }
        G::Declip => {
            line(c, &[(-1.0, 0.8), (-0.55, -0.3), (0.55, -0.3), (1.0, 0.8)]);
            let arc: Vec<(f32, f32)> = (0..=12).map(|i| {
                let t = i as f32 / 12.0;
                (-0.55 + 1.1 * t, -0.3 - 0.6 * (t * std::f32::consts::PI).sin())
            }).collect();
            let pts: Vec<(f32, f32)> = arc.iter().map(|&(x, y)| p(x, y)).collect();
            for seg in pts.chunks(2) {
                if seg.len() == 2 {
                    c.stroke_polyline(seg, w * 0.8, col.alpha(0.8));
                }
            }
        }
        G::Trim => {
            line(c, &[(-0.55, -0.9), (-0.55, 0.9)]);
            line(c, &[(0.55, -0.9), (0.55, 0.9)]);
            wave(c, -0.45, 0.45, 0.0, 0.65, 1.5);
            line(c, &[(-1.0, 0.0), (-0.75, 0.0)]);
            line(c, &[(0.75, 0.0), (1.0, 0.0)]);
        }
        G::Pitch => {
            line(c, &[(0.4, -0.95), (0.4, 0.95)]);
            line(c, &[(0.1, -0.6), (0.4, -0.95), (0.7, -0.6)]);
            line(c, &[(0.1, 0.6), (0.4, 0.95), (0.7, 0.6)]);
            c.fill_circle(p(-0.45, 0.45).0, p(-0.45, 0.45).1, h * 0.3, col);
            line(c, &[(-0.2, 0.45), (-0.2, -0.7), (0.05, -0.5)]);
        }
        G::Resample => {
            for i in 0..7 {
                let t = i as f32 / 6.0;
                let (x, y) = p(-0.9 + 1.8 * t, -0.7 * (t * std::f32::consts::TAU).sin());
                c.fill_rect(Rect::new(x - 0.5, y, 1.0, cy + h * 0.9 - y), col.alpha(0.45));
                c.fill_circle(x, y, h * 0.16, col);
            }
        }
        G::Mono => {
            c.stroke_circle(p(-0.3, 0.0).0, cy, h * 0.6, w, col.alpha(0.6));
            c.stroke_circle(p(0.3, 0.0).0, cy, h * 0.6, w, col);
        }
        G::Pads => {
            for (i, j) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = p(-0.9 + i as f32 * 0.95, -0.9 + j as f32 * 0.95);
                c.fill_rrect(Rect::new(x, y, h * 0.85, h * 0.85), h * 0.15, col.alpha(if i == j { 1.0 } else { 0.5 }));
            }
        }
        G::Drums => {
            line(c, &[(-0.85, -0.3), (-0.85, 0.55)]);
            line(c, &[(0.85, -0.3), (0.85, 0.55)]);
            let top: Vec<(f32, f32)> = (0..=20).map(|i| {
                let a = i as f32 / 20.0 * std::f32::consts::TAU;
                p(0.85 * a.cos(), -0.3 + 0.3 * a.sin())
            }).collect();
            c.stroke_polyline(&top, w, col);
            let bottom: Vec<(f32, f32)> = (0..=10).map(|i| {
                let a = i as f32 / 10.0 * std::f32::consts::PI;
                p(0.85 * a.cos(), 0.55 + 0.3 * a.sin())
            }).collect();
            c.stroke_polyline(&bottom, w, col);
            line(c, &[(-0.2, -0.95), (0.3, -0.45)]);
        }
        G::Gain => {
            let pts = [(-0.9f32, 0.6f32), (-0.4, 0.1), (0.1, 0.1), (0.9, -0.7)];
            line(c, &pts);
            line(c, &[(0.45, -0.75), (0.9, -0.7), (0.85, -0.25)]);
            c.fill_rect(Rect::new(p(-0.9, 0.0).0, p(0.0, 0.85).1, s * 0.9, 1.0), col.alpha(0.5));
        }
    }
}

/// Fade curve of `shape` in `r`, from silence to full.
fn draw_fade_curve(c: &mut Canvas, r: Rect, shape: FadeShape, col: Color, width: f32) {
    let pts: Vec<(f32, f32)> = (0..=24)
        .map(|i| {
            let t = i as f32 / 24.0;
            (r.x + r.w * t, r.bottom() - r.h * shape.gain(t))
        })
        .collect();
    c.stroke_polyline(&pts, width, col);
}

/// Nice ruler step (seconds) for at least `min_px` pixels between ticks.
fn ruler_step(px_per_sec: f64, min_px: f64) -> f64 {
    const STEPS: [f64; 19] = [0.0001, 0.0002, 0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0];
    STEPS.iter().copied().find(|s| s * px_per_sec >= min_px).unwrap_or(120.0)
}

fn ruler_label(t: f64, step: f64) -> String {
    let sep = if ponzik_i18n::num(0.5, 1).contains(',') { ',' } else { '.' };
    let (m, s) = ((t / 60.0).floor() as u64, t % 60.0);
    let decimals = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else if step >= 0.01 { 2 } else if step >= 0.001 { 3 } else { 4 };
    let sec = format!("{:.*}", decimals, s).replace('.', &sep.to_string());
    let sec = if s < 10.0 && m > 0 { format!("0{sec}") } else { sec };
    if m > 0 { format!("{m}:{sec}") } else { sec }
}

impl App {
    /// Chop | Edit switch at the start of the Lab header; returns what is left.
    pub(super) fn ae_mode_switch(&mut self, c: &mut Canvas, input: &Input, header: Rect) -> Rect {
        let w = 190.0;
        let r = Rect::new(header.x, header.y, w, header.h);
        let tips = [
            (tr("Chop: cut a sound into pieces for pads and patterns"), "F8"),
            (tr("Audio editor: edit, clean, stretch and slice a sound, Edison style"), ""),
        ];
        let sel = self.audio_edit.active as usize;
        if let Some(i) = self.segmented_tips(c, input, wid("ae-mode", 0, 0), r, &[tr("Chop"), tr("Edit")], &tips, sel) {
            self.ae_set_active(i == 1);
        }
        Rect::new(r.right() + 16.0, header.y, header.right() - r.right() - 16.0, header.h)
    }

    /// The editor in the Lab's place.
    pub(super) fn ae_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        if !input.down {
            if self.audio_edit.grab.is_some_and(|g| matches!(g, Grab::Select(_) | Grab::Edge(_))) {
                self.ae_end_select();
            }
            self.audio_edit.grab = None;
        }
        self.ae_poll_capture();
        if self.audio_edit.playing && self.engine.audition_position().is_none() {
            self.audio_edit.playing = false;
        }
        self.drop_zone(input, body, DropTarget::Lab);
        self.ae_header(c, input, header);
        if self.audio_edit.doc.is_none() {
            self.ae_empty(c, input, body);
            return;
        }
        self.ae_refresh();
        // The view eases to its target.
        let dt = (self.time - self.audio_edit.last_time).clamp(0.0, 0.1);
        self.audio_edit.last_time = self.time;
        let k = 1.0 - (-dt as f64 / T_LAYOUT as f64).exp();
        let ((vs, vl), (ss, sl)) = (self.audio_edit.view, self.audio_edit.shown);
        let next = (ss + (vs - ss) * k, sl + (vl - sl) * k);
        if (next.0 - vs).abs() < 0.5 && (next.1 - vl).abs() < 0.5 {
            self.audio_edit.shown = (vs, vl);
        } else {
            self.audio_edit.shown = next;
            self.anim.set(wid("ae-moving", 0, 0), 0.0);
        }

        let t = self.theme;
        let toolbar = Rect::new(body.x, body.y, body.w, TOOLBAR_H);
        self.ae_toolbar(c, input, toolbar);
        let main_y = toolbar.bottom() + 10.0;
        let panel = Rect::new(body.right() - PANEL_W, main_y, PANEL_W, body.bottom() - main_y);
        let main = Rect::new(body.x, main_y, body.w - PANEL_W - 12.0, body.bottom() - main_y);
        let overview = Rect::new(main.x, main.y, main.w, 34.0);
        let well = Rect::new(main.x, overview.bottom() + 8.0, main.w, main.bottom() - STATUS_H - 8.0 - overview.bottom() - 8.0);
        c.fill_rrect(well, 12.0, t.well);
        let ruler = Rect::new(well.x, well.y, well.w, 34.0);
        let wave = Rect::new(well.x, ruler.bottom(), well.w, well.h - ruler.h);
        let status = Rect::new(main.x, main.bottom() - STATUS_H, main.w, STATUS_H);
        self.ae_overview(c, input, overview);
        self.ae_ruler(c, input, ruler);
        let prev = c.push_clip(wave);
        if self.audio_edit.capture.is_some() {
            self.ae_recording(c, wave);
        } else {
            self.ae_wave(c, input, wave);
        }
        c.set_clip(Some(prev));
        self.ae_status(c, status);
        self.ae_panel(c, input, panel);
    }

    /// Name, format and the Save buttons.
    fn ae_header(&mut self, c: &mut Canvas, input: &Input, header: Rect) {
        let t = self.theme;
        let r = self.ae_mode_switch(c, input, header);
        let cy = r.y + r.h * 0.5;
        let Some(doc) = self.audio_edit.doc.as_ref() else {
            self.text(c, self.f.sans_med, 15.0, r.x, cy, tr("Audio editor"), t.text);
            return;
        };
        let (len, sr, mono) = (doc.len(), doc.sample.sample_rate, self.audio_edit.peaks.as_ref().is_some_and(|p| p.mono));
        let name = self.audio_edit.name.clone();
        glyph(c, G::Wave, r.x + 10.0, cy, 16.0, t.accent);
        let name = self.fit(self.f.sans_med, 15.0, &name, 260.0);
        let nw = self.text(c, self.f.sans_med, 15.0, r.x + 26.0, cy, &name, t.text);
        let chips = [
            fmt_secs(len as f32 / sr.max(1.0)),
            tr_fmt!("{} Hz", fr(sr, 0)),
            if mono { tr("Mono").into() } else { tr("Stereo").into() },
        ];
        let mut x = r.x + 26.0 + nw + 14.0;
        for chip in chips {
            let w = self.fonts.measure(self.f.mono, 11.0, &chip) + 16.0;
            let cr = Rect::new(x, cy - 11.0, w, 22.0);
            c.fill_rrect(cr, 11.0, t.raised);
            self.text_center(c, self.f.mono, 11.0, cr.x + w * 0.5, cy, &chip, t.text2);
            x += w + 6.0;
        }
        if let Source::Channel(_) | Source::Clip(_) = self.audio_edit.source {
            let s = tr("edit copy, the original is kept");
            self.text(c, self.f.sans, 12.0, x + 6.0, cy, s, t.muted);
        }
        // Save buttons.
        let labels = [(tr("Save FLAC"), true), (tr("Save WAV"), false)];
        let mut right = r.right();
        for (i, (label, flac)) in labels.into_iter().enumerate() {
            let w = self.fonts.measure(self.f.sans_med, 13.0, label) + 50.0;
            let b = Rect::new(right - w, r.y, w, r.h);
            let tip = if flac {
                tr("Save as a new FLAC file next to the project\nLossless and smaller; the original is kept")
            } else {
                tr("Save as a new WAV file next to the project\n24 bits, slice cuts included; the original is kept")
            };
            if self.lab_button(c, input, wid("ae-save", i, 0), b, Some(Icon::Save), label, if flac { 0 } else { 1 }, tip) {
                self.ae_save(flac);
            }
            right = b.x - 8.0;
        }
    }

    /// Nothing open: drop a sound, record one, or try the example.
    fn ae_empty(&mut self, c: &mut Canvas, input: &Input, body: Rect) {
        let t = self.theme;
        let card = Rect::new(body.x + body.w * 0.5 - 300.0, body.y + body.h * 0.5 - 170.0, 600.0, 300.0);
        c.fill_rrect(card, 18.0, t.panel);
        c.stroke_rrect(card, 18.0, 1.0, t.hairline);
        glyph(c, G::Wave, card.x + card.w * 0.5, card.y + 60.0, 40.0, t.accent);
        self.text_center(c, self.f.sans_med, 18.0, card.x + card.w * 0.5, card.y + 112.0, tr("Audio editor"), t.text);
        let line = tr("Drop a sound here, open a clip from its panel, or record straight in");
        self.text_center(c, self.f.sans, 13.0, card.x + card.w * 0.5, card.y + 140.0, line, t.text2);
        let (rec_on, w) = (self.audio_edit.capture.is_some(), 200.0);
        let b1 = Rect::new(card.x + card.w * 0.5 - w - 6.0, card.y + 186.0, w, 42.0);
        let label = if rec_on { tr("Stop recording") } else { tr("Record") };
        if self.lab_button(c, input, wid("ae-empty-rec", 0, 0), b1, Some(Icon::Record), label, 1, tr("Record from the microphone into the editor")) {
            self.ae_toggle_record();
        }
        let b2 = Rect::new(card.x + card.w * 0.5 + 6.0, card.y + 186.0, w, 42.0);
        if self.lab_button(c, input, wid("ae-empty-demo", 0, 0), b2, Some(Icon::Play), tr("Example loop"), 0, tr("Open a drum loop to try the editor")) {
            let s = Arc::new(super::samplelab::demo_break(self.sample_rate as f32));
            self.open_sample_in_editor(s, None, Source::Browser);
        }
        if rec_on {
            let secs = self.audio_edit.capture.as_ref().map_or(0.0, |cap| self.time - cap.started);
            self.text_center(c, self.f.mono, 13.0, card.x + card.w * 0.5, card.y + 258.0, &fmt_secs(secs), t.negative);
        }
    }

    // --- Toolbar ---

    /// Square tool button with an editor glyph; `on` shows it pressed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ae_tool(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, g: G, on: bool, enabled: bool, tip: (&str, &str)) -> bool {
        let t = self.theme;
        let hov = enabled && self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.92 } else { 1.0 }, T_PRESS);
        let fill = t.raised.alpha(hv.max(onv)).mix(t.raised_hi, onv * 0.5);
        let rr = r.scale(press);
        c.fill_rrect(rr, 9.0, fill);
        if on {
            c.stroke_rrect(rr, 9.0, 1.0, t.accent.alpha(0.45));
        }
        let col = if enabled { t.text2.mix(t.text, hv).mix(t.accent, onv) } else { t.muted.alpha(0.45) };
        let (cx, cy) = r.center();
        glyph(c, g, cx, cy, 17.0 * press, col);
        if self.tips_hover(input, r, tip) && hov {
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Tooltip of a zone, even disabled; true when hovered.
    fn tips_hover(&mut self, input: &Input, r: Rect, tip: (&str, &str)) -> bool {
        let on = input.inside && r.contains(input.x, input.y);
        if on {
            self.set_tip(tip.0, tip.1);
        }
        on
    }

    fn ae_toolbar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline);
        let cy = r.y + r.h * 0.5;
        let has_sel = self.audio_edit.doc.as_ref().is_some_and(|d| d.sel.is_some());
        let has_clip = self.audio_edit.clipboard.is_some();

        // Play: a big round button.
        let playing = self.ae_playhead().is_some();
        let id = wid("ae-play", 0, 0);
        let pr = Rect::new(r.x + 8.0, cy - 18.0, 36.0, 36.0);
        let hov = self.hovered(input, id, pr);
        let hv = self.hover_anim(id, hov);
        c.fill_circle(pr.x + 18.0, cy, 18.0, t.accent.mix(t.accent_hot, hv * 0.3));
        draw_icon(c, if playing { Icon::Stop } else { Icon::Play }, pr.x + 18.0 + if playing { 0.0 } else { 1.5 }, cy, 16.0, t.on_accent);
        if hov {
            self.set_tip(if has_sel { tr("Play the selection") } else { tr("Play from the cursor") }, tr("Space"));
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.ae_toggle_play();
            }
        }
        let mut x = pr.right() + 8.0;
        let size = 36.0;
        let next = |x: &mut f32, gap: f32| {
            let b = Rect::new(*x + gap, cy - size * 0.5, size, size);
            *x = b.right() + 4.0;
            b
        };
        let looping = self.audio_edit.looping;
        let b = next(&mut x, 0.0);
        if self.ae_tool(c, input, wid("ae-loop", 0, 0), b, G::I(Icon::Loop), looping, true, (tr("Loop the selection while playing"), "L")) {
            self.audio_edit.looping = !looping;
        }
        let rec = self.audio_edit.capture.is_some();
        let b = next(&mut x, 0.0);
        let id = wid("ae-rec", 0, 0);
        if self.ae_tool(c, input, id, b, G::I(Icon::Record), rec, true, (tr("Record from the microphone into the editor"), "R")) {
            self.ae_toggle_record();
        }
        if rec {
            let pulse = 0.5 + 0.5 * (self.time * 6.0).sin();
            c.fill_circle(b.x + b.w * 0.5, b.y + b.h * 0.5, 5.5, t.negative.alpha(0.6 + 0.4 * pulse));
        }
        let sep = |c: &mut Canvas, x: f32| c.fill_rect(Rect::new(x, cy - 14.0, 1.0, 28.0), t.hairline);
        let s0 = x + 4.0;
        sep(c, s0);
        let (can_undo, can_redo) = (self.ae_can_undo(false), self.ae_can_undo(true));
        let b = next(&mut x, 10.0);
        if self.ae_tool(c, input, wid("ae-undo", 0, 0), b, G::I(Icon::Undo), false, can_undo, (tr("Undo in the editor"), tr("Ctrl+Z"))) {
            self.ae_undo(false);
        }
        let b = next(&mut x, 0.0);
        if self.ae_tool(c, input, wid("ae-redo", 0, 0), b, G::I(Icon::Redo), false, can_redo, (tr("Redo in the editor"), tr("Ctrl+Y"))) {
            self.ae_undo(true);
        }
        sep(c, x + 4.0);
        let tools: [(G, &str, &str, bool, u8); 6] = [
            (G::Scissors, tr("Cut"), tr("Ctrl+X"), has_sel, 0),
            (G::Copy, tr("Copy"), tr("Ctrl+C"), has_sel, 1),
            (G::Paste, tr("Paste: replaces the selection, or goes in at the cursor"), tr("Ctrl+V"), has_clip, 2),
            (G::I(Icon::Trash), tr("Delete the selection"), tr("Del"), has_sel, 3),
            (G::Crop, tr("Crop: keep only the selection"), tr("Ctrl+Del"), has_sel, 4),
            (G::Silence, tr("Insert silence at the cursor (or silence the selection)"), tr("Ctrl+I"), true, 5),
        ];
        for (i, (g, tip, key, enabled, action)) in tools.into_iter().enumerate() {
            let b = next(&mut x, if i == 0 { 10.0 } else { 0.0 });
            if self.ae_tool(c, input, wid("ae-edit", i, 0), b, g, false, enabled, (tip, key)) {
                match action {
                    0 => self.ae_cut(),
                    1 => {
                        self.ae_copy();
                    }
                    2 => self.ae_paste(),
                    3 => self.ae_delete(),
                    4 => self.ae_crop(),
                    _ => self.ae_insert_silence(),
                }
            }
        }
        sep(c, x + 4.0);
        let (sz, sb) = (self.audio_edit.snap_zero, self.audio_edit.snap_beat);
        let b = next(&mut x, 10.0);
        if self.ae_tool(c, input, wid("ae-snapz", 0, 0), b, G::ZeroX, sz, true, (tr("Snap to zero crossings: cuts never click\nHold Alt to place freely"), "")) {
            self.audio_edit.snap_zero = !sz;
        }
        let b = next(&mut x, 0.0);
        let has_bpm = self.audio_edit.doc.as_ref().is_some_and(|d| d.bpm.is_some());
        let tip = if has_bpm { tr("Snap to the beat (sixteenths of the sound's tempo)") } else { tr("Snap to the beat: detect the tempo first (Slices tab)") };
        if self.ae_tool(c, input, wid("ae-snapb", 0, 0), b, G::I(Icon::Magnet), sb, true, (tip, "")) {
            self.audio_edit.snap_beat = !sb;
        }
        sep(c, x + 4.0);
        // Waveform or spectrogram.
        let seg = Rect::new(x + 10.0, cy - 18.0, 88.0, 36.0);
        x = seg.right() + 4.0;
        let spec = self.audio_edit.spectrum as usize;
        let tips = [(tr("Waveform"), ""), (tr("Spectrogram: frequencies over time, bass at the bottom"), tr("Shift+S"))];
        if let Some(i) = self.ae_glyph_segmented(c, input, wid("ae-viewmode", 0, 0), seg, &[G::Wave, G::Spectrum], &tips, spec) {
            self.audio_edit.spectrum = i == 1;
        }
        let b = next(&mut x, 4.0);
        if self.ae_tool(c, input, wid("ae-zsel", 0, 0), b, G::ZoomSel, false, has_sel, (tr("Zoom to the selection"), "Z")) {
            self.ae_zoom_selection();
        }
        let b = next(&mut x, 0.0);
        if self.ae_tool(c, input, wid("ae-zall", 0, 0), b, G::I(Icon::Columns), false, true, (tr("Zoom out to see the whole sound"), tr("Home"))) {
            self.ae_zoom_all();
        }
        sep(c, x + 4.0);
        let b = next(&mut x, 10.0);
        if self.ae_tool(c, input, wid("ae-marker", 0, 0), b, G::Marker, false, true, (tr("Add a marker at the cursor"), "M")) {
            self.ae_add_marker();
        }
        let b = next(&mut x, 0.0);
        if self.ae_tool(c, input, wid("ae-region", 0, 0), b, G::Region, false, has_sel, (tr("Make a region of the selection"), tr("Shift+M"))) {
            self.ae_add_region();
        }
        // The last result, on the right, fading after a while.
        if let Some(note) = self.audio_edit.note.as_ref() {
            let age = self.time - note.at;
            let a = (1.0 - (age - 5.0) / 1.0).clamp(0.0, 1.0);
            if a > 0.0 {
                let text = note.text.clone();
                let room = r.right() - 16.0 - (x + 16.0);
                if room > 80.0 {
                    let s = self.fit(self.f.sans, 13.0, &text, room - 30.0);
                    let w = self.fonts.measure(self.f.sans, 13.0, &s) + 34.0;
                    let pill = Rect::new(r.right() - 10.0 - w, cy - 14.0, w, 28.0);
                    c.fill_rrect(pill, 14.0, t.accent.alpha(0.12 * a));
                    draw_icon(c, Icon::Check, pill.x + 15.0, cy, 12.0, t.accent.alpha(a));
                    self.text(c, self.f.sans, 13.0, pill.x + 26.0, cy, &s, t.text.alpha(a));
                }
            }
        }
    }

    /// Segmented control of editor glyphs.
    fn ae_glyph_segmented(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, gs: &[G], tips: &[(&str, &str)], sel: usize) -> Option<usize> {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.bg_deep);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let n = gs.len().max(1) as f32;
        let seg_w = (r.w - 6.0) / n;
        let x = self.anim.to(id, sel as f32, T_LAYOUT);
        c.fill_rrect(Rect::new(r.x + 3.0 + x * seg_w, r.y + 3.0, seg_w, r.h - 6.0), 7.0, t.raised_hi);
        let mut picked = None;
        for (i, &g) in gs.iter().enumerate() {
            let seg = Rect::new(r.x + 3.0 + i as f32 * seg_w, r.y, seg_w, r.h);
            let hov = self.hovered(input, id ^ (i as u64 + 1), seg);
            let hv = self.hover_anim(id ^ (i as u64 + 1), hov);
            let selected = 1.0 - (x - i as f32).abs().min(1.0);
            let (cx, cy) = seg.center();
            glyph(c, g, cx, cy, 16.0, t.muted.mix(t.text, hv * 0.7).mix(t.accent, selected));
            if hov {
                if let Some(tip) = tips.get(i) {
                    self.set_tip(tip.0, tip.1);
                }
                if i != sel {
                    self.set_cursor(Cursor::Hand);
                    if input.pressed {
                        picked = Some(i);
                    }
                }
            }
        }
        picked
    }

    // --- Overview, ruler ---

    fn ae_overview(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.audio_edit.doc.as_ref() else { return };
        let Some(peaks) = self.audio_edit.peaks.as_ref() else { return };
        let len = doc.len().max(1) as f64;
        let sel = doc.sel;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset_xy(8.0, 5.0);
        let mid = g.y + g.h * 0.5;
        let n = peaks.left.len().max(1);
        let gw = g.w.max(1.0) as usize;
        for px in 0..gw {
            let a = px * n / gw;
            let b = ((px + 1) * n / gw).max(a + 1).min(n);
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for j in a.min(n - 1)..b {
                lo = lo.min(peaks.left[j].0.min(peaks.right[j].0));
                hi = hi.max(peaks.left[j].1.max(peaks.right[j].1));
            }
            let frame = a * BUCKET;
            let inside = sel.is_some_and(|(sa, sb)| frame >= sa && frame < sb);
            let col = if inside { t.accent_hot.alpha(0.85) } else { t.accent.alpha(0.55) };
            c.fill_rect(Rect::new(g.x + px as f32, mid - hi.min(1.0) * g.h * 0.5, 1.0, ((hi - lo).min(2.0) * g.h * 0.5).max(1.0)), col);
        }
        let (vs, vl) = self.audio_edit.shown;
        let vx = g.x + (vs / len) as f32 * g.w;
        let vw = ((vl / len) as f32 * g.w).max(6.0);
        let id = wid("ae-ov", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        let win = Rect::new(vx, r.y + 1.0, vw, r.h - 2.0);
        if vl < len - 1.0 {
            c.fill_rrect(win, 8.0, t.select.alpha(0.08 + 0.06 * hv));
            c.stroke_rrect(win, 8.0, 1.0, t.select.alpha(0.6 + 0.3 * hv));
        }
        if let Some(p) = self.ae_playhead() {
            let x = g.x + (p / len) as f32 * g.w;
            c.fill_rect(Rect::new(x.round(), r.y + 2.0, 1.0, r.h - 4.0), t.playhead);
        }
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
            self.audio_edit.grab = Some(Grab::Overview);
        }
        if self.is_dragging(id) && self.audio_edit.grab == Some(Grab::Overview) {
            let center = ((input.x - g.x) / g.w).clamp(0.0, 1.0) as f64 * len;
            let vl = self.audio_edit.view.1;
            self.audio_edit.view.0 = (center - vl * 0.5).clamp(0.0, (len - vl).max(0.0));
            self.audio_edit.shown.0 = self.audio_edit.view.0;
        }
        if hov {
            self.set_tip(tr("The whole sound\nClick or drag to move the view"), "");
            self.set_cursor(Cursor::Hand);
        }
    }

    /// Time ruler, markers (flags) and regions (bands).
    fn ae_ruler(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.audio_edit.doc.clone() else { return };
        let sr = doc.sample.sample_rate.max(1.0) as f64;
        let (vs, vl) = self.audio_edit.shown;
        let x_of = |f: f64| r.x + ((f - vs) / vl) as f32 * r.w;
        let f_at = |x: f32| (vs + ((x - r.x) / r.w) as f64 * vl).clamp(0.0, doc.len() as f64);
        let prev = c.push_clip(r);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        let px_per_sec = r.w as f64 / (vl / sr);
        let step = ruler_step(px_per_sec, 90.0);
        let minor = step / 5.0;
        let t0 = (vs / sr / minor).floor() * minor;
        let mut tt = t0;
        while tt <= (vs + vl) / sr + minor {
            let x = x_of(tt * sr).round();
            let major = ((tt / step).round() * step - tt).abs() < minor * 0.25;
            let h = if major { 9.0 } else { 4.0 };
            c.fill_rect(Rect::new(x, r.bottom() - h, 1.0, h), if major { t.muted } else { t.hairline });
            if major && x >= r.x {
                let s = ruler_label(tt.max(0.0), step);
                self.text(c, self.f.mono, 10.0, x + 4.0, r.y + 9.0, &s, t.muted);
            }
            tt += minor;
        }
        // Regions: bands at the bottom of the ruler.
        let mut del_region = None;
        for (k, reg) in doc.regions.iter().enumerate() {
            let (xa, xb) = (x_of(reg.start as f64), x_of(reg.end as f64));
            if xb < r.x || xa > r.right() {
                continue;
            }
            let col = t.markers[(k + 3) % t.markers.len()];
            let band = Rect::new(xa, r.y + 17.0, (xb - xa).max(2.0), 12.0);
            let id = wid("ae-region", k, 0);
            let hov = self.hovered(input, id, band);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(band, 4.0, col.alpha(0.35 + 0.25 * hv));
            let name = self.fit(self.f.sans_med, 10.0, &reg.name, band.w - 8.0);
            self.text(c, self.f.sans_med, 10.0, band.x + 5.0, band.y + 6.0, &name, t.text);
            if hov {
                self.set_tip(&tr_fmt!("{}\nClick: select it · right-click: remove it", reg.name), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.ae_select(reg.start, reg.end);
                } else if input.right_pressed {
                    del_region = Some(k);
                }
            }
        }
        // Markers: flags.
        let mut del_marker = None;
        for (k, m) in doc.markers.iter().enumerate() {
            let x = x_of(m.pos as f64).round();
            if x < r.x - 60.0 || x > r.right() {
                continue;
            }
            let col = t.markers[k % t.markers.len()];
            let id = wid("ae-mk", k, 0);
            let label_w = self.fonts.measure(self.f.sans_med, 10.0, &m.name) + 12.0;
            let flag = Rect::new(x, r.y + 2.0, label_w, 14.0);
            let hit = Rect::new(x - 4.0, r.y, label_w + 4.0, r.h);
            let hov = self.hovered(input, id, hit) || self.audio_edit.grab == Some(Grab::Marker(k));
            let hv = self.hover_anim(id, hov);
            c.fill_rect(Rect::new(x, r.y + 2.0, 1.5, r.h - 2.0), col);
            c.fill_rrect(flag, 3.0, col.alpha(0.75 + 0.25 * hv));
            self.text(c, self.f.sans_med, 10.0, flag.x + 6.0, flag.y + 7.0, &m.name, t.on_accent);
            if hov {
                self.set_tip(&tr_fmt!("{}\nDrag to move · click: put the cursor there · right-click: remove", m.name), "");
                self.set_cursor(Cursor::ResizeH);
                if input.pressed {
                    self.begin_drag(id, input, 0.0);
                    self.audio_edit.grab = Some(Grab::Marker(k));
                    if let Some(d) = self.audio_edit.doc.as_mut() {
                        d.cursor = m.pos;
                        d.sel = None;
                    }
                } else if input.right_pressed {
                    del_marker = Some(k);
                }
            }
        }
        if let Some(Grab::Marker(k)) = self.audio_edit.grab
            && input.down
        {
            let f = self.ae_snap(f_at(input.x) as usize, input.mods);
            if let Some(d) = self.audio_edit.doc.as_mut()
                && let Some(m) = d.markers.get_mut(k)
            {
                m.pos = f;
                d.cursor = f;
            }
        }
        if let Some(k) = del_marker {
            self.ae_edit(tr("Remove a marker"), |d, _| (k < d.markers.len()).then(|| tr_fmt!("{} removed", d.markers.remove(k).name)));
        }
        if let Some(k) = del_region {
            self.ae_edit(tr("Remove a region"), |d, _| (k < d.regions.len()).then(|| tr_fmt!("{} removed", d.regions.remove(k).name)));
        }
        // Click on the bare ruler: cursor there, playing from it.
        let id = wid("ae-ruler", 0, 0);
        if self.audio_edit.grab.is_none() && self.hovered(input, id, r) {
            self.set_tip(tr("Click to play from here"), "");
            if input.pressed {
                let f = f_at(input.x) as usize;
                if let Some(d) = self.audio_edit.doc.as_mut() {
                    d.cursor = f;
                    d.sel = None;
                }
                let len = doc.len();
                self.ae_play_range(f, len, false);
            }
        }
        if let Some(p) = self.ae_playhead() {
            let x = x_of(p).round();
            c.fill_rect(Rect::new(x, r.y, 1.0, r.h), t.playhead);
        }
        c.set_clip(Some(prev));
    }

    // --- Waveform ---

    fn ae_wave(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.audio_edit.doc.clone() else { return };
        let len = doc.len();
        let lenf = len.max(1) as f64;
        let (vs, vl) = self.audio_edit.shown;
        let x_of = |f: f64| r.x + ((f - vs) / vl) as f32 * r.w;
        let f_at = |x: f32| (vs + ((x - r.x) / r.w) as f64 * vl).clamp(0.0, lenf);
        let lane = Rect::new(r.x, r.y, r.w, LANE_H);
        let area = Rect::new(r.x, lane.bottom(), r.w, r.h - LANE_H);
        let spectrum = self.audio_edit.spectrum;
        let mono = self.audio_edit.peaks.as_ref().is_none_or(|p| p.mono);

        // Slices: alternate shading.
        let bounds = doc.bounds();
        if bounds.len() > 2 {
            for k in 0..bounds.len() - 1 {
                let (xa, xb) = (x_of(bounds[k] as f64), x_of(bounds[k + 1] as f64));
                if xb < r.x || xa > r.right() || k % 2 == 0 {
                    continue;
                }
                c.fill_rect(Rect::new(xa, area.y, xb - xa, area.h), t.select.alpha(0.05));
            }
        }
        // Regions: tinted spans.
        for (k, reg) in doc.regions.iter().enumerate() {
            let (xa, xb) = (x_of(reg.start as f64).max(r.x), x_of(reg.end as f64).min(r.right()));
            if xb > xa {
                let col = t.markers[(k + 3) % t.markers.len()];
                c.fill_rect(Rect::new(xa, area.y, xb - xa, area.h), col.alpha(0.05));
                c.fill_rect(Rect::new(xa, area.y, xb - xa, 2.0), col.alpha(0.8));
            }
        }

        if spectrum {
            self.ae_spectrogram(c, area, &doc);
        } else {
            let lanes: Vec<(Rect, bool)> = if mono {
                vec![(area, true)]
            } else {
                let h = area.h * 0.5;
                vec![(Rect::new(area.x, area.y, area.w, h), true), (Rect::new(area.x, area.y + h, area.w, h), false)]
            };
            for (k, &(lr, left)) in lanes.iter().enumerate() {
                self.ae_channel(c, lr, &doc, left);
                if k == 1 {
                    c.fill_rect(Rect::new(lr.x, lr.y, lr.w, 1.0), t.hairline);
                }
                if !mono {
                    self.text(c, self.f.mono, 10.0, lr.x + 8.0, lr.y + 10.0, if left { "L" } else { "R" }, t.muted);
                }
            }
        }

        // Selection.
        if let Some((a, b)) = doc.sel {
            let (xa, xb) = (x_of(a as f64), x_of(b as f64));
            if xb > r.x && xa < r.right() {
                let sr = Rect::new(xa.max(r.x - 2.0), area.y, (xb.min(r.right() + 2.0) - xa.max(r.x - 2.0)).max(1.0), area.h);
                c.fill_rect(sr, t.accent.alpha(if spectrum { 0.12 } else { 0.16 }));
                c.fill_rect(Rect::new(xa.round() - 0.5, r.y, 1.5, r.h), t.accent);
                c.fill_rect(Rect::new(xb.round() - 1.0, r.y, 1.5, r.h), t.accent);
                // Edge tabs, to grab.
                for x in [xa, xb] {
                    c.fill_rrect(Rect::new(x - 4.0, area.bottom() - 18.0, 8.0, 18.0), 3.0, t.accent);
                }
            }
        }
        // Markers through the waveform.
        for (k, m) in doc.markers.iter().enumerate() {
            let x = x_of(m.pos as f64).round();
            if x >= r.x && x <= r.right() {
                let col = t.markers[k % t.markers.len()];
                let mut y = area.y;
                while y < area.bottom() {
                    c.fill_rect(Rect::new(x, y, 1.0, 5.0), col.alpha(0.7));
                    y += 9.0;
                }
            }
        }
        // Cursor.
        if doc.sel.is_none() {
            let x = x_of(doc.cursor as f64).round();
            if x >= r.x && x <= r.right() {
                c.fill_rect(Rect::new(x, r.y, 1.5, r.h), t.text.alpha(0.8));
            }
        }
        // Slice lane and cuts.
        self.ae_slice_lane(c, input, lane, area, &bounds);
        // Playhead.
        if let Some(p) = self.ae_playhead() {
            let x = x_of(p).round();
            if x >= r.x && x <= r.right() {
                c.fill_rect(Rect::new(x - 0.5, r.y, 2.0, r.h), t.playhead);
            }
        }

        // Gestures.
        self.ae_wave_input(c, input, area, &doc, f_at);
        // Zoom readout, while zoomed in very close.
        let fpp = vl / r.w as f64;
        if fpp < 1.0 {
            let s = tr_fmt!("{} samples across", fr(vl as f32, 0));
            self.text_right(c, self.f.mono, 10.0, r.right() - 10.0, area.bottom() - 10.0, &s, t.muted);
        }
        let _ = x_of;
    }

    /// One channel's waveform in `r`.
    fn ae_channel(&mut self, c: &mut Canvas, r: Rect, doc: &super::audioedit::Doc, left: bool) {
        let t = self.theme;
        let Some(peaks) = self.audio_edit.peaks.as_ref() else { return };
        let peaks = if left { &peaks.left } else { &peaks.right };
        let x = if left { &doc.sample.left } else { &doc.sample.right };
        let len = x.len();
        let (vs, vl) = self.audio_edit.shown;
        let x_of = |f: f64| r.x + ((f - vs) / vl) as f32 * r.w;
        let f_at = |px: f32| (vs + ((px - r.x) / r.w) as f64 * vl).clamp(0.0, len as f64);
        let mid = r.y + r.h * 0.5;
        let amp = r.h * 0.45;
        c.fill_rect(Rect::new(r.x, mid.round(), r.w, 1.0), t.hairline);
        for db in [-6.0f32, -12.0] {
            let g = 10f32.powf(db / 20.0) * amp;
            c.fill_rect(Rect::new(r.x, (mid - g).round(), r.w, 1.0), t.hairline.alpha(0.35));
            c.fill_rect(Rect::new(r.x, (mid + g).round(), r.w, 1.0), t.hairline.alpha(0.35));
        }
        let sel = doc.sel;
        let fpp = vl / r.w as f64;
        let in_sel = |f: usize| sel.is_some_and(|(a, b)| f >= a && f < b);
        if fpp < 1.5 {
            // Close: the samples themselves, joined.
            let first = f_at(r.x).floor().max(0.0) as usize;
            let last = (f_at(r.right()).ceil() as usize + 2).min(len);
            if last <= first {
                return;
            }
            let pts: Vec<(f32, f32)> = (first..last).map(|i| (x_of(i as f64), mid - x[i].clamp(-1.2, 1.2) * amp)).collect();
            c.stroke_polyline(&pts, 1.6, t.accent);
            if fpp < 0.25 {
                for (k, &(px, py)) in pts.iter().enumerate() {
                    let col = if in_sel(first + k) { t.accent_hot } else { t.accent };
                    c.fill_rect(Rect::new(px - 0.5, py.min(mid), 1.0, (py - mid).abs()), col.alpha(0.35));
                    c.fill_circle(px, py, 2.6, col);
                }
            }
            return;
        }
        let x0 = x_of(0.0).max(r.x) as i32;
        let x1 = x_of(len as f64).min(r.right()) as i32;
        let n = peaks.len();
        for px in x0..x1 {
            let fa = f_at(px as f32);
            let fb = f_at(px as f32 + 1.0);
            let (mut lo, mut hi, mut sq) = (0.0f32, 0.0f32, 0.0f32);
            if fpp >= BUCKET as f64 {
                let (ba, bb) = ((fa as usize / BUCKET).min(n.saturating_sub(1)), (fb as usize / BUCKET).clamp(fa as usize / BUCKET + 1, n));
                for &(l, h) in &peaks[ba..bb] {
                    lo = lo.min(l);
                    hi = hi.max(h);
                }
                // A light inner band: about the RMS of the peaks.
                sq = (hi - lo) * 0.25;
            } else {
                let (ia, ib) = (fa as usize, (fb as usize).max(fa as usize + 1).min(len));
                for &v in &x[ia..ib] {
                    lo = lo.min(v);
                    hi = hi.max(v);
                    sq += v * v;
                }
                sq = (sq / (ib - ia).max(1) as f32).sqrt();
            }
            let f = fa as usize;
            let selected = in_sel(f);
            let col = if selected { t.accent_hot } else { t.accent };
            let xx = px as f32;
            let (top, bottom) = (mid - hi.min(1.2) * amp, mid - lo.max(-1.2) * amp);
            c.fill_rect(Rect::new(xx, top, 1.0, (bottom - top).max(1.0)), col.alpha(0.78));
            c.fill_rect(Rect::new(xx, mid - sq.min(1.0) * amp, 1.0, (2.0 * sq.min(1.0) * amp).max(0.0)), col.mix(t.text, 0.35).alpha(0.55));
        }
    }

    /// The spectrogram of the view, recomputed once the view settles.
    fn ae_spectrogram(&mut self, c: &mut Canvas, r: Rect, doc: &super::audioedit::Doc) {
        let t = self.theme;
        let (vs, vl) = self.audio_edit.shown;
        let phys = c.to_physical(r);
        let (w, h) = (phys.w.max(1.0) as usize, phys.h.max(1.0) as usize);
        let key = (Arc::as_ptr(&doc.sample) as usize, vs.to_bits(), vl.to_bits(), w, h);
        let settled = self.audio_edit.shown == self.audio_edit.view;
        let stale = self.audio_edit.spec.as_ref().is_none_or(|s| s.key != key);
        if stale && (settled || self.audio_edit.spec.as_ref().is_none_or(|s| s.w != w || s.h != h)) {
            let mono = ponzik_dsp::onset::mono(&doc.sample.left, &doc.sample.right);
            let cols = w.min(1_600);
            let sr = doc.sample.sample_rate;
            let fft = if vl / (cols as f64) < 256.0 { 2048 } else { 4096 };
            let sg = spectrogram::compute(&mono, sr, vs, vs + vl, cols, SPEC_ROWS, fft);
            let mut masks = [vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]];
            for py in 0..h {
                let row = ((h - 1 - py) as f32 / h as f32 * SPEC_ROWS as f32) as usize;
                for px in 0..w {
                    let col = px * cols / w;
                    let db = sg.at(col, row.min(SPEC_ROWS - 1));
                    let lvl = |lo: f32, span: f32| (((db - lo) / span).clamp(0.0, 1.0) * 255.0) as u8;
                    masks[0][py * w + px] = (lvl(-100.0, 50.0) as f32 * 0.8) as u8;
                    masks[1][py * w + px] = lvl(-58.0, 30.0);
                    masks[2][py * w + px] = lvl(-30.0, 24.0);
                }
            }
            self.audio_edit.spec = Some(SpecImage { key, w, h, masks });
        }
        c.fill_rect(r, t.bg_deep);
        if let Some(img) = self.audio_edit.spec.as_ref() {
            let (x, y) = (phys.x.round() as i32, phys.y.round() as i32);
            let cols = [t.select.mix(t.bg_deep, 0.25), t.accent, t.accent_hot.mix(WHITE, 0.25)];
            for (m, col) in img.masks.iter().zip(cols) {
                c.draw_mask(x, y, img.w, img.h, m, col);
            }
        }
        // Frequency scale.
        let sr = doc.sample.sample_rate;
        for hz in [100.0f32, 1_000.0, 10_000.0] {
            let rows = (spectrogram::LOW_HZ.ln()..=(sr * 0.5).ln()).contains(&hz.ln());
            if !rows {
                continue;
            }
            let frac = (hz / spectrogram::LOW_HZ).ln() / ((sr * 0.5) / spectrogram::LOW_HZ).ln();
            let y = (r.bottom() - frac * r.h).round();
            c.fill_rect(Rect::new(r.x, y, r.w, 1.0), t.text.alpha(0.08));
            let label = if hz >= 1_000.0 { format!("{} kHz", hz / 1_000.0) } else { format!("{hz} Hz") };
            let lw = self.fonts.measure(self.f.mono, 10.0, &label) + 10.0;
            c.fill_rrect(Rect::new(r.x + 4.0, y - 15.0, lw, 14.0), 4.0, t.bg_deep.alpha(0.75));
            self.text(c, self.f.mono, 10.0, r.x + 9.0, y - 8.0, &label, t.text);
        }
    }

    /// Handles and cuts of the slices; drag to move, click the lane to add,
    /// right-click a handle to remove.
    fn ae_slice_lane(&mut self, c: &mut Canvas, input: &Input, lane: Rect, area: Rect, bounds: &[usize]) {
        let t = self.theme;
        let (vs, vl) = self.audio_edit.shown;
        let lenf = self.audio_edit.doc.as_ref().map_or(1.0, |d| d.len().max(1) as f64);
        let x_of = |f: f64| lane.x + ((f - vs) / vl) as f32 * lane.w;
        let f_at = |x: f32| (vs + ((x - lane.x) / lane.w) as f64 * vl).clamp(0.0, lenf);
        c.fill_rect(lane, t.bg_deep.alpha(0.35));
        let mut over_handle = false;
        let mut remove = None;
        for (k, &bound) in bounds.iter().enumerate().take(bounds.len().saturating_sub(1)) {
            let x = x_of(bound as f64).round();
            if x < lane.x - 30.0 || x > lane.right() {
                continue;
            }
            let label = format!("{}", k + 1);
            let lw = self.fonts.measure(self.f.mono, 10.0, &label) + 8.0;
            let tab = Rect::new(x, lane.y + 3.0, lw, lane.h - 6.0);
            let movable = k > 0;
            let id = wid("ae-slice", k, 0);
            let grabbed = self.audio_edit.grab == Some(Grab::Slice(k - movable as usize)) && movable;
            let hov = movable && (self.hovered(input, id, Rect::new(x - 4.0, lane.y, lw + 4.0, lane.h)) || grabbed);
            let hv = self.hover_anim(id, hov);
            let col = t.accent.mix(t.accent_hot, hv);
            c.fill_rect(Rect::new(x, area.y, 1.0, area.h), col.alpha(0.45 + 0.35 * hv));
            c.fill_rrect(tab, 4.0, col.alpha(0.85));
            self.text(c, self.f.mono, 10.0, tab.x + 4.0, tab.y + tab.h * 0.5, &label, t.on_accent);
            if hov {
                over_handle = true;
                self.set_cursor(Cursor::ResizeH);
                self.set_tip(tr("Slice cut\nDrag to move · right-click to remove"), "");
                if input.pressed {
                    self.begin_drag(id, input, 0.0);
                    self.audio_edit.grab = Some(Grab::Slice(k - 1));
                } else if input.right_pressed {
                    remove = Some(k - 1);
                }
            }
        }
        if let Some(Grab::Slice(j)) = self.audio_edit.grab
            && input.down
        {
            let f = self.ae_snap(f_at(input.x) as usize, input.mods);
            if let Some(d) = self.audio_edit.doc.as_mut() {
                let lo = if j == 0 { 1 } else { d.slices[j - 1] + 1 };
                let hi = d.slices.get(j + 1).copied().unwrap_or(d.len()).saturating_sub(1);
                if let Some(s) = d.slices.get_mut(j) {
                    *s = f.clamp(lo, hi.max(lo));
                }
            }
        }
        if let Some(j) = remove {
            self.ae_remove_slice(j);
        }
        let id = wid("ae-lane", 0, 0);
        if !over_handle && self.audio_edit.grab.is_none() && self.hovered(input, id, lane) {
            self.set_tip(tr("Slice lane\nClick to add a slice cut here"), "");
            self.set_cursor(Cursor::Crosshair);
            let x = input.x.round();
            c.fill_rect(Rect::new(x, lane.y, 1.0, lane.h + area.h), t.accent.alpha(0.35));
            if input.pressed {
                let f = self.ae_snap(f_at(input.x) as usize, input.mods);
                self.ae_add_slice(f);
            }
        }
    }

    /// Selecting, extending, the cursor, zoom and scroll.
    fn ae_wave_input(&mut self, c: &mut Canvas, input: &Input, area: Rect, doc: &super::audioedit::Doc, f_at: impl Fn(f32) -> f64) {
        let _ = c;
        let id = wid("ae-wave", 0, 0);
        let (vs, vl) = self.audio_edit.shown;
        let x_of = |f: f64| area.x + ((f - vs) / vl) as f32 * area.w;
        let hov = self.hovered(input, id, area);
        if hov {
            if input.wheel != 0.0 {
                let at = f_at(input.x);
                if input.mods.shift {
                    self.ae_scroll(-input.wheel as f64 * vl * 0.15);
                } else {
                    self.ae_zoom(if input.wheel > 0.0 { 0.8 } else { 1.25 }, at);
                }
            }
            let near_edge = doc.sel.and_then(|(a, b)| {
                let (xa, xb) = (x_of(a as f64), x_of(b as f64));
                if (input.x - xa).abs() < 6.0 { Some(true) } else if (input.x - xb).abs() < 6.0 { Some(false) } else { None }
            });
            if self.audio_edit.grab.is_none() {
                self.set_cursor(if near_edge.is_some() { Cursor::ResizeH } else { Cursor::Text });
                self.set_tip(tr("Drag to select · Shift+click extends · double-click selects a slice\nRight-click plays from here · wheel zooms, Shift+wheel scrolls"), "");
            }
            if input.pressed {
                self.begin_drag(id, input, 0.0);
                let f = self.ae_snap(f_at(input.x) as usize, input.mods);
                if input.double {
                    // The slice (or region) under the pointer.
                    let b = doc.bounds();
                    let k = b.partition_point(|&m| m <= f).saturating_sub(1).min(b.len() - 2);
                    let (a, e) = doc.regions.iter().find(|r| f >= r.start && f < r.end).map_or((b[k], b[k + 1]), |r| (r.start, r.end));
                    self.ae_select(a, e);
                    self.audio_edit.grab = None;
                } else if let Some(start) = near_edge {
                    self.audio_edit.grab = Some(Grab::Edge(start));
                } else if input.mods.shift && let Some((a, b)) = doc.sel {
                    let anchor = if f < (a + b) / 2 { b } else { a };
                    self.audio_edit.grab = Some(Grab::Select(anchor));
                    self.ae_select(anchor, f);
                } else {
                    self.audio_edit.grab = Some(Grab::Select(f));
                    if let Some(d) = self.audio_edit.doc.as_mut() {
                        d.sel = None;
                        d.cursor = f;
                    }
                }
            }
            if input.right_pressed {
                let f = self.ae_snap(f_at(input.x) as usize, input.mods);
                if let Some(d) = self.audio_edit.doc.as_mut() {
                    d.cursor = f;
                    d.sel = None;
                }
                let len = doc.len();
                self.ae_play_range(f, len, false);
            }
            if input.middle_pressed {
                self.audio_edit.grab = Some(Grab::Pan(self.audio_edit.view.0, input.x));
            }
        }
        match self.audio_edit.grab {
            Some(Grab::Select(anchor)) if input.down => {
                let f = self.ae_snap(f_at(input.x.clamp(area.x, area.right())) as usize, input.mods);
                // Under 3 pixels it stays a click: the cursor moves, nothing is selected.
                let moved = (x_of(f as f64) - x_of(anchor as f64)).abs() >= 3.0;
                if moved {
                    self.ae_select(anchor, f);
                    if let Some(d) = self.audio_edit.doc.as_mut() {
                        d.cursor = anchor.min(f);
                    }
                }
                self.ae_autoscroll(input, area);
            }
            Some(Grab::Edge(start)) if input.down => {
                let f = self.ae_snap(f_at(input.x.clamp(area.x, area.right())) as usize, input.mods);
                if let Some((a, b)) = doc.sel {
                    let (na, nb) = if start { (f, b) } else { (a, f) };
                    self.ae_select(na, nb);
                    if na > nb {
                        self.audio_edit.grab = Some(Grab::Edge(!start));
                    }
                }
                self.ae_autoscroll(input, area);
            }
            Some(Grab::Pan(v0, x0)) if input.middle_down => {
                let len = doc.len().max(1) as f64;
                let df = (x0 - input.x) as f64 / area.w as f64 * vl;
                self.audio_edit.view.0 = (v0 + df).clamp(0.0, (len - vl).max(0.0));
                self.audio_edit.shown.0 = self.audio_edit.view.0;
            }
            Some(Grab::Pan(..)) => self.audio_edit.grab = None,
            _ => {}
        }
    }

    /// Dragging past an edge scrolls the view.
    fn ae_autoscroll(&mut self, input: &Input, area: Rect) {
        let vl = self.audio_edit.view.1;
        let over = if input.x < area.x { input.x - area.x } else if input.x > area.right() { input.x - area.right() } else { 0.0 };
        if over != 0.0 {
            self.ae_scroll(over as f64 / area.w as f64 * vl * 0.25);
        }
    }

    /// End of a selection gesture: the selection is kept in the undo history
    /// only through the edits made on it, so nothing to do but tidy up.
    fn ae_end_select(&mut self) {
        if let Some(d) = self.audio_edit.doc.as_mut()
            && let Some((a, b)) = d.sel
            && b <= a
        {
            d.sel = None;
        }
    }

    /// A recording in progress: its level as it grows, and the time.
    fn ae_recording(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        let Some(cap) = self.audio_edit.capture.as_ref() else { return };
        let secs = self.time - cap.started;
        let levels = cap.levels.clone();
        c.fill_rect(r, t.negative.alpha(0.04));
        let mid = r.y + r.h * 0.5 + 20.0;
        let span = levels.len().max(600);
        // The take so far, as a mirrored level envelope.
        if levels.len() > 1 {
            let x_at = |i: usize| r.x + 20.0 + i as f32 * (r.w - 40.0) / span as f32;
            let top: Vec<(f32, f32)> = levels.iter().enumerate().map(|(i, &l)| (x_at(i), mid - l * r.h * 0.36 - 0.5)).collect();
            let mut shape: Vec<(f32, f32)> = top.clone();
            shape.extend(top.iter().rev().map(|&(x, y)| (x, 2.0 * mid - y)));
            let mut sh = Shape::new();
            sh.polygon(&shape);
            c.fill_path(&sh, t.negative.alpha(0.55));
            c.stroke_polyline(&top, 1.2, t.negative);
            let bottom: Vec<(f32, f32)> = top.iter().map(|&(x, y)| (x, 2.0 * mid - y)).collect();
            c.stroke_polyline(&bottom, 1.2, t.negative);
            let (hx, _) = top[top.len() - 1];
            c.fill_rect(Rect::new(hx, r.y + 70.0, 1.5, r.h - 80.0), t.negative);
        }
        let pulse = 0.5 + 0.5 * (self.time * 5.0).sin();
        c.fill_circle(r.x + 30.0, r.y + 30.0, 8.0, t.negative.alpha(0.6 + 0.4 * pulse));
        let s = tr_fmt!("Recording · {} / {}", fmt_secs(secs), fmt_secs(REC_SECONDS));
        self.text(c, self.f.sans_med, 15.0, r.x + 48.0, r.y + 30.0, &s, t.text);
        self.text(c, self.f.sans, 13.0, r.x + 48.0, r.y + 52.0, tr("Press R or the record button to stop: the take opens here"), t.text2);
    }

    // --- Status bar ---

    fn ae_status(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.audio_edit.doc.as_ref() else { return };
        let sr = doc.sample.sample_rate.max(1.0) as f64;
        let cy = r.y + r.h * 0.5;
        c.fill_rrect(r, 10.0, t.panel);
        let mut x = r.x + 12.0;
        let pos = self.ae_playhead().unwrap_or(doc.cursor as f64);
        let mut items: Vec<(&str, String)> = vec![(tr("Position"), fmt_clock(pos / sr))];
        if let Some((a, b)) = doc.sel {
            let (a, b) = (a, b);
            items.push((tr("Selection"), format!("{} → {}", fmt_clock(a as f64 / sr), fmt_clock(b as f64 / sr))));
            items.push((tr("Length"), tr_fmt!("{} · {} samples", fmt_secs(((b - a) as f64 / sr) as f32), fr((b - a) as f32, 0))));
            // Levels of the selection, cached.
            let key = (Arc::as_ptr(&doc.sample) as usize, a, b);
            let stats = match self.audio_edit.stats {
                Some((k0, k1, k2, p, q)) if (k0, k1, k2) == key => (p, q),
                _ => {
                    let s = &doc.sample;
                    let p = edit::peak(&s.left[a..b]).max(edit::peak(&s.right[a..b]));
                    let q = 0.5 * (edit::rms(&s.left[a..b]) + edit::rms(&s.right[a..b]));
                    self.audio_edit.stats = Some((key.0, key.1, key.2, p, q));
                    (p, q)
                }
            };
            let db = |v: f32| if v < 1e-6 { "−∞".to_string() } else { signed(20.0 * v.log10(), 1) };
            items.push((tr("Peak"), tr_fmt!("{} dB", db(stats.0))));
            items.push((tr("RMS"), tr_fmt!("{} dB", db(stats.1))));
        }
        if let Some(bpm) = doc.bpm {
            items.push((tr("Tempo"), tr_fmt!("{} BPM", fr(bpm, 2))));
        }
        for (label, value) in items {
            let lw = self.text(c, self.f.sans, 11.0, x, cy, label, t.muted);
            let vw = self.text(c, self.f.mono, 11.0, x + lw + 6.0, cy, &value, t.text2);
            x += lw + vw + 22.0;
            if x > r.right() - 40.0 {
                break;
            }
        }
    }

    // --- Tool panel ---

    fn ae_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline);
        // Tabs.
        let tabs = Rect::new(r.x + 10.0, r.y + 10.0, r.w - 20.0, 50.0);
        let items: [(G, &str, &str); 4] = [
            (G::Gain, tr("Volume"), tr("Gain, normalize, fades, reverse, DC offset")),
            (G::Noise, tr("Clean up"), tr("Noise reduction, declick, declip, trim silence")),
            (G::I(Icon::Stretch), tr("Time"), tr("Stretch, pitch, seamless loop, sample rate")),
            (G::I(Icon::Slice), tr("Slices"), tr("Transients, tempo, slices to Slicer, pads or MIDI")),
        ];
        let cur = Tab::ALL.iter().position(|&x| x == self.audio_edit.tab).unwrap_or(0);
        let tw = tabs.w / 4.0;
        let sx = self.anim.to(wid("ae-tabx", 0, 0), cur as f32, T_LAYOUT);
        c.fill_rrect(tabs, 12.0, t.bg_deep);
        c.fill_rrect(Rect::new(tabs.x + 3.0 + sx * tw, tabs.y + 3.0, tw - 6.0, tabs.h - 6.0), 9.0, t.raised_hi);
        for (i, (g, label, tip)) in items.into_iter().enumerate() {
            let cell = Rect::new(tabs.x + i as f32 * tw, tabs.y, tw, tabs.h);
            let id = wid("ae-tab", i, 0);
            let hov = self.hovered(input, id, cell);
            let hv = self.hover_anim(id, hov);
            let on = 1.0 - (sx - i as f32).abs().min(1.0);
            let col = t.muted.mix(t.text2, hv).mix(t.accent, on);
            glyph(c, g, cell.x + cell.w * 0.5, cell.y + 17.0, 16.0, col);
            let lc = t.muted.mix(t.text, hv.max(on));
            self.text_center(c, self.f.sans_med, 11.0, cell.x + cell.w * 0.5, cell.y + 37.0, label, lc);
            if hov {
                self.set_tip(tip, "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.audio_edit.tab = Tab::ALL[i];
                }
            }
        }
        let footer_h = 96.0;
        let content = Rect::new(r.x + 16.0, tabs.bottom() + 14.0, r.w - 32.0, r.bottom() - footer_h - tabs.bottom() - 14.0);
        let clip = Rect::new(r.x, content.y - 4.0, r.w, content.h + 4.0);
        // Scrolls when the tab is taller than the panel; the wheel scrolls
        // here rather than turning the sliders under it.
        let over = input.inside && clip.contains(input.x, input.y);
        let max = (self.audio_edit.panel_content - content.h).max(0.0);
        if over && input.wheel != 0.0 && max > 0.0 {
            self.audio_edit.panel_scroll -= input.wheel * 48.0;
        }
        self.audio_edit.panel_scroll = self.audio_edit.panel_scroll.clamp(0.0, max);
        let scroll = self.anim.to(wid("ae-pscroll", 0, 0), self.audio_edit.panel_scroll, T_LAYOUT);
        let mut inner = input.clone();
        inner.wheel = 0.0;
        if !over {
            inner.inside = false;
        }
        let prev = c.push_clip(clip);
        let moved = Rect::new(content.x, content.y - scroll, content.w, content.h);
        let end = match self.audio_edit.tab {
            Tab::Volume => self.ae_tab_volume(c, &inner, moved),
            Tab::Clean => self.ae_tab_clean(c, &inner, moved),
            Tab::Time => self.ae_tab_time(c, &inner, moved),
            Tab::Slices => self.ae_tab_slices(c, &inner, moved),
        };
        self.audio_edit.panel_content = end - moved.y;
        c.set_clip(Some(prev));
        // Hints that more is hidden above or below.
        if scroll > 1.0 {
            c.fill_rrect_vgrad(Rect::new(r.x + 1.0, clip.y, r.w - 2.0, 18.0), 0.0, t.panel, t.panel.alpha(0.0));
        }
        if scroll < max - 1.0 {
            c.fill_rrect_vgrad(Rect::new(r.x + 1.0, clip.bottom() - 22.0, r.w - 2.0, 22.0), 0.0, t.panel.alpha(0.0), t.panel);
            draw_icon(c, Icon::ChevronDown, r.x + r.w * 0.5, clip.bottom() - 8.0, 12.0, t.muted);
        }
        let footer = Rect::new(r.x + 16.0, r.bottom() - footer_h, r.w - 32.0, footer_h - 12.0);
        self.ae_footer(c, input, footer);
    }

    /// Section title; returns the y under it.
    fn ae_title(&mut self, c: &mut Canvas, x: f32, y: f32, w: f32, title: &str, g: G) -> f32 {
        let t = self.theme;
        glyph(c, g, x + 8.0, y + 9.0, 14.0, t.accent);
        self.text(c, self.f.sans_med, 13.0, x + 24.0, y + 9.0, title, t.text);
        let tw = self.fonts.measure(self.f.sans_med, 13.0, title);
        c.fill_rect(Rect::new(x + 32.0 + tw, y + 9.0, (w - 32.0 - tw).max(0.0), 1.0), t.hairline);
        y + 24.0
    }

    /// Action button with a glyph; `style` 1 is the main action.
    #[allow(clippy::too_many_arguments)]
    fn ae_action(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, g: G, label: &str, style: u8, tip: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        let (fill, fg) = if style == 1 {
            (t.accent.mix(t.accent_hot, hv * 0.25), t.on_accent)
        } else {
            (t.raised.mix(t.raised_hi, hv), t.text2.mix(t.text, hv))
        };
        c.fill_rrect(rr, 9.0, fill);
        if style != 1 {
            c.stroke_rrect(rr, 9.0, 1.0, t.hairline);
        }
        let cy = rr.y + rr.h * 0.5;
        glyph(c, g, rr.x + 15.0, cy, 14.0, if style == 1 { fg } else { t.accent.mix(fg, 0.3) });
        self.text_fit(c, self.f.sans_med, 12.0, rr.x + 28.0, cy, label, rr.w - 33.0, fg);
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Tile: the glyph above its label.
    #[allow(clippy::too_many_arguments)]
    fn ae_tile(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, g: G, label: &str, style: u8, tip: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        let (fill, fg) = if style == 1 {
            (t.accent.mix(t.accent_hot, hv * 0.25), t.on_accent)
        } else {
            (t.raised.mix(t.raised_hi, hv), t.text2.mix(t.text, hv))
        };
        c.fill_rrect(rr, 10.0, fill);
        if style != 1 {
            c.stroke_rrect(rr, 10.0, 1.0, t.hairline);
        }
        let cx = rr.x + rr.w * 0.5;
        glyph(c, g, cx, rr.y + 16.0, 16.0, if style == 1 { fg } else { t.accent.mix(fg, 0.3) });
        let s = self.fit(self.f.sans_med, 11.0, label, rr.w - 8.0);
        self.text_center(c, self.f.sans_med, 11.0, cx, rr.bottom() - 12.0, &s, fg);
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Slider row (label, value, track); returns the new 0..1 position.
    #[allow(clippy::too_many_arguments)]
    fn ae_slider(&mut self, c: &mut Canvas, input: &Input, key: &str, r: Rect, label: &str, norm: f32, text: &str, default: f32, tip: &str) -> Option<f32> {
        let id = wid("ae-sl", key.len(), key.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)));
        let out = self.lab_slider(c, input, id, r, label, norm, text, default, tip);
        if out.is_some() {
            self.audio_edit.grab = Some(Grab::Slider);
        }
        out
    }

    fn ae_tab_volume(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        let has_sel = self.audio_edit.doc.as_ref().is_some_and(|d| d.sel.is_some());
        let scope = if has_sel { tr("on the selection") } else { tr("on the whole sound") };
        let mut y = r.y;
        self.text(c, self.f.sans, 11.0, r.x, y + 6.0, &tr_fmt!("Tools work {}", scope), t.muted);
        y += 18.0;
        // Gain.
        y = self.ae_title(c, r.x, y, r.w, tr("Gain"), G::Gain);
        let g = self.audio_edit.gain_db;
        let half = (r.w - 10.0) * 0.6;
        if let Some(v) = self.ae_slider(c, input, "gain", Rect::new(r.x, y, half, 36.0), tr("Gain"), (g + 24.0) / 48.0, &tr_fmt!("{} dB", signed(g, 1)), 0.5, tr("Volume change, −24 to +24 dB")) {
            self.audio_edit.gain_db = ((v * 48.0 - 24.0) * 2.0).round() / 2.0;
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-gain", 0, 0), b, G::Gain, tr("Apply"), 0, tr("Apply the gain")) {
            self.ae_gain();
        }
        y += 46.0;
        // Normalize.
        y = self.ae_title(c, r.x, y, r.w, tr("Normalize"), G::I(Icon::Normalize));
        let mode = self.audio_edit.norm;
        let seg = Rect::new(r.x, y, r.w, 30.0);
        let tips = [(tr("Peak: the loudest sample reaches the target"), ""), (tr("Loudness: the integrated loudness (EBU R128) reaches the target"), "")];
        if let Some(i) = self.segmented_tips(c, input, wid("ae-norm", 0, 0), seg, &[tr("Peak"), tr("Loudness")], &tips, (mode == NormMode::Lufs) as usize) {
            self.audio_edit.norm = if i == 1 { NormMode::Lufs } else { NormMode::Peak };
        }
        y += 38.0;
        match mode {
            NormMode::Peak => {
                let p = self.audio_edit.peak_target;
                if let Some(v) = self.ae_slider(c, input, "peak", Rect::new(r.x, y, half, 36.0), tr("Target"), (p + 12.0) / 12.0, &tr_fmt!("{} dBFS", signed(p, 1)), 0.975, tr("Highest peak after normalizing")) {
                    self.audio_edit.peak_target = ((v * 12.0 - 12.0) * 10.0).round() / 10.0;
                }
            }
            NormMode::Lufs => {
                let l = self.audio_edit.lufs_target;
                if let Some(v) = self.ae_slider(c, input, "lufs", Rect::new(r.x, y, half, 36.0), tr("Target"), (l + 30.0) / 24.0, &tr_fmt!("{} LUFS", signed(l, 1)), 16.0 / 24.0, tr("−14 LUFS: streaming · −23: broadcast · −9: loud club")) {
                    self.audio_edit.lufs_target = (v * 24.0 - 30.0).round();
                }
            }
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-norm", 0, 0), b, G::I(Icon::Normalize), tr("Normalize"), 1, tr("Normalize")) {
            self.ae_normalize();
        }
        y += 46.0;
        // Fades.
        y = self.ae_title(c, r.x, y, r.w, tr("Fades"), G::FadeIn);
        let shapes: [(FadeShape, &str); 5] = [
            (FadeShape::Linear, tr("Linear")),
            (FadeShape::EqualPower, tr("Equal power: the natural crossfade")),
            (FadeShape::Slow, tr("Slow start")),
            (FadeShape::Fast, tr("Fast start")),
            (FadeShape::SCurve, tr("S-curve: slow at both ends")),
        ];
        let tw = (r.w - 4.0 * 6.0) / 5.0;
        for (i, (shape, tip)) in shapes.into_iter().enumerate() {
            let cell = Rect::new(r.x + i as f32 * (tw + 6.0), y, tw, 38.0);
            let id = wid("ae-shape", i, 0);
            let on = self.audio_edit.fade == shape;
            let hov = self.hovered(input, id, cell);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(cell, 8.0, if on { t.accent.alpha(0.16) } else { t.raised.alpha(0.5 + 0.5 * hv) });
            c.stroke_rrect(cell, 8.0, 1.0, if on { t.accent.alpha(0.7) } else { t.hairline });
            draw_fade_curve(c, cell.inset_xy(9.0, 8.0), shape, if on { t.accent } else { t.text2.mix(t.text, hv) }, 1.6);
            if hov {
                self.set_tip(tip, "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.audio_edit.fade = shape;
                }
            }
        }
        y += 46.0;
        let bw = (r.w - 8.0) * 0.5;
        if self.ae_action(c, input, wid("ae-a-fin", 0, 0), Rect::new(r.x, y, bw, 32.0), G::FadeIn, tr("Fade in"), 0, tr("Fade the selection in, from silence\nCtrl+Shift+I")) {
            self.ae_fade(true);
        }
        if self.ae_action(c, input, wid("ae-a-fout", 0, 0), Rect::new(r.x + bw + 8.0, y, bw, 32.0), G::FadeOut, tr("Fade out"), 0, tr("Fade the selection out, to silence\nCtrl+Shift+O")) {
            self.ae_fade(false);
        }
        y += 40.0;
        if self.ae_action(c, input, wid("ae-a-rev", 0, 0), Rect::new(r.x, y, bw, 32.0), G::I(Icon::Swap), tr("Reverse"), 0, tr("Play backwards\nCtrl+R")) {
            self.ae_reverse();
        }
        if self.ae_action(c, input, wid("ae-a-dc", 0, 0), Rect::new(r.x + bw + 8.0, y, bw, 32.0), G::Dc, tr("DC offset"), 0, tr("Remove the DC offset: centres the waveform on zero")) {
            self.ae_dc();
        }
        y + 40.0
    }

    fn ae_tab_clean(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        let mut y = r.y;
        y = self.ae_title(c, r.x, y, r.w, tr("Noise reduction"), G::Noise);
        // Learn, and the learnt profile.
        let learnt = self.audio_edit.noise.is_some();
        let b = Rect::new(r.x, y, 118.0, 32.0);
        if self.ae_action(c, input, wid("ae-a-learn", 0, 0), b, G::I(Icon::Search), tr("Learn noise"), if learnt { 0 } else { 1 }, tr("Learn the noise from the selection\nSelect a stretch with only the noise (hiss, hum, room)")) {
            self.ae_learn_noise();
        }
        let graph = Rect::new(b.right() + 10.0, y, r.right() - b.right() - 10.0, 32.0);
        c.fill_rrect(graph, 8.0, t.well);
        if let Some(p) = self.audio_edit.noise.as_ref() {
            let db = p.db();
            let sr = p.sample_rate;
            let g = graph.inset_xy(6.0, 4.0);
            let pts: Vec<(f32, f32)> = (0..=48)
                .map(|i| {
                    let frac = i as f32 / 48.0;
                    let hz = 40.0 * ((sr * 0.5) / 40.0).powf(frac);
                    let bin = ((hz / sr * ponzik_dsp::denoise::FFT as f32) as usize).min(db.len() - 1);
                    let v = ((db[bin] + 110.0) / 80.0).clamp(0.0, 1.0);
                    (g.x + g.w * frac, g.bottom() - v * g.h)
                })
                .collect();
            let mut fill = pts.clone();
            fill.push((g.right(), g.bottom()));
            fill.push((g.x, g.bottom()));
            let mut sh = Shape::new();
            sh.polygon(&fill);
            c.fill_path(&sh, t.select.alpha(0.35));
            c.stroke_polyline(&pts, 1.3, t.select);
        } else {
            self.text_center(c, self.f.sans, 11.0, graph.x + graph.w * 0.5, graph.y + 16.0, tr("no noise learnt yet"), t.muted);
        }
        self.tip_zone(input, graph, tr("The learnt noise, bass on the left"), "");
        y += 42.0;
        let d = self.audio_edit.denoise;
        if let Some(v) = self.ae_slider(c, input, "nr-red", Rect::new(r.x, y, r.w, 36.0), tr("Reduction"), (d.reduction_db - 6.0) / 34.0, &tr_fmt!("{} dB", fr(d.reduction_db, 0)), (24.0 - 6.0) / 34.0, tr("How far the noise goes down")) {
            self.audio_edit.denoise.reduction_db = (6.0 + v * 34.0).round();
        }
        y += 40.0;
        if let Some(v) = self.ae_slider(c, input, "nr-thr", Rect::new(r.x, y, r.w, 36.0), tr("Threshold"), (d.threshold_db + 6.0) / 18.0, &tr_fmt!("{} dB", signed(d.threshold_db, 1)), 0.5, tr("Margin over the learnt noise: higher removes more, and may eat quiet details")) {
            self.audio_edit.denoise.threshold_db = ((v * 18.0 - 6.0) * 2.0).round() / 2.0;
        }
        y += 40.0;
        if let Some(v) = self.ae_slider(c, input, "nr-sm", Rect::new(r.x, y, r.w, 36.0), tr("Smoothing"), d.smoothing, &format!("{}%", fr(d.smoothing * 100.0, 0)), 0.5, tr("More: calmer, no warbling (musical noise) · less: more precise")) {
            self.audio_edit.denoise.smoothing = (v * 20.0).round() / 20.0;
        }
        y += 42.0;
        if self.ae_action(c, input, wid("ae-a-nr", 0, 0), Rect::new(r.x, y, r.w, 32.0), G::Noise, tr("Reduce noise"), if learnt { 1 } else { 0 }, tr("Remove the learnt noise from the selection, or everything")) {
            self.ae_denoise();
        }
        y += 44.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Repair"), G::Declick);
        let half = (r.w - 10.0) * 0.6;
        let s = self.audio_edit.declick;
        if let Some(v) = self.ae_slider(c, input, "dclk", Rect::new(r.x, y, half, 36.0), tr("Sensitivity"), s, &format!("{}%", fr(s * 100.0, 0)), 0.5, tr("Higher catches smaller clicks")) {
            self.audio_edit.declick = (v * 20.0).round() / 20.0;
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-dclk", 0, 0), b, G::Declick, tr("Declick"), 0, tr("Find clicks and pops and bridge them smoothly")) {
            self.ae_declick();
        }
        y += 42.0;
        if self.ae_action(c, input, wid("ae-a-dclp", 0, 0), Rect::new(r.x, y, r.w, 30.0), G::Declip, tr("Declip: rebuild flattened peaks"), 0, tr("Rebuild the tops of clipped peaks, then lower the level to fit")) {
            self.ae_declip();
        }
        y += 42.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Trim silence"), G::Trim);
        let th = self.audio_edit.trim_db;
        if let Some(v) = self.ae_slider(c, input, "trim", Rect::new(r.x, y, half, 36.0), tr("Threshold"), (th + 72.0) / 48.0, &tr_fmt!("{} dB", signed(th, 0)), 0.5, tr("Quieter than this counts as silence")) {
            self.audio_edit.trim_db = (v * 48.0 - 72.0).round();
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-trim", 0, 0), b, G::Trim, tr("Trim"), 0, tr("Cut the silence at the start and the end")) {
            self.ae_trim_silence();
        }
        y + 44.0
    }

    fn ae_tab_time(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let mut y = r.y;
        y = self.ae_title(c, r.x, y, r.w, tr("Stretch and pitch"), G::I(Icon::Stretch));
        let algo = self.audio_edit.algo;
        let seg = Rect::new(r.x, y, r.w, 30.0);
        let tips = [
            (tr("Beats: keeps drum hits sharp"), ""),
            (tr("Tones: smooth on chords, pads and voices"), ""),
            (tr("Re-pitch: speed and pitch move together, like a turntable"), ""),
        ];
        let sel = WarpAlgo::ALL.iter().position(|&a| a == algo).unwrap_or(1);
        if let Some(i) = self.segmented_tips(c, input, wid("ae-algo", 0, 0), seg, &[tr("Beats"), tr("Tones"), tr("Re-pitch")], &tips, sel) {
            self.audio_edit.algo = WarpAlgo::ALL[i];
        }
        y += 40.0;
        let half = (r.w - 10.0) * 0.6;
        let st = self.audio_edit.stretch;
        let norm = (st.log2() + 1.0) / 2.0;
        if let Some(v) = self.ae_slider(c, input, "stretch", Rect::new(r.x, y, half, 36.0), tr("Length"), norm, &format!("{}%", fr(st * 100.0, 1)), 0.5, tr("New length: 50% is twice as fast, 200% twice as slow")) {
            self.audio_edit.stretch = ((v * 2.0 - 1.0).exp2() * 1_000.0).round() / 1_000.0;
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-str", 0, 0), b, G::I(Icon::Stretch), tr("Stretch"), 1, tr("Change the length, keeping the pitch")) {
            self.ae_stretch(st as f64);
        }
        y += 44.0;
        let p = self.audio_edit.semitones;
        if let Some(v) = self.ae_slider(c, input, "pitch", Rect::new(r.x, y, half, 36.0), tr("Pitch"), (p + 12.0) / 24.0, &tr_fmt!("{} st", signed(p, 1)), 0.5, tr("Semitones up or down, keeping the length\nShift: finer")) {
            self.audio_edit.semitones = ((v * 24.0 - 12.0) * 10.0).round() / 10.0;
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-pitch", 0, 0), b, G::Pitch, tr("Shift"), 0, tr("Shift the pitch, keeping the length")) {
            self.ae_pitch();
        }
        y += 44.0;
        let bpm = self.project.bpm;
        let label = tr_fmt!("Match the project tempo ({} BPM)", fr(bpm, 1));
        if self.ae_action(c, input, wid("ae-a-match", 0, 0), Rect::new(r.x, y, r.w, 30.0), G::I(Icon::Metronome), &label, 0, tr("Stretch from the sound's tempo to the project's")) {
            self.ae_match_tempo();
        }
        y += 44.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Seamless loop"), G::I(Icon::Loop));
        let ms = self.audio_edit.xfade_ms;
        if let Some(v) = self.ae_slider(c, input, "xfade", Rect::new(r.x, y, half, 36.0), tr("Crossfade"), (ms / 500.0).sqrt(), &format!("{} ms", fr(ms, 0)), (30.0f32 / 500.0).sqrt(), tr("How long the end blends into the start")) {
            self.audio_edit.xfade_ms = (v * v * 500.0).round().max(1.0);
        }
        let b = Rect::new(r.x + half + 10.0, y + 4.0, r.w - half - 10.0, 30.0);
        if self.ae_action(c, input, wid("ae-a-loop", 0, 0), b, G::I(Icon::Loop), tr("Make loop"), 0, tr("Keep the selection as a loop whose end flows into its start")) {
            self.ae_loop();
        }
        y += 46.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Convert"), G::Resample);
        let rates = [22_050u32, 44_100, 48_000, 96_000];
        let labels: Vec<String> = rates.iter().map(|&k| format!("{}k", fr(k as f32 / 1_000.0, if k % 1_000 == 0 { 0 } else { 2 }))).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let tips: Vec<(&str, &str)> = rates.iter().map(|_| (tr("Sample rate to convert to"), "")).collect();
        let sel = rates.iter().position(|&k| k == self.audio_edit.rate).unwrap_or(2);
        if let Some(i) = self.segmented_tips(c, input, wid("ae-rate", 0, 0), Rect::new(r.x, y, r.w, 30.0), &refs, &tips, sel) {
            self.audio_edit.rate = rates[i];
        }
        y += 38.0;
        let bw = (r.w - 8.0) * 0.5;
        if self.ae_action(c, input, wid("ae-a-rate", 0, 0), Rect::new(r.x, y, bw, 30.0), G::Resample, tr("Convert"), 0, tr("Resample to the chosen rate (windowed sinc, no aliasing)")) {
            self.ae_resample();
        }
        if self.ae_action(c, input, wid("ae-a-mono", 0, 0), Rect::new(r.x + bw + 8.0, y, bw, 30.0), G::Mono, tr("Make mono"), 0, tr("Mix both channels into one")) {
            self.ae_to_mono();
        }
        y + 40.0
    }

    fn ae_tab_slices(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        let mut y = r.y;
        y = self.ae_title(c, r.x, y, r.w, tr("Transients"), G::I(Icon::Slice));
        let n = self.audio_edit.doc.as_ref().map_or(1, |d| d.bounds().len() - 1);
        let sens = self.audio_edit.sensitivity;
        let mut redetect = false;
        if let Some(v) = self.ae_slider(c, input, "sens", Rect::new(r.x, y, r.w, 36.0), tr("Sensitivity"), sens, &format!("{}%", fr(sens * 100.0, 0)), 0.5, tr("Higher finds quieter hits")) {
            self.audio_edit.sensitivity = (v * 100.0).round() / 100.0;
            redetect = true;
        }
        y += 40.0;
        let gap = self.audio_edit.min_gap_ms;
        if let Some(v) = self.ae_slider(c, input, "gap", Rect::new(r.x, y, r.w, 36.0), tr("Minimum gap"), (gap - 10.0) / 240.0, &format!("{} ms", fr(gap, 0)), (45.0 - 10.0) / 240.0, tr("Hits closer than this count as one")) {
            self.audio_edit.min_gap_ms = (10.0 + v * 240.0).round();
            redetect = true;
        }
        if redetect {
            self.ae_detect_slices_quiet();
        }
        y += 42.0;
        let big = format!("{n}");
        let w = self.text(c, self.f.sans_med, 22.0, r.x, y + 13.0, &big, t.accent);
        self.text(c, self.f.sans, 12.0, r.x + w + 8.0, y + 14.0, if n == 1 { tr("slice") } else { tr("slices") }, t.text2);
        let bw = 86.0;
        if self.ae_action(c, input, wid("ae-a-clear", 0, 0), Rect::new(r.right() - bw, y, bw, 28.0), G::I(Icon::Trash), tr("Clear"), 0, tr("Remove every slice cut")) {
            self.ae_clear_slices();
        }
        if self.ae_action(c, input, wid("ae-a-det", 0, 0), Rect::new(r.right() - 2.0 * bw - 8.0, y, bw, 28.0), G::I(Icon::Slice), tr("Detect"), 0, tr("Cut at every hit (spectral flux)")) {
            self.ae_detect_slices();
        }
        y += 40.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Tempo"), G::I(Icon::Metronome));
        let est = self.audio_edit.tempo;
        let bpm = self.audio_edit.doc.as_ref().and_then(|d| d.bpm);
        match bpm {
            Some(b) => {
                let s = fr(b, 2);
                let w = self.text(c, self.f.sans_med, 22.0, r.x, y + 13.0, &s, t.text);
                self.text(c, self.f.sans, 12.0, r.x + w + 6.0, y + 14.0, "BPM", t.text2);
                if let Some(conf) = est.map(|e| e.confidence) {
                    let bar = Rect::new(r.x + w + 42.0, y + 11.0, 52.0, 5.0);
                    c.fill_rrect(bar, 2.5, t.raised_hi);
                    c.fill_rrect(Rect::new(bar.x, bar.y, bar.w * conf, bar.h), 2.5, t.accent);
                    self.tip_zone(input, bar.inset_xy(0.0, -6.0), &tr_fmt!("Confidence {}%", fr(conf * 100.0, 0)), "");
                }
            }
            None => {
                self.text(c, self.f.sans, 13.0, r.x, y + 13.0, tr("No clear beat"), t.muted);
            }
        }
        if let Some(alt) = est.and_then(|e| e.alternative) {
            let label = tr_fmt!("or {}", fr(alt, 1));
            let b = Rect::new(r.right() - bw, y, bw, 28.0);
            if self.ae_action(c, input, wid("ae-a-alt", 0, 0), b, G::I(Icon::Swap), &label, 0, tr("Half or double tempo: the beat fits both, pick the one you hear")) {
                self.ae_use_alternative();
            }
        }
        y += 36.0;
        let half = (r.w - 8.0) * 0.5;
        if self.ae_action(c, input, wid("ae-a-tempo", 0, 0), Rect::new(r.x, y, half, 30.0), G::I(Icon::Metronome), tr("Detect tempo"), 0, tr("Find the tempo from the hits (autocorrelation)")) {
            self.ae_detect_tempo(true);
        }
        if self.ae_action(c, input, wid("ae-a-match2", 0, 0), Rect::new(r.x + half + 8.0, y, half, 30.0), G::I(Icon::Stretch), tr("Match project"), 0, &tr_fmt!("Stretch to the project tempo ({} BPM)", fr(self.project.bpm, 1))) {
            self.ae_match_tempo();
        }
        y += 44.0;
        y = self.ae_title(c, r.x, y, r.w, tr("Use the slices"), G::Pads);
        let items: [(G, &str, &str, u8); 6] = [
            (G::I(Icon::Slice), tr("To Slicer"), tr("A new Slicer plays one slice per key, from C5"), 0),
            (G::I(Icon::Midi), tr("Slice to MIDI"), tr("Slicer plus a pattern that replays the slices in their original rhythm"), 1),
            (G::Pads, tr("To Drum Pads"), tr("One slice per pad (16 at most)"), 2),
            (G::Drums, tr("Drum pattern"), tr("Make a drum pattern from a break: kicks, snares and hats on pads, the break's rhythm in a pattern"), 3),
            (G::I(Icon::Export), tr("Export slices"), tr("Every slice as its own WAV file, next to the project"), 4),
            (G::I(Icon::Sample), tr("To Sampler"), tr("The whole sound in a new Sampler"), 5),
        ];
        for (i, (g, label, tip, action)) in items.into_iter().enumerate() {
            let cell = Rect::new(r.x + (i % 2) as f32 * (half + 8.0), y + (i / 2) as f32 * 38.0, half, 32.0);
            if self.ae_action(c, input, wid("ae-a-use", i, 0), cell, g, label, (i == 1) as u8, tip) {
                match action {
                    0 => self.ae_to_slicer(false),
                    1 => self.ae_to_slicer(true),
                    2 => self.ae_to_pads(),
                    3 => self.ae_drum_pattern(),
                    4 => {
                        self.ae_export_slices();
                    }
                    _ => self.ae_to_sampler(),
                }
            }
        }
        y + 3.0 * 38.0
    }

    /// The ways out of the editor: Sampler, playlist, the source replaced.
    fn ae_footer(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(Rect::new(r.x - 16.0, r.y - 6.0, r.w + 32.0, 1.0), t.hairline);
        self.text(c, self.f.sans_med, 12.0, r.x, r.y + 10.0, tr("Send the sound"), t.text2);
        let w = (r.w - 16.0) / 3.0;
        let y = r.y + 24.0;
        let source = self.audio_edit.source;
        let replace_tip = match source {
            Source::Channel(_) => tr("Replace the channel's sample: it plays the edited sound\nCtrl+Z in the project brings the old one back"),
            Source::Clip(_) => tr("Replace the clip's audio: the playlist clip plays the edited sound\nCtrl+Z in the project brings the old one back"),
            Source::Browser => tr("Replace the original: only for a sound opened from a channel or a clip"),
        };
        let items: [(G, &str, &str); 3] = [
            (G::I(Icon::Sample), tr("Sampler"), tr("A new Sampler channel plays the edited sound")),
            (G::I(Icon::Playlist), tr("Playlist"), tr("Place the edited sound as an audio clip in the playlist")),
            (G::I(Icon::Swap), tr("Replace"), replace_tip),
        ];
        for (i, (g, label, tip)) in items.into_iter().enumerate() {
            let b = Rect::new(r.x + i as f32 * (w + 8.0), y, w, 46.0);
            let enabled = i < 2 || source != Source::Browser;
            let style = (i == 2 && enabled) as u8;
            if self.ae_tile(c, input, wid("ae-send", i, 0), b, g, label, style, tip) && enabled {
                match i {
                    0 => self.ae_to_sampler(),
                    1 => self.ae_to_playlist(),
                    _ => self.ae_replace_source(),
                }
            }
            if !enabled {
                c.fill_rrect(b, 9.0, t.panel.alpha(0.55));
            }
        }
        if self.audio_edit.grab == Some(Grab::Slider) && !input.down {
            self.audio_edit.grab = None;
        }
    }
}

impl App {
    /// Slices again after a setting moved; a slider drag makes one undo step.
    fn ae_detect_slices_quiet(&mut self) {
        self.ae_detect_slices();
    }
}
