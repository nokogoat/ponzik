//! Vue Mixer : master et pistes utilisées, inserts, pan, fader, VU-mètres.

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, TRACKS};
use ponzik_engine::registry;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{App, Focus, Meter, Popup};
use super::theme::*;
use super::widgets::{Btn, wid};

const STRIP_W: f32 = 112.0;
const STRIP_GAP: f32 = 8.0;
const DB_TICKS: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];

/// Position de fader (0..1) ↔ gain linéaire, loi cubique : 0 dB vers 79 %, +6 dB en haut.
pub fn pos_to_gain(pos: f32) -> f32 {
    2.0 * pos.clamp(0.0, 1.0).powi(3)
}

pub fn gain_to_pos(gain: f32) -> f32 {
    (gain.max(0.0) / 2.0).cbrt().min(1.0)
}

pub fn fmt_db(gain: f32) -> String {
    let d = 20.0 * gain.max(1e-9).log10();
    if d < -90.0 {
        return "−∞ dB".into();
    }
    let s = format!("{:.1}", d.abs()).replace('.', ",");
    if d < -0.05 {
        format!("−{s} dB")
    } else if d > 0.05 {
        format!("+{s} dB")
    } else {
        "0,0 dB".into()
    }
}

fn fmt_tick(d: f32) -> String {
    match d {
        d if d > 0.0 => format!("+{d:.0}"),
        d if d < 0.0 => format!("−{:.0}", -d),
        _ => "0".into(),
    }
}

impl App {
    /// Pistes affichées : le master, celles des canaux, et celles qui ont des effets.
    fn visible_tracks(&self, all: bool) -> Vec<usize> {
        let mut out = vec![MASTER];
        for t in 1..TRACKS {
            let used = all
                || self.project.channels.iter().any(|c| c.track == t)
                || self.project.tracks[t].inserts.iter().any(Option::is_some)
                || self.project.tracks.iter().any(|x| x.output == t || x.sends.iter().any(|s| s.0 == t && s.1 > 0.0));
            if used {
                out.push(t);
            }
        }
        out
    }

    pub(super) fn mixer_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let all_key = wid("mx-all", 0, 0);
        let all = self.anim.get(all_key).unwrap_or(0.0) > 0.5;
        let toggle = Rect::new(header.right() - 210.0, header.y, 210.0, header.h);
        if self.button(c, input, wid("mx-all-btn", 0, 0), toggle, if all { "Pistes utilisées" } else { "Toutes les pistes" }, Btn::Secondary) {
            self.anim.set(all_key, if all { 0.0 } else { 1.0 });
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);
        let tracks = self.visible_tracks(all);
        let content_w = tracks.len() as f32 * (STRIP_W + STRIP_GAP) + 12.0;
        if input.inside && inner.contains(input.x, input.y) && input.wheel != 0.0 && input.mods.shift {
            self.mixer_scroll = (self.mixer_scroll - input.wheel * 80.0).clamp(0.0, (content_w - inner.w).max(0.0));
        }
        self.mixer_scroll = self.mixer_scroll.clamp(0.0, (content_w - inner.w).max(0.0));
        let sx = self.anim.to(wid("mx-sx", 0, 0), self.mixer_scroll, T_LAYOUT);
        let prev = c.push_clip(inner);
        let mut x = inner.x - sx;
        for (k, &tr) in tracks.iter().enumerate() {
            let r = Rect::new(x, inner.y, STRIP_W, inner.h);
            if r.right() > inner.x && r.x < inner.right() {
                self.mixer_strip(c, input, r, tr);
            }
            x += STRIP_W + STRIP_GAP;
            if k == 0 {
                x += 12.0;
            }
        }
        c.set_clip(Some(prev));
    }

    fn mixer_strip(&mut self, c: &mut Canvas, input: &Input, r: Rect, tr: usize) {
        let t = self.theme;
        let focused = self.focused_track() == tr && matches!(self.focus, Focus::Track(_));
        let fv = self.anim.to(wid("mx-focus", tr, 0), focused as u8 as f32, T_HOVER);
        c.fill_rrect(r, 12.0, t.strip.mix(t.raised, fv * 0.6));
        c.stroke_rrect(r, 12.0, 1.0, if focused { t.select.alpha(0.6) } else { t.hairline });
        let inner = r.inset(8.0);

        // Nom : clic pour afficher la chaîne d'effets en bas.
        let name_r = Rect::new(inner.x, inner.y, inner.w, 28.0);
        let nid = wid("mx-name", tr, 0);
        let nhov = self.hovered(input, nid, name_r);
        if nhov && input.pressed {
            self.focus = Focus::Track(tr);
            self.panel = true;
            if input.double {
                let text = self.project.tracks[tr].name.clone();
                self.open_popup(Popup::Rename { target: super::app::Rename::Track(tr), text }, name_r);
            }
        }
        if nhov && input.right_pressed {
            self.focus = Focus::Track(tr);
            self.open_popup(Popup::TrackMenu(tr), Rect::new(input.x, input.y, 0.0, 0.0));
        }
        let name = self.project.tracks[tr].name.clone();
        let label = self.fit(self.f.sans_med, 13.0, &name, inner.w - 4.0);
        self.text_center(c, self.f.sans_med, 13.0, inner.x + inner.w * 0.5, inner.y + 10.0, &label, t.text);
        let num = if tr == MASTER { "M".to_string() } else { tr.to_string() };
        self.text_center(c, self.f.mono, 10.0, inner.x + inner.w * 0.5, inner.y + 25.0, &num, t.muted);

        // Inserts.
        let mut y = inner.y + 38.0;
        for slot in 0..INSERT_SLOTS {
            let row = Rect::new(inner.x, y, inner.w, 20.0);
            let id = wid("mx-slot", tr, slot);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            let fx = self.project.tracks[tr].inserts[slot].clone();
            match &fx {
                Some(def) => {
                    let name = registry::effect_kind(&def.kind).map_or(def.kind.as_str(), |k| k.name);
                    c.fill_rrect(row, 6.0, t.raised.mix(t.raised_hi, hv));
                    let dot = if def.bypass { t.muted } else { t.accent };
                    c.fill_circle(row.x + 9.0, row.y + 10.0, 3.0, dot);
                    self.text_fit(c, self.f.sans, 11.0, row.x + 18.0, row.y + 10.0, name, row.w - 22.0, if def.bypass { t.muted } else { t.text2 });
                }
                None => {
                    c.stroke_rrect(row, 6.0, 1.0, t.hairline.alpha(0.6 + hv));
                    if hv > 0.01 {
                        self.text_center(c, self.f.sans, 11.0, row.x + row.w * 0.5, row.y + 10.0, "+", t.muted.alpha(hv));
                    }
                }
            }
            if hov && input.pressed {
                self.focus = Focus::Track(tr);
                self.panel = true;
                if fx.is_none() {
                    self.open_popup(Popup::AddEffect { track: tr }, row);
                }
            }
            if hov && input.right_pressed && fx.is_some() {
                self.project.tracks[tr].inserts[slot] = None;
                self.commit("Retirer un effet");
            }
            y += 22.0;
        }

        // Envois actifs : destination et niveau. Hauteur fixe, pour que les
        // faders restent alignés d'une tranche à l'autre.
        y += 4.0;
        let sends_top = y;
        let mut shown = 0;
        for slot in 0..ponzik_engine::mixer::SEND_SLOTS {
            let (target, amount) = self.project.tracks[tr].sends[slot];
            if amount <= 0.0 || target == tr || shown == 2 {
                continue;
            }
            shown += 1;
            let row = Rect::new(inner.x, y, inner.w, 24.0);
            let label = format!("→ {}", self.project.tracks[target].name);
            self.text_fit(c, self.f.sans, 11.0, row.x + 2.0, row.y + 12.0, &label, row.w - 30.0, t.text2);
            if let Some(v) = self.mini_knob(c, input, wid("mx-send", tr, slot), (row.right() - 11.0, row.y + 12.0), amount, false) {
                // Un envoi ramené à zéro se libère.
                self.project.tracks[tr].sends[slot].1 = if v < 0.01 { 0.0 } else { v };
                self.commit("Niveau d'envoi");
            }
            y += 26.0;
        }
        y = sends_top + 52.0;

        // Pan.
        let pan_y = y + 20.0;
        let pan = self.project.tracks[tr].pan;
        if let Some(v) = self.mini_knob(c, input, wid("mx-pan", tr, 0), (inner.x + 18.0, pan_y), (pan + 1.0) * 0.5, true) {
            self.project.tracks[tr].pan = ((v * 2.0 - 1.0) * 100.0).round() / 100.0;
            self.commit("Pan de piste");
        }
        let pan_text = match (pan * 100.0).round() as i32 {
            0 => "C".to_string(),
            p if p < 0 => format!("G {}", -p),
            p => format!("D {p}"),
        };
        self.text_right(c, self.f.mono, 11.0, inner.right(), pan_y, &pan_text, t.text2);

        // Bas : sortie, mute ; valeur au-dessus ; fader au milieu.
        let mute = Rect::new(inner.x, inner.bottom() - 26.0, inner.w, 26.0);
        let out_r = Rect::new(inner.x, mute.y - 28.0, inner.w, 24.0);
        let readout_y = out_r.y - 12.0;
        let fader = Rect::new(inner.x, pan_y + 22.0, inner.w, readout_y - 12.0 - pan_y - 22.0);
        if fader.h > 40.0 {
            self.track_fader(c, input, fader, tr);
        }
        let gain = self.project.tracks[tr].gain;
        self.text_center(c, self.f.mono, 12.0, inner.x + inner.w * 0.5, readout_y, &fmt_db(gain), t.text2);

        let oid = wid("mx-out", tr, 0);
        let ohov = self.hovered(input, oid, out_r) && tr != MASTER;
        let ohv = self.hover_anim(oid, ohov);
        if ohv > 0.01 {
            c.fill_rrect(out_r, 6.0, t.raised.alpha(ohv));
        }
        let out_label = if tr == MASTER { "Sortie audio".to_string() } else { format!("→ {}", self.project.tracks[self.project.tracks[tr].output].name) };
        let lw = self.fit(self.f.sans, 11.0, &out_label, out_r.w - 8.0);
        self.text_center(c, self.f.sans, 11.0, out_r.x + out_r.w * 0.5, out_r.y + 12.0, &lw, t.muted.mix(t.text2, ohv));
        if ohov && input.pressed {
            self.open_popup(Popup::Output(tr), out_r);
        }

        let muted = self.project.tracks[tr].muted;
        let half = (mute.w - 4.0) * 0.5;
        let mute_r = Rect::new(mute.x, mute.y, if tr == MASTER { mute.w } else { half }, mute.h);
        let mid = wid("mx-mute", tr, 0);
        let hov = self.hovered(input, mid, mute_r);
        if hov && input.pressed {
            self.project.tracks[tr].muted = !muted;
            self.commit("Muet de piste");
        }
        let hv = self.hover_anim(mid, hov);
        let (fill, ink) = if muted { (t.text.mix(WHITE, hv * 0.2), t.bg_deep) } else { (t.raised.mix(t.raised_hi, hv), t.text2) };
        c.fill_rrect(mute_r, 8.0, fill);
        let (mx, my) = mute_r.center();
        self.text_center(c, self.f.sans_med, 12.0, mx, my, if tr == MASTER { "Mute" } else { "M" }, ink);
        if tr != MASTER {
            let solo_r = Rect::new(mute.x + half + 4.0, mute.y, half, mute.h);
            let sid = wid("mx-solo", tr, 0);
            let shov = self.hovered(input, sid, solo_r);
            if shov && input.pressed {
                self.toggle_solo(tr);
            }
            let shv = self.hover_anim(sid, shov);
            let on = self.solo == Some(tr);
            let (fill, ink) = if on { (t.select, t.bg_deep) } else { (t.raised.mix(t.raised_hi, shv), t.text2) };
            c.fill_rrect(solo_r, 8.0, fill);
            let (sx, sy) = solo_r.center();
            self.text_center(c, self.f.sans_med, 12.0, sx, sy, "S", ink);
        }
    }

    fn track_fader(&mut self, c: &mut Canvas, input: &Input, area: Rect, tr: usize) {
        let t = self.theme;
        const CAP_W: f32 = 34.0;
        const CAP_H: f32 = 18.0;
        let travel = (area.h - CAP_H).max(1.0);
        let y_of = |pos: f32| area.bottom() - CAP_H * 0.5 - pos * travel;
        let track_x = area.x + 42.0;
        let gain = self.project.tracks[tr].gain;
        let pos = gain_to_pos(gain);
        let cap = Rect::new(track_x + 2.0 - CAP_W * 0.5, y_of(pos) - CAP_H * 0.5, CAP_W, CAP_H);
        let zone = Rect::new(area.x + 24.0, area.y, 40.0, area.h);
        let id = wid("mx-fader", tr, 0);
        let hov = self.hovered(input, id, zone);
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                self.project.tracks[tr].gain = 1.0;
                self.commit("Volume de piste");
            } else {
                let grab = if cap.contains(input.x, input.y) { input.y - y_of(pos) } else { 0.0 };
                self.begin_drag(id, input, grab);
            }
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let p = ((area.bottom() - CAP_H * 0.5 - (input.y - d.v0)) / travel).clamp(0.0, 1.0);
            self.project.tracks[tr].gain = pos_to_gain(p);
            self.commit("Volume de piste");
        }
        if hov && input.wheel != 0.0 {
            self.project.tracks[tr].gain = pos_to_gain((pos + input.wheel * 0.02).clamp(0.0, 1.0));
            self.commit("Volume de piste");
        }

        for d in DB_TICKS {
            let y = y_of(gain_to_pos(10f32.powf(d / 20.0)));
            c.fill_rect(Rect::new(track_x - 10.0, y.round(), 5.0, 1.0), t.hairline.alpha(2.0));
            let label = fmt_tick(d);
            self.text_right(c, self.f.mono, 10.0, area.x + 22.0, y, &label, t.muted);
        }
        c.fill_rrect(Rect::new(track_x, area.y + CAP_H * 0.5, 4.0, travel), 2.0, t.well);

        let meter: Meter = self.track_meters[tr];
        let warn = y_of(gain_to_pos(10f32.powf(-6.0 / 20.0)));
        let bottom = y_of(0.0);
        for side in 0..2 {
            let x = track_x + 22.0 + side as f32 * 7.0;
            c.fill_rrect(Rect::new(x, area.y + CAP_H * 0.5, 5.0, travel), 2.0, t.well);
            let top = y_of(gain_to_pos(meter.level[side]));
            if bottom - top > 0.5 {
                let prev = c.push_clip(Rect::new(x, warn, 5.0, bottom - warn));
                c.fill_rrect(Rect::new(x, top, 5.0, bottom - top), 2.0, t.meter);
                c.set_clip(Some(prev));
                let prev = c.push_clip(Rect::new(x, area.y, 5.0, warn - area.y));
                c.fill_rrect(Rect::new(x, top, 5.0, bottom - top), 2.0, t.accent);
                c.set_clip(Some(prev));
            }
            let hold = meter.hold[side];
            if hold > 1e-4 {
                let col = if hold >= 0.999 { t.negative } else { t.text2 };
                c.fill_rect(Rect::new(x, y_of(gain_to_pos(hold)).round() - 1.0, 5.0, 2.0), col);
            }
        }
        let hv = self.hover_anim(id, (hov && cap.contains(input.x, input.y)) || self.is_dragging(id));
        c.fill_rrect_vgrad(cap, 6.0, t.text.mix(WHITE, hv * 0.5), t.text.mix(t.raised_hi, 0.25).mix(WHITE, hv * 0.3));
        c.fill_rect(Rect::new(cap.x + 6.0, cap.y + CAP_H * 0.5 - 1.0, CAP_W - 12.0, 2.0), t.bg_deep.alpha(0.55));
    }
}
