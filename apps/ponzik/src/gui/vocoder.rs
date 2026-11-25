//! Visual of the vocoder: one pair of bars per band, the modulator's level
//! (what the voice puts there) beside the output's, live from the effect's
//! lock-free shared state; under them the band layout on a frequency axis,
//! each modulator band joined to the carrier band it drives (the formant
//! shift slants the links). A dot lights up while the modulator sounds like a
//! consonant, when noise is let through.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use ponzik_engine::effects::vocoder::{self as voc, VocoderShared};
use ponzik_engine::{Command, ParamInfo};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::visuals::{device_key, x_of_freq};
use super::widgets::wid;

/// Width of the picture in the device card.
pub const VIS_W: f32 = 320.0;
/// Range of the bars, in dB under full scale.
const FLOOR_DB: f32 = -60.0;
const AXIS_H: f32 = 34.0;

thread_local! {
    static SHARED: RefCell<HashMap<(usize, usize), Arc<VocoderShared>>> = RefCell::new(HashMap::new());
}

/// Keeps the shared state of the vocoders sent to the engine; to be called on
/// every command before sending it.
pub fn capture(cmd: &Command) {
    let Command::SetInsert { track, slot, effect } = cmd else { return };
    let shared = effect.as_ref().and_then(|e| e.shared()).and_then(|s| s.downcast::<VocoderShared>().ok());
    SHARED.with(|m| {
        let mut m = m.borrow_mut();
        match shared {
            Some(s) => {
                m.insert((*track, *slot), s);
            }
            None => {
                m.remove(&(*track, *slot));
            }
        }
    });
}

fn level(lin: f32) -> f32 {
    ((20.0 * lin.max(1e-6).log10() - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

impl App {
    /// Shared state of the vocoder at a slot; when missing (project opened at
    /// start), the effect is made again with the same settings to get it.
    fn vocoder_shared(&mut self, track: usize, slot: usize) -> Option<Arc<VocoderShared>> {
        if let Some(s) = SHARED.with(|m| m.borrow().get(&(track, slot)).cloned()) {
            return Some(s);
        }
        let def = self.project.tracks.get(track)?.inserts[slot].clone()?;
        if def.kind != "vocoder" {
            return None;
        }
        let fx = ponzik_project::Project::make_effect(&def, self.sample_rate as f32, self.project.bpm)?;
        let cmd = Command::SetInsert { track, slot, effect: Some(fx) };
        capture(&cmd);
        self.send(cmd);
        SHARED.with(|m| m.borrow().get(&(track, slot)).cloned())
    }

    pub(super) fn vocoder_visual(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let shared = match device {
            Device::Effect { track, slot } => self.vocoder_shared(track, slot),
            _ => None,
        };
        let value = |app: &App, id: u32| params.iter().find(|p| p.id == id).map_or(0.0, |p| app.param_value(device, p));
        let layout = voc::layout_of(&|id| value(self, id), self.sample_rate as f32);
        let n = layout.count;

        // Bars: one slot per band, the modulator on the left, the output on the right.
        let bars = Rect::new(r.x + 10.0, r.y + 22.0, r.w - 20.0, r.h - 22.0 - AXIS_H - 8.0);
        for k in 1..4 {
            let y = (bars.bottom() - bars.h * k as f32 / 4.0).round();
            c.fill_rect(Rect::new(bars.x, y, bars.w, 1.0), t.hairline.alpha(0.7));
        }
        let slot_w = bars.w / n as f32;
        let bar_w = (slot_w * 0.36).max(1.0);
        let live = shared.as_ref().is_some_and(|s| s.bands() == n);
        for i in 0..n {
            let x = bars.x + slot_w * i as f32 + slot_w * 0.5;
            let (m, o) = match shared.as_ref().filter(|_| live) {
                Some(s) => (level(s.modulator(i)), level(s.output(i))),
                None => (0.0, 0.0),
            };
            c.fill_rect(Rect::new(x - bar_w - 0.5, bars.bottom() - 2.0, bar_w, 2.0), t.hairline);
            c.fill_rect(Rect::new(x + 0.5, bars.bottom() - 2.0, bar_w, 2.0), t.hairline);
            if m > 0.0 {
                let h = m * bars.h;
                c.fill_rect(Rect::new(x - bar_w - 0.5, bars.bottom() - h, bar_w, h), t.text2.alpha(0.55));
            }
            if o > 0.0 {
                let h = o * bars.h;
                c.fill_rect(Rect::new(x + 0.5, bars.bottom() - h, bar_w, h), t.accent);
            }
        }

        // Legend and consonant dot.
        let ly = r.y + 11.0;
        c.fill_rect(Rect::new(r.x + 12.0, ly - 4.0, 8.0, 8.0), t.text2.alpha(0.55));
        let w = self.text(c, self.f.sans, 11.0, r.x + 24.0, ly, tr("Modulator"), t.muted);
        c.fill_rect(Rect::new(r.x + 34.0 + w, ly - 4.0, 8.0, 8.0), t.accent);
        self.text(c, self.f.sans, 11.0, r.x + 46.0 + w, ly, tr("Output"), t.muted);
        let unvoiced = shared.as_ref().map_or(0.0, |s| s.unvoiced());
        let dot = (r.right() - 14.0, ly);
        c.fill_circle(dot.0, dot.1, 4.0, t.muted.alpha(0.5).mix(t.warning, unvoiced.clamp(0.0, 1.0)));
        let dot_r = Rect::new(dot.0 - 10.0, dot.1 - 10.0, 20.0, 20.0);
        self.tip_zone(input, dot_r, tr("Consonant detector\nLights up on \"s\" and \"t\" sounds: noise is mixed into the carrier (Unvoiced)"), "");
        let keyed = shared.as_ref().is_some_and(|s| s.keyed());
        if !keyed {
            self.text_right(c, self.f.sans, 11.0, dot.0 - 12.0, ly, tr("No key: listens to its track"), t.muted);
        }

        // Band layout on a frequency axis: modulator centers above, carrier centers below.
        let axis = Rect::new(r.x + 10.0, r.bottom() - AXIS_H - 4.0, r.w - 20.0, AXIS_H);
        let (top, bottom) = (axis.y + 6.0, axis.bottom() - 12.0);
        for f in [100.0, 1_000.0, 10_000.0] {
            let x = x_of_freq(axis, f).round();
            c.fill_rect(Rect::new(x, axis.y, 1.0, axis.h - 10.0), t.hairline.alpha(0.8));
            let label = if f >= 1_000.0 { format!("{}k", f as u32 / 1_000) } else { format!("{}", f as u32) };
            self.text(c, self.f.mono, 9.0, x + 3.0, axis.bottom() - 4.0, &label, t.muted);
        }
        let shifted = layout.carrier[..n] != layout.modulator[..n];
        for i in 0..n {
            let (xm, xc) = (x_of_freq(axis, layout.modulator[i]), x_of_freq(axis, layout.carrier[i]));
            let col = t.accent.alpha(0.35);
            if shifted {
                c.stroke_polyline(&[(xm, top + 3.0), (xc, bottom - 3.0)], 1.0, col);
            } else {
                c.fill_rect(Rect::new(xm - 0.5, top + 3.0, 1.0, bottom - top - 6.0), col);
            }
            c.fill_circle(xm, top, 1.8, t.text2);
            c.fill_circle(xc, bottom, 1.8, t.accent);
        }

        let id = wid("vocoder-vis", device_key(device), 0);
        if self.hovered(input, id, Rect::new(r.x, r.y + 20.0, r.w, r.h - 20.0)) {
            let i = (((input.x - bars.x) / slot_w).floor().max(0.0) as usize).min(n - 1);
            let hz = |f: f32| voc::params()[voc::P_LOW as usize].format(f);
            self.set_tip(
                &tr_fmt!(
                    "Band {} of {}: the voice at {} drives the carrier at {}\nPale bars: the modulator's level · colored bars: the output's",
                    i + 1,
                    n,
                    hz(layout.modulator[i]),
                    hz(layout.carrier[i])
                ),
                "",
            );
        }
    }

    /// Captures: a vocoder on track 1 keyed by track 2, set to a factory preset,
    /// fed a second and a half of a synthetic singer so the bars have a live
    /// look. `spec`: `<preset name>|default[,formant:<st>]`.
    pub fn screenshot_vocoder(&mut self, spec: &str) {
        use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};
        let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
        let track = 1.min(self.project.tracks.len() - 1);
        let key = 2.min(self.project.tracks.len() - 1);
        let mut def = ponzik_project::EffectDef { sidechain: Some(key), ..ponzik_project::EffectDef::new("vocoder") };
        for &(id, v) in ponzik_project::presets::factory_preset("vocoder", parts[0]).iter().flatten() {
            def.set(id, v);
        }
        for part in &parts[1..] {
            if let Some(st) = part.strip_prefix("formant:").and_then(|v| v.parse::<f32>().ok()) {
                def.set(voc::P_FORMANT, st);
            }
        }
        let slot = self.project.tracks[track].inserts.iter().position(Option::is_none).unwrap_or(0);
        self.project.tracks[track].inserts[slot] = Some(def.clone());
        self.project.tracks[track].name = tr("Synth").to_string();
        self.project.tracks[key].name = tr("Vocals").to_string();
        self.focus = super::app::Focus::Track(track);
        self.panel = true;
        self.commit(tr("Capture effects"));
        let sr = self.sample_rate as f32;
        let Some(mut fx) = ponzik_project::Project::make_effect(&def, sr, self.project.bpm) else { return };
        let shared = fx.shared().and_then(|s| s.downcast::<VocoderShared>().ok());
        let notes: Vec<Sung> = (0..3).map(|i| Sung { start: 0.1 + 0.6 * i as f32, dur: 0.55, midi: 52.0 + 3.0 * i as f32, vowel: i }).collect();
        let voice = sing(sr, &notes, &Style::default(), 1.5);
        // A saw pad on the track, for a carrier taken from the track.
        let pad: Vec<f32> = (0..voice.len())
            .map(|k| [130.81f32, 155.56, 196.0].iter().map(|f| ((k as f32 * f / sr).fract() * 2.0 - 1.0) * 0.12).sum())
            .collect();
        let (mut l, mut r) = (pad.clone(), pad);
        for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
            let kb = &voice[k * 256..k * 256 + a.len()];
            fx.sidechain(kb, kb);
            fx.process(a, b);
        }
        if let Some(s) = shared {
            SHARED.with(|m| m.borrow_mut().insert((track, slot), s));
        }
    }
}
