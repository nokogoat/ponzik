//! Grand Piano card: the case seen from the side with its lid (drag it to
//! open or close), a hammer whose felt shows its hardness (drag it), the
//! three pedals as they are pressed, and the 88 keys lighting up as they
//! play, with the dampers lifting off the strings.

use ponzik_engine::instruments::piano::keys::{FIRST_UNDAMPED, HIGHEST, LOWEST};
use ponzik_engine::instruments::piano::{P_HARDNESS, P_LID};
use ponzik_engine::sequence::{AutoScope, AutoTarget, Note};
use ponzik_engine::{ParamInfo, PlayMode};
use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::visuals::device_key;
use super::widgets::wid;

/// Width of the visual on the card.
pub const WIDTH: f32 = 330.0;

/// White key of each of the 12 semitones (or the white key left of a black one).
const WHITE_OF: [u8; 12] = [0, 0, 1, 1, 2, 3, 3, 4, 4, 5, 5, 6];
const IS_BLACK: [bool; 12] = [false, true, false, true, false, false, true, false, true, false, true, false];

/// Index of the white key of `key` from A0 (0) to C8 (51).
fn white_index(key: u8) -> f32 {
    let octave = (key as i32 / 12) as f32;
    let w = WHITE_OF[(key % 12) as usize] as f32;
    // C0 would be white index −5 from A0 (A0 is the 6th white key of octave 0).
    octave * 7.0 + w - (1.0 * 7.0 + 5.0)
}

impl App {
    /// Velocity (0: up) of every key of channel `ch`: held keys, the
    /// previewed note and the notes the transport is playing.
    fn sounding_keys(&self, ch: usize) -> [f32; 128] {
        let mut out = [0.0f32; 128];
        for h in self.held.iter().filter(|h| h.channel == ch) {
            out[(h.key & 127) as usize] = h.vel.max(0.3);
        }
        if let Some((c, k)) = self.preview
            && c == ch
        {
            out[(k & 127) as usize] = 1.0;
        }
        if !self.playing {
            return out;
        }
        let mut scan = |notes: &[Note], local: f64| {
            for n in notes {
                let dt = local - n.start as f64;
                if dt >= 0.0 && dt < n.len.max(1) as f64 {
                    let k = (n.key & 127) as usize;
                    out[k] = out[k].max(n.vel.max(0.3));
                }
            }
        };
        let pattern_notes = |p: usize| self.project.patterns.get(p).and_then(|p| p.notes.get(ch));
        match self.project.mode {
            PlayMode::Pattern(p) => {
                if let Some(notes) = pattern_notes(p) {
                    scan(notes, self.cycle_position());
                }
            }
            PlayMode::Song => {
                for clip in &self.project.clips {
                    let local = self.position - clip.start as f64;
                    if (0.0..clip.len as f64).contains(&local)
                        && let Some(notes) = pattern_notes(clip.pattern)
                    {
                        scan(notes, local + clip.offset as f64);
                    }
                }
            }
            PlayMode::Session => {}
        }
        out
    }

    /// Value (0 to 1) of controller `cc` of channel `ch` at the playhead, from
    /// its automation lanes (0 without any).
    fn controller_at(&self, ch: usize, cc: u8) -> f32 {
        let mut v = None;
        for a in self.project.automation.iter().filter(|a| a.target == AutoTarget::Cc { channel: ch, cc }) {
            let got = match (a.scope, self.project.mode) {
                (AutoScope::Song, PlayMode::Song) => a.value_at(self.position),
                (AutoScope::Pattern(p), PlayMode::Pattern(q)) if p == q => a.value_at(self.cycle_position()),
                (AutoScope::Pattern(p), PlayMode::Song) => self
                    .project
                    .clips
                    .iter()
                    .filter(|c| c.pattern == p)
                    .find_map(|c| {
                        let local = self.position - c.start as f64;
                        (0.0..c.len as f64).contains(&local).then(|| a.value_at(local + c.offset as f64)).flatten()
                    }),
                _ => None,
            };
            v = v.or(got);
        }
        v.unwrap_or(0.0).clamp(0.0, 1.0)
    }

    /// Draws the piano visual in `r` and lets the lid and the hammer be dragged.
    pub(super) fn grand_piano_view(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        let Device::Instrument(ch) = device else { return };
        let key = device_key(device);
        let info = |id: u32| params.iter().find(|p| p.id == id);
        let (Some(lid_p), Some(hard_p)) = (info(P_LID), info(P_HARDNESS)) else { return };
        c.fill_rrect(r, 10.0, t.well);
        let prev_clip = c.push_clip(r);
        let pad = 10.0;
        let keys_h = (r.h * 0.34).clamp(34.0, 60.0);
        let top = Rect::new(r.x + pad, r.y + pad, r.w - 2.0 * pad, r.h - keys_h - 3.0 * pad);
        let keyboard = Rect::new(r.x + pad, top.bottom() + pad, r.w - 2.0 * pad, keys_h);

        // --- Case and lid (left two thirds) ---
        let case = Rect::new(top.x, top.y, top.w * 0.62, top.h);
        let lid = self.param_value(device, lid_p);
        let lid_id = wid("piano-lid", key, 0);
        let lid_hov = self.hovered(input, lid_id, case);
        if lid_hov && input.pressed {
            self.begin_drag(lid_id, input, lid);
        }
        if self.is_dragging(lid_id) {
            let y0 = self.drag.as_ref().map_or(input.y, |d| d.y0);
            let v0 = self.drag.as_ref().map_or(lid, |d| d.v0);
            self.set_param_value(device, lid_p, v0 + (y0 - input.y) / 80.0);
        }
        if lid_hov {
            self.set_tip(tr("Lid: drag up to open, down to close"), "");
        }
        let lid = self.anim.to(wid("piano-lid-a", key, 0), lid, 0.12);
        let rim_y = case.bottom() - case.h * 0.42;
        let (x0, x1) = (case.x, case.right());
        // Rim: straight at the keyboard end, curved toward the tail.
        let mut body = Path::new();
        body.move_to((x0, rim_y));
        body.line_to((x1 - case.w * 0.25, rim_y));
        body.quad_to((x1, rim_y), (x1, rim_y + case.h * 0.12));
        body.line_to((x1, rim_y + case.h * 0.2));
        body.line_to((x0, rim_y + case.h * 0.2));
        body.close();
        c.fill_path(&body, t.raised_hi);
        // Legs.
        for lx in [x0 + 8.0, x1 - 14.0] {
            c.fill_rect(Rect::new(lx, rim_y + case.h * 0.2, 5.0, case.h * 0.18), t.raised);
        }
        // Lid on its hinge at the far side, open up to about 30°.
        let angle = lid * 0.5;
        // Long enough to cover the case, short enough to stay in the frame.
        let len = (case.w * 0.92).min((rim_y - case.y) / 0.5f32.sin());
        let hinge = (x1 - 2.0, rim_y - 1.0);
        let tip = (hinge.0 - len * angle.cos(), hinge.1 - len * angle.sin());
        let hv = self.hover_anim(lid_id, lid_hov || self.is_dragging(lid_id));
        let lid_col = t.text2.mix(t.accent, 0.3 + 0.7 * hv);
        if angle > 0.05 {
            // The opening the sound leaves through, and the prop stick.
            let mut open = Path::new();
            open.move_to(hinge);
            open.line_to(tip);
            open.line_to((tip.0, rim_y));
            open.close();
            c.fill_path(&open, t.accent.alpha(0.06 + 0.12 * lid));
            let top = (hinge.0 - 0.6 * len * angle.cos(), hinge.1 - 0.6 * len * angle.sin());
            c.stroke_polyline(&[(top.0, rim_y), top], 1.5, t.muted);
        }
        c.stroke_polyline(&[hinge, tip], 3.0, lid_col);

        // --- Hammer (right third): felt colored by its hardness ---
        let hammer = Rect::new(case.right() + pad, top.y, top.right() - case.right() - pad, top.h * 0.62);
        let hard = self.param_value(device, hard_p);
        let hid = wid("piano-hammer", key, 0);
        let h_hov = self.hovered(input, hid, hammer);
        if h_hov && input.pressed {
            self.begin_drag(hid, input, hard);
        }
        if self.is_dragging(hid) {
            let y0 = self.drag.as_ref().map_or(input.y, |d| d.y0);
            let v0 = self.drag.as_ref().map_or(hard, |d| d.v0);
            self.set_param_value(device, hard_p, v0 + (y0 - input.y) / 80.0);
        }
        if h_hov {
            self.set_tip(tr("Hammer hardness: drag up for a brighter felt"), "");
        }
        let hard = self.anim.to(wid("piano-hammer-a", key, 0), hard, 0.12);
        let hhv = self.hover_anim(hid, h_hov || self.is_dragging(hid));
        let (hx, hy) = (hammer.x + hammer.w * 0.5, hammer.y + hammer.h * 0.35);
        // String above, shank below.
        c.stroke_polyline(&[(hammer.x, hammer.y + 2.0), (hammer.right(), hammer.y + 2.0)], 1.0, t.text2);
        c.fill_rect(Rect::new(hx - 1.5, hy, 3.0, hammer.h * 0.6), t.raised_hi);
        let core = Rect::new(hx - 9.0, hy - 4.0, 18.0, 12.0);
        c.fill_rrect(core, 3.0, t.raised_hi);
        // Felt: thick and pale when soft, thin and hot when hard.
        let felt = 9.0 - 4.0 * hard;
        let felt_col = t.text2.mix(t.accent_hot, hard).mix(t.accent, hhv * 0.4);
        c.fill_rrect(Rect::new(core.x - felt * 0.4, core.y - felt, core.w + felt * 0.8, felt + 4.0), felt * 0.6, felt_col);

        // --- Pedals: una corda, sostenuto, sustain ---
        let pedals = Rect::new(hammer.x, hammer.bottom() + 4.0, hammer.w, top.bottom() - hammer.bottom() - 4.0);
        let values = [self.controller_at(ch, 67), self.controller_at(ch, 66), self.controller_at(ch, 64)];
        let labels = [tr("Una corda"), tr("Sostenuto"), tr("Sustain")];
        let pw = pedals.w / 3.0;
        for (i, (&v, label)) in values.iter().zip(labels).enumerate() {
            let v = self.anim.to(wid("piano-pedal", key, i), v, 0.06);
            let cx = pedals.x + pw * (i as f32 + 0.5);
            let py = pedals.y + 3.0 + v * 5.0;
            let col = t.muted.mix(t.accent, v);
            c.fill_rrect(Rect::new(cx - pw * 0.28, py, pw * 0.56, 6.0), 3.0, col);
            let pr = Rect::new(cx - pw * 0.5, pedals.y, pw, pedals.h);
            if input.inside && pr.contains(input.x, input.y) {
                self.set_tip(label, "");
            }
        }

        // --- Keyboard and dampers ---
        let sounding = self.sounding_keys(ch);
        let sustain = values[2];
        let lift = ((sustain - 0.22) / 0.48).clamp(0.0, 1.0);
        let whites = 52.0;
        let ww = keyboard.w / whites;
        let damper_h = 5.0;
        let keys_r = Rect::new(keyboard.x, keyboard.y + damper_h + 2.0, keyboard.w, keyboard.h - damper_h - 2.0);
        for k in LOWEST..=HIGHEST {
            if IS_BLACK[(k % 12) as usize] {
                continue;
            }
            let x = keys_r.x + white_index(k) * ww;
            let v = self.anim.to(wid("piano-key", key, k as usize), sounding[k as usize], 0.05);
            let col = t.text.mix(t.accent, v);
            c.fill_rect(Rect::new(x + 0.5, keys_r.y, ww - 1.0, keys_r.h), col);
        }
        for k in LOWEST..=HIGHEST {
            if !IS_BLACK[(k % 12) as usize] {
                continue;
            }
            let x = keys_r.x + (white_index(k) + 1.0) * ww - ww * 0.32;
            let v = self.anim.to(wid("piano-key", key, k as usize), sounding[k as usize], 0.05);
            let col = t.bg_deep.mix(t.accent, v);
            c.fill_rect(Rect::new(x, keys_r.y, ww * 0.64, keys_r.h * 0.62), col);
        }
        // A damper over each key that has one: up when its key or the pedal
        // lifts it.
        for k in LOWEST..FIRST_UNDAMPED {
            let held = sounding[k as usize] > 0.0;
            let up = if held { 1.0 } else { lift };
            let x = keys_r.x + white_index(k) * ww + if IS_BLACK[(k % 12) as usize] { ww * 0.7 } else { ww * 0.15 };
            let y = keyboard.y + 2.0 - up * 2.0;
            c.fill_rect(Rect::new(x, y, (ww * 0.5).max(1.0), damper_h - 2.0), t.muted.mix(t.accent, up * 0.6));
        }
        // Undamped top keys: a hairline instead.
        let x = keys_r.x + white_index(FIRST_UNDAMPED) * ww;
        c.fill_rect(Rect::new(x, keyboard.y + damper_h - 1.0, keys_r.right() - x, 1.0), t.hairline);
        c.set_clip(Some(prev_clip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_keys_span_the_keyboard() {
        assert_eq!(white_index(LOWEST), 0.0);
        assert_eq!(white_index(HIGHEST), 51.0);
        assert_eq!(white_index(60), 23.0);
    }
}
