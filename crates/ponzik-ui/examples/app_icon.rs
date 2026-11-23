//! Writes the application icons from the Ponzik mark:
//!
//! cargo run -p ponzik-ui --example app_icon -- assets/icons
//!
//! - `ponzik.svg`: the vector mark;
//! - `ponzik-256.png`: for the AppImage and the `.desktop` file;
//! - `ponzik.ico`: 16 to 256 px, compiled into `Ponzik.exe`.

use ponzik_ui::{Canvas, Color, Rect, brand, encode_png_rgba};

/// The mark at `size` px with real transparency: drawn on black and on white,
/// the difference gives the coverage, the black render the color.
fn render(size: usize) -> Vec<u32> {
    let draw = |bg: u32| {
        let mut c = Canvas::new(size, size);
        c.clear(Color::hex(bg));
        brand::draw_mark(&mut c, Rect::new(0.0, 0.0, size as f32, size as f32), true);
        c.pixels().to_vec()
    };
    let (black, white) = (draw(0x000000), draw(0xffffff));
    black
        .iter()
        .zip(&white)
        .map(|(&b, &w)| {
            let ch = |p: u32, s: u32| ((p >> s) & 0xff) as f32;
            let diff = (0..3).map(|i| ch(w, i * 8) - ch(b, i * 8)).sum::<f32>() / 3.0;
            let a = (1.0 - diff / 255.0).clamp(0.0, 1.0);
            if a <= 0.0 {
                return 0;
            }
            let un = |s: u32| ((ch(b, s) / a).round().min(255.0) as u32) << s;
            ((a * 255.0).round() as u32) << 24 | un(16) | un(8) | un(0)
        })
        .collect()
}

fn ico(sizes: &[usize]) -> Vec<u8> {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| encode_png_rgba(&render(s), s, s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&s, png) in sizes.iter().zip(&pngs) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for png in pngs {
        out.extend_from_slice(&png);
    }
    out
}

fn main() -> std::io::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "assets/icons".into()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("ponzik.svg"), brand::mark_svg())?;
    std::fs::write(dir.join("ponzik-256.png"), encode_png_rgba(&render(256), 256, 256))?;
    std::fs::write(dir.join("ponzik.ico"), ico(&[16, 24, 32, 48, 64, 128, 256]))?;
    println!("{}: ponzik.svg, ponzik-256.png, ponzik.ico", dir.display());
    Ok(())
}
