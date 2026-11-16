//! Interface graphique : boucle fenêtre ↔ rendu, à 60 images/s quand quelque
//! chose bouge, au repos complet sinon.

// Les fonctions de dessin prennent naturellement position, taille et couleur.
#![allow(clippy::too_many_arguments)]

mod app;
mod automation;
mod compat;
mod devices;
mod dnd;
mod float;
mod keys;
mod mixer;
mod notefx;
mod palette;
mod piano;
mod playlist;
mod rack;
mod session;
mod sync;
mod visuals;
mod theme;
mod widgets;

use std::time::{Duration, Instant};

use ponzik_engine::Command;
use ponzik_project::demo_project;
use ponzik_ui::{Canvas, encode_png};
use ponzik_window::{Event, Key, Modifiers, MouseButton, Window};

use app::{App, View};

const FRAME: Duration = Duration::from_micros(16_667);
const DOUBLE_CLICK: f32 = 0.35;

/// État souris/clavier d'une image.
#[derive(Default, Clone)]
pub struct Input {
    pub x: f32,
    pub y: f32,
    pub inside: bool,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub right_pressed: bool,
    /// Bouton droit maintenu : efface ce que l'on balaie, comme dans FL Studio.
    pub right_down: bool,
    /// Bouton du milieu : glisser pour faire défiler.
    pub middle_pressed: bool,
    pub middle_down: bool,
    /// Le clic de cette image est un double-clic.
    pub double: bool,
    pub wheel: f32,
    pub mods: Modifiers,
    pub keys: Vec<(Key, Modifiers)>,
    /// Texte saisi pendant cette image.
    pub text: String,
    /// Touches physiques (code, enfoncée), dans l'ordre d'arrivée.
    pub physical: Vec<(u16, bool)>,
    /// La fenêtre a perdu le focus pendant cette image.
    pub focus_lost: bool,
}

impl Input {
    fn begin_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.right_pressed = false;
        self.middle_pressed = false;
        self.double = false;
        self.wheel = 0.0;
        self.keys.clear();
        self.text.clear();
        self.physical.clear();
        self.focus_lost = false;
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
                    MouseButton::Right => {
                        self.right_pressed = true;
                        self.right_down = true;
                    }
                    MouseButton::Middle => {
                        self.middle_pressed = true;
                        self.middle_down = true;
                    }
                }
            }
            Event::MouseUp { button: MouseButton::Left, .. } => {
                self.down = false;
                self.released = true;
            }
            Event::MouseUp { button: MouseButton::Right, .. } => self.right_down = false,
            Event::MouseUp { button: MouseButton::Middle, .. } => self.middle_down = false,
            Event::Wheel { x, y, delta, mods } => {
                (self.x, self.y, self.mods) = (x, y, mods);
                self.wheel += delta;
            }
            Event::MouseLeave => self.inside = false,
            Event::KeyDown { key, mods } => self.keys.push((key, mods)),
            Event::Text(c) => self.text.push(c),
            Event::Physical { code, down } => self.physical.push((code, down)),
            Event::FocusLost => {
                self.focus_lost = true;
                self.right_down = false;
                self.middle_down = false;
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

/// Repère les doubles-clics : deux pressions proches dans le temps et l'espace.
#[derive(Default)]
struct ClickTracker {
    last: Option<(Instant, f32, f32)>,
}

impl ClickTracker {
    fn check(&mut self, input: &mut Input) {
        if !input.pressed {
            return;
        }
        let now = Instant::now();
        if let Some((t, x, y)) = self.last
            && now.duration_since(t).as_secs_f32() < DOUBLE_CLICK
            && (x - input.x).abs() < 5.0
            && (y - input.y).abs() < 5.0
        {
            input.double = true;
            self.last = None;
        } else {
            self.last = Some((now, input.x, input.y));
        }
    }
}

pub fn run(project: ponzik_project::Project) -> Result<(), String> {
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let none = |_: &str| None;
    let (mut engine, handle) = project.build_engine(config.sample_rate as f32, &none);
    let output_name = output.device_name().to_string();
    let stream = output.start(move |out, ch| engine.process(out, ch)).map_err(|e| e.to_string())?;

    let mut window = Window::open("Ponzik", 1600, 960).map_err(|e| e.to_string())?;
    let mut app = App::new(handle, project, config.sample_rate);
    for m in &app.midi_inputs {
        eprintln!("MIDI : {}", m.name());
    }
    app.start_autosave();
    app.toast("Espace pour jouer · Ctrl+K pour tout trouver");
    eprintln!("Ponzik · {} · {} Hz · {}", window.backend_name(), config.sample_rate, output_name);
    let result = event_loop(&mut window, &mut app, &stream);
    // Sortie normale (fermeture ou erreur rapportée) : le verrou de session est retiré.
    app.close_autosave();
    result
}

fn event_loop(window: &mut Window, app: &mut App, stream: &ponzik_audio::OutputStream) -> Result<(), String> {
    let (w, h) = window.size();
    let started = Instant::now();
    let mut canvas = Canvas::new(w as usize, h as usize);
    let mut input = Input::default();
    let mut clicks = ClickTracker::default();
    let mut events = Vec::new();
    let mut dirty = true;
    let mut last_frame = Instant::now() - FRAME;

    loop {
        // Au repos, on dort jusqu'au prochain événement ; sinon on vise 60 images/s
        // en continuant d'écouter la souris jusqu'à l'échéance.
        if dirty || app.needs_redraw() {
            let deadline = last_frame + FRAME;
            let slice = if app.midi_inputs.is_empty() { FRAME } else { Duration::from_millis(4) };
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                window.wait_events(left.min(slice), &mut events);
                dirty |= app.poll_midi();
            }
        } else {
            // Avec un contrôleur MIDI branché, on se réveille souvent pour ses notes.
            let idle = if app.midi_inputs.is_empty() { 500 } else { 4 };
            window.wait_events(Duration::from_millis(idle), &mut events);
        }
        window.poll_events(&mut events);

        input.begin_frame();
        for ev in events.drain(..) {
            if ev == Event::CloseRequested {
                return Ok(());
            }
            dirty |= input.apply(&ev);
        }
        clicks.check(&mut input);
        dirty |= app.poll_midi();
        app.autosave_tick(started.elapsed().as_secs_f64());
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
pub fn screenshot(path: &str, width: usize, height: usize, view: &str) -> Result<(), String> {
    const SR: u32 = 48_000;
    // « bc-playlist » : la démo breakcore dans la vue playlist.
    let (project, view) = match view.strip_prefix("bc-") {
        Some(v) => (ponzik_project::demo_breakcore(), v),
        None => (demo_project(), view),
    };
    let none = |_: &str| None;
    let (mut engine, handle) = project.build_engine(SR as f32, &none);
    let mut app = App::new(handle, project, SR);
    let view = match view {
        "piano" => View::Piano,
        "playlist" => View::Playlist,
        "mixer" => View::Mixer,
        "session" => View::Session,
        _ => View::Rack,
    };
    app.set_view(view);
    if view == View::Piano {
        app.focus = app::Focus::Channel(4);
    }
    if view == View::Mixer {
        let track = std::env::var("PONZIK_FOCUS_TRACK").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        app.focus = app::Focus::Track(track);
    }
    if view == View::Playlist {
        app.set_mode(ponzik_engine::PlayMode::Song);
    }
    // Fichier de test : posé dans la playlist et chargé dans un sampler.
    if let Ok(path) = std::env::var("PONZIK_TEST_WAV") {
        app.add_audio_clip(&path, None);
        app.add_sample_channel(&path);
    }
    // PONZIK_CHANNEL=n : canal choisi ; PONZIK_INSTRUMENT=type : son instrument ;
    // PONZIK_NOTE_FX=arpeggiator,chord : ses effets de notes.
    if let Some(ch) = std::env::var("PONZIK_CHANNEL").ok().and_then(|v| v.parse::<usize>().ok()) {
        app.focus = app::Focus::Channel(ch.min(app.project.channels.len().saturating_sub(1)));
    }
    if let app::Focus::Channel(ch) = app.focus {
        if let Ok(kind) = std::env::var("PONZIK_INSTRUMENT") {
            app.project.channels[ch].instrument = ponzik_project::InstrumentDef::new(&kind);
            app.commit("Instrument");
        }
        if let Ok(list) = std::env::var("PONZIK_NOTE_FX") {
            for (slot, kind) in list.split(',').enumerate() {
                app.add_note_fx(ch, slot, kind.trim());
            }
        }
    }
    if view == View::Session {
        app.launch_scene(0);
    } else {
        app.toggle_play();
    }
    let _ = Command::Play;

    // PONZIK_RECOVER : montre la carte de reprise après un crash.
    if std::env::var_os("PONZIK_RECOVER").is_some() {
        app.recovery = Some((std::path::PathBuf::new(), "Démo · il y a 3 min".into()));
    }
    // PONZIK_SCALE=9,minor : gamme du projet (tonique, mode).
    if let Some((key, scale)) = std::env::var("PONZIK_SCALE").ok().and_then(|v| {
        let (k, s) = v.split_once(',')?;
        Some((k.trim().parse::<u8>().ok()?, ponzik_project::ScaleKind::from_id(s.trim())?))
    }) {
        app.project.key = key;
        app.project.scale = scale;
    }
    // PONZIK_INSERTS=3:compressor@2,lofi;0:master_limiter : effets ajoutés (piste:type@clé).
    if let Ok(spec) = std::env::var("PONZIK_INSERTS") {
        for part in spec.split(';') {
            let Some((track, list)) = part.split_once(':') else { continue };
            let track = track.trim().parse::<usize>().ok().filter(|&t| t < ponzik_engine::mixer::TRACKS);
            let Some(track) = track else { continue };
            for item in list.split(',') {
                let (kind, key) = item.split_once('@').map_or((item, None), |(k, key)| (k, key.trim().parse().ok()));
                let inserts = &mut app.project.tracks[track].inserts;
                if let Some(slot) = inserts.iter().position(Option::is_none) {
                    let def = ponzik_project::EffectDef::new(kind.trim());
                    inserts[slot] = Some(ponzik_project::EffectDef { sidechain: key, ..def });
                }
            }
        }
        app.commit("Effets de capture");
    }
    // PONZIK_POPUP : ouvre un menu pour la capture.
    if let Ok(name) = std::env::var("PONZIK_POPUP") {
        app.screenshot_popup(&name);
    }
    // PONZIK_FLOAT=n : détache les n premiers appareils du panneau dans des fenêtres.
    if let Some(n) = std::env::var("PONZIK_FLOAT").ok().and_then(|v| v.parse().ok()) {
        app.screenshot_floats(n);
    }
    let mut canvas = Canvas::new(width, height);
    // PONZIK_HOVER=x,y : souris immobile à cet endroit (survols, infobulles).
    let mut input = Input::default();
    if let Some((x, y)) = std::env::var("PONZIK_HOVER").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }) {
        (input.x, input.y, input.inside) = (x, y, true);
    }
    let mut audio = vec![0.0f32; 800 * 2];
    // ~1,6 s : la tête de lecture tombe sur un temps fort, les VU-mètres sont lancés.
    for _ in 0..96 {
        engine.process(&mut audio, 2);
        app.frame(&mut canvas, &input, 1.0 / 60.0);
    }
    let png = encode_png(canvas.pixels(), width, height);
    std::fs::write(path, png).map_err(|e| format!("{path} : {e}"))
}
