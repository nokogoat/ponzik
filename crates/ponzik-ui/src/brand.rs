//! The Ponzik mark: a green stepped pyramid (three tiers, a lit capstone) on a
//! dark teal tile. One geometry for the in-app drawing and for the SVG, PNG and
//! ICO icons (`cargo run -p ponzik-ui --example app_icon`).

use crate::{Canvas, Color, Path, Rect};

/// Apex, base corners and the foot of the ridge, in a unit square.
const APEX: (f32, f32) = (0.5, 0.19);
const BASE_Y: f32 = 0.77;
const BASE_L: f32 = 0.15;
const BASE_R: f32 = 0.85;
const RIDGE: f32 = 0.6;
/// Tiers, top to bottom: (top, bottom) heights.
const TIERS: [(f32, f32); 3] = [(0.19, 0.405), (0.44, 0.585), (0.62, 0.77)];

const TILE_TOP: u32 = 0x14352d;
const TILE_BOTTOM: u32 = 0x060e0c;
/// Faces lit from the left: (left face, right face), capstone then body.
const CAP: (u32, u32) = (0xb8ffc4, 0x56e070);
const BODY: (u32, u32) = (0x3fd45a, 0x1c8a3f);
const TILE_RADIUS: f32 = 0.225;

fn edge(x_base: f32, y: f32) -> f32 {
    let t = (y - APEX.1) / (BASE_Y - APEX.1);
    APEX.0 + (x_base - APEX.0) * t
}

/// Polygons of the pyramid in unit coordinates, with their color.
fn faces() -> Vec<(Vec<(f32, f32)>, u32)> {
    let mut out = Vec::new();
    for (i, &(y0, y1)) in TIERS.iter().enumerate() {
        let (left, right) = if i == 0 { CAP } else { BODY };
        let quad = |xb: f32, xr: f32| vec![(edge(xb, y0), y0), (edge(xr, y0), y0), (edge(xr, y1), y1), (edge(xb, y1), y1)];
        out.push((quad(BASE_L, RIDGE), left));
        out.push((quad(RIDGE, BASE_R), right));
    }
    out
}

/// Draws the mark in `r` (square); `tile`: on its dark rounded tile.
pub fn draw_mark(c: &mut Canvas, r: Rect, tile: bool) {
    let s = r.w.min(r.h);
    let (ox, oy) = (r.x + (r.w - s) * 0.5, r.y + (r.h - s) * 0.5);
    if tile {
        let t = Rect::new(ox, oy, s, s);
        c.fill_rrect_vgrad(t, s * TILE_RADIUS, Color::hex(TILE_TOP), Color::hex(TILE_BOTTOM));
        c.stroke_rrect(t.inset(s * 0.012), s * (TILE_RADIUS - 0.012), (s * 0.014).max(1.0), Color::hex(BODY.0).alpha(0.28));
    }
    // A soft ground line under the base.
    c.fill_rrect(
        Rect::new(ox + s * (BASE_L - 0.03), oy + s * (BASE_Y + 0.035), s * (BASE_R - BASE_L + 0.06), (s * 0.018).max(1.0)),
        s * 0.009,
        Color::hex(BODY.0).alpha(0.35),
    );
    for (pts, col) in faces() {
        let mut p = Path::new();
        let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (ox + x * s, oy + y * s)).collect();
        p.polygon(&pts);
        c.fill_path(&p, Color::hex(col));
    }
}

/// The mark as an SVG document (512 × 512).
pub fn mark_svg() -> String {
    let k = 512.0;
    let hex = |c: u32| format!("#{c:06x}");
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"512\" height=\"512\" viewBox=\"0 0 512 512\">\n\
         <defs><linearGradient id=\"tile\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
         <stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/></linearGradient></defs>\n\
         <rect width=\"512\" height=\"512\" rx=\"{r}\" fill=\"url(#tile)\"/>\n\
         <rect x=\"{i}\" y=\"{i}\" width=\"{w}\" height=\"{w}\" rx=\"{ri}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"0.28\" stroke-width=\"{sw}\"/>\n\
         <rect x=\"{gx}\" y=\"{gy}\" width=\"{gw}\" height=\"{gh}\" rx=\"{gr}\" fill=\"{}\" fill-opacity=\"0.35\"/>\n",
        hex(TILE_TOP),
        hex(TILE_BOTTOM),
        hex(BODY.0),
        hex(BODY.0),
        r = k * TILE_RADIUS,
        i = k * 0.012,
        w = k * (1.0 - 0.024),
        ri = k * (TILE_RADIUS - 0.012),
        sw = k * 0.014,
        gx = k * (BASE_L - 0.03),
        gy = k * (BASE_Y + 0.035),
        gw = k * (BASE_R - BASE_L + 0.06),
        gh = k * 0.018,
        gr = k * 0.009,
    );
    for (pts, col) in faces() {
        let points: Vec<String> = pts.iter().map(|&(x, y)| format!("{:.2},{:.2}", x * k, y * k)).collect();
        s.push_str(&format!("<polygon points=\"{}\" fill=\"{}\"/>\n", points.join(" "), hex(col)));
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_is_drawn_inside_its_tile() {
        let mut c = Canvas::new(64, 64);
        c.clear(Color::hex(0xffffff));
        draw_mark(&mut c, Rect::new(0.0, 0.0, 64.0, 64.0), true);
        let px = |x: usize, y: usize| c.pixels()[y * 64 + x] & 0xff_ffff;
        // Corners stay outside the rounded tile; the capstone is lit green.
        assert_eq!(px(0, 0), 0xffffff);
        let cap = px(30, 18);
        assert!((cap >> 8) & 0xff > 0xc0, "capstone {cap:06x}");
        assert!(mark_svg().matches("<polygon").count() == 6);
    }
}
