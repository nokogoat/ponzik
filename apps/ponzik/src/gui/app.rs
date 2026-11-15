//! Écran principal : transport, Channel Rack, mixer, sortie.

#![allow(clippy::too_many_arguments)]

use std::f32::consts::PI;

use ponzik_engine::{Channel, Command, EngineHandle, Step};
use ponzik_ui::{Anim, Canvas, Color, FontId, Fonts, Path, Rect, embedded};
use ponzik_window::Key;

use super::Input;
use super::theme::*;

const STEPS: usize = 16;
const SCOPE_LEN: usize = 8_192;
/// Fenêtre affichée par l'oscilloscope, en échantillons à 24 kHz (≈ 43 ms).
const SCOPE_WINDOW: usize = 1_024;
const DB_TICKS: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];

/// Copie, côté interface, de l'état d'un canal du moteur.
pub struct ChannelView {
    pub name: String,
    pub instrument: &'static str,
    pub melodic: bool,
    pub steps: Vec<Option<Step>>,
    pub root_note: u8,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
}

impl ChannelView {
    pub fn from_channel(c: &Channel) -> Self {
        Self {
            name: c.name.clone(),
            instrument: c.instrument.name(),
            melodic: c.instrument.is_melodic(),
            steps: c.steps[..STEPS].to_vec(),
            root_note: c.root_note,
            gain: c.gain,
            pan: c.pan,
            muted: c.muted,
        }
    }
}

/// VU-mètre crête : montée instantanée, retombée à 20 dB/s, maintien 1,2 s.
#[derive(Default, Clone, Copy)]
struct Meter {
    level: [f32; 2],
    hold: [f32; 2],
    hold_age: [f32; 2],
}

impl Meter {
    fn feed(&mut self, peak: [f32; 2], dt: f32) {
        for (i, &p) in peak.iter().enumerate() {
            self.level[i] = p.max(self.level[i] * 10f32.powf(-dt));
            if p >= self.hold[i] {
                self.hold[i] = p;
                self.hold_age[i] = 0.0;
            } else {
                self.hold_age[i] += dt;
                if self.hold_age[i] > 1.2 {
                    self.hold[i] *= 10f32.powf(-1.5 * dt);
                }
            }
        }
    }

    fn active(&self) -> bool {
        self.level.iter().chain(&self.hold).any(|&v| v > 1e-4)
    }

    fn peak(&self) -> f32 {
        self.level[0].max(self.level[1])
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Channel(usize),
    Master,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Paint { channel: usize, on: bool },
    Fader { target: Target, grab: f32 },
    Pan { channel: usize, start_y: f32, start: f32 },
    Bpm { start_y: f32, start: f32 },
}

struct FontSet {
    sans: FontId,
    sans_med: FontId,
    mono: FontId,
    mono_med: FontId,
}

// Familles d'identifiants d'animation.
const A_PLAY_HOVER: u64 = 1;
const A_PLAY_PRESS: u64 = 2;
const A_BPM_HOVER: u64 = 3;
const A_BEAT_LED: u64 = 4;
const A_STEP_LED: u64 = 5;
const A_PAD_HOVER: u64 = 6;
const A_PAD_HEAD: u64 = 7;
const A_FLASH: u64 = 8;
const A_MUTE_HOVER: u64 = 9;
const A_CAP_HOVER: u64 = 10;
const A_KNOB_HOVER: u64 = 11;
const A_STRIP_MUTE: u64 = 12;

fn id(kind: u64, a: usize, b: usize) -> u64 {
    kind << 48 | (a as u64) << 24 | b as u64
}

/// Position de fader (0..1) ↔ gain linéaire, loi cubique : 0 dB vers 79 %, +6 dB en haut.
fn pos_to_gain(pos: f32) -> f32 {
    2.0 * pos.clamp(0.0, 1.0).powi(3)
}

fn gain_to_pos(gain: f32) -> f32 {
    (gain.max(0.0) / 2.0).cbrt().min(1.0)
}

fn db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}

/// « −3,1 dB », signe moins typographique, virgule décimale.
fn fmt_db(gain: f32) -> String {
    let d = db(gain);
    if d < -90.0 {
        return "−∞ dB".into();
    }
    let s = format!("{:.1}", d.abs()).replace('.', ",");
    if d < -0.05 {
        format!("−{s} dB")
    } else if d > 0.05 {
        format!("+{s} dB")
    } else {
        "0,0 dB".into()
    }
}

fn fmt_tick(d: f32) -> String {
    match d {
        d if d > 0.0 => format!("+{d:.0}"),
        d if d < 0.0 => format!("−{:.0}", -d),
        _ => "0".into(),
    }
}

fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[note as usize % 12], note as i32 / 12 - 1)
}

fn fmt_pan(pan: f32) -> String {
    let v = (pan * 100.0).round() as i32;
    match v {
        0 => "C".into(),
        v if v < 0 => format!("G {}", -v),
        v => format!("D {v}"),
    }
}

pub struct App {
    engine: EngineHandle,
    channels: Vec<ChannelView>,
    bpm: f32,
    master_gain: f32,
    playing: bool,
    playhead: Option<usize>,
    meters: Vec<Meter>,
    master_meter: Meter,
    scope: Vec<f32>,
    scope_at: usize,
    load: f32,
    anim: Anim,
    drag: Drag,
    fonts: Fonts,
    f: FontSet,
    sample_rate: u32,
    device: String,
    project: String,
}

impl App {
    pub fn new(
        engine: EngineHandle,
        channels: Vec<ChannelView>,
        bpm: f32,
        sample_rate: u32,
        device: String,
        project: String,
    ) -> Self {
        let mut fonts = Fonts::new();
        let mut load = |data| fonts.add(data).expect("police embarquée invalide");
        let f = FontSet {
            sans: load(embedded::SANS_REGULAR),
            sans_med: load(embedded::SANS_MEDIUM),
            mono: load(embedded::MONO_REGULAR),
            mono_med: load(embedded::MONO_MEDIUM),
        };
        Self {
            engine,
            meters: vec![Meter::default(); channels.len()],
            channels,
            bpm,
            master_gain: 0.5,
            playing: false,
            playhead: None,
            master_meter: Meter::default(),
            scope: vec![0.0; SCOPE_LEN],
            scope_at: 0,
            load: 0.0,
            anim: Anim::new(),
            drag: Drag::None,
            fonts,
            f,
            sample_rate,
            device,
            project,
        }
    }

    fn send(&mut self, cmd: Command) {
        if self.engine.send(cmd).is_err() {
            eprintln!("file de commandes pleine : {cmd:?}");
        }
    }

    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.send(if self.playing { Command::Play } else { Command::Stop });
        if !self.playing {
            self.playhead = None;
        }
    }

    fn set_bpm(&mut self, bpm: f32) {
        let bpm = (bpm * 10.0).round() / 10.0;
        let bpm = bpm.clamp(40.0, 300.0);
        if bpm != self.bpm {
            self.bpm = bpm;
            self.send(Command::SetBpm(bpm));
        }
    }

    fn set_step(&mut self, ch: usize, step: usize, on: bool) {
        let c = &mut self.channels[ch];
        let value = on.then_some(Step { note: c.root_note, velocity: 0.8, length: 1.0 });
        if c.steps[step].is_some() == on {
            return;
        }
        c.steps[step] = value;
        self.send(Command::SetStep { channel: ch, step, value });
    }

    fn toggle_mute(&mut self, ch: usize) {
        let muted = !self.channels[ch].muted;
        self.channels[ch].muted = muted;
        self.send(Command::SetChannelMute { channel: ch, muted });
    }

    fn set_gain(&mut self, target: Target, gain: f32) {
        match target {
            Target::Channel(ch) => {
                self.channels[ch].gain = gain;
                self.send(Command::SetChannelGain { channel: ch, gain });
            }
            Target::Master => {
                self.master_gain = gain;
                self.send(Command::SetMasterGain(gain));
            }
        }
    }

    fn gain_of(&self, target: Target) -> f32 {
        match target {
            Target::Channel(ch) => self.channels[ch].gain,
            Target::Master => self.master_gain,
        }
    }

    /// Faux quand tout est immobile : la boucle peut dormir.
    pub fn needs_redraw(&self) -> bool {
        self.playing
            || self.anim.is_moving()
            || self.drag != Drag::None
            || self.master_meter.active()
            || self.meters.iter().any(Meter::active)
    }

    fn hover(&self, input: &Input, r: Rect) -> bool {
        self.drag == Drag::None && input.inside && r.contains(input.x, input.y)
    }

    fn poll_engine(&mut self, dt: f32) {
        let head = if self.playing { self.engine.playhead() } else { None };
        if head != self.playhead {
            if let Some(i) = head {
                for (ci, ch) in self.channels.iter().enumerate() {
                    if !ch.muted && ch.steps[i].is_some() {
                        self.anim.set(id(A_FLASH, ci, i), 1.0);
                    }
                }
            }
            self.playhead = head;
        }
        for (i, m) in self.meters.iter_mut().enumerate() {
            m.feed(self.engine.take_channel_peak(i), dt);
        }
        self.master_meter.feed(self.engine.take_master_peak(), dt);
        let load = self.engine.take_load();
        self.load += (load - self.load) * (1.0 - (-dt / 0.4).exp());
        let (scope, at) = (&mut self.scope, &mut self.scope_at);
        self.engine.drain_scope(|s| {
            scope[*at] = s;
            *at = (*at + 1) % SCOPE_LEN;
        });
    }

    fn handle_keys(&mut self, input: &Input) {
        for &(key, mods) in &input.keys {
            match key {
                Key::Space => self.toggle_play(),
                Key::Up => self.set_bpm(self.bpm + if mods.shift { 0.1 } else { 1.0 }),
                Key::Down => self.set_bpm(self.bpm - if mods.shift { 0.1 } else { 1.0 }),
                _ => {}
            }
        }
    }

    pub fn frame(&mut self, c: &mut Canvas, input: &Input, dt: f32) {
        self.anim.begin_frame(dt);
        self.poll_engine(dt);
        self.handle_keys(input);

        let (w, h) = (c.width() as f32, c.height() as f32);
        c.set_clip(None);
        c.clear(BG);
        self.top_bar(c, input, Rect::new(0.0, 0.0, w, TOP_BAR));

        let content = Rect::new(PAGE, TOP_BAR + PAGE, w - 2.0 * PAGE, h - TOP_BAR - 2.0 * PAGE);
        let (rack, rest) = content.take_top(self.rack_height());
        self.rack(c, input, rack);

        let bottom = rest.take_top(SECTION_GAP).1;
        let n = self.channels.len() as f32;
        let mixer_w = 2.0 * CARD_PAD + n * STRIP_W + (n - 1.0) * STRIP_GAP + 24.0 + MASTER_W;
        if bottom.w - mixer_w - SECTION_GAP >= 300.0 {
            let (mixer, rest) = bottom.take_left(mixer_w);
            self.mixer(c, input, mixer);
            self.output(c, rest.take_left(SECTION_GAP).1);
        } else {
            self.mixer(c, input, bottom);
        }

        if !input.down {
            self.drag = Drag::None;
        }
    }

    // --- Texte ---

    fn text(&mut self, c: &mut Canvas, f: FontId, size: f32, x: f32, cy: f32, s: &str, col: Color) -> f32 {
        let baseline = cy + self.fonts.cap_height(f, size) * 0.5;
        self.fonts.draw(c, f, size, x, baseline, s, col)
    }

    fn text_right(&mut self, c: &mut Canvas, f: FontId, size: f32, right: f32, cy: f32, s: &str, col: Color) -> f32 {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, right - w, cy, s, col);
        right - w
    }

    fn text_center(&mut self, c: &mut Canvas, f: FontId, size: f32, cx: f32, cy: f32, s: &str, col: Color) {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, cx - w * 0.5, cy, s, col);
    }

    fn card(c: &mut Canvas, r: Rect) {
        c.fill_rrect(r, CARD_RADIUS, CARD);
        c.stroke_rrect(r, CARD_RADIUS, 1.0, HAIRLINE);
    }

    /// En-tête de carte : titre à gauche, méta à droite ; renvoie le corps.
    fn card_header(&mut self, c: &mut Canvas, inner: Rect, title: &str, meta: &str) -> Rect {
        let cy = inner.y + 12.0;
        self.text(c, self.f.sans_med, 15.0, inner.x, cy, title, TEXT);
        self.text_right(c, self.f.mono, 12.0, inner.right(), cy, meta, MUTED);
        inner.take_top(24.0 + 16.0).1
    }

    // --- Barre de transport ---

    fn top_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        c.fill_rect(r, BG_DEEP);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), HAIRLINE);
        let cy = r.y + r.h * 0.5;

        let cluster = 48.0 + 12.0 + 136.0 + 12.0 + 164.0;
        let cluster_x = ((r.w - cluster) * 0.5).round();

        // Les textes latéraux s'effacent plutôt que de passer sous le transport.
        let wordmark = self.text(c, self.f.sans_med, 18.0, PAGE, cy, "Ponzik", TEXT);
        let div = PAGE + wordmark + 16.0;
        let project = self.project.clone();
        if div + 16.0 + self.fonts.measure(self.f.sans, 14.0, &project) + 24.0 <= cluster_x {
            c.fill_rect(Rect::new(div, cy - 10.0, 1.0, 20.0), HAIRLINE);
            self.text(c, self.f.sans, 14.0, div + 16.0, cy, &project, TEXT2);
        }

        let mut x = cluster_x;
        self.play_button(c, input, Rect::new(x, cy - 20.0, 48.0, 40.0));
        x += 48.0 + 12.0;
        self.bpm_field(c, input, Rect::new(x, cy - 20.0, 136.0, 40.0));
        x += 136.0 + 12.0;
        self.position_field(c, Rect::new(x, cy - 20.0, 164.0, 40.0));
        let free_from = cluster_x + cluster + 24.0;

        let device = format!("{} kHz · {}", self.sample_rate / 1000, self.device);
        let dsp = format!("DSP {:>2.0} %", (self.load * 100.0).min(999.0));
        let device_w = self.fonts.measure(self.f.sans, 13.0, &device);
        let dsp_w = self.fonts.measure(self.f.mono, 13.0, &dsp);
        let right = r.w - PAGE;
        if right - device_w - 24.0 - dsp_w >= free_from {
            let left = self.text_right(c, self.f.sans, 13.0, right, cy, &device, MUTED);
            self.text_right(c, self.f.mono, 13.0, left - 24.0, cy, &dsp, MUTED);
        } else if right - dsp_w >= free_from {
            self.text_right(c, self.f.mono, 13.0, right, cy, &dsp, MUTED);
        }
    }

    fn play_button(&mut self, c: &mut Canvas, input: &Input, b: Rect) {
        let hov = self.hover(input, b);
        if hov && input.pressed {
            self.toggle_play();
        }
        let hv = self.anim.to(id(A_PLAY_HOVER, 0, 0), hov as u8 as f32, T_HOVER);
        let scale = self.anim.to(id(A_PLAY_PRESS, 0, 0), if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let r = b.scale(scale);
        let fill = if self.playing { AMBER } else { TEXT };
        c.fill_rrect(r, 10.0, fill.mix(WHITE, 0.12 * hv));
        c.fill_rect(Rect::new(r.x + 8.0, r.y + 1.0, r.w - 16.0, 1.0), WHITE.alpha(0.35));
        let (cx, cy) = r.center();
        if self.playing {
            c.fill_rrect(Rect::new(cx - 6.5, cy - 6.5, 13.0, 13.0), 2.0, ON_AMBER);
        } else {
            let mut p = Path::new();
            p.polygon(&[(cx - 5.0, cy - 8.0), (cx + 8.5, cy), (cx - 5.0, cy + 8.0)]);
            c.fill_path(&p, BG_DEEP);
        }
    }

    fn bpm_field(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let hov = self.hover(input, r);
        if hov && input.pressed {
            self.drag = Drag::Bpm { start_y: input.y, start: self.bpm };
        }
        if let Drag::Bpm { start_y, start } = self.drag {
            let per_px = if input.mods.shift { 0.02 } else { 0.25 };
            self.set_bpm(start + (start_y - input.y) * per_px);
        }
        if hov && input.wheel != 0.0 {
            self.set_bpm(self.bpm + input.wheel * if input.mods.shift { 0.1 } else { 1.0 });
        }
        let active = hov || matches!(self.drag, Drag::Bpm { .. });
        let hv = self.anim.to(id(A_BPM_HOVER, 0, 0), active as u8 as f32, T_HOVER);
        c.fill_rrect(r, 10.0, RAISED.mix(RAISED_HI, hv));
        c.stroke_rrect(r, 10.0, 1.0, HAIRLINE);
        let (_, cy) = r.center();
        let bpm = format!("{:.1}", self.bpm).replace('.', ",");
        self.text(c, self.f.mono_med, 20.0, r.x + 16.0, cy, &bpm, TEXT);
        self.text_right(c, self.f.sans, 12.0, r.right() - 14.0, cy, "BPM", MUTED);
    }

    fn position_field(&mut self, c: &mut Canvas, r: Rect) {
        c.fill_rrect(r, 10.0, RAISED);
        c.stroke_rrect(r, 10.0, 1.0, HAIRLINE);
        let (_, cy) = r.center();
        for beat in 0..4 {
            let lit = self.playhead.is_some_and(|s| s / 4 == beat);
            let v = self.anim.to(id(A_BEAT_LED, beat, 0), lit as u8 as f32, T_HEAD);
            let x = r.x + 18.0 + beat as f32 * 16.0;
            c.fill_circle(x, cy, 4.0, RAISED_HI.mix(AMBER, v));
        }
        let pos = match self.playhead {
            Some(s) => format!("{:02} / {STEPS}", s + 1),
            None => format!("–– / {STEPS}"),
        };
        let col = if self.playing { TEXT } else { TEXT2 };
        self.text_right(c, self.f.mono, 15.0, r.right() - 14.0, cy, &pos, col);
    }

    // --- Channel Rack ---

    fn rack_height(&self) -> f32 {
        let n = self.channels.len() as f32;
        2.0 * CARD_PAD + 24.0 + 16.0 + 28.0 + 8.0 + n * ROW_H + (n - 1.0).max(0.0) * ROW_GAP
    }

    fn pad_x(pads: Rect, i: usize) -> (f32, f32) {
        let gaps = 12.0 * PAD_GAP + 3.0 * GROUP_GAP;
        let w = ((pads.w - gaps) / STEPS as f32).floor();
        let group = i / 4;
        let x = pads.x + i as f32 * w + (i - group) as f32 * PAD_GAP + group as f32 * GROUP_GAP;
        (x, w)
    }

    fn rack(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        Self::card(c, r);
        let inner = r.inset(CARD_PAD);
        let meta = format!("{} canaux · {STEPS} pas", self.channels.len());
        let body = self.card_header(c, inner, "Channel Rack", &meta);
        let pads = Rect::new(body.x + NAME_COL, body.y, body.w - NAME_COL, body.h);

        // Numéros de pas, et la LED qui court au-dessus.
        for i in 0..STEPS {
            let (x, w) = Self::pad_x(pads, i);
            let cx = x + w * 0.5;
            let here = self.playhead == Some(i);
            let led = self.anim.to(id(A_STEP_LED, i, 0), here as u8 as f32, T_HEAD);
            if led > 0.01 {
                c.fill_circle(cx, body.y + 4.0, 3.0, AMBER.alpha(led));
            }
            let col = if here {
                TEXT
            } else if i % 4 == 0 {
                TEXT2
            } else {
                MUTED
            };
            self.text_center(c, self.f.mono, 12.0, cx, body.y + 20.0, &(i + 1).to_string(), col);
        }

        let mut y = body.y + 28.0 + 8.0;
        for ci in 0..self.channels.len() {
            self.channel_row(c, input, Rect::new(body.x, y, body.w, ROW_H), pads, ci);
            y += ROW_H + ROW_GAP;
        }
    }

    fn channel_row(&mut self, c: &mut Canvas, input: &Input, row: Rect, pads: Rect, ci: usize) {
        let cy = row.y + row.h * 0.5;

        // Interrupteur du canal : la LED est allumée quand le canal joue.
        let mb = Rect::new(row.x, cy - 16.0, 32.0, 32.0);
        let hov = self.hover(input, mb);
        if hov && input.pressed {
            self.toggle_mute(ci);
        }
        let hv = self.anim.to(id(A_MUTE_HOVER, ci, 0), hov as u8 as f32, T_HOVER);
        c.fill_rrect(mb, 8.0, RAISED.mix(RAISED_HI, hv));
        let (mx, my) = mb.center();
        if self.channels[ci].muted {
            c.stroke_circle(mx, my, 5.0, 1.5, MUTED);
        } else {
            c.fill_circle(mx, my, 5.0, TEXT);
        }

        let (name, instrument, muted) = {
            let ch = &self.channels[ci];
            (ch.name.clone(), ch.instrument, ch.muted)
        };
        self.text(c, self.f.sans_med, 15.0, row.x + 48.0, cy - 8.0, &name, if muted { TEXT2 } else { TEXT });
        self.text(c, self.f.sans, 12.0, row.x + 48.0, cy + 11.0, instrument, MUTED);

        for i in 0..STEPS {
            let (x, w) = Self::pad_x(pads, i);
            let pad = Rect::new(x, cy - PAD_H * 0.5, w, PAD_H);
            self.pad(c, input, pad, ci, i);
        }
    }

    fn pad(&mut self, c: &mut Canvas, input: &Input, pad: Rect, ci: usize, i: usize) {
        let over = input.inside && pad.contains(input.x, input.y);
        let hov = over && (self.drag == Drag::None || matches!(self.drag, Drag::Paint { channel, .. } if channel == ci));
        if hov && input.pressed && self.drag == Drag::None {
            let on = self.channels[ci].steps[i].is_none();
            self.set_step(ci, i, on);
            self.drag = Drag::Paint { channel: ci, on };
        } else if let Drag::Paint { channel, on } = self.drag
            && channel == ci
            && hov
            && input.down
        {
            // Peindre en glissant, comme sur une boîte à rythmes.
            self.set_step(ci, i, on);
        }
        if hov && input.right_pressed {
            self.set_step(ci, i, false);
        }

        let hv = self.anim.to(id(A_PAD_HOVER, ci, i), hov as u8 as f32, T_HOVER);
        let head = self.anim.to(id(A_PAD_HEAD, ci, i), (self.playhead == Some(i)) as u8 as f32, T_HEAD);
        let flash = self.anim.to(id(A_FLASH, ci, i), 0.0, T_FLASH);
        let ch = &self.channels[ci];

        match ch.steps[i] {
            Some(step) => {
                let (mut top, mut bottom) = (AMBER, AMBER_DEEP);
                if ch.muted {
                    top = top.mix(RAISED_HI, 0.72);
                    bottom = bottom.mix(RAISED_HI, 0.72);
                }
                top = top.mix(AMBER_HOT, (flash * 0.9 + hv * 0.18).min(1.0));
                bottom = bottom.mix(AMBER_HOT, (flash * 0.6 + hv * 0.12).min(1.0));
                c.fill_rrect_vgrad(pad, 8.0, top, bottom);
                c.fill_rect(Rect::new(pad.x + 8.0, pad.y + 1.0, pad.w - 16.0, 1.0), WHITE.alpha(0.32));
                if ch.melodic {
                    let label = note_name(step.note);
                    let ink = if ch.muted { TEXT2 } else { ON_AMBER };
                    let (cx, cy) = pad.center();
                    self.text_center(c, self.f.mono_med, 12.0, cx, cy, &label, ink);
                }
            }
            None => {
                let base = if (i / 4) % 2 == 0 { PAD_A } else { PAD_B };
                c.fill_rrect(pad, 8.0, base.mix(RAISED_HI, hv * 0.8).mix(PAD_HEAD, head * 0.6));
            }
        }
    }

    // --- Mixer ---

    fn mixer(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        Self::card(c, r);
        let inner = r.inset(CARD_PAD);
        let meta = format!("{} pistes · master", self.channels.len());
        let body = self.card_header(c, inner, "Mixer", &meta);
        let mut x = body.x;
        for ci in 0..self.channels.len() {
            self.strip(c, input, Rect::new(x, body.y, STRIP_W, body.h), Target::Channel(ci));
            x += STRIP_W + STRIP_GAP;
        }
        x += 24.0 - STRIP_GAP;
        self.strip(c, input, Rect::new(x, body.y, MASTER_W, body.h), Target::Master);
    }

    fn strip(&mut self, c: &mut Canvas, input: &Input, r: Rect, target: Target) {
        c.fill_rrect(r, 12.0, STRIP);
        c.stroke_rrect(r, 12.0, 1.0, HAIRLINE);
        let inner = r.inset(12.0);
        let name = match target {
            Target::Channel(ci) => self.channels[ci].name.clone(),
            Target::Master => "Master".into(),
        };
        self.text_center(c, self.f.sans_med, 14.0, inner.x + inner.w * 0.5, inner.y + 8.0, &name, TEXT);

        let knob_row = Rect::new(inner.x, inner.y + 28.0, inner.w, 44.0);
        match target {
            Target::Channel(ci) => self.pan_knob(c, input, knob_row, ci),
            Target::Master => {
                let (_, cy) = knob_row.center();
                self.text(c, self.f.sans, 12.0, knob_row.x, cy, "Crête", MUTED);
                let peak = fmt_db(self.master_meter.peak());
                let peak = peak.trim_end_matches(" dB");
                self.text_right(c, self.f.mono_med, 15.0, knob_row.right(), cy, peak, TEXT);
            }
        }

        let mute = Rect::new(inner.x, inner.bottom() - 28.0, inner.w, 28.0);
        let readout_cy = mute.y - 8.0 - 8.0;
        let fader = Rect::new(inner.x, knob_row.bottom() + 16.0, inner.w, readout_cy - 8.0 - 12.0 - knob_row.bottom() - 16.0);
        self.fader(c, input, fader, target);

        let gain = self.gain_of(target);
        self.text_center(c, self.f.mono, 13.0, inner.x + inner.w * 0.5, readout_cy, &fmt_db(gain), TEXT2);

        if let Target::Channel(ci) = target {
            let hov = self.hover(input, mute);
            if hov && input.pressed {
                self.toggle_mute(ci);
            }
            let hv = self.anim.to(id(A_STRIP_MUTE, ci, 0), hov as u8 as f32, T_HOVER);
            let muted = self.channels[ci].muted;
            let (fill, ink) = if muted { (TEXT.mix(WHITE, hv * 0.2), BG_DEEP) } else { (RAISED.mix(RAISED_HI, hv), TEXT2) };
            c.fill_rrect(mute, 8.0, fill);
            if !muted {
                c.stroke_rrect(mute, 8.0, 1.0, HAIRLINE);
            }
            let (mx, my) = mute.center();
            self.text_center(c, self.f.sans_med, 13.0, mx, my, "Mute", ink);
        }
    }

    fn pan_knob(&mut self, c: &mut Canvas, input: &Input, row: Rect, ci: usize) {
        let (cx, cy) = (row.x + 22.0, row.y + row.h * 0.5);
        let hit = Rect::new(row.x, row.y, 44.0, 44.0);
        let hov = self.hover(input, hit);
        if hov && input.pressed {
            if input.mods.ctrl || input.mods.alt {
                self.set_pan(ci, 0.0);
            } else {
                self.drag = Drag::Pan { channel: ci, start_y: input.y, start: self.channels[ci].pan };
            }
        }
        if let Drag::Pan { channel, start_y, start } = self.drag
            && channel == ci
        {
            let per_px = if input.mods.shift { 0.002 } else { 0.01 };
            self.set_pan(ci, start + (start_y - input.y) * per_px);
        }
        if hov && input.wheel != 0.0 {
            self.set_pan(ci, self.channels[ci].pan + input.wheel * 0.05);
        }
        let active = hov || matches!(self.drag, Drag::Pan { channel, .. } if channel == ci);
        let hv = self.anim.to(id(A_KNOB_HOVER, ci, 0), active as u8 as f32, T_HOVER);

        // Course de 270° : de 135° (bas gauche) à 405° (bas droite), centre en haut.
        let pan = self.channels[ci].pan;
        let (a_min, a_mid) = (0.75 * PI, 1.5 * PI);
        let a = a_mid + pan * 0.75 * PI;
        c.stroke_arc(cx, cy, 19.0, a_min, a_min + 1.5 * PI, 3.0, RAISED_HI);
        if pan.abs() > 0.005 {
            c.stroke_arc(cx, cy, 19.0, a_mid.min(a), a_mid.max(a), 3.0, TEXT2);
        }
        c.fill_circle(cx, cy, 13.0, RAISED_HI.mix(Color::hex(0x36363a), hv));
        c.stroke_circle(cx, cy, 13.0, 1.0, HAIRLINE);
        let (dx, dy) = (a.cos(), a.sin());
        c.stroke_polyline(&[(cx + dx * 4.0, cy + dy * 4.0), (cx + dx * 10.0, cy + dy * 10.0)], 2.0, TEXT);

        let label = fmt_pan(pan);
        self.text_right(c, self.f.mono, 12.0, row.right(), cy, &label, TEXT2);
    }

    fn set_pan(&mut self, ci: usize, pan: f32) {
        let pan = (pan.clamp(-1.0, 1.0) * 100.0).round() / 100.0;
        if pan != self.channels[ci].pan {
            self.channels[ci].pan = pan;
            self.send(Command::SetChannelPan { channel: ci, pan });
        }
    }

    fn fader(&mut self, c: &mut Canvas, input: &Input, area: Rect, target: Target) {
        const CAP_W: f32 = 36.0;
        const CAP_H: f32 = 20.0;
        let travel = (area.h - CAP_H).max(1.0);
        let y_of = |pos: f32| area.bottom() - CAP_H * 0.5 - pos * travel;
        let master = target == Target::Master;
        let track_x = area.x + 46.0;

        let gain = self.gain_of(target);
        let pos = gain_to_pos(gain);
        let cap = Rect::new(track_x + 2.0 - CAP_W * 0.5, y_of(pos) - CAP_H * 0.5, CAP_W, CAP_H);
        let zone = Rect::new(area.x + 28.0, area.y, 40.0, area.h);
        debug_assert!(zone.contains(track_x, area.y + 1.0));

        let hov_zone = self.hover(input, zone);
        if hov_zone && input.pressed {
            if input.mods.ctrl || input.mods.alt {
                self.set_gain(target, 1.0);
            } else {
                // Clic sur le capuchon : on le saisit ; ailleurs : il saute sous la souris.
                let grab = if cap.contains(input.x, input.y) { input.y - y_of(pos) } else { 0.0 };
                self.drag = Drag::Fader { target, grab };
            }
        }
        if let Drag::Fader { target: t, grab } = self.drag
            && t == target
        {
            let p = ((area.bottom() - CAP_H * 0.5 - (input.y - grab)) / travel).clamp(0.0, 1.0);
            self.set_gain(target, pos_to_gain(p));
        }
        if hov_zone && input.wheel != 0.0 {
            self.set_gain(target, pos_to_gain((pos + input.wheel * 0.02).clamp(0.0, 1.0)));
        }

        // Graduations.
        for d in DB_TICKS {
            let y = y_of(gain_to_pos(10f32.powf(d / 20.0)));
            c.fill_rect(Rect::new(track_x - 10.0, y.round(), 5.0, 1.0), HAIRLINE.alpha(2.0));
            let label = fmt_tick(d);
            self.text_right(c, self.f.mono, 11.0, area.x + 24.0, y, &label, MUTED);
        }

        // Rail.
        c.fill_rrect(Rect::new(track_x, area.y + CAP_H * 0.5, 4.0, travel), 2.0, WELL);

        // VU-mètre stéréo, à la même échelle que le fader.
        let meter = match target {
            Target::Channel(ci) => self.meters[ci],
            Target::Master => self.master_meter,
        };
        let (bar_w, bar_gap) = if master { (6.0, 3.0) } else { (4.0, 2.0) };
        let warn = y_of(gain_to_pos(10f32.powf(-6.0 / 20.0)));
        let bottom = y_of(0.0);
        for side in 0..2 {
            let x = track_x + 22.0 + side as f32 * (bar_w + bar_gap);
            c.fill_rrect(Rect::new(x, area.y + CAP_H * 0.5, bar_w, travel), 2.0, WELL);
            let top = y_of(gain_to_pos(meter.level[side]));
            if bottom - top > 0.5 {
                c.set_clip(Some(Rect::new(x, warn, bar_w, bottom - warn)));
                c.fill_rrect(Rect::new(x, top, bar_w, bottom - top), 2.0, METER);
                c.set_clip(Some(Rect::new(x, area.y, bar_w, warn - area.y)));
                c.fill_rrect(Rect::new(x, top, bar_w, bottom - top), 2.0, AMBER);
                c.set_clip(None);
            }
            let hold = meter.hold[side];
            if hold > 1e-4 {
                let col = if hold >= 0.999 { NEGATIVE } else { TEXT2 };
                c.fill_rect(Rect::new(x, y_of(gain_to_pos(hold)).round() - 1.0, bar_w, 2.0), col);
            }
        }

        // Capuchon de fader, façon console.
        let active = hov_zone && cap.contains(input.x, input.y) || matches!(self.drag, Drag::Fader { target: t, .. } if t == target);
        let key = match target {
            Target::Channel(ci) => ci,
            Target::Master => usize::MAX >> 40,
        };
        let hv = self.anim.to(id(A_CAP_HOVER, key, 0), active as u8 as f32, T_HOVER);
        c.fill_rrect_vgrad(cap, 6.0, TEXT.mix(WHITE, hv * 0.5), Color::hex(0xc4c4c8).mix(WHITE, hv * 0.3));
        c.fill_rect(Rect::new(cap.x + 6.0, cap.y + CAP_H * 0.5 - 1.0, CAP_W - 12.0, 2.0), BG_DEEP.alpha(0.55));
    }

    // --- Sortie ---

    fn output(&mut self, c: &mut Canvas, r: Rect) {
        Self::card(c, r);
        let inner = r.inset(CARD_PAD);
        let meta = format!("{} kHz · stéréo", self.sample_rate / 1000);
        let body = self.card_header(c, inner, "Sortie", &meta);
        let (scope, stats) = body.take_top(body.h - 48.0 - SECTION_GAP);
        let stats = stats.take_top(SECTION_GAP).1;

        c.fill_rrect(scope, 12.0, WELL);
        c.stroke_rrect(scope, 12.0, 1.0, HAIRLINE);
        let (_, mid) = scope.center();
        let plot = scope.inset_xy(12.0, 12.0);
        let amp = plot.h * 0.5;
        c.fill_rect(Rect::new(plot.x, mid.round(), plot.w, 1.0), HAIRLINE);
        for q in [-0.5, 0.5] {
            c.fill_rect(Rect::new(plot.x, (mid + q * amp).round(), plot.w, 1.0), HAIRLINE.alpha(0.5));
        }

        // Déclenchement sur un passage par zéro montant : la forme d'onde reste stable.
        let end = self.scope_at + SCOPE_LEN;
        let at = |i: usize| self.scope[i % SCOPE_LEN];
        let latest = end - SCOPE_WINDOW;
        let mut start = latest;
        for i in (latest - 2_048..latest).rev() {
            if at(i - 1) < 0.0 && at(i) >= 0.0 {
                start = i;
                break;
            }
        }
        let cols = plot.w.max(1.0) as usize;
        let mut top = Vec::with_capacity(cols);
        let mut bottom = Vec::with_capacity(cols);
        for col in 0..cols {
            let a = start + col * SCOPE_WINDOW / cols;
            let b = (start + (col + 1) * SCOPE_WINDOW / cols).max(a + 1);
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for i in a..b {
                lo = lo.min(at(i));
                hi = hi.max(at(i));
            }
            let (mut y_hi, mut y_lo) = (mid - hi.clamp(-1.0, 1.0) * amp, mid - lo.clamp(-1.0, 1.0) * amp);
            if y_lo - y_hi < 1.4 {
                let m = (y_lo + y_hi) * 0.5;
                (y_hi, y_lo) = (m - 0.7, m + 0.7);
            }
            let x = plot.x + col as f32 + 0.5;
            top.push((x, y_hi));
            bottom.push((x, y_lo));
        }
        let mut wave = Path::new();
        let outline: Vec<(f32, f32)> = top.into_iter().chain(bottom.into_iter().rev()).collect();
        wave.polygon(&outline);
        c.set_clip(Some(scope.inset(1.0)));
        c.fill_path(&wave, TEXT.alpha(0.9));
        c.set_clip(None);

        // Chiffres : crête, RMS sur ~300 ms, charge DSP.
        let n = 7_200;
        let rms = ((0..n).map(|k| at(end - 1 - k).powi(2)).sum::<f32>() / n as f32).sqrt();
        let figures = [
            ("Crête", fmt_db(self.master_meter.peak())),
            ("RMS", fmt_db(rms)),
            ("DSP", format!("{:.1} %", self.load * 100.0).replace('.', ",")),
        ];
        let col_w = stats.w / 3.0;
        for (k, (label, value)) in figures.iter().enumerate() {
            let x = stats.x + k as f32 * col_w;
            self.text(c, self.f.sans, 12.0, x, stats.y + 6.0, label, MUTED);
            self.text(c, self.f.mono_med, 20.0, x, stats.y + 34.0, value, TEXT);
        }
    }
}
