//! Once warmed up, drawing a frame (shapes, strokes, arcs, text) and the
//! damage tracking that follows allocate nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use ponzik_ui::{Canvas, Color, Fonts, Path, Rect, embedded};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn frame(c: &mut Canvas, fonts: &mut Fonts, font: ponzik_ui::FontId, path: &mut Path, t: f32) {
    let green = Color::hex(0x40e070);
    c.clear(Color::hex(0x101418));
    c.fill_rrect_vgrad(Rect::new(10.0, 10.0, 300.0, 40.0), 8.0, Color::hex(0x203040), Color::hex(0x102030));
    c.stroke_rrect(Rect::new(10.0, 60.0, 300.0, 120.0), 6.0, 1.0, green);
    let prev = c.push_clip(Rect::new(20.0, 70.0, 280.0, 100.0));
    c.stroke_polyline(&[(20.0, 150.0), (90.0, 80.0 + t), (160.0, 140.0), (260.0, 100.0)], 2.0, green);
    c.fill_rect(Rect::new(40.0 + t, 70.0, 2.0, 100.0), green.alpha(0.7));
    c.set_clip(Some(prev));
    for k in 0..8 {
        let x = 30.0 + k as f32 * 36.0;
        c.stroke_arc(x, 210.0, 12.0, 2.3, 2.3 + (k as f32 + t) * 0.3, 3.0, green);
        c.fill_circle(x, 210.0, 7.0, Color::hex(0x2a3038));
    }
    path.clear();
    path.polygon(&[(200.0, 190.0), (230.0, 230.0), (170.0, 230.0)]);
    c.fill_path(path, green);
    fonts.draw(c, font, 13.0, 20.0, 34.0, "Ponzik · 1.2.3 · -6.0 dB", Color::hex(0xffffff));
}

#[test]
fn drawing_a_tracked_frame_does_not_allocate() {
    let mut fonts = Fonts::new();
    let font = fonts.add(embedded::SANS_REGULAR).unwrap();
    let mut c = Canvas::new(330, 250);
    let mut path = Path::new();
    // Warm-up: glyph cache, buffers of the canvas and of the damage tracking.
    for t in 0..4 {
        c.begin_frame();
        frame(&mut c, &mut fonts, font, &mut path, t as f32);
        c.end_frame();
    }
    // The counter does see allocations.
    let probe = ALLOCS.load(Ordering::SeqCst);
    drop(std::hint::black_box(vec![1u8; 16]));
    assert!(ALLOCS.load(Ordering::SeqCst) > probe);

    let before = ALLOCS.load(Ordering::SeqCst);
    for t in 0..20 {
        c.begin_frame();
        frame(&mut c, &mut fonts, font, &mut path, (t % 4) as f32);
        assert!(!c.end_frame().is_empty() || t % 4 == 0);
    }
    assert_eq!(ALLOCS.load(Ordering::SeqCst) - before, 0);
}
