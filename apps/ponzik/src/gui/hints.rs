//! First-time hint bubbles: a short tip pointing at the widget it is about,
//! shown until it is dismissed or the gesture it teaches is done once. Seen
//! hints are kept in the settings.

use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Color, Path, Rect};

use super::Input;
use super::app::App;
use super::theme::*;
use super::widgets::wid;

const BUBBLE_W: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hint {
    /// A loop in the playlist: its right edge repeats it.
    DragToRepeat,
    /// A loop on a sampler channel: one click fits it to the pattern.
    FitLoop,
    /// Many devices in the bottom panel: the strip, the wheel, Ctrl+arrows.
    DeviceStrip,
}

impl Hint {
    fn bit(self) -> u32 {
        1 << self as u32
    }

    fn title(self) -> &'static str {
        match self {
            Hint::DragToRepeat => tr("Make the loop last"),
            Hint::FitLoop => tr("A loop on this channel"),
            Hint::DeviceStrip => tr("Find your devices"),
        }
    }

    fn body(self) -> &'static str {
        match self {
            Hint::DragToRepeat => tr("Drag the right edge of the clip to repeat the loop as far as you want. Right-click its title: loop to the end of the song, ×2, ×4, ×8."),
            Hint::FitLoop => tr("Click the loop badge to play it once per pattern, in time with the project tempo."),
            Hint::DeviceStrip => tr("Click a name to jump to its device, drag it to reorder. The wheel scrolls the panel; Ctrl+← and Ctrl+→ go from device to device."),
        }
    }
}

/// The hint wanted this frame and the bubble drawn last frame.
#[derive(Default)]
pub struct Hints {
    pub want: Option<(Hint, Rect)>,
    pub bubble: Option<Rect>,
}

impl App {
    /// The hint will not show again.
    pub fn hint_seen(&mut self, h: Hint) {
        self.prefs.hints_seen |= h.bit();
    }

    /// Not seen yet (and no tour in progress).
    pub fn hint_pending(&self, h: Hint) -> bool {
        self.prefs.hints_seen & h.bit() == 0 && self.tour.step.is_none()
    }

    /// Asks for the bubble of `h` next to `anchor` (the first asked wins).
    pub fn want_hint(&mut self, h: Hint, anchor: Rect) {
        if self.hint_pending(h) && self.hints.want.is_none() {
            self.hints.want = Some((h, anchor));
        }
    }

    /// The mouse is over the bubble: clicks stay there.
    pub(super) fn over_hint(&self, input: &Input) -> bool {
        self.hints.bubble.is_some_and(|r| r.contains(input.x, input.y))
    }

    /// The bubble, above its widget when there is room, else below.
    pub(super) fn hint_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let Some((hint, a)) = self.hints.want.take() else {
            self.hints.bubble = None;
            return;
        };
        let t = self.theme;
        let appear = self.anim.to(wid("hint", hint as usize, 0), 1.0, T_LAYOUT);
        let pad = 16.0;
        let lines = self.wrap(self.f.sans, 12.5, hint.body(), BUBBLE_W - 2.0 * pad);
        let h = pad + 20.0 + lines.len() as f32 * 18.0 + 10.0 + 28.0 + pad;
        let above = a.y - 12.0 - h > screen.y + 8.0;
        let y = if above { a.y - 12.0 - h } else { a.bottom() + 12.0 };
        let x = (a.x + a.w * 0.5 - BUBBLE_W * 0.5).clamp(screen.x + 12.0, screen.right() - BUBBLE_W - 12.0).round();
        let y = (y + (1.0 - appear) * if above { 6.0 } else { -6.0 }).round();
        let r = Rect::new(x, y, BUBBLE_W, h.round());
        self.hints.bubble = Some(r);

        c.stroke_rrect(a.inset(-3.0), 8.0, 2.0, t.accent.alpha(0.8 * appear));
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * appear) as u8));
        c.fill_rrect(r, 14.0, t.raised.alpha(appear));
        c.stroke_rrect(r, 14.0, 1.0, t.accent.alpha(0.5 * appear));
        let ax = (a.x + a.w * 0.5).clamp(r.x + 24.0, r.right() - 24.0);
        let (base, tip) = if above { (r.bottom() - 1.0, r.bottom() + 8.0) } else { (r.y + 1.0, r.y - 8.0) };
        let mut tri = Path::new();
        tri.polygon(&[(ax - 9.0, base), (ax, tip), (ax + 9.0, base)]);
        c.fill_path(&tri, t.raised.alpha(appear));

        let mut ty = r.y + pad + 8.0;
        self.text(c, self.f.sans_med, 14.0, r.x + pad, ty, hint.title(), t.text.alpha(appear));
        ty += 22.0;
        for line in &lines {
            self.text(c, self.f.sans, 12.5, r.x + pad, ty, line, t.text2.alpha(appear));
            ty += 18.0;
        }
        let ok = tr("Got it");
        let w = self.fonts.measure(self.f.sans_med, 13.0, ok) + 32.0;
        let ok_r = Rect::new(r.right() - pad - w, r.bottom() - pad - 28.0, w, 28.0);
        if self.lab_button(c, input, wid("hint-ok", hint as usize, 0), ok_r, None, ok, 1, "") {
            self.hint_seen(hint);
        }
    }
}
