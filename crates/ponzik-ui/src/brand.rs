//! The Ponzik mark: a stepped pyramid in one flat green, three tiers from the
//! base (the rhythm) to the tip (the melody). One geometry for the in-app
//! drawing and for the SVG, PNG and ICO icons
//! (`cargo run -p ponzik-ui --example app_icon`).

use crate::{Canvas, Color, Path, Rect};

/// Apex and base corners, in a unit square.
const APEX: (f32, f32) = (0.5, 0.14);
const BASE_Y: f32 = 0.86;
const BASE_L: f32 = 0.06;
const BASE_R: f32 = 0.94;
/// Tiers, top to bottom: (top, bottom) heights.
const TIERS: [(f32, f32); 3] = [(0.14, 0.40), (0.45, 0.63), (0.68, 0.86)];
/// The one green of the mark.
pub const GREEN: u32 = 0x22c55e;

fn edge(x_base: f32, y: f32) -> f32 {
    let t = (y - APEX.1) / (BASE_Y - APEX.1);
    APEX.0 + (x_base - APEX.0) * t
}

/// Polygons of the pyramid in unit coordinates, one per tier.
fn tiers() -> Vec<Vec<(f32, f32)>> {
    TIERS
        .iter()
        .map(|&(y0, y1)| vec![(edge(BASE_L, y0), y0), (edge(BASE_R, y0), y0), (edge(BASE_R, y1), y1), (edge(BASE_L, y1), y1)])
        .collect()
}

/// Draws the mark in `r` (square).
pub fn draw_mark(c: &mut Canvas, r: Rect) {
    let s = r.w.min(r.h);
    let (ox, oy) = (r.x + (r.w - s) * 0.5, r.y + (r.h - s) * 0.5);
    for pts in tiers() {
        let mut p = Path::new();
        let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (ox + x * s, oy + y * s)).collect();
        p.polygon(&pts);
        c.fill_path(&p, Color::hex(GREEN));
    }
}

/// The mark as an SVG document (512 × 512, transparent background).
pub fn mark_svg() -> String {
    let k = 512.0;
    let mut s = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"512\" height=\"512\" viewBox=\"0 0 512 512\">\n");
    for pts in tiers() {
        let points: Vec<String> = pts.iter().map(|&(x, y)| format!("{:.1},{:.1}", x * k, y * k)).collect();
        s.push_str(&format!("<polygon points=\"{}\" fill=\"#{GREEN:06x}\"/>\n", points.join(" ")));
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_is_flat_green_on_transparent() {
        let mut c = Canvas::new(64, 64);
        c.clear(Color::hex(0xffffff));
        draw_mark(&mut c, Rect::new(0.0, 0.0, 64.0, 64.0));
        let px = |x: usize, y: usize| c.pixels()[y * 64 + x] & 0xff_ffff;
        assert_eq!(px(1, 1), 0xffffff, "no background tile");
        assert_eq!(px(32, 52), GREEN, "base tier");
        assert_eq!(mark_svg().matches("<polygon").count(), 3);
        assert!(!mark_svg().contains("<rect"));
    }
}
