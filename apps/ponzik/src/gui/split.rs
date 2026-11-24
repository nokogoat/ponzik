//! Resizable layout: the sizes of the docked areas (browser, device panel,
//! the piano roll's lanes, the playlist's pattern list, the Channel Rack's
//! names), their zooms (key and track heights), the splitters that change
//! them and their place in the settings file.
//!
//! A splitter is a thin zone between two areas: hovered, it shows a grip and
//! the resize cursor; dragged, the size follows the pointer at once, bounded;
//! dragged under half its minimum, a foldable area snaps shut and leaves a
//! small tab to open it again; double-clicked, it goes back to its default.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::json::Json;
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, View};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Bounds of a resizable area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Dragged near zero, the area folds away.
    pub foldable: bool,
}

impl Span {
    pub const fn new(min: f32, max: f32, default: f32, foldable: bool) -> Self {
        Self { min, max, default, foldable }
    }

    /// A size kept inside the bounds (a window too small for the minimum gets the minimum).
    pub fn fit(&self, v: f32) -> f32 {
        if v.is_finite() { v.clamp(self.min, self.max.max(self.min)) } else { self.default.clamp(self.min, self.max.max(self.min)) }
    }

    /// Size for a pointer asking for `raw` pixels: bounded and whole; 0 (folded)
    /// when a foldable area is pulled under half its minimum.
    pub fn drag(&self, raw: f32) -> f32 {
        if self.foldable && raw < self.min * 0.5 {
            return 0.0;
        }
        self.fit(raw).round()
    }

    /// The same bounds with another maximum (the window decides).
    pub fn up_to(self, max: f32) -> Self {
        Self { max, ..self }
    }
}

pub const SIDEBAR_SPAN: Span = Span::new(190.0, 460.0, SIDEBAR, true);
pub const PANEL_SPAN: Span = Span::new(180.0, 620.0, PANEL, true);
/// The device panel shrunk to its header: what is edited and its chain of
/// devices, the cards hidden (a stop between folded and the cards' height).
pub const PANEL_PEEK: f32 = 46.0;
/// Velocity and note lanes under the piano roll, together.
pub const LANES_SPAN: Span = Span::new(56.0, 420.0, 148.0, true);
/// Height of a key of the piano roll.
pub const KEY_SPAN: Span = Span::new(8.0, 36.0, 16.0, false);
/// Height of a playlist track.
pub const TRACK_SPAN: Span = Span::new(32.0, 140.0, 52.0, false);
/// The playlist's list of patterns.
pub const PATTERNS_SPAN: Span = Span::new(120.0, 360.0, 196.0, true);
/// The Channel Rack's name column.
pub const RACK_SPAN: Span = Span::new(200.0, 400.0, 252.0, false);
/// Half the thickness of a splitter's hit zone.
pub const GRIP: f32 = 4.0;
/// The piano roll keeps at least this much for its notes.
pub const NOTES_MIN: f32 = 120.0;

/// Device panel height that fits the window when none was chosen for a view.
/// On a short screen, the editors that need height (piano roll, mixer, Lab)
/// show only the panel's header: the notes get the room, a drag brings the
/// cards back. Cards are never cut in half by default.
pub fn auto_panel(view: View, avail: f32) -> f32 {
    let short = match view {
        View::Piano | View::Mixer | View::Lab | View::Session => 860.0,
        _ => 560.0,
    };
    if avail < short { PANEL_PEEK } else { PANEL }
}

/// Device panel height for a pointer asking for `raw` pixels: folded, the
/// header alone, or the cards (bounded by `span`).
pub fn panel_drag(span: Span, raw: f32) -> f32 {
    if raw < PANEL_PEEK * 0.5 {
        0.0
    } else if raw < (PANEL_PEEK + span.min) * 0.5 {
        PANEL_PEEK
    } else {
        span.fit(raw).round()
    }
}

/// A saved or computed panel height kept valid: the header stop, or within `span`.
pub fn panel_fit(span: Span, h: f32) -> f32 {
    if h.is_finite() && h <= PANEL_PEEK + 1.0 { PANEL_PEEK } else { span.fit(h) }
}

/// Height of the lanes under the piano roll when none was chosen: about a
/// third of the editor, never more than the default.
pub fn auto_lanes(body: f32) -> f32 {
    (body * 0.32).clamp(LANES_SPAN.min, LANES_SPAN.default).round()
}

/// Velocity and note lane heights in `total` (gap of 8 between them).
/// Short, only the velocities show.
pub fn lane_split(total: f32) -> (f32, f32) {
    if total < 130.0 {
        return (total, 0.0);
    }
    // The note lane carries its row of chips (`lanes::CHIPS_H`) above its curve.
    let lane = ((total - 8.0) * 0.55).round().max(64.0);
    (total - 8.0 - lane, lane)
}

/// Sizes of the interface, saved in the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Sizes {
    pub sidebar_w: f32,
    /// Device panel height chosen in each view (`View::ALL` order); `None`: fits the window.
    pub panel_h: [Option<f32>; 6],
    /// Lanes under the piano roll; `None`: fits the editor.
    pub lanes_h: Option<f32>,
    pub lanes_open: bool,
    pub key_h: f32,
    pub track_h: f32,
    pub patterns_w: f32,
    pub patterns_open: bool,
    pub rack_w: f32,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            sidebar_w: SIDEBAR_SPAN.default,
            panel_h: [None; 6],
            lanes_h: None,
            lanes_open: true,
            key_h: KEY_SPAN.default,
            track_h: TRACK_SPAN.default,
            patterns_w: PATTERNS_SPAN.default,
            patterns_open: true,
            rack_w: RACK_SPAN.default,
        }
    }
}

fn view_key(v: View) -> &'static str {
    match v {
        View::Rack => "rack",
        View::Piano => "piano",
        View::Playlist => "playlist",
        View::Session => "session",
        View::Mixer => "mixer",
        View::Lab => "lab",
    }
}

impl Sizes {
    pub fn panel(&self, v: View) -> Option<f32> {
        self.panel_h[v.index()]
    }

    pub fn set_panel(&mut self, v: View, h: Option<f32>) {
        self.panel_h[v.index()] = h;
    }

    pub fn to_json(&self) -> Json {
        let panel = View::ALL
            .iter()
            .filter_map(|&v| self.panel(v).map(|h| (view_key(v).to_string(), Json::from(h as f64))))
            .collect();
        let mut f: Vec<(String, Json)> = vec![
            ("sidebar".into(), (self.sidebar_w as f64).into()),
            ("panel".into(), Json::Obj(panel)),
            ("piano_lanes_open".into(), self.lanes_open.into()),
            ("key_height".into(), (self.key_h as f64).into()),
            ("track_height".into(), (self.track_h as f64).into()),
            ("patterns".into(), (self.patterns_w as f64).into()),
            ("patterns_open".into(), self.patterns_open.into()),
            ("rack_names".into(), (self.rack_w as f64).into()),
        ];
        if let Some(h) = self.lanes_h {
            f.push(("piano_lanes".into(), (h as f64).into()));
        }
        Json::Obj(f)
    }

    /// Tolerant reading: what is missing or out of bounds keeps or meets its bounds.
    pub fn from_json(j: Option<&Json>) -> Self {
        let mut s = Self::default();
        let Some(j) = j else { return s };
        let num = |k: &str| j.get(k).and_then(Json::as_f64).map(|v| v as f32).filter(|v| v.is_finite());
        let flag = |k: &str| j.get(k).and_then(Json::as_bool);
        if let Some(v) = num("sidebar") {
            s.sidebar_w = SIDEBAR_SPAN.fit(v);
        }
        for v in View::ALL {
            let h = j.get("panel").and_then(|p| p.get(view_key(v))).and_then(Json::as_f64).map(|h| h as f32).filter(|h| h.is_finite());
            s.set_panel(v, h.map(|h| panel_fit(PANEL_SPAN, h)));
        }
        s.lanes_h = num("piano_lanes").map(|v| LANES_SPAN.fit(v));
        s.lanes_open = flag("piano_lanes_open").unwrap_or(true);
        s.key_h = num("key_height").map_or(s.key_h, |v| KEY_SPAN.fit(v).round());
        s.track_h = num("track_height").map_or(s.track_h, |v| TRACK_SPAN.fit(v).round());
        s.patterns_w = num("patterns").map_or(s.patterns_w, |v| PATTERNS_SPAN.fit(v));
        s.patterns_open = flag("patterns_open").unwrap_or(true);
        s.rack_w = num("rack_names").map_or(s.rack_w, |v| RACK_SPAN.fit(v));
        s
    }
}

/// Which toolbar items fit in `room`. Items are (width with its gap, rank):
/// rank 0 always stays; higher ranks fold first, the later one first among
/// equals. Once anything folds, `more` pixels are kept for the "More" button.
pub fn fit_toolbar(room: f32, items: &[(f32, u8)], more: f32) -> Vec<bool> {
    let mut shown = vec![true; items.len()];
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| items[i].1 > 0).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(items[i].1), std::cmp::Reverse(i)));
    for i in order {
        let used: f32 = items.iter().zip(&shown).filter(|(_, s)| **s).map(|(it, _)| it.0).sum();
        let folded = shown.iter().any(|s| !s);
        if used + if folded { more } else { 0.0 } <= room {
            break;
        }
        shown[i] = false;
    }
    shown
}

/// What a splitter asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Split {
    Size(f32),
    /// Pulled to the edge: fold the area.
    Fold,
    /// Double-clicked: the default size.
    Reset,
}

/// Drag id of the splitter named `key`.
pub fn splitter_id(key: &str) -> u64 {
    wid("splitter", key.len(), key.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)))
}

/// Direction of a splitter's bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// A vertical bar between side-by-side areas: drags left and right.
    Columns,
    /// A horizontal bar between stacked areas: drags up and down.
    Rows,
}

impl App {
    /// Maximize: the view takes the window (browser and devices hidden); again
    /// (or opening one of them), it gives the room back.
    pub fn toggle_maximize(&mut self) {
        match self.maximized.take() {
            Some((browser, panel)) => {
                (self.sidebar, self.panel) = (browser, panel);
                self.toast(tr("View restored"));
            }
            None => {
                self.maximized = Some((self.sidebar, self.panel));
                (self.sidebar, self.panel) = (false, false);
                let k = self.key_label(super::keymap::Shortcut::Maximize);
                let msg = if k.is_empty() { tr("View maximized").to_string() } else { tr_fmt!("View maximized · {} to restore", k) };
                self.toast(&msg);
            }
        }
    }

    /// Captures: `sidebar:200,panel:180,lanes:60,key:12,track:40,patterns:150,rack:220`
    /// (sizes; the panel's in the current view), `fold:sidebar|panel|lanes|patterns`, `max`.
    pub fn screenshot_layout(&mut self, spec: &str) {
        for part in spec.split(',').map(str::trim) {
            let (k, v) = part.split_once(':').unwrap_or((part, ""));
            let n = v.parse::<f32>().ok();
            let s = &mut self.layout.sizes;
            match (k, n) {
                ("sidebar", Some(n)) => s.sidebar_w = SIDEBAR_SPAN.fit(n),
                ("panel", Some(n)) => s.set_panel(self.view, Some(panel_fit(PANEL_SPAN, n))),
                ("lanes", Some(n)) => s.lanes_h = Some(LANES_SPAN.fit(n)),
                ("key", Some(n)) => s.key_h = KEY_SPAN.fit(n),
                ("track", Some(n)) => s.track_h = TRACK_SPAN.fit(n),
                ("patterns", Some(n)) => s.patterns_w = PATTERNS_SPAN.fit(n),
                ("rack", Some(n)) => s.rack_w = RACK_SPAN.fit(n),
                ("fold", _) => match v {
                    "sidebar" => self.sidebar = false,
                    "panel" => self.panel = false,
                    "lanes" => s.lanes_open = false,
                    "patterns" => s.patterns_open = false,
                    _ => {}
                },
                ("max", _) => self.toggle_maximize(),
                _ => {}
            }
        }
    }

    /// A splitter on `zone`. `size` is the area's size now; `grow` is +1 when
    /// moving right (or down) makes it bigger, -1 otherwise.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn splitter(&mut self, c: &mut Canvas, input: &Input, key: &str, zone: Rect, axis: Axis, size: f32, grow: f32, span: Span, tip: &str) -> Option<Split> {
        let t = self.theme;
        let id = splitter_id(key);
        let hov = self.hovered(input, id, zone);
        let dragging = self.is_dragging(id);
        let mut out = None;
        if hov && input.pressed {
            if input.double {
                out = Some(Split::Reset);
            } else {
                self.begin_drag(id, input, size);
            }
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let delta = if axis == Axis::Columns { input.x - d.x0 } else { input.y - d.y0 };
            let v = span.drag(d.v0 + grow * delta);
            if v == 0.0 {
                out = Some(Split::Fold);
            } else if v != size {
                out = Some(Split::Size(v));
            }
        }
        if hov || dragging {
            self.set_cursor(if axis == Axis::Columns { Cursor::ResizeH } else { Cursor::ResizeV });
        }
        if hov && !dragging {
            let fold = if span.foldable { tr("\nDrag to the edge: fold") } else { "" };
            self.set_tip(&tr_fmt!("{}\nDouble-click: default size{}", tip, fold), "");
        }
        let hv = self.hover_anim(id, hov || dragging);
        if hv > 0.01 {
            draw_grip(c, zone, axis, t.accent, hv);
        }
        out
    }

    /// The splitter `key` was let go this frame: time to save.
    pub(super) fn splitter_released(&self, input: &Input, key: &str) -> bool {
        !input.down && self.is_dragging(splitter_id(key))
    }

    /// The tab a folded area leaves on its edge: a click opens it again.
    /// `toward`: the chevron's direction (where the area comes from).
    pub(super) fn fold_tab(&mut self, c: &mut Canvas, input: &Input, key: &str, r: Rect, toward: Icon, tip: &str) -> bool {
        let t = self.theme;
        let id = wid("fold-tab", key.len(), key.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)));
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let tab = r.inset(-2.0 * hv);
        c.fill_rrect(tab, 7.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(tab, 7.0, 1.0, t.hairline);
        let (cx, cy) = tab.center();
        draw_icon(c, toward, cx, cy, 10.0 + 2.0 * hv, t.muted.mix(t.accent, hv));
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }
}

/// The grip of a splitter: a hairline along it and a pill at its middle.
pub fn draw_grip(c: &mut Canvas, zone: Rect, axis: Axis, col: ponzik_ui::Color, k: f32) {
    let (cx, cy) = zone.center();
    match axis {
        Axis::Columns => {
            c.fill_rect(Rect::new(cx - 1.0, zone.y, 2.0, zone.h), col.alpha(0.35 * k));
            c.fill_rrect(Rect::new(cx - 2.5, cy - 18.0, 5.0, 36.0), 2.5, col.alpha(k));
        }
        Axis::Rows => {
            c.fill_rect(Rect::new(zone.x, cy - 1.0, zone.w, 2.0), col.alpha(0.35 * k));
            c.fill_rrect(Rect::new(cx - 18.0, cy - 2.5, 36.0, 5.0), 2.5, col.alpha(k));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_project::json;

    #[test]
    fn spans_clamp_snap_and_fold() {
        let s = Span::new(100.0, 300.0, 200.0, true);
        assert_eq!(s.drag(150.4), 150.0);
        assert_eq!(s.drag(900.0), 300.0);
        // Between half the minimum and the minimum: held at the minimum.
        assert_eq!(s.drag(60.0), 100.0);
        // Under half: folded.
        assert_eq!(s.drag(49.0), 0.0);
        assert_eq!(s.drag(-30.0), 0.0);
        let fixed = Span { foldable: false, ..s };
        assert_eq!(fixed.drag(-30.0), 100.0);
        // A window too small for the minimum still gets the minimum.
        assert_eq!(s.up_to(40.0).fit(500.0), 100.0);
        assert_eq!(s.fit(f32::NAN), 200.0);
    }

    #[test]
    fn toolbars_fold_the_least_needed_first() {
        let items = [(100.0, 0), (40.0, 2), (40.0, 1), (40.0, 2), (100.0, 0)];
        // Room for all: nothing folds, no button.
        assert_eq!(fit_toolbar(320.0, &items, 30.0), vec![true; 5]);
        // A bit short: the last of the least needed goes, and the button takes room.
        assert_eq!(fit_toolbar(310.0, &items, 30.0), vec![true, true, true, false, true]);
        assert_eq!(fit_toolbar(280.0, &items, 30.0), vec![true, false, true, false, true]);
        // Very short: only what must stay.
        assert_eq!(fit_toolbar(100.0, &items, 30.0), vec![true, false, false, false, true]);
    }

    #[test]
    fn auto_sizes_follow_the_window() {
        // 720 px tall: the piano roll keeps the room, the panel shows its header.
        assert_eq!(auto_panel(View::Piano, 720.0 - TOP_BAR), PANEL_PEEK);
        assert_eq!(auto_panel(View::Rack, 720.0 - TOP_BAR), PANEL);
        assert_eq!(auto_panel(View::Piano, 1080.0 - TOP_BAR), PANEL);
        // The panel's stops: folded, header, cards.
        assert_eq!(panel_drag(PANEL_SPAN, 10.0), 0.0);
        assert_eq!(panel_drag(PANEL_SPAN, 60.0), PANEL_PEEK);
        assert_eq!(panel_drag(PANEL_SPAN, 120.0), PANEL_SPAN.min);
        assert_eq!(panel_drag(PANEL_SPAN, 300.4), 300.0);
        assert_eq!(panel_fit(PANEL_SPAN, 46.0), PANEL_PEEK);
        assert_eq!(panel_fit(PANEL_SPAN, 100.0), PANEL_SPAN.min);
        assert_eq!(auto_lanes(1000.0), LANES_SPAN.default);
        assert_eq!(auto_lanes(150.0), LANES_SPAN.min);
        // Velocity and note lanes share the room; short, the velocity alone.
        let (v, l) = lane_split(148.0);
        assert_eq!(v + l + 8.0, 148.0);
        assert!(l >= 52.0 && v >= 52.0);
        assert_eq!(lane_split(80.0), (80.0, 0.0));
    }

    #[test]
    fn sizes_round_trip_and_tolerate_junk() {
        let mut s = Sizes { sidebar_w: 300.0, key_h: 22.0, track_h: 80.0, lanes_open: false, lanes_h: Some(200.0), ..Sizes::default() };
        s.set_panel(View::Piano, Some(190.0));
        let back = Sizes::from_json(Some(&json::parse(&s.to_json().pretty()).unwrap()));
        assert_eq!(back, s);
        assert_eq!(Sizes::from_json(None), Sizes::default());
        let junk = json::parse(r#"{"sidebar": 5000, "panel": {"piano": "big", "mixer": 10, "rack": 100}, "key_height": -3, "track_height": "x"}"#).unwrap();
        let s = Sizes::from_json(Some(&junk));
        assert_eq!(s.sidebar_w, SIDEBAR_SPAN.max);
        assert_eq!(s.panel(View::Piano), None);
        assert_eq!(s.panel(View::Mixer), Some(PANEL_PEEK));
        assert_eq!(s.panel(View::Rack), Some(PANEL_SPAN.min));
        assert_eq!(s.key_h, KEY_SPAN.min);
        assert_eq!(s.track_h, TRACK_SPAN.default);
    }
}
