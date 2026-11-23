//! Keyboard focus in the device panel and the transport bar: Ctrl+Tab
//! enters, Tab and Shift+Tab go from control to control, the arrows adjust
//! (Shift: finely), Home resets, Enter activates or types a value, Escape
//! leaves. A click on a control focuses it too; plain Tab moves the focus
//! only once the keyboard took over (otherwise it still switches views).

use ponzik_engine::{ParamInfo, Unit};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::{Key, Modifiers};

use super::Input;
use super::app::App;

/// Who has the keyboard, and the controls that can get it.
#[derive(Default)]
pub struct KbFocus {
    pub focus: Option<u64>,
    /// Controls drawn now register (device panel, transport).
    pub scope: bool,
    order: Vec<u64>,
    prev: Vec<u64>,
    /// Keys for the focused control this frame.
    keys: Vec<(Key, Modifiers)>,
    /// The focus ring shows (the keyboard was used).
    pub visible: bool,
    clicked: bool,
    /// Screenshots: the n-th control gets the focus once the controls are known.
    pub pending: Option<usize>,
}

impl App {
    /// Start of a frame: the keys that belong to the focus are taken out;
    /// returns the keys left for the shortcuts when some were taken.
    pub(super) fn kb_begin(&mut self, input: &Input) -> Option<Vec<(Key, Modifiers)>> {
        let k = &mut self.kb;
        k.prev = std::mem::take(&mut k.order);
        k.keys.clear();
        k.clicked = false;
        if k.focus.is_some_and(|f| !k.prev.contains(&f)) {
            k.focus = None;
        }
        if let Some(n) = k.pending.filter(|_| !k.prev.is_empty()) {
            k.focus = k.prev.get(n % k.prev.len()).copied();
            k.visible = true;
            k.pending = None;
        }
        if self.popup.is_some() || self.sidebar_focus {
            return None;
        }
        let mut rest = Vec::with_capacity(input.keys.len());
        let mut took = false;
        for &(key, mods) in &input.keys {
            let focused = self.kb.focus.is_some();
            match key {
                Key::Tab if !mods.alt && (mods.ctrl || (focused && self.kb.visible)) => {
                    self.kb_step(if mods.shift { -1 } else { 1 });
                    self.kb.visible = true;
                    took = true;
                }
                Key::Escape if focused && self.kb.visible => {
                    self.kb.focus = None;
                    took = true;
                }
                Key::Up | Key::Down | Key::Left | Key::Right | Key::Enter | Key::Home if focused && !mods.ctrl && !mods.alt => {
                    self.kb.keys.push((key, mods));
                    self.kb.visible = true;
                    took = true;
                }
                _ => rest.push((key, mods)),
            }
        }
        took.then_some(rest)
    }

    /// End of a frame: a click elsewhere than on a control drops the focus.
    pub(super) fn kb_end(&mut self, input: &Input) {
        if input.pressed && !self.kb.clicked {
            self.kb.focus = None;
        }
    }

    fn kb_step(&mut self, dir: i32) {
        let order = &self.kb.prev;
        if order.is_empty() {
            return;
        }
        let n = order.len() as i32;
        let next = match self.kb.focus.and_then(|f| order.iter().position(|&o| o == f)) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        self.kb.focus = Some(order[next as usize]);
    }

    /// A control drawn in scope: it joins the Tab order, a click focuses it,
    /// and it gets a ring while the keyboard has it. True when focused.
    pub(super) fn kb_control(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect) -> bool {
        if !self.kb.scope {
            return false;
        }
        self.kb.order.push(id);
        if input.pressed && self.hovered(input, id, r) {
            if self.kb.focus != Some(id) {
                self.kb.visible = false;
            }
            self.kb.focus = Some(id);
            self.kb.clicked = true;
        }
        let focused = self.kb.focus == Some(id);
        if focused && self.kb.visible {
            c.stroke_rrect(r.inset(-3.0), 10.0, 2.0, self.theme.accent);
        }
        focused
    }

    /// Keys for `id` this frame (none unless it is focused).
    pub(super) fn kb_keys(&self, id: u64) -> &[(Key, Modifiers)] {
        if self.kb.focus == Some(id) { &self.kb.keys } else { &[] }
    }

    /// Enter pressed on `id` this frame.
    pub(super) fn kb_enter(&self, id: u64) -> bool {
        self.kb_keys(id).iter().any(|(k, _)| *k == Key::Enter)
    }

    /// Arrows on `id`: steps up (+) or down (−), Shift for fine steps
    /// (a tenth); `None` without arrows.
    pub(super) fn kb_steps(&self, id: u64) -> Option<f32> {
        let mut n = 0.0;
        for &(k, mods) in self.kb_keys(id) {
            let fine = if mods.shift { 0.1 } else { 1.0 };
            match k {
                Key::Up | Key::Right => n += fine,
                Key::Down | Key::Left => n -= fine,
                _ => {}
            }
        }
        (n != 0.0).then_some(n)
    }

    /// The keyboard's change of a parameter: arrows step it, Home resets it.
    pub(super) fn kb_param(&self, id: u64, info: &ParamInfo, value: f32) -> Option<f32> {
        if self.kb_keys(id).iter().any(|(k, _)| *k == Key::Home) {
            return Some(info.default);
        }
        let steps = self.kb_steps(id)?;
        if info.unit == Unit::Choice {
            let count = (info.max - info.min).round() as i32 + 1;
            let cur = value.round() as i32;
            return Some((cur + steps.signum() as i32).rem_euclid(count.max(1)) as f32);
        }
        Some(info.from_norm(info.to_norm(value) + steps * 0.02))
    }
}
