//! First-run tour: three bubbles anchored on the real widgets (a beat in the
//! Channel Rack, a bass in the piano roll, the export in the menu). It can be
//! skipped at any step and replayed from the palette.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::{App, Focus, View};
use super::theme::*;
use super::widgets::wid;

pub const STEPS: usize = 3;
const BUBBLE_W: f32 = 340.0;

/// Tour state: the current step, and where each step's widget was drawn
/// this frame (views fill it in as they draw).
#[derive(Default)]
pub struct Tour {
    pub step: Option<usize>,
    pub anchors: [Option<Rect>; STEPS],
    /// The bubble of the last frame: clicks there never reach the widgets below.
    pub bubble: Option<Rect>,
}

fn title(step: usize) -> &'static str {
    match step {
        0 => tr("Make a beat"),
        1 => tr("Add a bass"),
        _ => tr("Export your track"),
    }
}

fn body(step: usize) -> &'static str {
    match step {
        0 => tr("Each row is a sound, each square a sixteenth note. Click squares to switch them on, press Space to hear the loop."),
        1 => tr("Click in the grid to draw notes, drag their end to make them longer, right-click to erase. The arrows above pick the channel."),
        _ => tr("When it sounds good, open this menu, then Export and WAV. Ctrl+K finds every other action."),
    }
}

impl App {
    /// Starts the tour from its first step.
    pub fn start_tour(&mut self) {
        self.popup = None;
        self.stop_lesson();
        self.go_to_tour_step(0);
    }

    /// First launch: the tour, unless it was already done or skipped.
    pub fn start_tour_if_new(&mut self) {
        if !self.prefs.tour_done {
            self.start_tour();
        }
    }

    fn go_to_tour_step(&mut self, step: usize) {
        self.tour.step = Some(step);
        self.tour.bubble = None;
        match step {
            0 if self.view_shown(View::Rack) => self.set_view(View::Rack),
            1 if self.view_shown(View::Piano) => {
                // The bass channel when there is one, otherwise the selected channel.
                let bass = tr("Bass");
                if let Some(ch) = self.project.channels.iter().position(|c| c.name == bass) {
                    self.focus = Focus::Channel(ch);
                }
                self.set_view(View::Piano);
            }
            _ => {}
        }
    }

    fn end_tour(&mut self) {
        self.tour.step = None;
        self.tour.bubble = None;
        if !self.prefs.tour_done {
            self.prefs.tour_done = true;
            self.save_settings();
        }
    }

    /// Records where the widget of `step` is on screen this frame.
    pub(super) fn tour_anchor(&mut self, step: usize, r: Rect) {
        if self.tour.step == Some(step) {
            self.tour.anchors[step] = Some(r);
        }
    }

    /// True when the mouse is over the tour bubble.
    pub(super) fn over_tour(&self, input: &Input) -> bool {
        self.tour.bubble.is_some_and(|r| r.contains(input.x, input.y))
    }

    /// The bubble of the current step, pointing at its widget; drawn above the views.
    pub(super) fn tour_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let Some(step) = self.tour.step else { return };
        let anchors = std::mem::take(&mut self.tour.anchors);
        let t = self.theme;
        let appear = self.anim.to(wid("tour", step, 0), 1.0, T_LAYOUT);
        // Steps whose widget is hidden (a view switched off) still show, centred.
        let anchor = anchors[step];

        // Text, wrapped to the bubble.
        let pad = 18.0;
        let lines = self.wrap(self.f.sans, 13.0, body(step), BUBBLE_W - 2.0 * pad);
        let h = pad + 16.0 + 14.0 + lines.len() as f32 * 19.0 + 16.0 + 32.0 + pad;

        // Below the widget when there is room, so it stays clickable; inside
        // the top of large areas that fill the screen; otherwise above.
        let (x, y, arrow_up) = match anchor {
            Some(a) if a.bottom() + 14.0 + h < screen.bottom() - 8.0 => (a.x + a.w * 0.5 - BUBBLE_W * 0.5, a.bottom() + 14.0, Some(true)),
            Some(a) if a.h > 220.0 => (a.right() - BUBBLE_W - 24.0, a.y + 24.0, None),
            Some(a) => (a.x + a.w * 0.5 - BUBBLE_W * 0.5, a.y - 14.0 - h, Some(false)),
            None => (screen.x + (screen.w - BUBBLE_W) * 0.5, screen.y + (screen.h - h) * 0.5, None),
        };
        let x = x.clamp(screen.x + 12.0, screen.right() - BUBBLE_W - 12.0).round();
        let y = (y + (1.0 - appear) * 6.0).round();
        let r = Rect::new(x, y, BUBBLE_W, h.round());
        self.tour.bubble = Some(r);

        // The widget: a calm accent ring around it.
        if let Some(a) = anchor {
            c.stroke_rrect(a.inset(-3.0), 12.0, 2.0, t.accent.alpha(0.85 * appear));
        }
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, t.shade(90.0 * appear));
        c.fill_rrect(r, 14.0, t.raised.alpha(appear));
        c.stroke_rrect(r, 14.0, 1.0, t.accent.alpha(0.45 * appear));
        // Arrow towards the widget.
        if let (Some(a), Some(up)) = (anchor, arrow_up) {
            let ax = (a.x + a.w * 0.5).clamp(r.x + 24.0, r.right() - 24.0);
            let (base, tip) = if up { (r.y + 1.0, r.y - 8.0) } else { (r.bottom() - 1.0, r.bottom() + 8.0) };
            let mut tri = Path::new();
            tri.polygon(&[(ax - 9.0, base), (ax, tip), (ax + 9.0, base)]);
            c.fill_path(&tri, t.raised.alpha(appear));
        }

        let mut ty = r.y + pad + 6.0;
        self.text(c, self.f.sans, 11.0, r.x + pad, ty, &tr_fmt!("Step {} of {}", step + 1, STEPS), t.accent.alpha(appear));
        ty += 20.0;
        self.text(c, self.f.sans_med, 15.0, r.x + pad, ty, title(step), t.text.alpha(appear));
        ty += 22.0;
        for line in &lines {
            self.text(c, self.f.sans, 13.0, r.x + pad, ty, line, t.text2.alpha(appear));
            ty += 19.0;
        }

        // Buttons: skip on the left, next (or done) on the right.
        let by = r.bottom() - pad - 32.0;
        let last = step + 1 == STEPS;
        let next = if last { tr("Done") } else { tr("Next") };
        let nw = self.fonts.measure(self.f.sans_med, 13.0, next) + 32.0;
        let next_r = Rect::new(r.right() - pad - nw, by, nw, 32.0);
        let mut go_next = self.lab_button(c, input, wid("tour-next", step, 0), next_r, None, next, 1, "");
        let skip = tr("Skip the tour");
        let sw = self.fonts.measure(self.f.sans_med, 13.0, skip) + 28.0;
        let skip_r = Rect::new(r.x + pad - 8.0, by, sw, 32.0);
        let mut skip_now = self.lab_button(c, input, wid("tour-skip", step, 0), skip_r, None, skip, 0, tr("Replay it any time from the palette (Ctrl+K)"));
        // Step dots between the buttons.
        let dots_x = (skip_r.right() + next_r.x) * 0.5 - (STEPS as f32 - 1.0) * 6.0;
        for k in 0..STEPS {
            let col = if k == step { t.accent } else { t.muted.alpha(0.5) };
            c.fill_circle(dots_x + k as f32 * 12.0, by + 16.0, 3.0, col.alpha(appear));
        }
        if self.popup.is_none() {
            for (k, _) in &input.keys {
                match k {
                    ponzik_window::Key::Escape => skip_now = true,
                    ponzik_window::Key::Enter => go_next = true,
                    _ => {}
                }
            }
        }
        if skip_now || (go_next && last) {
            self.end_tour();
        } else if go_next {
            self.go_to_tour_step(step + 1);
        }
    }

    /// Screenshot setup: `PONZIK_TOUR=n` shows step n.
    pub(super) fn screenshot_tour(&mut self, step: usize) {
        self.go_to_tour_step(step.min(STEPS - 1));
    }
}
