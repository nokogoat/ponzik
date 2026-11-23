//! Visual of Ponzik Wave: the frames of the selected table, stacked in depth
//! from the front (position 0) to the back (position 1), and in front of
//! them the wave the oscillator plays, the crossfade of the two frames around
//! the position. The frames are the engine's own band-limited tables, computed
//! once per table; the stack's screen points are kept until the table, the
//! position or the card moves. Dragging sideways moves the position.

use std::cell::RefCell;
use std::collections::HashMap;

use ponzik_engine::ParamInfo;
use ponzik_engine::instruments::{WAVE_POSITION, WAVE_SET, wave_table_frames};
use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::Device;
use super::visuals::device_key;
use super::widgets::wid;

/// Width of the picture in the device card.
pub const WAVE_W: f32 = 300.0;

/// Points per period drawn.
const POINTS: usize = 128;
/// Size of one frame against the picture: the rest is the depth of the stack.
const FRAME_W: f32 = 0.62;
const FRAME_H: f32 = 0.42;

type Pt = (f32, f32);

/// What a card draws, kept between frames.
#[derive(Default)]
struct WaveView {
    set: Option<usize>,
    frames: Vec<Vec<f32>>,
    /// Table, position and rectangle the points below were laid out for.
    layout: Option<(usize, u32, [u32; 4])>,
    stack: Vec<Vec<Pt>>,
    played: Vec<Pt>,
    fill: Path,
    /// Zero line of the front and back frames, and of the played wave.
    spine: [Pt; 3],
}

thread_local! {
    static VIEWS: RefCell<HashMap<usize, WaveView>> = RefCell::new(HashMap::new());
}

/// Where a frame at `depth` (0 = front, 1 = back) sits: its left edge and zero line.
fn origin(g: Rect, depth: f32) -> Pt {
    let (fw, fh) = (g.w * FRAME_W, g.h * FRAME_H);
    (g.x + depth * (g.w - fw), g.bottom() - 0.5 * fh - depth * (g.h - fh))
}

/// Screen points of one period of `wave` with its zero line at `o`.
fn lay_out(out: &mut Vec<Pt>, g: Rect, o: Pt, wave: impl Iterator<Item = f32>) {
    let (fw, amp) = (g.w * FRAME_W, 0.5 * g.h * FRAME_H * 0.92);
    out.clear();
    out.extend(wave.enumerate().map(|(i, v)| (o.0 + i as f32 / POINTS as f32 * fw, o.1 - v * amp)));
}

impl WaveView {
    fn update(&mut self, set: usize, position: f32, g: Rect) {
        if self.set != Some(set) {
            self.frames = wave_table_frames(set, POINTS);
            self.set = Some(set);
            self.layout = None;
        }
        let key = (set, position.to_bits(), [g.x.to_bits(), g.y.to_bits(), g.w.to_bits(), g.h.to_bits()]);
        if self.layout == Some(key) || self.frames.is_empty() {
            return;
        }
        self.layout = Some(key);
        let n = self.frames.len();
        let depth = |f: usize| f as f32 / (n - 1).max(1) as f32;
        self.stack.resize_with(n, Vec::new);
        for (f, (pts, wave)) in self.stack.iter_mut().zip(&self.frames).enumerate() {
            // One whole period: the last point closes on the first.
            lay_out(pts, g, origin(g, depth(f)), wave.iter().chain(&wave[..1]).copied());
        }
        // The oscillator crossfades the two frames around the position.
        let at = position.clamp(0.0, 1.0) * (n - 1) as f32;
        let a = (at as usize).min(n - 1);
        let b = (a + 1).min(n - 1);
        let mix = at - a as f32;
        let (wa, wb) = (&self.frames[a], &self.frames[b]);
        let o = origin(g, position.clamp(0.0, 1.0));
        let wave = (0..=POINTS).map(|i| {
            let i = i % POINTS;
            wa[i] + (wb[i] - wa[i]) * mix
        });
        lay_out(&mut self.played, g, o, wave);
        self.fill.clear();
        let mut area = self.played.clone();
        area.push((self.played[self.played.len() - 1].0, o.1));
        area.push((o.0, o.1));
        self.fill.polygon(&area);
        self.spine = [origin(g, 0.0), origin(g, 1.0), o];
    }
}

impl App {
    pub(super) fn wavetable_visual(&mut self, c: &mut Canvas, input: &Input, r: Rect, device: Device, params: &'static [ParamInfo]) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let find = |id: u32| params.iter().find(|p| p.id == id);
        let (Some(set_p), Some(pos_p)) = (find(WAVE_SET), find(WAVE_POSITION)) else { return };
        let g = r.inset_xy(16.0, 16.0);

        let id = wid("wavetable", device_key(device), 0);
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            let v0 = self.param_value(device, pos_p);
            self.begin_drag(id, input, v0);
            self.drag = self.drag.map(|mut d| {
                d.y0 = input.x;
                d
            });
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let v = d.v0 + (input.x - d.y0) / (g.w * (1.0 - FRAME_W)).max(1.0);
            self.set_param_value(device, pos_p, v.clamp(pos_p.min, pos_p.max));
        }
        if hov {
            self.set_tip(tr("The table's frames, front to back, and in front the wave played\nDrag sideways: position"), "");
        }

        let set = self.param_value(device, set_p).round().max(0.0) as usize;
        let position = self.param_value(device, pos_p);
        let near = self.hover_anim(id, hov || self.is_dragging(id));
        VIEWS.with(|views| {
            let mut views = views.borrow_mut();
            let view = views.entry(device_key(device)).or_default();
            view.update(set, position, g);
            if view.stack.is_empty() {
                return;
            }
            // Depth axis through the frames' zero lines, the position on it.
            let [front, back, at] = view.spine;
            c.stroke_polyline(&[front, back], 1.0, t.hairline.alpha(1.6));
            // Back to front, so nearer frames cover farther ones.
            let n = view.stack.len();
            let at_frame = position.clamp(0.0, 1.0) * (n - 1) as f32;
            for (f, pts) in view.stack.iter().enumerate().rev() {
                let close = (1.0 - (f as f32 - at_frame).abs()).max(0.0);
                c.stroke_polyline(pts, 1.0, t.text2.alpha(0.22 + 0.3 * close));
            }
            c.fill_path(&view.fill, t.accent.alpha(0.12 + 0.04 * near));
            c.stroke_polyline(&[(at.0, at.1), (at.0 + g.w * FRAME_W, at.1)], 1.0, t.accent.alpha(0.35));
            c.stroke_polyline(&view.played, 1.6, t.accent);
            c.fill_circle(at.0, at.1, 3.0 + near, t.accent);
        });
    }
}
