//! Drum Pads card: a 4×4 grid of sample pads that light up when they play
//! (brighter for harder hits), the selected pad's waveform with its start
//! and decay handles, and its settings. Drop a WAV from the browser onto a
//! pad to load it (Shift: add a velocity layer); right-click a pad for its
//! menu. The starter kit comes from the bundled library when there is one,
//! otherwise from renders of the native Drum Machine.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_engine::instruments::DrumMachine;
use ponzik_engine::instruments::drum_pads::{
    DECAY_FULL, FIRST_NOTE, LAYERS, P_VOLUME, PAD_NAMES, PADS, S_ATTACK, S_CHOKE, S_CUTOFF, S_DECAY, S_FILTER, S_FINE,
    S_MODE, S_PAN, S_RESO, S_REVERSE, S_START, S_TUNE, S_VELOCITY, S_VOLUME, pad_param, tune_ratio,
};
use ponzik_engine::{Command, Instrument, ParamInfo, Sample, Unit};
use ponzik_ui::{Canvas, Color, Path as Shape, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, Focus, MenuItem, Popup};
use super::devices::{Device, device_target, params_of};
use super::dnd::DropTarget;
use super::theme::*;
use super::visuals::device_key;
use super::widgets::{Icon, draw_icon, wid};

const KIND: &str = "drum_pads";
const PAD_W: f32 = 92.0;
const PAD_GAP: f32 = 6.0;
const GRID_W: f32 = 4.0 * PAD_W + 3.0 * PAD_GAP;
const WAVE_W: f32 = 260.0;
const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const GAP: f32 = 18.0;
/// Pad controls, two rows of six; the master volume gets its own column.
const ROWS: [[u32; 6]; 2] =
    [[S_VOLUME, S_PAN, S_TUNE, S_FINE, S_VELOCITY, S_CHOKE], [S_ATTACK, S_DECAY, S_FILTER, S_CUTOFF, S_RESO, S_MODE]];
/// Width of the Drum Pads card.
pub const WIDTH: f32 = 28.0 + GRID_W + GAP + WAVE_W + GAP + 6.0 * CELL_W + GAP + CELL_W;

/// What the pad menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadOp {
    Clear,
    Reverse,
    Choke(u8),
    Copy,
    Paste,
    /// Drops the loudest velocity layer.
    RemoveLayer,
    StarterKit,
}

/// A copied pad: its settings (slot, value) and its layers.
#[derive(Clone)]
struct PadClip {
    params: Vec<(u32, f32)>,
    layers: Vec<String>,
}

thread_local! {
    /// Selected pad and shown layer, per channel.
    static SELECTED: RefCell<HashMap<usize, (usize, usize)>> = RefCell::new(HashMap::new());
    /// Velocity of the last pad played with the mouse, per channel.
    static AUDITION: RefCell<HashMap<usize, f32>> = RefCell::new(HashMap::new());
    static CLIPBOARD: RefCell<Option<PadClip>> = const { RefCell::new(None) };
}

fn selected(ch: usize) -> (usize, usize) {
    SELECTED.with(|s| s.borrow().get(&ch).copied()).unwrap_or((0, 0))
}

fn select(ch: usize, pad: usize, layer: usize) {
    SELECTED.with(|s| s.borrow_mut().insert(ch, (pad, layer)));
}

fn info(id: u32) -> Option<&'static ParamInfo> {
    params_of(KIND, true).iter().find(|p| p.id == id)
}

/// Kit renders are shared by several pads: those pads show their own name.
fn is_kit_render(path: &str) -> bool {
    Path::new(path).starts_with(kit_dir())
}

fn kit_dir() -> PathBuf {
    super::compat::config_dir().join("kits").join("drum-machine")
}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

impl App {
    fn pad_layers(&self, ch: usize, pad: usize) -> &[String] {
        self.project.channels.get(ch).and_then(|c| c.instrument.pads.get(pad)).map_or(&[], Vec::as_slice)
    }

    fn pad_value(&self, ch: usize, id: u32) -> f32 {
        let default = info(id).map_or(0.0, |p| p.default);
        self.project.channels.get(ch).and_then(|c| c.instrument.get(id)).unwrap_or(default)
    }

    fn is_pads(&self, ch: usize) -> bool {
        self.project.channels.get(ch).is_some_and(|c| c.instrument.kind == KIND)
    }

    /// Loads a file onto a pad: replaces its layers, or adds a velocity layer.
    pub(super) fn load_pad_sample(&mut self, ch: usize, pad: usize, path: &str, add_layer: bool) {
        if !self.is_pads(ch) || pad >= PADS || !self.ensure_sample(path) {
            return;
        }
        let pads = &mut self.project.channels[ch].instrument.pads;
        if pads.len() <= pad {
            pads.resize(pad + 1, Vec::new());
        }
        let layers = &mut pads[pad];
        let layer = if add_layer && !layers.is_empty() {
            if layers.len() >= LAYERS {
                self.toast("This pad already has four velocity layers");
                return;
            }
            layers.push(path.to_string());
            layers.len() - 1
        } else {
            *layers = vec![path.to_string()];
            0
        };
        select(ch, pad, layer);
        self.focus = Focus::Channel(ch);
        self.commit(if layer > 0 { "Add a velocity layer" } else { "Load a pad sample" });
    }

    /// Plays a pad from the mouse, at `velocity`.
    fn hit_pad(&mut self, ch: usize, pad: usize, velocity: f32) {
        self.release_preview();
        let note = FIRST_NOTE + pad as u8;
        self.send(Command::NoteOn { channel: ch, note, velocity });
        self.preview = Some((ch, note));
        AUDITION.with(|a| a.borrow_mut().insert(ch, velocity));
    }

    pub(super) fn pad_menu(&self, ch: usize, pad: usize) -> (String, Vec<MenuItem>) {
        let layers = self.pad_layers(ch, pad);
        let title = layers.first().map_or_else(|| PAD_NAMES[pad].to_string(), |p| stem(p));
        let choke = self.pad_value(ch, pad_param(pad, S_CHOKE)) as u8;
        let reversed = self.pad_value(ch, pad_param(pad, S_REVERSE)) > 0.5;
        let op = |o| Action::Pad(ch, pad, o);
        let mut v = vec![
            MenuItem::new(Icon::Swap, "Reverse", op(PadOp::Reverse)).on(reversed),
            MenuItem::new(Icon::Duplicate, "Copy pad", op(PadOp::Copy)),
        ];
        if CLIPBOARD.with(|c| c.borrow().is_some()) {
            v.push(MenuItem::new(Icon::Duplicate, "Paste pad", op(PadOp::Paste)).tip("Sample, layers and settings"));
        }
        if layers.len() > 1 {
            v.push(MenuItem::new(Icon::Close, "Remove loudest layer", op(PadOp::RemoveLayer)).detail(format!("{} layers", layers.len())));
        }
        if !layers.is_empty() {
            v.push(MenuItem::new(Icon::Trash, "Clear", op(PadOp::Clear)).tip("Removes the sample, keeps the settings"));
        }
        for g in 0..=4u8 {
            let name = if g == 0 { "No choke group".to_string() } else { format!("Choke group {g}") };
            let it = MenuItem::plain(&name, op(PadOp::Choke(g))).on(choke == g);
            let it = if g == 0 { it.sep().tip("Pads of the same group cut each other, like closed and open hats") } else { it };
            v.push(it);
        }
        v.push(MenuItem::new(Icon::Preset, "Load starter kit", op(PadOp::StarterKit)).sep().tip("Fills all sixteen pads"));
        (title, v)
    }

    pub(super) fn pad_action(&mut self, ch: usize, pad: usize, op: PadOp) {
        if !self.is_pads(ch) || pad >= PADS {
            return;
        }
        let first = pad_param(pad, 0);
        let last = pad_param(pad + 1, 0);
        match op {
            PadOp::Clear => {
                if let Some(l) = self.project.channels[ch].instrument.pads.get_mut(pad) {
                    l.clear();
                }
                self.commit("Clear a pad");
            }
            PadOp::RemoveLayer => {
                if let Some(l) = self.project.channels[ch].instrument.pads.get_mut(pad) {
                    l.pop();
                }
                select(ch, pad, 0);
                self.commit("Remove a velocity layer");
            }
            PadOp::Reverse => {
                let id = pad_param(pad, S_REVERSE);
                let on = self.pad_value(ch, id) > 0.5;
                self.project.channels[ch].instrument.set(id, if on { 0.0 } else { 1.0 });
                self.commit("Reverse a pad");
            }
            PadOp::Choke(g) => {
                self.project.channels[ch].instrument.set(pad_param(pad, S_CHOKE), f32::from(g));
                self.commit("Choke group");
            }
            PadOp::Copy => {
                let def = &self.project.channels[ch].instrument;
                let params = def.params.iter().filter(|(id, _)| (first..last).contains(id)).map(|&(id, v)| (id - first, v)).collect();
                let layers = def.pads.get(pad).cloned().unwrap_or_default();
                CLIPBOARD.with(|c| *c.borrow_mut() = Some(PadClip { params, layers }));
                self.toast(&format!("{} copied", PAD_NAMES[pad]));
            }
            PadOp::Paste => {
                let Some(clip) = CLIPBOARD.with(|c| c.borrow().clone()) else { return };
                let def = &mut self.project.channels[ch].instrument;
                def.params.retain(|(id, _)| !(first..last).contains(id));
                for (slot, v) in clip.params {
                    def.set(first + slot, v);
                }
                if def.pads.len() <= pad {
                    def.pads.resize(pad + 1, Vec::new());
                }
                def.pads[pad] = clip.layers;
                self.commit("Paste a pad");
            }
            PadOp::StarterKit => self.load_starter_kit(ch),
        }
    }

    /// Fills the sixteen pads: bundled library files when found, renders of
    /// the native Drum Machine otherwise (cached as WAV files).
    pub(super) fn load_starter_kit(&mut self, ch: usize) {
        if !self.is_pads(ch) {
            return;
        }
        let library = kit_from_library(&library_files());
        let mut rendered: [Option<String>; 8] = Default::default();
        let mut from_library = 0;
        let mut plan: Vec<(String, f32)> = Vec::with_capacity(PADS);
        for pad in 0..PADS {
            if let Some(path) = library[pad].as_ref().filter(|p| self.ensure_sample(&p.to_string_lossy())) {
                from_library += 1;
                plan.push((path.to_string_lossy().into_owned(), 0.0));
                continue;
            }
            let (src, tune) = KIT_MAP[pad];
            if rendered[src].is_none() {
                rendered[src] = self.rendered_kit_sample(src);
            }
            let Some(path) = rendered[src].clone() else { continue };
            plan.push((path, tune));
        }
        if plan.is_empty() {
            self.toast("Could not build the starter kit");
            return;
        }
        let def = &mut self.project.channels[ch].instrument;
        def.pads = vec![Vec::new(); PADS];
        for (pad, (path, tune)) in plan.into_iter().enumerate() {
            def.pads[pad] = vec![path];
            def.set(pad_param(pad, S_TUNE), tune);
        }
        select(ch, 0, 0);
        self.commit("Load the starter kit");
        self.toast(&if from_library > 0 {
            format!("Starter kit loaded ({from_library} pads from the library)")
        } else {
            "Starter kit loaded from the Drum Machine".to_string()
        });
    }

    /// Path of a Drum Machine render, written once to the kit cache.
    fn rendered_kit_sample(&mut self, src: usize) -> Option<String> {
        let (note, name) = KIT_SOURCES[src];
        let path = kit_dir().join(format!("{name}.wav"));
        let key = path.to_string_lossy().into_owned();
        if self.samples.contains_key(&key) {
            return Some(key);
        }
        if !path.is_file() {
            let sample = render_drum_machine(note, name);
            let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| ponzik_engine::wav::write_sample(&path, &sample));
            if let Err(e) = written {
                // No disk cache: the kit still plays, it just won't reopen with the project.
                eprintln!("{}: {e}", path.display());
                self.samples.insert(key.clone(), Arc::new(sample));
                return Some(key);
            }
        }
        self.ensure_sample(&key).then_some(key)
    }

    /// Screenshot setup: `kit` loads the starter kit, `kit,n` also plays pad n.
    pub(super) fn screenshot_pads(&mut self, ch: usize, spec: &str) {
        let mut parts = spec.split(',');
        if parts.next().is_some_and(|p| p.trim() == "kit") {
            self.load_starter_kit(ch);
        }
        if let Some(pad) = parts.next().and_then(|p| p.trim().parse::<usize>().ok()).filter(|&p| p < PADS) {
            select(ch, pad, 0);
            self.hit_pad(ch, pad, 0.8);
        }
        // The channel's beat moves to the kick pad, with soft and hard hats on top.
        let step = ponzik_engine::TICKS_PER_STEP;
        for pat in &mut self.project.patterns {
            let Some(notes) = pat.notes.get_mut(ch) else { continue };
            for n in notes.iter_mut() {
                n.key = FIRST_NOTE;
            }
            for k in (2..pat.len / step).step_by(4) {
                let vel = if k % 8 == 2 { 1.0 } else { 0.45 };
                notes.push(ponzik_engine::Note { start: k * step, len: step, key: FIRST_NOTE + 6, vel });
            }
            notes.sort_by_key(|n| n.start);
        }
        self.commit("Screenshot beat");
    }

    /// The Drum Pads card.
    pub(super) fn pads_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, device: Device, params: &'static [ParamInfo]) {
        let Device::Instrument(ch) = device else { return };
        let (sel, layer) = selected(ch);
        self.pad_grid(c, input, Rect::new(body.x, body.y + 4.0, GRID_W, body.h - 8.0), ch, sel);
        let wave = Rect::new(body.x + GRID_W + GAP, body.y + 4.0, WAVE_W, body.h - 8.0);
        self.pad_wave(c, input, wave, ch, sel, layer);
        self.pad_controls(c, input, wave.right() + GAP, body, device, params, sel);
    }

    fn pad_grid(&mut self, c: &mut Canvas, input: &Input, r: Rect, ch: usize, sel: usize) {
        let t = self.theme;
        let pad_h = ((r.h - 3.0 * PAD_GAP) / 4.0).max(24.0);
        let mut hits = self.pad_hits::<PADS>(ch);
        if let Some((pc, note)) = self.preview
            && pc == ch
            && let Some(p) = note.checked_sub(FIRST_NOTE).map(usize::from).filter(|&p| p < PADS)
        {
            hits[p] = AUDITION.with(|a| a.borrow().get(&ch).copied()).unwrap_or(1.0);
        }
        // Pad 1 (note 36) bottom left, like FPC and hardware pads.
        let rect_of = |pad: usize| {
            let (col, row) = (pad % 4, 3 - pad / 4);
            Rect::new(r.x + col as f32 * (PAD_W + PAD_GAP), r.y + row as f32 * (pad_h + PAD_GAP), PAD_W, pad_h)
        };
        for pad in 0..PADS {
            let pr = rect_of(pad);
            let id = wid("pads-pad", ch, pad);
            let hov = self.hovered(input, id, pr);
            let hv = self.hover_anim(id, hov);
            let flash_id = wid("pads-flash", ch, pad);
            let cur = self.anim.get(flash_id).unwrap_or(0.0);
            let glow = if hits[pad] > cur { hits[pad] } else { self.anim.to(flash_id, hits[pad], T_FLASH) };
            self.anim.set(flash_id, glow);
            let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.95 } else { 1.0 }, T_PRESS);
            let layers: Vec<String> = self.pad_layers(ch, pad).to_vec();
            let loaded = !layers.is_empty();
            let base = if loaded { t.pad_a.mix(t.raised_hi, hv * 0.6) } else { t.well.mix(t.raised, hv * 0.8) };
            let face = pr.scale(press);
            c.fill_rrect(face, 9.0, base.mix(t.accent, glow * 0.85));
            if !loaded {
                c.stroke_rrect(face, 9.0, 1.0, t.hairline.alpha(1.4));
            }
            // Mini waveform along the bottom of the pad.
            let ink = t.text2.mix(t.text, glow).mix(t.on_accent, (glow - 0.5) * 2.0);
            if let Some(path) = layers.first()
                && let Some(peaks) = self.waveform(path)
            {
                let w = Rect::new(face.x + 6.0, face.y + 19.0, face.w - 12.0, (face.h - 23.0).max(4.0));
                let col = t.accent.mix(t.on_accent, glow).alpha(0.55 + 0.35 * glow);
                mini_wave(c, &peaks, w, col);
            }
            let label = layers.first().filter(|p| !is_kit_render(p)).map_or_else(|| PAD_NAMES[pad].to_string(), |p| stem(p));
            let label = self.short_label(self.f.sans_med, 11.0, &label, face.w - 30.0);
            let label_col = if loaded { ink } else { t.muted.mix(t.text2, hv) };
            self.text(c, self.f.sans_med, 11.0, face.x + 7.0, face.y + 10.0, &label, label_col);
            // Choke group and velocity layers, top right.
            let choke = self.pad_value(ch, pad_param(pad, S_CHOKE)) as u8;
            let mut bx = face.right() - 7.0;
            if choke > 0 {
                let tag = format!("C{choke}");
                bx = self.text_right(c, self.f.mono, 9.0, bx, face.y + 10.0, &tag, t.select.mix(t.on_accent, glow)) - 4.0;
            }
            for k in (1..layers.len()).rev() {
                c.fill_circle(bx - 2.0, face.y + 10.0, 2.0, ink.alpha(0.5 + 0.1 * k as f32));
                bx -= 6.0;
            }
            if hov {
                let what = layers.first().map_or("Empty".to_string(), |p| stem(p));
                self.set_tip(
                    &format!(
                        "{} · note {} · {what}\nClick: play (higher is louder) · right-click: menu\nDrop a WAV here · Shift + drop: add a velocity layer",
                        PAD_NAMES[pad],
                        FIRST_NOTE as usize + pad
                    ),
                    "",
                );
                if input.pressed && self.drag.is_none() {
                    let vel = (0.35 + 0.65 * (1.0 - (input.y - pr.y) / pr.h)).clamp(0.05, 1.0);
                    select(ch, pad, 0);
                    self.hit_pad(ch, pad, vel);
                }
                if input.right_pressed && self.drag.is_none() {
                    select(ch, pad, 0);
                    self.open_popup(Popup::PadMenu { channel: ch, pad }, Rect::new(input.x, input.y, 0.0, 0.0));
                }
            }
            self.drop_zone(input, pr, DropTarget::Pad { channel: ch, pad, add_layer: input.mods.shift });
        }
        let ring = rect_of(sel);
        let rx = self.anim.to(wid("pads-sel-x", ch, 0), ring.x, T_LAYOUT);
        let ry = self.anim.to(wid("pads-sel-y", ch, 0), ring.y, T_LAYOUT);
        c.stroke_rrect(Rect::new(rx, ry, PAD_W, pad_h).inset(-2.0), 11.0, 1.5, t.select);
    }

    /// The selected pad's waveform: start and decay handles, envelope, layers.
    fn pad_wave(&mut self, c: &mut Canvas, input: &Input, r: Rect, ch: usize, pad: usize, layer: usize) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let layers: Vec<String> = self.pad_layers(ch, pad).to_vec();
        let layer = layer.min(layers.len().saturating_sub(1));
        // Header: pad name, reverse toggle, layer chips.
        let reversed = self.pad_value(ch, pad_param(pad, S_REVERSE)) > 0.5;
        let rev = Rect::new(r.right() - 30.0, r.y + 4.0, 24.0, 24.0);
        if self.icon_button(c, input, wid("pads-rev", ch, pad), rev, Icon::Swap, reversed, ("Reverse", "")) {
            self.pad_action(ch, pad, PadOp::Reverse);
        }
        let mut chip_x = rev.x - 4.0;
        for k in (0..layers.len()).rev().filter(|_| layers.len() > 1) {
            let chip = Rect::new(chip_x - 22.0, r.y + 8.0, 22.0, 16.0);
            chip_x -= 25.0;
            let id = wid("pads-layer", ch, pad * LAYERS + k);
            let hov = self.hovered(input, id, chip);
            let on = k == layer;
            c.fill_rrect(chip, 5.0, if on { t.accent } else { t.raised.mix(t.raised_hi, f32::from(u8::from(hov))) });
            self.text_center(c, self.f.mono, 9.0, chip.x + 11.0, chip.y + 8.0, &format!("V{}", k + 1), if on { t.on_accent } else { t.text2 });
            if hov {
                self.set_tip(&format!("Velocity layer {} of {} (softest first): {}", k + 1, layers.len(), stem(&layers[k])), "");
                if input.pressed {
                    select(ch, pad, k);
                    let vel = ((k as f32 + 0.5) / layers.len() as f32).clamp(0.05, 1.0);
                    self.hit_pad(ch, pad, vel);
                }
            }
        }
        let title = layers.get(layer).map_or_else(|| PAD_NAMES[pad].to_string(), |p| stem(p));
        let title = self.fit(self.f.sans_med, 11.0, &title, chip_x - r.x - 14.0);
        self.text(c, self.f.sans_med, 11.0, r.x + 10.0, r.y + 16.0, &title, t.text2);

        let g = Rect::new(r.x + 8.0, r.y + 30.0, r.w - 16.0, r.h - 64.0);
        let kit = Rect::new(r.x + 8.0, r.bottom() - 30.0, 100.0, 24.0);
        self.kit_button(c, input, kit, ch);
        let Some(path) = layers.get(layer).cloned() else {
            self.empty_state(c, g, Icon::Sample, "Drop a WAV on a pad");
            return;
        };
        let (Some(peaks), Some(sample)) = (self.waveform(&path), self.samples.get(&path).cloned()) else { return };
        let id = |s| pad_param(pad, s);
        let start = self.pad_value(ch, id(S_START));
        let attack = self.pad_value(ch, id(S_ATTACK));
        let decay = self.pad_value(ch, id(S_DECAY));
        let ratio = tune_ratio(self.pad_value(ch, id(S_TUNE)), self.pad_value(ch, id(S_FINE))) as f32;
        // Played length in seconds, and the envelope over it.
        let sx = g.x + start * g.w;
        let span = (g.right() - sx).max(1.0);
        let played = (sample.duration() * (1.0 - start) / ratio).max(1e-3);
        let env = |x: f32| -> f32 {
            if x < sx {
                return 0.0;
            }
            let secs = (x - sx) / span * played;
            if secs < attack {
                return secs / attack.max(1e-6);
            }
            if decay >= DECAY_FULL { 1.0 } else { (-(secs - attack) * 6.9 / decay).exp() }
        };
        let mid = g.y + g.h * 0.5;
        let n = peaks.len().max(1);
        let cols = g.w as usize;
        for px in 0..cols {
            // Reversed: the file plays from its end, drawn right to left.
            let src = if reversed { cols - 1 - px } else { px };
            let a = src * n / cols;
            let b = ((src + 1) * n / cols).max(a + 1);
            let (lo, hi) = peaks[a.min(n - 1)..b.min(n)].iter().fold((0.0f32, 0.0f32), |(l, h), &(x, y)| (l.min(x), h.max(y)));
            let x = g.x + px as f32;
            let full = Rect::new(x, mid - hi * g.h * 0.5, 1.0, ((hi - lo) * g.h * 0.5).max(1.0));
            c.fill_rect(full, t.text2.alpha(0.18));
            let e = env(if reversed { g.x + (cols - 1 - px) as f32 } else { x });
            if e > 0.002 {
                let shaped = Rect::new(x, mid - hi * e * g.h * 0.5, 1.0, ((hi - lo) * e * g.h * 0.5).max(1.0));
                c.fill_rect(shaped, t.accent.alpha(0.8));
            }
        }
        // Envelope line.
        let pts: Vec<(f32, f32)> =
            (0..=cols / 2).map(|k| g.x + k as f32 * 2.0).filter(|&x| x >= sx).map(|x| (x, g.bottom() - env(x) * g.h)).collect();
        if pts.len() > 1 {
            c.stroke_polyline(&pts, 1.5, t.select.alpha(0.9));
        }

        // Start handle.
        let start_id = wid("pads-start", ch, pad);
        let hit = Rect::new(sx - 6.0, g.y - 4.0, 12.0, g.h + 8.0);
        let hov = self.hovered(input, start_id, hit);
        if hov && input.pressed {
            self.begin_drag(start_id, input, 0.0);
        }
        if let Some(p) = info(id(S_START))
            && self.is_dragging(start_id)
        {
            self.set_param_value(Device::Instrument(ch), p, ((input.x - g.x) / g.w).clamp(p.min, p.max));
        }
        let hv = self.hover_anim(start_id, hov || self.is_dragging(start_id));
        if hov {
            self.set_tip("Start: drag to skip the beginning of the sample", "");
            self.set_cursor(Cursor::ResizeH);
        }
        c.fill_rect(Rect::new(sx.round() - 1.0, g.y - 2.0, 2.0, g.h + 4.0), t.text.alpha(0.6 + 0.4 * hv));
        let mut tri = Shape::new();
        tri.polygon(&[(sx - 5.0, g.y - 4.0), (sx + 5.0, g.y - 4.0), (sx, g.y + 3.0)]);
        c.fill_path(&tri, t.text);

        // Decay handle: where the envelope falls to −60 dB (or the end: full length).
        let dx = if decay >= DECAY_FULL { g.right() } else { (sx + (attack + decay) / played * span).min(g.right()) };
        let dy = g.bottom() - env(dx.min(g.right() - 0.5)) * g.h;
        let decay_id = wid("pads-decay", ch, pad);
        let hit = Rect::new(dx - 8.0, g.y, 16.0, g.h);
        let hov = self.hovered(input, decay_id, hit);
        if hov && input.pressed {
            self.begin_drag(decay_id, input, 0.0);
        }
        if let Some(p) = info(id(S_DECAY))
            && self.is_dragging(decay_id)
        {
            let frac = (input.x - sx) / span;
            let v = if frac >= 0.98 { p.max } else { (frac * played - attack).clamp(p.min, DECAY_FULL - 0.01) };
            self.set_param_value(Device::Instrument(ch), p, v);
        }
        let hv = self.hover_anim(decay_id, hov || self.is_dragging(decay_id));
        if hov {
            self.set_tip("Decay: drag left to shorten the hit, all the way right for the full sample", "");
            self.set_cursor(Cursor::ResizeH);
        }
        let hy = dy.clamp(g.y + 5.0, g.bottom() - 5.0);
        c.fill_circle(dx.min(g.right() - 5.0), hy, 4.5 + hv * 1.5, t.bg_deep);
        c.stroke_circle(dx.min(g.right() - 5.0), hy, 4.5 + hv * 1.5, 1.6, t.select);

        // Footer: length and channels.
        let info_line = format!("{:.2} s · {}", sample.duration(), if sample.is_mono() { "mono" } else { "stereo" });
        self.text_right(c, self.f.mono, 10.0, r.right() - 10.0, kit.y + 12.0, &info_line, t.muted);
    }

    fn kit_button(&mut self, c: &mut Canvas, input: &Input, r: Rect, ch: usize) {
        let t = self.theme;
        let id = wid("pads-kit", ch, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 8.0, t.raised.mix(t.raised_hi, hv));
        draw_icon(c, Icon::Preset, r.x + 13.0, r.y + r.h * 0.5, 13.0, t.accent);
        self.text(c, self.f.sans_med, 11.0, r.x + 25.0, r.y + r.h * 0.5, "Starter kit", t.text2.mix(t.text, hv));
        if hov {
            self.set_tip("Load the starter kit on all sixteen pads", "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.load_starter_kit(ch);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pad_controls(&mut self, c: &mut Canvas, input: &Input, x: f32, body: Rect, device: Device, params: &'static [ParamInfo], pad: usize) {
        let t = self.theme;
        let find = |id: u32| params.iter().find(|p| p.id == id);
        c.fill_rect(Rect::new(x - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        let head = format!("Pad {} · {}", pad + 1, PAD_NAMES[pad]);
        self.text(c, self.f.sans_med, 11.0, x + 4.0, body.y + 6.0, &head, t.muted);
        let mx = x + 6.0 * CELL_W + GAP;
        c.fill_rect(Rect::new(mx - GAP * 0.5, body.y + 20.0, 1.0, body.h - 28.0), t.hairline);
        self.text(c, self.f.sans_med, 11.0, mx + 4.0, body.y + 6.0, "Master", t.muted);
        let mut cells: Vec<(Rect, u32)> = Vec::with_capacity(13);
        for (row, slots) in ROWS.iter().enumerate() {
            for (col, &slot) in slots.iter().enumerate() {
                let cell = Rect::new(x + col as f32 * CELL_W, body.y + 16.0 + row as f32 * CELL_H, CELL_W, CELL_H);
                cells.push((cell, pad_param(pad, slot)));
            }
        }
        cells.push((Rect::new(mx, body.y + 16.0, CELL_W, CELL_H), P_VOLUME));
        let key = device_key(device);
        for (cell, pid) in cells {
            let Some(p) = find(pid) else { continue };
            let v = self.param_value(device, p);
            let id = wid("prm", key, p.id as usize);
            let changed = if p.unit == Unit::Choice { self.choice(c, input, id, cell, p, v) } else { self.knob(c, input, id, cell, p, v) };
            if let Some(nv) = changed {
                self.set_param_value(device, p, nv);
            }
            let target = device_target(device, p.id);
            if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                self.open_popup(Popup::Param { target, value: v, default: p.default }, Rect::new(input.x, input.y, 0.0, 0.0));
            }
            if self.is_automated(target) {
                c.fill_circle(cell.right() - 10.0, cell.y + 6.0, 3.0, t.select);
            }
        }
    }
}

/// Peak level squeezed into `r`, one column per pixel, rising from the
/// bottom (a small pad has no room for both halves of the wave).
fn mini_wave(c: &mut Canvas, peaks: &[(f32, f32)], r: Rect, col: Color) {
    let n = peaks.len();
    let cols = r.w as usize;
    if n == 0 || cols == 0 {
        return;
    }
    let top = peaks.iter().fold(0.0f32, |m, &(lo, hi)| m.max(-lo).max(hi)).max(1e-3);
    for px in 0..cols {
        let a = px * n / cols;
        let b = ((px + 1) * n / cols).max(a + 1).min(n);
        let level = peaks[a.min(n - 1)..b].iter().fold(0.0f32, |m, &(lo, hi)| m.max(-lo).max(hi)) / top;
        let h = (level.sqrt() * r.h).max(1.0);
        c.fill_rect(Rect::new(r.x + px as f32, r.bottom() - h, 1.0, h), col);
    }
}

// --- Starter kit ---

/// Drum Machine pads rendered for the kit: note and file name.
const KIT_SOURCES: [(u8, &str); 8] = [
    (36, "kick"),
    (37, "rim"),
    (38, "snare"),
    (39, "clap"),
    (40, "open-hat"),
    (41, "low-tom"),
    (42, "closed-hat"),
    (43, "high-tom"),
];

/// Each pad: rendered source and tuning (semitones), General MIDI layout.
const KIT_MAP: [(usize, f32); PADS] = [
    (0, 0.0),  // Kick
    (1, 0.0),  // Rim
    (2, 0.0),  // Snare
    (3, 0.0),  // Clap
    (2, 3.0),  // Snare 2
    (5, -3.0), // Floor Tom
    (6, 0.0),  // Closed Hat
    (5, 0.0),  // Floor Tom 2
    (6, -2.0), // Pedal Hat
    (7, -4.0), // Low Tom
    (4, 0.0),  // Open Hat
    (7, -2.0), // Mid Tom
    (7, 0.0),  // Mid Tom 2
    (4, -5.0), // Crash
    (7, 3.0),  // High Tom
    (4, -9.0), // Ride
];

/// One Drum Machine hit, trimmed after it falls silent.
fn render_drum_machine(note: u8, name: &str) -> Sample {
    const SR: f32 = 48_000.0;
    let mut dm = DrumMachine::new();
    dm.set_sample_rate(SR);
    dm.note_on(note, 1.0);
    let n = (SR * 1.5) as usize;
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        dm.render(bl, br);
    }
    let end = l.iter().zip(&r).rposition(|(a, b)| a.abs().max(b.abs()) > 1e-4).map_or(0, |i| i + 1);
    let end = (end + (SR * 0.01) as usize).min(n);
    l.truncate(end);
    r.truncate(end);
    Sample { name: name.to_string(), sample_rate: SR, left: l, right: r, markers: Vec::new() }
}

/// Folders where the bundled sound library may live.
fn library_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(p) = std::env::var_os("PONZIK_LIBRARY") {
        roots.push(PathBuf::from(p));
    }
    roots.push(super::compat::projects_dir().join("Library"));
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        for rel in ["library", "../share/ponzik/library", "../../library", "../../assets/library"] {
            roots.push(dir.join(rel));
        }
    }
    roots
}

fn library_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 || out.len() > 4_000 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for root in library_roots() {
        walk(&root, 0, &mut out);
    }
    out.sort();
    out
}

/// Words that identify each pad's sound in a file name.
const KIT_WORDS: [&[&str]; PADS] = [
    &["kick", "bd"],
    &["rim", "stick"],
    &["snare", "sd"],
    &["clap"],
    &[],
    &[],
    &["closed hat", "closed-hat", "closed_hat", "closedhat", "chh", "hat closed", "hh closed", "hihat"],
    &[],
    &["pedal"],
    &[],
    &["open hat", "open-hat", "open_hat", "openhat", "ohh", "hat open", "hh open"],
    &[],
    &[],
    &["crash", "cymbal"],
    &[],
    &["ride"],
];
/// Tom pads, lowest first.
const TOM_PADS: [usize; 6] = [5, 7, 9, 11, 12, 14];

/// Library files matched to pads by name; toms are spread over the tom pads.
fn kit_from_library(files: &[PathBuf]) -> [Option<PathBuf>; PADS] {
    let mut out: [Option<PathBuf>; PADS] = Default::default();
    let names: Vec<String> = files.iter().map(|f| f.file_stem().map_or(String::new(), |s| s.to_string_lossy().to_lowercase())).collect();
    let has_word = |name: &str, w: &str| {
        // Short tags (bd, sd, chh…) must stand alone; longer words may be part of a name.
        if w.len() > 3 {
            return name.contains(w);
        }
        name.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == w)
    };
    let mut used = vec![false; files.len()];
    // Closed hats match any "hihat", so open and pedal hats pick first.
    for pad in [10, 8, 13, 15, 0, 1, 2, 3, 6] {
        let words = KIT_WORDS[pad];
        let hit = (0..files.len()).find(|&i| !used[i] && words.iter().any(|w| has_word(&names[i], w)));
        if let Some(i) = hit {
            used[i] = true;
            out[pad] = Some(files[i].clone());
        }
    }
    let toms: Vec<usize> = (0..files.len()).filter(|&i| !used[i] && has_word(&names[i], "tom")).collect();
    for (k, &pad) in TOM_PADS.iter().enumerate() {
        if let Some(&i) = toms.get(k) {
            out[pad] = Some(files[i].clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drum_machine_renders_are_trimmed_hits() {
        let kick = render_drum_machine(36, "kick");
        let hat = render_drum_machine(42, "closed-hat");
        assert!(kick.left.iter().any(|s| s.abs() > 0.1));
        assert!(hat.len() < kick.len(), "a closed hat is shorter than a kick");
        assert!(kick.len() < (48_000.0 * 1.5) as usize);
    }

    #[test]
    fn library_files_land_on_their_pads() {
        let files: Vec<PathBuf> = [
            "lib/drums/Kick 01.wav",
            "lib/drums/Snare Tight.wav",
            "lib/drums/Hihat Closed.wav",
            "lib/drums/Open Hat.wav",
            "lib/drums/Tom Low.wav",
            "lib/drums/Tom High.wav",
            "lib/fx/Sdrone.wav",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let kit = kit_from_library(&files);
        let name = |p: usize| kit[p].as_ref().and_then(|f| f.file_stem()).map(|s| s.to_string_lossy().into_owned());
        assert_eq!(name(0).as_deref(), Some("Kick 01"));
        assert_eq!(name(2).as_deref(), Some("Snare Tight"));
        assert_eq!(name(6).as_deref(), Some("Hihat Closed"));
        assert_eq!(name(10).as_deref(), Some("Open Hat"));
        assert_eq!(name(5).as_deref(), Some("Tom Low"));
        assert_eq!(name(7).as_deref(), Some("Tom High"));
        assert_eq!(name(3), None);
        // "sd" alone is a snare tag, not a part of "Sdrone".
        assert!(kit.iter().flatten().all(|f| !f.to_string_lossy().contains("Sdrone")));
    }

    #[test]
    fn kit_map_covers_every_pad() {
        assert!(KIT_MAP.iter().all(|&(src, tune)| src < KIT_SOURCES.len() && tune.abs() <= 24.0));
        // The hats of the kit are in the hat choke group by default.
        let hats = [6, 8, 10];
        for pad in hats {
            let p = info(pad_param(pad, S_CHOKE)).unwrap();
            assert_eq!(p.default, 1.0);
        }
    }
}
