//! The envelope editor shared by every native instrument: delay, attack,
//! hold, decay, sustain and release drawn as they play, edited by dragging
//! their points (times sideways, the sustain level up and down) and their
//! segments (the curve's bend), with the value read out on hover and the
//! last played voice's stage moving along it.
//!
//! Every handle moves a regular parameter of the instrument: undo,
//! automation, modulation and typed values work as for its knobs.

use ponzik_dsp::env::env_segment;
use ponzik_engine::{EnvStage, ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Popup};
use super::devices::{Device, device_target};
use super::visuals::device_key;
use super::widgets::wid;

/// Width of a delay or hold slot, in slots of attack, decay or release.
const HALF: f32 = 0.5;
/// Width of the sustain plateau, in slots.
const PLATEAU: f32 = 0.6;
/// Pixels of vertical drag for the whole range of a curve.
const CURVE_DRAG: f32 = 90.0;
/// Radius of a handle, and how close the pointer must come to it.
const HANDLE_R: f32 = 4.5;
const HANDLE_HIT: f32 = 9.0;
/// Short envelopes are zoomed in up to this much to fill most of the graph.
const MAX_ZOOM: f32 = 4.0;
/// Part of the width a zoomed envelope fills.
const FILL: f32 = 0.88;

/// The parameters of one envelope, found by name in a group of parameters.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvSpec {
    pub delay: Option<&'static ParamInfo>,
    pub attack: Option<&'static ParamInfo>,
    pub hold: Option<&'static ParamInfo>,
    pub decay: Option<&'static ParamInfo>,
    pub sustain: Option<&'static ParamInfo>,
    pub release: Option<&'static ParamInfo>,
    /// Bends of the attack, decay and release.
    pub curves: [Option<&'static ParamInfo>; 3],
}

impl EnvSpec {
    /// The envelope of a group (`group` its name): two of attack, decay and
    /// release in seconds, or an amp group's decay alone (a drum's). `None`
    /// for anything else (a string's decay ratio, a filter's decay).
    pub fn find(group: &str, params: &[&'static ParamInfo]) -> Option<Self> {
        let named = |n: &str| params.iter().copied().find(|p| p.name == n);
        let timed = |n: &str| named(n).filter(|p| p.unit == Unit::Seconds && p.max > p.min);
        let spec = Self {
            delay: timed("Delay"),
            attack: timed("Attack"),
            hold: timed("Hold"),
            decay: timed("Decay"),
            sustain: named("Sustain").filter(|p| p.unit != Unit::Seconds),
            release: timed("Release"),
            curves: [named("Attack curve"), named("Decay curve"), named("Release curve")],
        };
        let stages = [spec.attack, spec.decay, spec.release].iter().filter(|p| p.is_some()).count();
        (stages >= 2 || (stages == 1 && spec.decay.is_some() && group == "Amp")).then_some(spec)
    }

    /// Parameters the graph edits on its own, without a knob of their own
    /// (the curves): the knobs of the group skip them.
    pub fn graph_only(&self, p: &ParamInfo) -> bool {
        self.curves.iter().flatten().any(|c| c.id == p.id)
    }

    /// True if `p` belongs to this envelope.
    pub fn has(&self, p: &ParamInfo) -> bool {
        self.ordered().iter().any(|q| q.id == p.id) || self.graph_only(p)
    }

    /// The envelope's knobs in the order it plays: delay, attack, hold, decay, sustain, release.
    pub fn ordered(&self) -> Vec<&'static ParamInfo> {
        [self.delay, self.attack, self.hold, self.decay, self.sustain, self.release].into_iter().flatten().collect()
    }

    /// Without a sustain knob: a decay falls to silence, else the level holds full.
    fn plateau(&self) -> bool {
        self.sustain.is_some() || (self.decay.is_none() && self.release.is_some())
    }
}

/// Values of an envelope: times as knob positions (0 to 1), the sustain
/// level (0 to 1) and the curves (−1 to 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnvValues {
    pub delay: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub curves: [f32; 3],
}

/// Where an envelope's points fall in its rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnvGeom {
    /// Width of one attack, decay or release slot (the most each can take).
    pub slot: f32,
    /// Start, end of delay, peak (end of attack), end of hold, end of decay,
    /// end of the sustain plateau, end of release.
    pub x: [f32; 7],
    pub top: f32,
    pub bottom: f32,
    /// Level of the sustain plateau, 0 to 1.
    pub sustain: f32,
}

/// What a handle moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Delay,
    Attack,
    Hold,
    /// The end of the decay: its time sideways, the sustain level up and down.
    Decay,
    /// The end of the plateau: the sustain level only.
    Sustain,
    Release,
    /// The bend of the attack (0), decay (1) or release (2) segment.
    Curve(usize),
}

impl EnvGeom {
    /// Lays out `v` in `r`: every slot takes its knob's position of its full
    /// width; `zoom` widens every slot (1: the longest envelope fills `r`).
    pub fn new(spec: &EnvSpec, v: &EnvValues, r: Rect, zoom: f32) -> Self {
        let full = [spec.attack, spec.decay, spec.release].iter().filter(|p| p.is_some()).count() as f32;
        let half = [spec.delay, spec.hold].iter().filter(|p| p.is_some()).count() as f32;
        let plateau = if spec.plateau() { PLATEAU } else { 0.0 };
        let slot = r.w / (full + HALF * half + plateau).max(1.0) * zoom.max(1.0);
        let on = |p: Option<&'static ParamInfo>, n: f32, w: f32| if p.is_some() { n.clamp(0.0, 1.0) * w } else { 0.0 };
        let mut x = [r.x; 7];
        x[1] = x[0] + on(spec.delay, v.delay, slot * HALF);
        x[2] = x[1] + on(spec.attack, v.attack, slot);
        x[3] = x[2] + on(spec.hold, v.hold, slot * HALF);
        x[4] = x[3] + on(spec.decay, v.decay, slot);
        x[5] = x[4] + slot * plateau;
        x[6] = x[5] + on(spec.release, v.release, slot);
        let sustain = if spec.sustain.is_some() { v.sustain.clamp(0.0, 1.0) } else if spec.decay.is_some() { 0.0 } else { 1.0 };
        Self { slot, x, top: r.y, bottom: r.bottom(), sustain }
    }

    /// The zoom that makes this layout (made at zoom 1) fill most of `w`.
    pub fn fit_zoom(&self, w: f32) -> f32 {
        (w * FILL / (self.x[6] - self.x[0]).max(1.0)).clamp(1.0, MAX_ZOOM)
    }

    pub fn y_of(&self, level: f32) -> f32 {
        self.bottom - level.clamp(0.0, 1.0) * (self.bottom - self.top)
    }

    pub fn level_at(&self, y: f32) -> f32 {
        ((self.bottom - y) / (self.bottom - self.top).max(1.0)).clamp(0.0, 1.0)
    }

    /// Knob position a handle dragged to `px` gives its time (0 to 1).
    pub fn time_at(&self, h: Handle, px: f32) -> f32 {
        let (from, w) = match h {
            Handle::Delay => (self.x[0], self.slot * HALF),
            Handle::Attack => (self.x[1], self.slot),
            Handle::Hold => (self.x[2], self.slot * HALF),
            Handle::Decay => (self.x[3], self.slot),
            Handle::Release => (self.x[5], self.slot),
            Handle::Sustain | Handle::Curve(_) => return 0.0,
        };
        ((px - from) / w.max(1.0)).clamp(0.0, 1.0)
    }

    /// The segments that bend (attack, decay, release): from, to, levels.
    fn segments(&self) -> [(f32, f32, f32, f32); 3] {
        [
            (self.x[1], self.x[2], 0.0, 1.0),
            (self.x[3], self.x[4], 1.0, self.sustain),
            (self.x[5], self.x[6], self.sustain, 0.0),
        ]
    }

    /// The outline, point by point (about every 2 px), with each segment's curve.
    pub fn outline(&self, curves: [f32; 3]) -> Vec<(f32, f32)> {
        let mut pts = vec![(self.x[0], self.y_of(0.0)), (self.x[1], self.y_of(0.0))];
        for (k, (a, b, from, to)) in self.segments().into_iter().enumerate() {
            let n = (((b - a) / 2.0).ceil() as usize).max(1);
            for i in 0..=n {
                let t = i as f32 / n as f32;
                pts.push((a + (b - a) * t, self.y_of(env_segment(from, to, t, curves[k]))));
            }
            if k == 0 {
                pts.push((self.x[3], self.y_of(1.0)));
            }
        }
        pts
    }

    /// Where the voice is: its stage, how far through it (0 to 1) and its level.
    pub fn voice_at(&self, stage: EnvStage, progress: f32, level: f32) -> Option<(f32, f32)> {
        let p = progress.clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| a + (b - a) * p;
        let x = match stage {
            EnvStage::Idle => return None,
            EnvStage::Delay => lerp(self.x[0], self.x[1]),
            EnvStage::Attack => lerp(self.x[1], self.x[2]),
            EnvStage::Hold => lerp(self.x[2], self.x[3]),
            EnvStage::Decay => lerp(self.x[3], self.x[4]),
            EnvStage::Sustain => (self.x[4] + self.x[5]) * 0.5,
            EnvStage::Release => lerp(self.x[5], self.x[6]),
        };
        Some((x, self.y_of(level)))
    }
}

impl App {
    /// The envelope's values; with `follow`, those a playing curve gives them.
    fn env_values(&self, device: Device, spec: &EnvSpec, follow: bool) -> EnvValues {
        let value = |p: &ParamInfo| {
            let live = if follow { self.live_auto_value(device_target(device, p.id)) } else { None };
            live.map_or_else(|| self.param_value(device, p), |v| p.clamp(v))
        };
        let norm = |p: Option<&'static ParamInfo>| p.map_or(0.0, |p| p.to_norm(value(p)));
        let curve = |p: Option<&'static ParamInfo>| p.map_or(0.0, value);
        EnvValues {
            delay: norm(spec.delay),
            attack: norm(spec.attack),
            hold: norm(spec.hold),
            decay: norm(spec.decay),
            sustain: spec.sustain.map_or(1.0, |p| value(p).clamp(0.0, 1.0)),
            release: norm(spec.release),
            curves: spec.curves.map(curve),
        }
    }

    /// The envelope editor in `r`; `live` shows the last played voice on it.
    pub(super) fn envelope_editor(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, spec: &EnvSpec, live: bool) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let g = Rect::new(r.x + 10.0, r.y + 10.0, r.w - 20.0, r.h - 24.0);
        let key = device_key(device);
        let base = spec.attack.or(spec.decay).or(spec.release).map_or(0, |p| p.id as usize);
        // Short envelopes zoom in; the zoom holds still while a point is dragged.
        let zid = wid("env-zoom", key, base);
        let busy = self.drag.is_some_and(|d| (0..9).any(|k| d.id == wid("env-h", key, base * 16 + k)));
        // While a curve plays the envelope's parameters, the graph follows it.
        let v = self.env_values(device, spec, !busy);
        let zoom = match self.anim.get(zid) {
            Some(z) if busy => z,
            _ => {
                let want = EnvGeom::new(spec, &v, g, 1.0).fit_zoom(g.w);
                self.anim.to(zid, want, super::theme::T_LAYOUT)
            }
        };
        let geom = EnvGeom::new(spec, &v, g, zoom);
        let clip = c.push_clip(r);
        let id_of = |h: Handle| {
            let k = match h {
                Handle::Delay => 0,
                Handle::Attack => 1,
                Handle::Hold => 2,
                Handle::Decay => 3,
                Handle::Sustain => 4,
                Handle::Release => 5,
                Handle::Curve(i) => 6 + i,
            };
            wid("env-h", key, base * 16 + k)
        };

        // Stage boundaries and their letters.
        let letters: [(Option<&'static ParamInfo>, &str, usize); 6] = [
            (spec.delay, "D", 0),
            (spec.attack, "A", 1),
            (spec.hold, "H", 2),
            (spec.decay, "D", 3),
            (spec.sustain, "S", 4),
            (spec.release, "R", 5),
        ];
        for (p, letter, k) in letters {
            if p.is_none() {
                continue;
            }
            let (a, b) = (geom.x[k], geom.x[k + 1]);
            if k > 0 {
                c.fill_rect(Rect::new(a.round(), g.y, 1.0, g.h), t.hairline.alpha(0.8));
            }
            if b - a >= 9.0 {
                self.text_center(c, self.f.sans_med, 9.0, (a + b) * 0.5, r.bottom() - 7.0, letter, t.muted.alpha(0.8));
            }
        }

        // The outline, filled.
        let pts = geom.outline(v.curves);
        let mut poly = pts.clone();
        poly.push((geom.x[6], g.bottom()));
        poly.push((g.x, g.bottom()));
        let mut fill = Path::new();
        fill.polygon(&poly);
        c.fill_path(&fill, t.accent.alpha(0.16));

        // What the pointer is on: a handle first, else a segment that bends.
        let handles: Vec<(Handle, f32, f32, Option<&'static ParamInfo>)> = [
            (Handle::Delay, geom.x[1], geom.y_of(0.0), spec.delay),
            (Handle::Attack, geom.x[2], geom.y_of(1.0), spec.attack),
            (Handle::Hold, geom.x[3], geom.y_of(1.0), spec.hold.filter(|_| v.hold > 0.0)),
            (Handle::Decay, geom.x[4], geom.y_of(geom.sustain), spec.decay),
            (Handle::Sustain, geom.x[5], geom.y_of(geom.sustain), spec.sustain.filter(|_| spec.plateau())),
            (Handle::Release, geom.x[6], geom.y_of(0.0), spec.release),
        ]
        .into_iter()
        .filter(|h| h.3.is_some())
        .collect();
        let dragging = handles.iter().map(|h| h.0).chain((0..3).map(Handle::Curve)).find(|&h| self.is_dragging(id_of(h)));
        let inside = input.inside && r.contains(input.x, input.y) && (self.drag.is_none() || dragging.is_some());
        let near = handles
            .iter()
            .filter(|h| (h.1 - input.x).hypot(h.2 - input.y) < HANDLE_HIT)
            .min_by(|a, b| (a.1 - input.x).hypot(a.2 - input.y).total_cmp(&(b.1 - input.x).hypot(b.2 - input.y)))
            .map(|h| h.0);
        let segs = geom.segments();
        let on_seg = (0..3).find(|&k| spec.curves[k].is_some() && input.x > segs[k].0 + 2.0 && input.x < segs[k].1 - 2.0);
        let hot = dragging.or(if inside { near.or(on_seg.map(Handle::Curve)) } else { None });

        if let Some(Handle::Curve(k)) = hot {
            let (a, b, from, to) = segs[k];
            let n = (((b - a) / 2.0).ceil() as usize).max(1);
            let seg: Vec<(f32, f32)> = (0..=n).map(|i| i as f32 / n as f32).map(|s| (a + (b - a) * s, geom.y_of(env_segment(from, to, s, v.curves[k])))).collect();
            c.fill_rect(Rect::new(a, g.y, b - a, g.h), t.select.alpha(0.06));
            c.stroke_polyline(&seg, 3.0, t.select.alpha(0.45));
        }
        c.stroke_polyline(&pts, 1.8, t.accent);

        // Drags.
        if inside && input.pressed && self.drag.is_none() {
            if let Some(h) = hot {
                let id = id_of(h);
                if input.double {
                    self.env_reset(device, spec, h);
                } else {
                    let v0 = if let Handle::Curve(k) = h { v.curves[k] } else { 0.0 };
                    self.begin_drag(id, input, v0);
                }
            }
        }
        if inside && input.right_pressed && self.drag.is_none() {
            let target = hot.and_then(|h| self.env_param(spec, h, input.mods.shift)).map(|p| (device_target(device, p.id), p));
            if let Some((target, p)) = target {
                let value = self.param_value(device, p);
                self.open_popup(Popup::Param { target, value, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
            }
        }
        if let Some(h) = dragging {
            self.env_drag(input, device, spec, &geom, h);
        }

        // Handles.
        for &(h, hx, hy, _) in &handles {
            let id = id_of(h);
            let on = hot == Some(h);
            let hv = self.hover_anim(id, on);
            let rad = HANDLE_R + 1.5 * hv;
            c.fill_circle(hx, hy, rad, t.bg_deep);
            c.stroke_circle(hx, hy, rad, 1.6, t.text2.mix(t.select, hv));
        }

        // The last played voice.
        if live
            && let Device::Instrument(ch) = device
            && let Some(probe) = self.editors.env_probe(ch)
        {
            let (stage, progress, level) = probe.read();
            if let Some((x, y)) = geom.voice_at(stage, progress, level) {
                c.fill_circle(x, y, 4.0, t.text);
                c.stroke_circle(x, y, 4.0, 1.2, t.accent);
            }
        }

        c.set_clip(Some(clip));

        // Readout and tip.
        if let Some(h) = hot.filter(|_| inside || dragging.is_some()) {
            if let Some(p) = self.env_param(spec, h, input.mods.shift) {
                let value = self.param_value(device, p);
                let text = format!("{} {}", tr(p.name), p.format(value));
                let tw = self.fonts.measure(self.f.mono_med, 10.0, &text) + 12.0;
                let tag = Rect::new((r.right() - tw - 6.0).max(r.x + 4.0), r.y + 4.0, tw, 16.0);
                c.fill_rrect(tag, 5.0, t.bg_deep.alpha(0.85));
                self.text(c, self.f.mono_med, 10.0, tag.x + 6.0, tag.y + 8.0, &text, t.text);
            }
            self.set_cursor(match h {
                Handle::Curve(_) | Handle::Sustain => Cursor::ResizeV,
                Handle::Decay => Cursor::Hand,
                _ => Cursor::ResizeH,
            });
            if dragging.is_none() {
                let name = self.env_param(spec, h, false).map_or("", |p| tr(p.name));
                let how = match h {
                    Handle::Curve(_) => tr("Drag up or down to bend it"),
                    Handle::Decay => tr("Sideways: decay time · up and down: sustain level"),
                    Handle::Sustain => tr("Up and down: the level held"),
                    Handle::Attack if spec.hold.is_some() => tr("Drag: its time · Shift+drag: hold"),
                    _ => tr("Drag: its time"),
                };
                self.set_tip(&tr_fmt!("{}\n{} · double-click: default · right-click: automate, type", name, how), "");
            }
        } else if inside {
            self.set_tip(tr("Envelope\nDrag a point: its time or level · drag a segment: its curve · right-click: automate"), "");
        }
    }

    /// The parameter a handle moves (Shift on the attack: the hold).
    fn env_param(&self, spec: &EnvSpec, h: Handle, shift: bool) -> Option<&'static ParamInfo> {
        match h {
            Handle::Delay => spec.delay,
            Handle::Attack if shift && spec.hold.is_some() => spec.hold,
            Handle::Attack => spec.attack,
            Handle::Hold => spec.hold,
            Handle::Decay => spec.decay,
            Handle::Sustain => spec.sustain,
            Handle::Release => spec.release,
            Handle::Curve(k) => spec.curves[k],
        }
    }

    fn env_drag(&mut self, input: &Input, device: Device, spec: &EnvSpec, geom: &EnvGeom, h: Handle) {
        let set = |app: &mut App, p: Option<&'static ParamInfo>, v: f32| {
            if let Some(p) = p {
                app.set_param_value(device, p, v);
            }
        };
        match h {
            Handle::Curve(k) => {
                let Some(d) = self.drag else { return };
                let v = (d.v0 + (d.y0 - input.y) / CURVE_DRAG).clamp(-1.0, 1.0);
                set(self, spec.curves[k], (v * 100.0).round() / 100.0);
            }
            Handle::Sustain => set(self, spec.sustain, geom.level_at(input.y)),
            Handle::Decay => {
                set(self, spec.decay, spec.decay.map_or(0.0, |p| p.from_norm(geom.time_at(h, input.x))));
                set(self, spec.sustain, geom.level_at(input.y));
            }
            Handle::Attack if input.mods.shift && spec.hold.is_some() => {
                let n = ((input.x - geom.x[2]) / (geom.slot * HALF).max(1.0)).clamp(0.0, 1.0);
                set(self, spec.hold, spec.hold.map_or(0.0, |p| p.from_norm(n)));
            }
            _ => {
                let p = self.env_param(spec, h, false);
                set(self, p, p.map_or(0.0, |p| p.from_norm(geom.time_at(h, input.x))));
            }
        }
    }

    /// Double-click on a handle: its parameters back to their defaults.
    fn env_reset(&mut self, device: Device, spec: &EnvSpec, h: Handle) {
        let ps: Vec<&'static ParamInfo> = match h {
            Handle::Decay => [spec.decay, spec.sustain].into_iter().flatten().collect(),
            h => self.env_param(spec, h, false).into_iter().collect(),
        };
        for p in ps {
            self.set_param_value(device, p, p.default);
        }
    }

    /// `PONZIK_ENVELOPE=shaped`: every envelope of the selected channel's
    /// instrument gets a delay, a hold and bent segments, for a capture.
    pub fn screenshot_envelope(&mut self, spec: &str) {
        let Some(ch) = self.piano_channel() else { return };
        if spec.trim() != "shaped" {
            return;
        }
        let kind = self.project.channels[ch].instrument.kind.clone();
        for p in super::devices::params_of(&kind, true) {
            let v = match p.name {
                "Delay" => 0.08,
                "Hold" => 0.12,
                "Attack curve" => 0.6,
                "Decay curve" => -0.7,
                "Release curve" => -0.5,
                _ => continue,
            };
            self.project.channels[ch].instrument.set(p.id, p.clamp(v));
        }
        self.commit(tr("Envelope"));
    }

    /// The parameters of `device` an envelope editor shows, for tests and captures.
    #[cfg(test)]
    pub(super) fn env_target(device: Device, p: &ParamInfo) -> ponzik_engine::AutoTarget {
        device_target(device, p.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::registry;

    fn params(kind: &str) -> &'static [ParamInfo] {
        registry::create_instrument(kind).map(|i| i.params()).expect("instrument")
    }

    fn group(kind: &str, name: &str) -> Vec<&'static ParamInfo> {
        params(kind).iter().filter(|p| p.group == name).collect()
    }

    fn spec(kind: &str, name: &str) -> Option<EnvSpec> {
        EnvSpec::find(name, &group(kind, name))
    }

    #[test]
    fn every_native_envelope_is_found() {
        for (kind, g) in [("pulse", "Amp"), ("wavetable", "Amp"), ("wavetable", "Filter Envelope"), ("sampler", "Amp"), ("granular", "Amp")] {
            let s = spec(kind, g).unwrap_or_else(|| panic!("{kind} {g}"));
            assert!(s.delay.is_some() && s.attack.is_some() && s.hold.is_some() && s.decay.is_some(), "{kind} {g}");
            assert!(s.sustain.is_some() && s.release.is_some() && s.curves.iter().all(Option::is_some), "{kind} {g}");
            assert_eq!(s.ordered().iter().map(|p| p.name).collect::<Vec<_>>(), ["Delay", "Attack", "Hold", "Decay", "Sustain", "Release"]);
        }
        // Partial ones: attack and release of a bow, decay of a kick.
        let bow = spec("strings", "Bow").expect("strings");
        assert!(bow.attack.is_some() && bow.release.is_some() && bow.decay.is_none());
        assert!(spec("kick", "Amp").is_some_and(|s| s.decay.is_some() && s.attack.is_none()));
        // Not envelopes: a string's decay, a 303 filter decay alone.
        assert!(spec("pluck", "String").is_none());
        assert!(spec("bass303", "Filter").is_none());
    }

    fn pulse_amp() -> EnvSpec {
        spec("pulse", "Amp").expect("pulse")
    }

    #[test]
    fn layout_fits_and_maps_back() {
        let s = pulse_amp();
        let r = Rect::new(10.0, 20.0, 400.0, 80.0);
        let v = EnvValues { delay: 0.25, attack: 0.4, hold: 0.5, decay: 0.6, sustain: 0.7, release: 0.8, curves: [0.0; 3] };
        let g = EnvGeom::new(&s, &v, r, 1.0);
        // Points in order, all inside; full knobs fill the width.
        assert!(g.x.windows(2).all(|w| w[0] <= w[1]));
        let full = EnvGeom::new(&s, &EnvValues { delay: 1.0, attack: 1.0, hold: 1.0, decay: 1.0, release: 1.0, ..v }, r, 1.0);
        assert!((full.x[6] - r.right()).abs() < 0.01, "{:?}", full.x);
        // Dragging a handle to where it is gives back its knob position.
        for (h, x, n) in [(Handle::Delay, g.x[1], 0.25), (Handle::Attack, g.x[2], 0.4), (Handle::Hold, g.x[3], 0.5), (Handle::Decay, g.x[4], 0.6), (Handle::Release, g.x[6], 0.8)] {
            assert!((g.time_at(h, x) - n).abs() < 1e-4, "{h:?}");
        }
        // Zoomed in, a handle still maps back to its knob; a short envelope zooms to fill the graph.
        let z = EnvGeom::new(&s, &v, r, 2.5);
        assert!((z.time_at(Handle::Attack, z.x[2]) - 0.4).abs() < 1e-4);
        let short = EnvGeom::new(&s, &EnvValues { attack: 0.1, decay: 0.2, release: 0.2, ..EnvValues::default() }, r, 1.0);
        let zoom = short.fit_zoom(r.w);
        assert!(zoom > 1.0 && zoom <= MAX_ZOOM);
        let fitted = EnvGeom::new(&s, &EnvValues { attack: 0.1, decay: 0.2, release: 0.2, ..EnvValues::default() }, r, zoom);
        assert!(fitted.x[6] <= r.right() + 0.01 && fitted.x[6] - r.x > r.w * 0.5);
        assert!((g.level_at(g.y_of(0.7)) - 0.7).abs() < 1e-4);
        assert_eq!(g.y_of(1.0), r.y);
        assert_eq!(g.y_of(0.0), r.bottom());
    }

    #[test]
    fn drag_sets_parameter_values() {
        let s = pulse_amp();
        let r = Rect::new(0.0, 0.0, 300.0, 60.0);
        let v = EnvValues { sustain: 0.5, ..EnvValues::default() };
        let g = EnvGeom::new(&s, &v, r, 1.0);
        let attack = s.attack.unwrap();
        // Half the attack slot: the knob's middle (a log knob: the geometric middle).
        let n = g.time_at(Handle::Attack, g.x[1] + g.slot * 0.5);
        assert!((n - 0.5).abs() < 1e-4);
        let secs = attack.from_norm(n);
        assert!((secs - (attack.min * attack.max).sqrt()).abs() < 1e-3, "{secs}");
        // Past the slot: the longest; before it: the shortest.
        assert_eq!(attack.from_norm(g.time_at(Handle::Attack, 1e4)), attack.max);
        assert_eq!(attack.from_norm(g.time_at(Handle::Attack, -1e4)), attack.min);
        // A quarter from the top: 75 % sustain.
        assert!((g.level_at(r.y + r.h * 0.25) - 0.75).abs() < 1e-4);
    }

    #[test]
    fn outline_follows_the_curves() {
        let s = pulse_amp();
        let r = Rect::new(0.0, 0.0, 400.0, 100.0);
        let v = EnvValues { attack: 0.6, decay: 0.6, sustain: 0.4, release: 0.6, ..EnvValues::default() };
        let straight = EnvGeom::new(&s, &v, r, 1.0);
        let mid_attack = |curve: f32| {
            let pts = straight.outline([curve, 0.0, 0.0]);
            let x = (straight.x[1] + straight.x[2]) * 0.5;
            pts.iter().min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs())).map(|p| straight.level_at(p.1)).unwrap()
        };
        // Straight at 0, above the line when bent up, below when bent down.
        assert!((mid_attack(0.0) - 0.5).abs() < 0.03);
        assert!(mid_attack(0.8) > 0.6);
        assert!(mid_attack(-0.8) < 0.4);
        // Starts silent, ends silent, sustains at its level.
        let pts = straight.outline([0.0; 3]);
        assert_eq!(straight.level_at(pts[0].1), 0.0);
        assert!(straight.level_at(pts.last().unwrap().1) < 1e-4);
        assert!(pts.iter().any(|p| (straight.level_at(p.1) - 0.4).abs() < 1e-4));
    }

    #[test]
    fn the_voice_is_drawn_on_its_stage() {
        let s = pulse_amp();
        let g = EnvGeom::new(&s, &EnvValues { attack: 0.5, decay: 0.5, sustain: 0.5, release: 0.5, ..Default::default() }, Rect::new(0.0, 0.0, 300.0, 50.0), 1.0);
        assert_eq!(g.voice_at(EnvStage::Idle, 0.0, 0.0), None);
        let (x, y) = g.voice_at(EnvStage::Attack, 0.5, 0.5).unwrap();
        assert!((x - (g.x[1] + g.x[2]) * 0.5).abs() < 1e-3 && (y - 25.0).abs() < 1e-3);
        let (x, _) = g.voice_at(EnvStage::Release, 1.0, 0.0).unwrap();
        assert_eq!(x, g.x[6]);
    }

    #[test]
    fn handles_move_regular_parameters() {
        let s = pulse_amp();
        let device = Device::Instrument(2);
        // Each handle moves the instrument parameter of its name: automation targets like its knob.
        for (h, name) in [(Handle::Delay, "Delay"), (Handle::Attack, "Attack"), (Handle::Hold, "Hold"), (Handle::Decay, "Decay"), (Handle::Sustain, "Sustain"), (Handle::Release, "Release"), (Handle::Curve(0), "Attack curve"), (Handle::Curve(1), "Decay curve"), (Handle::Curve(2), "Release curve")] {
            let p = match h {
                Handle::Delay => s.delay,
                Handle::Attack => s.attack,
                Handle::Hold => s.hold,
                Handle::Decay => s.decay,
                Handle::Sustain => s.sustain,
                Handle::Release => s.release,
                Handle::Curve(k) => s.curves[k],
            }
            .unwrap();
            assert_eq!(p.name, name);
            assert_eq!(App::env_target(device, p), ponzik_engine::AutoTarget::InstrumentParam { channel: 2, id: p.id });
        }
        assert!(s.graph_only(s.curves[1].unwrap()) && !s.graph_only(s.attack.unwrap()));
    }
}
