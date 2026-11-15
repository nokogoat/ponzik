//! Interface graphique : boucle fenêtre ↔ rendu, à 60 images/s quand quelque
//! chose bouge, au repos complet sinon.

mod app;
mod theme;

use std::time::{Duration, Instant};

use ponzik_engine::{Command, Engine};
use ponzik_ui::{Canvas, encode_png};
use ponzik_window::{Event, Key, Modifiers, MouseButton, Window};

use crate::demo;
use app::{App, ChannelView};

const FRAME: Duration = Duration::from_micros(16_667);
const PROJECT: &str = "Groove house en La mineur";

/// État souris/clavier d'une image.
#[derive(Default)]
pub struct Input {
    pub x: f32,
    pub y: f32,
    pub inside: bool,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub right_pressed: bool,
    pub wheel: f32,
    pub mods: Modifiers,
    pub keys: Vec<(Key, Modifiers)>,
}

impl Input {
    fn begin_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.right_pressed = false;
        self.wheel = 0.0;
        self.keys.clear();
    }

    /// Renvoie vrai si l'événement change quelque chose à l'écran.
    fn apply(&mut self, ev: &Event) -> bool {
        match *ev {
            Event::MouseMove { x, y } => {
                (self.x, self.y, self.inside) = (x, y, true);
            }
            Event::MouseDown { x, y, button, mods } => {
                (self.x, self.y, self.inside, self.mods) = (x, y, true, mods);
                match button {
                    MouseButton::Left => {
                        self.down = true;
                        self.pressed = true;
                    }
                    MouseButton::Right => self.right_pressed = true,
                    MouseButton::Middle => {}
                }
            }
            Event::MouseUp { button: MouseButton::Left, .. } => {
                self.down = false;
                self.released = true;
            }
            Event::MouseUp { .. } => {}
            Event::Wheel { x, y, delta, mods } => {
                (self.x, self.y, self.mods) = (x, y, mods);
                self.wheel += delta;
            }
            Event::MouseLeave => self.inside = false,
            Event::KeyDown { key, mods } => self.keys.push((key, mods)),
            Event::FocusLost => {
                if self.down {
                    self.down = false;
                    self.released = true;
                }
            }
            Event::KeyUp { .. } | Event::CloseRequested => return false,
            Event::Resized { .. } | Event::Redraw => {}
        }
        true
    }
}

fn demo_views() -> (Vec<ponzik_engine::Channel>, Vec<ChannelView>) {
    let rack = demo::rack();
    let views = rack.iter().map(ChannelView::from_channel).collect();
    (rack, views)
}

pub fn run() -> Result<(), String> {
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let (rack, views) = demo_views();
    let (mut engine, mut handle) = Engine::new(config.sample_rate as f32, rack);
    let _ = handle.send(Command::SetBpm(demo::BPM));
    let output_name = output.device_name().to_string();
    let device = output_name.split(' ').next().unwrap_or("audio").to_string();
    let stream = output.start(move |out, ch| engine.process(out, ch)).map_err(|e| e.to_string())?;

    let mut window = Window::open("Ponzik", 1600, 960).map_err(|e| e.to_string())?;
    let mut app = App::new(handle, views, demo::BPM, config.sample_rate, device, PROJECT.into());
    let (w, h) = window.size();
    eprintln!("Ponzik · {} · {} Hz · {}", window.backend_name(), config.sample_rate, output_name);
    let mut canvas = Canvas::new(w as usize, h as usize);
    let mut input = Input::default();
    let mut events = Vec::new();
    let mut dirty = true;
    let mut last_frame = Instant::now() - FRAME;

    loop {
        // Au repos, on dort jusqu'au prochain événement ; sinon on vise 60 images/s
        // en continuant d'écouter la souris jusqu'à l'échéance.
        if dirty || app.needs_redraw() {
            let deadline = last_frame + FRAME;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                window.wait_events(left, &mut events);
            }
        } else {
            window.wait_events(Duration::from_millis(500), &mut events);
        }
        window.poll_events(&mut events);

        input.begin_frame();
        for ev in events.drain(..) {
            if ev == Event::CloseRequested {
                return Ok(());
            }
            dirty |= input.apply(&ev);
        }
        if !stream.is_running() {
            return Err("le pilote audio s'est arrêté".into());
        }

        let (w, h) = window.size();
        if (w as usize, h as usize) != (canvas.width(), canvas.height()) {
            canvas.resize(w as usize, h as usize);
            dirty = true;
        }
        if dirty || app.needs_redraw() {
            let now = Instant::now();
            let dt = now.duration_since(last_frame).as_secs_f32();
            last_frame = now;
            app.frame(&mut canvas, &input, dt);
            window.present(canvas.pixels(), w, h).map_err(|e| e.to_string())?;
            dirty = false;
        }
    }
}

/// Rend l'interface hors écran, en pleine lecture, dans un PNG.
pub fn screenshot(path: &str, width: usize, height: usize) -> Result<(), String> {
    const SR: u32 = 48_000;
    let (rack, views) = demo_views();
    let (mut engine, mut handle) = Engine::new(SR as f32, rack);
    let _ = handle.send(Command::SetBpm(demo::BPM));
    let mut app = App::new(handle, views, demo::BPM, SR, "ALSA".into(), PROJECT.into());
    app.toggle_play();

    let mut canvas = Canvas::new(width, height);
    let input = Input::default();
    let mut audio = vec![0.0f32; 800 * 2];
    // ~1,6 s : la tête de lecture tombe sur un temps fort, les VU-mètres sont lancés.
    for _ in 0..96 {
        engine.process(&mut audio, 2);
        app.frame(&mut canvas, &input, 1.0 / 60.0);
    }
    let png = encode_png(canvas.pixels(), width, height);
    std::fs::write(path, png).map_err(|e| format!("{path} : {e}"))
}
