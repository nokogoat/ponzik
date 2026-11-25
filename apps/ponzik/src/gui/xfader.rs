//! The mixer's crossfader and cue, after Ableton Live: a crossfader in the
//! header (its curve, its position, the cue blend), and on each strip the
//! side it listens to (A or B) and its cue button.
//!
//! The audio backend opens one stereo pair, so the cue is a prelisten mixed
//! into what you hear (the cue blend: 0 the master only, 1 the cued tracks
//! only); the master, its meters, recordings and exports never get it.

use ponzik_engine::mixer::MASTER;
use ponzik_engine::xfader::{Curve, Side};
use ponzik_i18n::{tr, tr_ctx};
use ponzik_project::TrackKind;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::widgets::{Icon, wid};

/// Height of the strip's A / cue / B row.
const ROW_H: f32 = 22.0;

impl App {
    /// Sets the crossfader (one undo step per gesture).
    fn set_crossfader(&mut self, pos: f32) {
        let pos = pos.clamp(0.0, 1.0);
        if self.project.live.xfader.pos != pos {
            self.project.live.xfader.pos = pos;
            self.commit(tr("Crossfader"));
        }
    }

    /// Crossfader controls in the mixer's header, right to left from
    /// `right`; returns the x left of them.
    pub(super) fn mixer_xfader(&mut self, c: &mut Canvas, input: &Input, header: Rect, right: f32) -> f32 {
        let t = self.theme;
        let x = self.project.live.xfader;
        let toggle = Rect::new(right - 36.0, header.y, 36.0, header.h);
        let tip = if x.shown { (tr("Hide the crossfader"), "") } else { (tr("Crossfader and cue\nTracks on side A or B, prelisten on the cue"), "") };
        if self.icon_button(c, input, wid("xf-show", 0, 0), toggle, Icon::Swap, x.shown, tip) {
            self.project.live.xfader.shown = !x.shown;
            self.commit(tr("Crossfader"));
        }
        if !x.shown {
            return toggle.x - 8.0;
        }
        let cy = header.y + header.h * 0.5;
        // Cue blend: what you hear while a track is cued.
        let cue = Rect::new(toggle.x - 8.0 - 104.0, cy - 11.0, 104.0, 22.0);
        if let Some(v) = self.hslider(c, input, wid("xf-cue", 0, 0), cue, x.cue_mix, 1.0) {
            self.project.live.xfader.cue_mix = v;
            self.commit(tr("Cue blend"));
        }
        self.text(c, self.f.mono_med, 10.0, cue.x + 6.0, cy, tr_ctx("cue button", "CUE"), t.text2);
        self.tip_zone(input, cue, tr("Cue blend\nLeft: the master · right: only the cued tracks\nThe master itself never gets the cue"), "");
        // The crossfader: A on the left, B on the right.
        let fader = Rect::new(cue.x - 12.0 - 220.0, cy - 11.0, 220.0, 22.0);
        if let Some(v) = self.hslider(c, input, wid("xf-pos", 0, 0), fader, x.pos, 0.5) {
            self.set_crossfader(v);
        }
        self.text(c, self.f.mono_med, 11.0, fader.x + 8.0, cy, "A", t.text);
        self.text_right(c, self.f.mono_med, 11.0, fader.right() - 8.0, cy, "B", t.text);
        self.tip_zone(input, fader, tr("Crossfader\nDrag: from side A to side B · double-click: middle"), "");
        // Curve.
        let curve = Rect::new(fader.x - 8.0 - 132.0, cy - 14.0, 132.0, 28.0);
        let labels = [tr("Linear"), tr("Power"), tr("Cut")];
        let tips = [
            (tr("Linear: each side falls in a straight line"), ""),
            (tr("Constant power: no dip in the middle"), ""),
            (tr("Cut: both sides at full level until the ends"), ""),
        ];
        let sel = Curve::ALL.iter().position(|k| *k == x.curve).unwrap_or(1);
        if let Some(i) = self.segmented_tips(c, input, wid("xf-curve", 0, 0), curve, &labels, &tips, sel) {
            self.project.live.xfader.curve = Curve::ALL[i];
            self.commit(tr("Crossfader curve"));
        }
        curve.x - 8.0
    }

    /// A horizontal slider in [0, 1] with a thumb: the new value while
    /// dragged (or wheeled), `reset` on a double-click.
    fn hslider(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, v: f32, reset: f32) -> Option<f32> {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, r.h * 0.5, t.bg_deep);
        c.stroke_rrect(r, r.h * 0.5, 1.0, t.hairline.mix(t.accent, hv * 0.4));
        let track = Rect::new(r.x + 26.0, r.y + r.h * 0.5 - 1.5, r.w - 48.0, 3.0);
        c.fill_rrect(track, 1.5, t.raised_hi);
        let tx = track.x + track.w * v.clamp(0.0, 1.0);
        c.fill_rrect(Rect::new(tx - 5.0, r.y + 3.0, 10.0, r.h - 6.0), 3.0, t.accent.mix(t.text, hv * 0.3));
        let mut out = None;
        if hov && input.double {
            out = Some(reset);
        } else if hov && input.pressed {
            self.begin_drag(id, input, v);
        }
        if self.is_dragging(id) && input.down {
            out = Some(((input.x - track.x) / track.w.max(1.0)).clamp(0.0, 1.0));
        }
        if hov && input.wheel != 0.0 {
            out = Some((v + input.wheel.signum() * 0.05).clamp(0.0, 1.0));
        }
        out.filter(|n| *n != v)
    }

    /// The strip's crossfader row (A, cue, B) above `y`; returns the height
    /// it takes (none while the crossfader is hidden).
    pub(super) fn strip_xfade_row(&mut self, c: &mut Canvas, input: &Input, y: f32, inner: Rect, tr_: usize) -> f32 {
        let kind = self.project.tracks[tr_].kind;
        if !self.project.live.xfader.shown || tr_ == MASTER || kind == TrackKind::Vca {
            return 0.0;
        }
        let t = self.theme;
        let row = Rect::new(inner.x, y - ROW_H - 4.0, inner.w, ROW_H);
        let w = (row.w - 8.0) / 3.0;
        let side = self.project.tracks[tr_].xfade;
        for (k, (label, s)) in [("A", Side::A), ("", Side::None), ("B", Side::B)].into_iter().enumerate() {
            let r = Rect::new(row.x + k as f32 * (w + 4.0), row.y, w, row.h);
            let id = wid("xf-strip", tr_, k);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let on = if k == 1 { self.is_cued(tr_) } else { side == s };
            let fill = if on { t.accent.mix(t.panel, 0.25) } else { t.raised.mix(t.raised_hi, hv) };
            c.fill_rrect(r, 6.0, fill);
            let ink = if on { t.bg_deep } else { t.text2 };
            let (cx, cy) = r.center();
            let label = if k == 1 { tr_ctx("cue button", "CUE") } else { label };
            self.text_center(c, self.f.mono_med, if k == 1 { 9.0 } else { 11.0 }, cx, cy, label, ink);
            if hov {
                let tip = match k {
                    0 => tr("Side A of the crossfader"),
                    2 => tr("Side B of the crossfader"),
                    _ => tr("Cue: prelisten this track\nNever reaches the master"),
                };
                self.set_tip(tip, "");
            }
            if hov && input.pressed {
                if k == 1 {
                    self.toggle_cue(tr_);
                } else {
                    self.project.tracks[tr_].xfade = if on { Side::None } else { s };
                    self.commit(tr("Crossfader side"));
                }
            }
        }
        ROW_H + 6.0
    }

    /// Screenshot setup (`PONZIK_XFADER=pos`): the crossfader shown at `pos`,
    /// tracks 1 and 2 on A, 3 and 4 on B, track 2 cued.
    pub fn screenshot_xfader(&mut self, spec: &str) {
        let x = &mut self.project.live.xfader;
        x.shown = true;
        x.pos = spec.trim().parse().unwrap_or(0.3);
        for (t, s) in [(1, Side::A), (2, Side::A), (3, Side::B), (4, Side::B)] {
            if let Some(track) = self.project.tracks.get_mut(t) {
                track.xfade = s;
            }
        }
        self.commit(tr("Crossfader"));
        self.toggle_cue(2);
    }
}
