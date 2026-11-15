//! Vue Channel Rack : un canal par ligne, le step sequencer du pattern courant.

use ponzik_engine::{TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{App, Focus, Popup, View};
use super::theme::*;
use super::widgets::{Btn, wid};

const ROW_H: f32 = 56.0;
const ROW_GAP: f32 = 6.0;
const LEFT: f32 = 252.0;
const MAX_STEPS: usize = 64;

pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[note as usize % 12], note as i32 / 12 - 1)
}

impl App {
    fn steps_in_pattern(&self) -> usize {
        let p = &self.project.patterns[self.project.current_pattern];
        ((p.len / TICKS_PER_STEP) as usize).clamp(1, MAX_STEPS)
    }

    /// Note qui commence dans le pas `step` du canal, s'il y en a une.
    fn note_in_step(&self, ch: usize, step: usize) -> Option<ponzik_engine::Note> {
        let p = &self.project.patterns[self.project.current_pattern];
        let (lo, hi) = (step as u32 * TICKS_PER_STEP, (step as u32 + 1) * TICKS_PER_STEP);
        p.notes.get(ch)?.iter().find(|n| n.start >= lo && n.start < hi).copied()
    }

    fn paint_step(&mut self, ch: usize, step: usize, on: bool) {
        let pat = self.project.current_pattern;
        let changed = if on {
            self.project.set_step(pat, ch, step as u32, true)
        } else {
            let (lo, hi) = (step as u32 * TICKS_PER_STEP, (step as u32 + 1) * TICKS_PER_STEP);
            let notes = &mut self.project.patterns[pat].notes;
            if notes.len() <= ch {
                false
            } else {
                let before = notes[ch].len();
                notes[ch].retain(|n| n.start < lo || n.start >= hi);
                notes[ch].len() != before
            }
        };
        if changed {
            self.commit("Pas");
        }
    }

    pub(super) fn rack_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        // En-tête : longueur du pattern et swing.
        let pat = self.project.current_pattern;
        let bars = (self.project.patterns[pat].len / TICKS_PER_BAR).clamp(1, 4) as usize;
        let len_box = Rect::new(header.right() - 228.0, header.y, 228.0, header.h);
        let sel = match bars {
            1 => 0,
            2 => 1,
            _ => 2,
        };
        if let Some(i) = self.segmented(c, input, wid("rack-len", 0, 0), len_box, &["1 mesure", "2", "4"], sel) {
            self.project.patterns[pat].len = [1, 2, 4][i] * TICKS_PER_BAR;
            self.commit("Longueur du pattern");
        }
        let swing_box = Rect::new(len_box.x - 12.0 - 132.0, header.y, 132.0, header.h);
        let swing_text = format!("{:.0} %", self.project.swing * 100.0);
        if let Some(v) = self.drag_field(c, input, wid("rack-swing", 0, 0), swing_box, self.project.swing, 0.004, 0.05, &swing_text, "Swing") {
            let v = (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
            if v != self.project.swing {
                self.project.swing = v;
                self.commit("Swing");
            }
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(16.0);
        let steps = self.steps_in_pattern();
        let pads = Rect::new(inner.x + LEFT, inner.y, inner.w - LEFT, inner.h);
        let geom = |i: usize| -> (f32, f32) {
            let groups = steps.div_ceil(4);
            let gaps = (steps - groups) as f32 * 4.0 + (groups - 1) as f32 * 12.0;
            let w = ((pads.w - gaps) / steps as f32).floor().max(6.0);
            let g = i / 4;
            (pads.x + i as f32 * w + (i - g) as f32 * 4.0 + g as f32 * 12.0, w)
        };

        // Numéros de pas, et la LED qui court au-dessus.
        let playing_here = self.playing && matches!(self.project.mode, ponzik_engine::PlayMode::Pattern(_));
        let head = playing_here.then(|| (self.cycle_position() / TICKS_PER_STEP as f64).floor() as usize);
        for i in 0..steps {
            let (x, w) = geom(i);
            let cx = x + w * 0.5;
            let here = head == Some(i);
            let led = self.anim.to(wid("step-led", i, 0), here as u8 as f32, T_HEAD);
            if led > 0.01 {
                c.fill_circle(cx, inner.y + 4.0, 3.0, t.accent.alpha(led));
            }
            let col = if here {
                t.text
            } else if i % 4 == 0 {
                t.text2
            } else {
                t.muted
            };
            if w >= 14.0 || i % 4 == 0 {
                self.text_center(c, self.f.mono, 12.0, cx, inner.y + 20.0, &(i + 1).to_string(), col);
            }
        }
        // Flash des pas déclenchés.
        if let Some(h) = head {
            let key = wid("rack-head", 0, 0);
            if self.anim.get(key) != Some(h as f32) {
                self.anim.set(key, h as f32);
                for ch in 0..self.project.channels.len() {
                    if !self.project.channels[ch].muted && self.note_in_step(ch, h).is_some() {
                        self.anim.set(wid("pad-flash", ch, h), 1.0);
                    }
                }
            }
        }

        let rows = Rect::new(inner.x, inner.y + 36.0, inner.w, inner.h - 36.0);
        let total = self.project.channels.len() as f32 * (ROW_H + ROW_GAP) + 48.0;
        let hov_rows = input.inside && rows.contains(input.x, input.y);
        let scroll_key = wid("rack-scroll", 0, 0);
        let mut target = self.anim.get(scroll_key ^ 1).unwrap_or(0.0);
        if hov_rows && input.wheel != 0.0 {
            target -= input.wheel * 60.0;
        }
        target = target.clamp(0.0, (total - rows.h).max(0.0));
        self.anim.set(scroll_key ^ 1, target);
        let scroll = self.anim.to(scroll_key, target, T_LAYOUT);

        let prev = c.push_clip(rows);
        let mut y = rows.y - scroll;
        for ch in 0..self.project.channels.len() {
            let row = Rect::new(rows.x, y, rows.w, ROW_H);
            if row.bottom() > rows.y && row.y < rows.bottom() {
                self.channel_row(c, input, row, ch, steps, &geom, head);
            }
            y += ROW_H + ROW_GAP;
        }
        let add = Rect::new(rows.x, y + 4.0, 200.0, 36.0);
        if self.button(c, input, wid("rack-add", 0, 0), add, "+ Ajouter un instrument", Btn::Ghost) {
            self.open_popup(Popup::AddInstrument { replace: None }, add);
        }
        c.set_clip(Some(prev));
    }

    #[allow(clippy::too_many_arguments)]
    fn channel_row(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        row: Rect,
        ch: usize,
        steps: usize,
        geom: &dyn Fn(usize) -> (f32, f32),
        head: Option<usize>,
    ) {
        let t = self.theme;
        let cy = row.y + row.h * 0.5;
        let selected = self.selected_channel() == Some(ch);
        let sel = self.anim.to(wid("row-sel", ch, 0), selected as u8 as f32, T_HOVER);
        let left = Rect::new(row.x, row.y, LEFT - 12.0, row.h);
        if sel > 0.01 {
            c.fill_rrect(left, 12.0, t.raised.alpha(sel));
            c.fill_rrect(Rect::new(left.x, left.y + 14.0, 3.0, left.h - 28.0), 1.5, t.select.alpha(sel));
        }

        // LED du canal : allumée quand il joue, éteinte quand il est muet.
        let led = Rect::new(row.x + 10.0, cy - 14.0, 28.0, 28.0);
        let id = wid("ch-led", ch, 0);
        let hov = self.hovered(input, id, led);
        let hv = self.hover_anim(id, hov);
        if hov && input.pressed {
            self.project.channels[ch].muted = !self.project.channels[ch].muted;
            self.commit("Muet");
        }
        c.fill_rrect(led, 8.0, t.raised_hi.alpha(0.4 + hv * 0.6));
        let (lx, ly) = led.center();
        let muted = self.project.channels[ch].muted;
        let activity = self.channel_meters.get(ch).map_or(0.0, |m| m.peak()).min(1.0).sqrt();
        if muted {
            c.stroke_circle(lx, ly, 5.0, 1.5, t.muted);
        } else {
            c.fill_circle(lx, ly, 5.0, t.text2.mix(t.accent, activity));
        }

        // Nom : clic pour sélectionner (et pré-écouter), double-clic pour le piano roll.
        let name_r = Rect::new(row.x + 46.0, row.y, LEFT - 46.0 - 12.0 - 64.0, row.h);
        let nid = wid("ch-name", ch, 0);
        let nhov = self.hovered(input, nid, name_r);
        if nhov && input.right_pressed {
            self.focus = Focus::Channel(ch);
            self.open_popup(Popup::ChannelMenu(ch), Rect::new(input.x, input.y, 0.0, 0.0));
        }
        if nhov && input.pressed {
            self.focus = Focus::Channel(ch);
            if input.double {
                self.set_view(View::Piano);
            }
            let note = self.project.channels[ch].root_note;
            self.preview_note(ch, note);
            self.begin_drag(nid, input, note as f32);
        }
        let nh = self.hover_anim(nid, nhov);
        let (name, kind) = {
            let c = &self.project.channels[ch];
            let kind = ponzik_engine::registry::instrument_kind(&c.instrument.kind).map_or("?", |k| k.name);
            (c.name.clone(), kind)
        };
        let track = self.project.channels[ch].track;
        let name_col = if muted { t.text2 } else { t.text.mix(WHITE, nh * 0.3) };
        self.text_fit(c, self.f.sans_med, 15.0, name_r.x, cy - 8.0, &name, name_r.w, name_col);
        let detail = format!("{kind} · piste {track}");
        self.text_fit(c, self.f.sans, 12.0, name_r.x, cy + 11.0, &detail, name_r.w, t.muted);

        // Volume et pan du canal, petits potards.
        let vol_c = (row.x + LEFT - 12.0 - 52.0, cy);
        let pan_c = (row.x + LEFT - 12.0 - 20.0, cy);
        let gain = self.project.channels[ch].gain;
        if let Some(v) = self.mini_knob(c, input, wid("ch-vol", ch, 0), vol_c, (gain / 1.25).clamp(0.0, 1.0), false) {
            self.project.channels[ch].gain = v * 1.25;
            self.commit("Volume du canal");
        }
        let pan = self.project.channels[ch].pan;
        if let Some(v) = self.mini_knob(c, input, wid("ch-pan", ch, 0), pan_c, (pan + 1.0) * 0.5, true) {
            self.project.channels[ch].pan = ((v * 2.0 - 1.0) * 100.0).round() / 100.0;
            self.commit("Pan du canal");
        }

        let kind = self.project.channels[ch].instrument.kind.clone();
        let melodic = self.is_melodic(&kind);
        for i in 0..steps {
            let (x, w) = geom(i);
            let pad = Rect::new(x, cy - 22.0, w, 44.0);
            self.pad(c, input, pad, ch, i, melodic, head);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pad(&mut self, c: &mut Canvas, input: &Input, pad: Rect, ch: usize, i: usize, melodic: bool, head: Option<usize>) {
        let t = self.theme;
        let paint_id = wid("paint", ch, 0);
        let over = input.inside && pad.contains(input.x, input.y);
        let can = self.drag.is_none() || self.is_dragging(paint_id);
        let hov = over && can;
        let note = self.note_in_step(ch, i);
        if hov && input.pressed && self.drag.is_none() {
            let on = note.is_none();
            self.paint_step(ch, i, on);
            self.begin_drag(paint_id, input, on as u8 as f32);
            self.focus = Focus::Channel(ch);
        } else if hov && input.down && let Some(d) = self.drag.filter(|d| d.id == paint_id) {
            // Peindre en glissant, comme sur une boîte à rythmes.
            let on = d.v0 > 0.5;
            if note.is_some() != on {
                self.paint_step(ch, i, on);
            }
        }
        // Clic droit maintenu : efface tout ce que l'on balaie.
        if over && input.right_down && self.drag.is_none() && note.is_some() {
            self.paint_step(ch, i, false);
        }
        let note = self.note_in_step(ch, i);

        let hv = self.hover_anim(wid("pad", ch, i), hov);
        let headv = self.anim.to(wid("pad-head", ch, i), (head == Some(i)) as u8 as f32, T_HEAD);
        let flash = self.anim.to(wid("pad-flash", ch, i), 0.0, T_FLASH);
        let muted = self.project.channels[ch].muted;
        let radius = (pad.w * 0.18).clamp(4.0, 8.0);
        match note {
            Some(n) => {
                let (mut top, mut bottom) = (t.accent, t.accent_deep);
                let vel = 0.55 + 0.45 * n.vel;
                top = top.mix(t.raised_hi, 1.0 - vel);
                bottom = bottom.mix(t.raised_hi, 1.0 - vel);
                if muted {
                    top = top.mix(t.raised_hi, 0.72);
                    bottom = bottom.mix(t.raised_hi, 0.72);
                }
                top = top.mix(t.accent_hot, (flash * 0.85 + hv * 0.18).min(1.0));
                bottom = bottom.mix(t.accent_hot, (flash * 0.55 + hv * 0.1).min(1.0));
                c.fill_rrect_vgrad(pad, radius, top, bottom);
                c.fill_rect(Rect::new(pad.x + radius, pad.y + 1.0, pad.w - 2.0 * radius, 1.0), WHITE.alpha(0.3));
                if melodic && pad.w >= 26.0 {
                    let (cx, cy) = pad.center();
                    let ink = if muted { t.text2 } else { t.on_accent };
                    self.text_center(c, self.f.mono_med, 11.0, cx, cy, &note_name(n.key), ink);
                }
            }
            None => {
                let base = if (i / 4) % 2 == 0 { t.pad_a } else { t.pad_b };
                c.fill_rrect(pad, radius, base.mix(t.raised_hi, hv * 0.8).mix(t.pad_head, headv * 0.6));
            }
        }
    }

    /// Petit potard sans légende (rack, mixer compact). Valeur normalisée 0..1.
    pub(super) fn mini_knob(&mut self, c: &mut Canvas, input: &Input, id: u64, (cx, cy): (f32, f32), norm: f32, bipolar: bool) -> Option<f32> {
        let t = self.theme;
        let hit = Rect::new(cx - 14.0, cy - 14.0, 28.0, 28.0);
        let hov = self.hovered(input, id, hit);
        let mut out = None;
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                out = Some(if bipolar { 0.5 } else { 0.8 });
            } else {
                self.begin_drag(id, input, norm);
            }
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let k = if d.fine { 0.001 } else { 0.006 };
            out = Some((d.v0 + (d.y0 - input.y) * k).clamp(0.0, 1.0));
        }
        if hov && input.wheel != 0.0 {
            out = Some((norm + input.wheel * 0.03).clamp(0.0, 1.0));
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        let v = out.unwrap_or(norm);
        let (a0, sweep) = (0.75 * std::f32::consts::PI, 1.5 * std::f32::consts::PI);
        let a = a0 + sweep * v;
        c.stroke_arc(cx, cy, 11.0, a0, a0 + sweep, 2.5, t.raised_hi);
        let from = if bipolar { a0 + sweep * 0.5 } else { a0 };
        if (a - from).abs() > 0.02 {
            c.stroke_arc(cx, cy, 11.0, from.min(a), from.max(a), 2.5, t.text2.mix(t.text, hv));
        }
        c.fill_circle(cx, cy, 7.0, t.raised_hi.mix(Color::hex(0x3a4644), hv));
        let (dx, dy) = (a.cos(), a.sin());
        c.stroke_polyline(&[(cx + dx * 2.0, cy + dy * 2.0), (cx + dx * 6.0, cy + dy * 6.0)], 1.6, t.text);
        out.filter(|o| (o - norm).abs() > 1e-6)
    }
}
