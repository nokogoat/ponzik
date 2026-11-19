//! Interface graphique : boucle fenêtre ↔ rendu, à 60 images/s quand quelque
//! chose bouge, au repos complet sinon.

// Les fonctions de dessin prennent naturellement position, taille et couleur.
#![allow(clippy::too_many_arguments)]

mod app;
mod arrange;
mod assistant;
mod automation;
mod capture;
mod autorec;
mod browser;
mod clip;
mod comp;
mod compat;
mod devices;
mod dnd;
mod editors;
mod dropouts;
mod float;
mod freeze;
mod grand;
mod groove;
mod keymap;
mod keys;
mod lanes;
mod learn;
mod library;
mod midiout;
mod mixer;
mod modulators;
mod notefx;
mod offline;
mod pads;
mod palette;
mod plugins;
mod piano;
mod playlist;
mod rack;
mod rackfx;
mod samplelab;
mod record;
mod session;
mod settings;
mod sync;
mod keys_visuals;
mod visuals;
mod warping;
mod theme;
mod tools;
mod tour;
mod tracks;
mod widgets;

use std::time::{Duration, Instant};

use ponzik_engine::Command;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::demo_project;
use ponzik_ui::{Canvas, encode_png};
use ponzik_window::{Event, Key, Modifiers, MouseButton, Window};

use app::{App, View};
pub use settings::init_language;
pub use compat::{library_dir, project_samples};

/// PIPEWIRE_LATENCY vient de nous (l'utilisateur ne l'a pas fixée).
static PIPEWIRE_LATENCY_OURS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Latence demandée au graphe PipeWire par le greffon ALSA, quand c'est nous
/// qui la fixons : relue à chaque ouverture de la sortie.
pub(crate) fn set_pipewire_latency(ms: u32) {
    if !cfg!(target_os = "linux") || !PIPEWIRE_LATENCY_OURS.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let frames = ms * 48;
    // SAFETY: appelé au démarrage, ou quand le thread audio et la capture sont
    // arrêtés pour rouvrir la sortie ; le reste de l'application lit
    // l'environnement par `std::env`, qui sérialise ces accès.
    unsafe { std::env::set_var("PIPEWIRE_LATENCY", format!("{frames}/48000")) };
}


const FRAME: Duration = Duration::from_micros(16_667);
/// Cadence quand seules la tête de lecture et les VU-mètres bougent.
const SLOW_FRAME: Duration = Duration::from_micros(33_333);
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
    /// Files dragged from another application hover the window.
    pub file_hover: bool,
    /// Files dropped from another application during this frame.
    pub dropped: Option<Vec<std::path::PathBuf>>,
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
        self.dropped = None;
    }

    /// Renvoie vrai si l'événement change quelque chose à l'écran.
    fn apply(&mut self, ev: &Event) -> bool {
        if let Event::FileDrop { x, y, paths } = ev {
            (self.x, self.y, self.inside, self.file_hover) = (*x, *y, true, false);
            self.dropped = Some(paths.clone());
            return true;
        }
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
            Event::DragHover { x, y } => (self.x, self.y, self.inside, self.file_hover) = (x, y, true, true),
            Event::DragLeave => (self.inside, self.file_hover) = (false, false),
            Event::FileDrop { .. } => {}
            Event::Resized { .. } | Event::Redraw | Event::ScaleChanged { .. } => {}
        }
        true
    }
}

/// Événement de la fenêtre (pixels physiques) → coordonnées logiques de l'interface.
fn to_logical(ev: Event, scale: f32) -> Event {
    let l = |v: f32| v / scale;
    match ev {
        Event::MouseMove { x, y } => Event::MouseMove { x: l(x), y: l(y) },
        Event::MouseDown { x, y, button, mods } => Event::MouseDown { x: l(x), y: l(y), button, mods },
        Event::MouseUp { x, y, button } => Event::MouseUp { x: l(x), y: l(y), button },
        Event::Wheel { x, y, delta, mods } => Event::Wheel { x: l(x), y: l(y), delta, mods },
        Event::DragHover { x, y } => Event::DragHover { x: l(x), y: l(y) },
        Event::FileDrop { x, y, paths } => Event::FileDrop { x: l(x), y: l(y), paths },
        other => other,
    }
}

/// Échelle du système : `PONZIK_SCALE_UI` (pour essayer ou capturer en 2×),
/// sinon celle que rapporte la fenêtre.
fn system_scale(window: Option<&Window>) -> f32 {
    std::env::var("PONZIK_SCALE_UI")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| s.clamp(0.5, 4.0))
        .unwrap_or_else(|| window.map_or(1.0, Window::scale_factor))
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
    // Tampon audio choisi (ou relevé après des craquements) aux sessions précédentes.
    settings::init_audio_buffer();
    // Sous PipeWire, l'appareil ALSA « default » ajoute son propre tampon
    // (souvent 1024 trames) : on lui demande la même latence que la nôtre.
    if std::env::var_os("PIPEWIRE_LATENCY").is_none() {
        PIPEWIRE_LATENCY_OURS.store(true, std::sync::atomic::Ordering::Relaxed);
        set_pipewire_latency(ponzik_audio::target_latency_ms());
    }
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let samples = compat::project_samples(&project);
    // Warped clips play resampled until their render, made in the background, is ready.
    let (mut engine, handle) =
        project.build_engine_with(config.sample_rate as f32, &|p| samples.get(p).cloned(), &|job| ponzik_project::warp::cached(job.key));
    engine.set_worker_threads(ponzik_engine::default_worker_threads());
    engine.set_adaptive_threads(true);
    // The plugin instances' editors, before the engine leaves for the audio thread.
    let editors = editors::PluginEditors::from_engine(&mut engine);
    let output_name = output.device_name().to_string();
    // Duplex : l'entrée, une fois branchée, arrive dans le même appel que la sortie.
    let stream = output
        .start_duplex(move |input, in_ch, out, ch| engine.process_duplex(input, in_ch, out, ch))
        .map_err(|e| e.to_string())?;

    // PONZIK_WINDOW=1880x992 : taille logique de la fenêtre à l'ouverture.
    let (ww, wh) = std::env::var("PONZIK_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((1600, 960));
    let mut window = Window::open("Ponzik", ww, wh).map_err(|e| e.to_string())?;
    let mut app = App::new(handle, project, config.sample_rate);
    app.editors = editors;
    app.editors.set_owner(window.native_handle());
    app.samples = samples;
    app.refresh_warps();
    app.set_output(stream);
    app.xrun_watch.reopened(0.0, 0);
    app.start_midi_out();
    for m in &app.midi_inputs {
        eprintln!("MIDI: {}", m.name());
    }
    app.load_settings();
    app.ctl.persist = true;
    app.start_tour_if_new();
    app.start_autosave();
    app.start_library();
    app.start_plugin_scan(false);
    let (play, find) = (app.key_label(keymap::Shortcut::PlayStop), app.key_label(keymap::Shortcut::Palette));
    app.toast(&tr_fmt!("{} to play · {} to find anything", play, find));
    eprintln!("Ponzik · {} · {} Hz · {}", window.backend_name(), config.sample_rate, output_name);
    // PONZIK_VIEW=playlist : vue affichée à l'ouverture ; PONZIK_AUTOPLAY=1 :
    // lecture lancée aussitôt (mesures de charge).
    if let Some(view) = std::env::var("PONZIK_VIEW").ok().and_then(|v| view_named(&v)) {
        if app.view_shown(view) {
            app.set_view(view);
        }
        if view == View::Playlist {
            app.set_mode(ponzik_engine::PlayMode::Song);
        }
    }
    if std::env::var_os("PONZIK_AUTOPLAY").is_some() {
        app.toggle_play();
    }
    let result = event_loop(&mut window, &mut app);
    // Plugin interfaces go before their windows, and both before the instances.
    app.editors.close_all();
    // Sortie normale (fermeture ou erreur rapportée) : le verrou de session est retiré,
    // les panneaux ouverts sont retenus pour la prochaine fois.
    app.close_autosave();
    app.save_settings();
    result
}

/// Temps passé par phase (interface, rastérisation, envoi), cumulé puis
/// affiché toutes les deux secondes avec `PONZIK_PROFILE=1`.
struct Profile {
    since: Instant,
    frames: u32,
    ui: Duration,
    raster: Duration,
    present: Duration,
    worst: Duration,
    dirty: f64,
    ops: usize,
    allocs: u64,
}

impl Profile {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            ui: Duration::ZERO,
            raster: Duration::ZERO,
            present: Duration::ZERO,
            worst: Duration::ZERO,
            dirty: 0.0,
            ops: 0,
            allocs: 0,
        }
    }

    fn frame(&mut self, start: Instant, ui: Instant, raster: Instant, end: Instant, dirty: f64, ops: usize, size: (u32, u32), allocs: u64) {
        self.frames += 1;
        self.allocs += allocs;
        self.ui += ui - start;
        self.raster += raster - ui;
        self.present += end - raster;
        self.worst = self.worst.max(end - start);
        self.dirty += dirty;
        self.ops += ops;
        let span = self.since.elapsed();
        if span >= Duration::from_secs(2) {
            let n = self.frames.max(1) as f64;
            let ms = |d: Duration| d.as_secs_f64() * 1000.0 / n;
            let busy = (self.ui + self.raster + self.present).as_secs_f64() / span.as_secs_f64();
            eprintln!(
                "profile: {}×{} · {:.1} fps · ui {:.2} ms · raster {:.2} ms · present {:.2} ms · worst {:.2} ms · dirty {:.1} % · {} ops · {} allocs/frame · thread {:.1} %",
                size.0,
                size.1,
                n / span.as_secs_f64(),
                ms(self.ui),
                ms(self.raster),
                ms(self.present),
                self.worst.as_secs_f64() * 1000.0,
                self.dirty * 100.0 / n,
                self.ops / self.frames.max(1) as usize,
                self.allocs / self.frames.max(1) as u64,
                busy * 100.0,
            );
            *self = Self::new();
        }
    }
}

fn event_loop(window: &mut Window, app: &mut App) -> Result<(), String> {
    let (w, h) = window.size();
    let started = Instant::now();
    let mut canvas = Canvas::new(w as usize, h as usize);
    let mut scale = system_scale(Some(window)) * app.ui_scale();
    canvas.set_scale(scale);
    let mut input = Input::default();
    let mut clicks = ClickTracker::default();
    let mut events = Vec::new();
    let mut dirty = true;
    let mut last_frame = Instant::now() - FRAME;
    // PONZIK_DEBUG_INPUT=1 : journal des événements et des ouvertures de menus.
    let debug_input = std::env::var_os("PONZIK_DEBUG_INPUT").is_some();
    // PONZIK_PROFILE=1 : temps par phase de l'image, toutes les 2 s.
    let mut profile = std::env::var_os("PONZIK_PROFILE").map(|_| Profile::new());
    let mut expose = true;

    loop {
        // Au repos, on dort jusqu'au prochain événement ; sinon on vise 60 images/s
        // en continuant d'écouter la souris jusqu'à l'échéance.
        if dirty || app.needs_redraw() {
            // Seuls la lecture et les VU-mètres bougent : 30 images/s suffisent.
            // Une souris qui bouge ou une animation repassent à 60.
            let smooth = dirty || app.wants_smooth_frames();
            let slice = if app.midi_inputs.is_empty() { FRAME } else { Duration::from_millis(4) };
            loop {
                let interval = if smooth || dirty || !events.is_empty() { FRAME } else { SLOW_FRAME };
                let Some(left) = (last_frame + interval).checked_duration_since(Instant::now()) else { break };
                window.wait_events(left.min(slice), &mut events);
                dirty |= app.poll_midi();
                // Un clic ou une touche n'attend pas la prochaine image : la note
                // part tout de suite, l'image est simplement dessinée en avance.
                if events.iter().any(|e| {
                    matches!(e, Event::MouseDown { .. } | Event::KeyDown { .. } | Event::Physical { down: true, .. })
                }) {
                    break;
                }
            }
        } else {
            // Avec un contrôleur MIDI branché, on se réveille souvent pour ses notes.
            let mut idle = if app.midi_inputs.is_empty() { 500 } else { 4 };
            // Plugin interfaces run on our loop: their timers and descriptors want pumping.
            if app.editors.any_open() {
                idle = idle.min(15);
            } else if app.editors.has_editors() {
                idle = idle.min(50);
            }
            window.wait_events(Duration::from_millis(idle), &mut events);
        }
        window.poll_events(&mut events);

        // Échelle de l'image (écran × réglage) : la souris immobile garde sa place à l'écran.
        let k = system_scale(Some(window)) * app.ui_scale();
        if k != scale {
            (input.x, input.y) = (input.x * scale / k, input.y * scale / k);
            scale = k;
            canvas.set_scale(k);
            dirty = true;
        }
        input.begin_frame();
        if debug_input {
            for ev in &events {
                if !matches!(ev, Event::MouseMove { .. }) {
                    eprintln!("[{:>8.3}] {ev:?}", started.elapsed().as_secs_f64());
                }
            }
        }
        for ev in events.drain(..) {
            if ev == Event::CloseRequested {
                return Ok(());
            }
            // Fenêtre découverte ou redimensionnée : l'image entière est renvoyée.
            if matches!(ev, Event::Redraw | Event::Resized { .. }) {
                expose = true;
            }
            dirty |= input.apply(&to_logical(ev, scale));
        }
        clicks.check(&mut input);
        dirty |= app.poll_midi();
        app.autosave_tick(started.elapsed().as_secs_f64());
        dirty |= app.library_tick();
        dirty |= app.browser_tick();
        dirty |= app.plugin_scan_tick();
        dirty |= app.editors_tick();
        dirty |= app.xrun_tick(started.elapsed().as_secs_f32());
        if app.rec.output.as_ref().is_some_and(|s| !s.is_running()) {
            return Err(tr("the audio driver stopped").into());
        }

        let (w, h) = window.size();
        if (w as usize, h as usize) != (canvas.width(), canvas.height()) {
            canvas.resize(w as usize, h as usize);
            canvas.set_scale(scale);
            dirty = true;
        }
        if dirty || app.needs_redraw() {
            let now = Instant::now();
            let dt = now.duration_since(last_frame).as_secs_f32();
            last_frame = now;
            let before = (app.popup.is_some(), app.rec.panel);
            let allocs = crate::alloc_count::allocations();
            // Image suivie : l'interface est enregistrée, puis seules les zones
            // qui ont changé sont rastérisées et envoyées à l'écran.
            canvas.begin_frame();
            app.frame(&mut canvas, &input, dt);
            let t_frame = Instant::now();
            canvas.end_frame();
            let t_raster = Instant::now();
            let after = (app.popup.is_some(), app.rec.panel);
            if debug_input && before != after {
                eprintln!("[{:>8.3}] menu {} → {} · panneau micro {} → {} (pressed {}, released {})",
                    started.elapsed().as_secs_f64(), before.0, after.0, before.1, after.1, input.pressed, input.released);
            }
            window.set_cursor(app.cursor);
            if std::mem::take(&mut expose) {
                window.present(canvas.pixels(), w, h).map_err(|e| e.to_string())?;
            } else {
                window.present_rects(canvas.pixels(), w, h, canvas.dirty_rects()).map_err(|e| e.to_string())?;
            }
            if let Some(p) = profile.as_mut() {
                let area: usize = canvas.dirty_rects().iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum();
                let allocs = crate::alloc_count::allocations() - allocs;
                p.frame(now, t_frame, t_raster, Instant::now(), area as f64 / (w as f64 * h as f64), canvas.recorded_ops(), (w, h), allocs);
            }
            // Taille de l'interface changée pendant l'image : la suivante suit tout de suite.
            dirty = system_scale(Some(window)) * app.ui_scale() != scale;
        }
    }
}

fn view_named(name: &str) -> Option<View> {
    Some(match name {
        "rack" => View::Rack,
        "piano" => View::Piano,
        "playlist" => View::Playlist,
        "mixer" => View::Mixer,
        "session" => View::Session,
        "lab" => View::Lab,
        _ => return None,
    })
}

/// Rend l'interface hors écran, en pleine lecture, dans un PNG. `width` × `height`
/// est la taille logique ; avec `PONZIK_SCALE_UI=2`, l'image fait le double en pixels.
pub fn screenshot(path: &str, width: usize, height: usize, view: &str) -> Result<(), String> {
    const SR: u32 = 48_000;
    // « bc-playlist » : la démo breakcore dans la vue playlist ; « zg-mixer » : la démo Zalgo Rave.
    let (project, view) = if let Some(v) = view.strip_prefix("bc-") {
        (ponzik_project::demo_breakcore(), v)
    } else if let Some(v) = view.strip_prefix("zg-") {
        (ponzik_project::demo_zalgo(), v)
    } else {
        (demo_project(), view)
    };
    let samples = compat::project_samples(&project);
    let (mut engine, handle) = project.build_engine(SR as f32, &|p| samples.get(p).cloned());
    let editors = editors::PluginEditors::from_engine(&mut engine);
    let mut app = App::new(handle, project, SR);
    app.editors = editors;
    app.samples = samples;
    // PONZIK_PRESET=n : profil d'espace de travail (0 Complet … 4 Minimal).
    if let Some(i) = std::env::var("PONZIK_PRESET").ok().and_then(|v| v.parse().ok()) {
        if let Some((_, _, ws)) = settings::presets().into_iter().nth(i) {
            app.apply_workspace(ws, false);
        }
    }
    // PONZIK_SCAN=1: plugins scanned now (child processes, cache untouched).
    if std::env::var_os("PONZIK_SCAN").is_some() {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let scanner = |path: &std::path::Path| {
            let mut c = std::process::Command::new(&exe);
            c.arg("scan-plugin").arg(path);
            c
        };
        app.set_plugins(ponzik_project::plugins::scan(&scanner, None, false, ponzik_project::plugins::SCAN_TIMEOUT));
    }
    // PONZIK_BROWSER=n: browser tab (0 instruments, 1 effects, 2 samples, 3 plugins).
    if let Some(i) = std::env::var("PONZIK_BROWSER").ok().and_then(|v| v.parse::<usize>().ok()) {
        app.sidebar_tab = i.min(3);
    }
    // PONZIK_BROWSE=fav|recent:0,2|tag:kick|star:0,3|play:2|sel:1|auto:off|vol:0.4 (";"-separated):
    // the browser's filters, stars and preview.
    if let Ok(spec) = std::env::var("PONZIK_BROWSE") {
        let items = app.browser_tab_payloads();
        app.screenshot_browser(&spec, &items);
    }
    // PONZIK_FLP=<file.flp>: an FL Studio project imported (its report closed).
    if let Ok(path) = std::env::var("PONZIK_FLP") {
        app.import_flp(std::path::Path::new(&path));
        app.popup = None;
    }
    // PONZIK_PL_SCROLL=lane,px: the first clip moved down to that lane, the lanes scrolled by px.
    if let Some((lane, px)) = std::env::var("PONZIK_PL_SCROLL").ok().and_then(|v| {
        let (l, p) = v.split_once(',')?;
        Some((l.trim().parse::<usize>().ok()?, p.trim().parse::<f32>().ok()?))
    }) {
        if let Some(cl) = app.project.audio_clips.first_mut() {
            cl.lane = lane;
        } else if let Some(cl) = app.project.clips.first_mut() {
            cl.lane = lane;
        }
        app.commit("Capture");
        app.playlist.scroll_y = px;
        app.anim.set(widgets::wid("pl-sy", 0, 0), px);
    }
    let view = view_named(view).unwrap_or(View::Rack);
    if app.view_shown(view) {
        app.set_view(view);
    }
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
            app.commit(tr("Instrument"));
        }
        // PONZIK_PADS=kit[,pad] : Drum Pads with the starter kit, pad n played.
        if let Ok(spec) = std::env::var("PONZIK_PADS") {
            app.screenshot_pads(ch, &spec);
        }
        // PONZIK_OPEN_EDITOR=1: the instrument's plugin window opens during the capture.
        if std::env::var_os("PONZIK_OPEN_EDITOR").is_some() {
            app.toggle_editor(devices::Device::Instrument(ch));
        }
        if let Ok(list) = std::env::var("PONZIK_NOTE_FX") {
            for (slot, kind) in list.split(',').enumerate() {
                app.add_note_fx(ch, slot, kind.trim());
            }
        }
    }
    // PONZIK_AB=1: the first device of the panel on B, a knob moved.
    if std::env::var_os("PONZIK_AB").is_some() {
        app.screenshot_ab();
    }
    // PONZIK_SESSION=slot:c,r|scene:r|follow|audio|rec (";"-separated): launch
    // settings, an audio slot, the inspector and the arrangement record.
    if let Ok(spec) = std::env::var("PONZIK_SESSION") {
        app.screenshot_session(&spec);
    }
    if view == View::Session {
        app.launch_scene(0);
    } else {
        app.toggle_play();
    }
    let _ = Command::Play;

    // PONZIK_RECOVER : montre la carte de reprise après un crash.
    if std::env::var_os("PONZIK_RECOVER").is_some() {
        app.recovery = Some((std::path::PathBuf::new(), tr("Demo · 3 min ago").into()));
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
            let track = track.trim().parse::<usize>().ok().filter(|&t| t < app.project.tracks.len());
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
        app.commit(tr("Capture effects"));
    }
    // PONZIK_RACK=3 : a rack with macros on track 3, focused.
    if let Ok(spec) = std::env::var("PONZIK_RACK") {
        app.screenshot_rack(&spec);
    }
    // PONZIK_MODS=edit,all : modulators on the focused track (depth mode, all kinds).
    if let Ok(spec) = std::env::var("PONZIK_MODS") {
        app.screenshot_mods(&spec);
    }
    // PONZIK_CHAIN_SCROLL=px : the device panel scrolled to the right.
    if let Some(px) = std::env::var("PONZIK_CHAIN_SCROLL").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
        app.chain_scroll = px;
        app.anim.set(widgets::wid("chain-sx", 0, 0), px);
    }
    // PONZIK_LISTEN=s[,apply] : les assistants de mixage écoutent s secondes de
    // lecture, puis appliquent leur suggestion.
    if let Ok(spec) = std::env::var("PONZIK_LISTEN") {
        let (secs, apply) = spec.split_once(',').map_or((spec.as_str(), false), |(s, a)| (s, a.trim() == "apply"));
        if let Ok(secs) = secs.trim().parse::<f32>() {
            app.screenshot_listen(&mut engine, secs, apply);
        }
    }
    // PONZIK_MIXER=bus:1,2,3,4;fold;vca:2,5;sel:4,5;return:delay@3 : bus, repli du
    // dernier bus, VCA, sélection et retours d'effet créés pour la capture.
    if let Ok(spec) = std::env::var("PONZIK_MIXER") {
        app.screenshot_mixer(&spec);
    }
    // PONZIK_TRACKS=64[,compact][,end][,drag:3>9][,menu:5]: the demo grown to
    // that many mixer tracks, every track shown.
    if let Ok(spec) = std::env::var("PONZIK_TRACKS") {
        app.screenshot_tracks(&spec);
    }
    // PONZIK_SAMPLELAB=demo|<wav> : ouvre l'Atelier sur un échantillon.
    if let Ok(spec) = std::env::var("PONZIK_SAMPLELAB") {
        app.screenshot_lab(&spec);
    }
    // PONZIK_RECORD=1 : piste armée et panneau d'enregistrement ; =takes : avec des prises
    // en boucle ; =devices : la liste des entrées.
    if let Ok(mode) = std::env::var("PONZIK_RECORD") {
        app.screenshot_record(&mode);
    }
    // PONZIK_COMP=1|folded|menu|take|solo|flat: a take group of three synthetic
    // vocal passes and its comp.
    if let Ok(spec) = std::env::var("PONZIK_COMP") {
        app.screenshot_comp(&spec);
    }
    // PONZIK_ARRANGE=1|menu|sig: sections, a tempo ramp and a 3/4 section.
    if let Ok(spec) = std::env::var("PONZIK_ARRANGE") {
        app.screenshot_arrange(&spec);
    }
    // PONZIK_LEARN=pending|linked|settings: MIDI learn, controller links.
    if let Ok(spec) = std::env::var("PONZIK_LEARN") {
        app.screenshot_learn(&spec);
    }
    // PONZIK_AUTOREC=latch|touch|write: an automation pass being recorded.
    if let Ok(spec) = std::env::var("PONZIK_AUTOREC") {
        app.screenshot_autorec(&spec);
    }
    // PONZIK_CAPTURE=1: a phrase played with the transport stopped, captured.
    if std::env::var_os("PONZIK_CAPTURE").is_some() {
        app.screenshot_capture();
    }
    // PONZIK_CLIP=audio[,markers|tones|off]|pattern: a clip and its clip panel.
    if let Ok(spec) = std::env::var("PONZIK_CLIP") {
        app.screenshot_clip(&spec);
    }
    // PONZIK_VARY=lane: chances, conditions and repeats on the pattern's notes,
    // the piano roll showing lane 0 (chance), 1 (condition) or 2 (repeats).
    if let Ok(spec) = std::env::var("PONZIK_VARY") {
        app.screenshot_vary(&spec);
    }
    // PONZIK_FREEZE=channel:4,track:3,prompt,bounce:1,consolidate:2,selection,menu:4:
    // freezes, bounces and their menus.
    if let Ok(spec) = std::env::var("PONZIK_FREEZE") {
        app.screenshot_freeze(&spec);
    }
    // PONZIK_EXPRESS=slides,pan|fine|bend|cc,groove: slides, note expressions,
    // a controller lane and a groove in the piano roll.
    if let Ok(spec) = std::env::var("PONZIK_EXPRESS") {
        app.screenshot_express(&spec);
    }
    // PONZIK_POPUP : ouvre un menu pour la capture.
    if let Ok(name) = std::env::var("PONZIK_POPUP") {
        app.screenshot_popup(&name);
    }
    // PONZIK_TOUR=n : la bulle n de la visite guidée.
    if let Some(step) = std::env::var("PONZIK_TOUR").ok().and_then(|v| v.trim().parse().ok()) {
        app.screenshot_tour(step);
    }
    // PONZIK_FLOAT=n : détache les n premiers appareils du panneau dans des fenêtres.
    if let Some(n) = std::env::var("PONZIK_FLOAT").ok().and_then(|v| v.parse().ok()) {
        app.screenshot_floats(n);
    }
    // PONZIK_UI_SIZE=125 : taille de l'interface choisie, en %.
    if let Some(pct) = std::env::var("PONZIK_UI_SIZE").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
        app.prefs.ui_scale = settings::UiScale::new(pct / 100.0);
    }
    let scale = system_scale(None) * app.ui_scale();
    let (pw, ph) = ((width as f32 * scale).round() as usize, (height as f32 * scale).round() as usize);
    let mut canvas = Canvas::new(pw, ph);
    canvas.set_scale(scale);
    // PONZIK_HOVER=x,y : souris immobile à cet endroit (survols, infobulles).
    let mut input = Input::default();
    if let Some((x, y)) = std::env::var("PONZIK_HOVER").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }) {
        (input.x, input.y, input.inside) = (x, y, true);
    }
    // PONZIK_FILE_DRAG=x,y: files from the file manager hover there;
    // PONZIK_FILE_DROP=path|path: and are dropped early in the capture.
    if let Some((x, y)) = std::env::var("PONZIK_FILE_DRAG").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }) {
        (input.x, input.y, input.inside, input.file_hover) = (x, y, true, true);
    }
    let file_drop: Option<Vec<std::path::PathBuf>> =
        std::env::var("PONZIK_FILE_DROP").ok().map(|v| v.split('|').map(std::path::PathBuf::from).collect());
    let mut audio = vec![0.0f32; 800 * 2];
    // ~1,6 s : la tête de lecture tombe sur un temps fort, les VU-mètres sont lancés.
    // PONZIK_BENCH : temps moyen d'une image, complète puis suivie (seules les
    // zones modifiées sont rastérisées, comme dans la fenêtre).
    let bench = std::env::var_os("PONZIK_BENCH").is_some();
    let (mut drawing, mut ui, mut raster, mut dirty) = (Duration::ZERO, Duration::ZERO, Duration::ZERO, 0.0);
    let mut allocs = 0;
    for i in 0..96 {
        if let Some(paths) = file_drop.as_ref().filter(|_| i == 8) {
            input.dropped = Some(paths.clone());
            input.file_hover = false;
        } else if i == 9 {
            input.dropped = None;
        }
        engine.process(&mut audio, 2);
        app.editors_tick();
        let a0 = crate::alloc_count::allocations();
        let t0 = Instant::now();
        if bench && i >= 48 {
            canvas.begin_frame();
            app.frame(&mut canvas, &input, 1.0 / 60.0);
            let t1 = Instant::now();
            canvas.end_frame();
            if i == 48 {
                continue;
            }
            ui += t1 - t0;
            raster += t1.elapsed();
            allocs += crate::alloc_count::allocations() - a0;
            dirty += canvas.dirty_rects().iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum::<usize>() as f64 / (pw * ph) as f64;
        } else {
            app.frame(&mut canvas, &input, 1.0 / 60.0);
            drawing += t0.elapsed();
        }
    }
    app.editors.close_all();
    if bench {
        let ms = |d: Duration, n: f64| d.as_secs_f64() * 1000.0 / n;
        eprintln!(
            "{view:?} · {pw}×{ph} (×{scale}) · full {:.2} ms/image · tracked ui {:.2} + raster {:.2} ms · dirty {:.1} % · {} ops · {} allocs/image",
            ms(drawing, 48.0),
            ms(ui, 47.0),
            ms(raster, 47.0),
            dirty * 100.0 / 47.0,
            canvas.recorded_ops(),
            allocs / 47
        );
    }
    let png = encode_png(canvas.pixels(), pw, ph);
    std::fs::write(path, png).map_err(|e| format!("{path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_events_become_logical() {
        let mods = Modifiers::default();
        assert_eq!(to_logical(Event::MouseMove { x: 300.0, y: 150.0 }, 2.0), Event::MouseMove { x: 150.0, y: 75.0 });
        assert_eq!(
            to_logical(Event::MouseDown { x: 150.0, y: 30.0, button: MouseButton::Left, mods }, 1.5),
            Event::MouseDown { x: 100.0, y: 20.0, button: MouseButton::Left, mods }
        );
        assert_eq!(
            to_logical(Event::MouseUp { x: 9.0, y: 3.0, button: MouseButton::Right }, 0.75),
            Event::MouseUp { x: 12.0, y: 4.0, button: MouseButton::Right }
        );
        // La molette garde ses crans ; seule la position change d'échelle.
        assert_eq!(
            to_logical(Event::Wheel { x: 40.0, y: 20.0, delta: -1.0, mods }, 2.0),
            Event::Wheel { x: 20.0, y: 10.0, delta: -1.0, mods }
        );
        // Tailles de fenêtre et le reste : inchangés.
        assert_eq!(to_logical(Event::Resized { width: 3200, height: 1920 }, 2.0), Event::Resized { width: 3200, height: 1920 });
        assert_eq!(to_logical(Event::MouseMove { x: 7.0, y: 9.0 }, 1.0), Event::MouseMove { x: 7.0, y: 9.0 });
        // Files dragged from another application land where the pointer is.
        assert_eq!(to_logical(Event::DragHover { x: 100.0, y: 60.0 }, 2.0), Event::DragHover { x: 50.0, y: 30.0 });
        let paths = vec![std::path::PathBuf::from("/a.wav")];
        assert_eq!(
            to_logical(Event::FileDrop { x: 30.0, y: 12.0, paths: paths.clone() }, 1.5),
            Event::FileDrop { x: 20.0, y: 8.0, paths }
        );
    }

    #[test]
    fn file_drags_reach_the_input() {
        let mut input = Input::default();
        assert!(input.apply(&Event::DragHover { x: 40.0, y: 50.0 }));
        assert!(input.file_hover && input.inside);
        assert_eq!((input.x, input.y), (40.0, 50.0));
        assert!(input.apply(&Event::DragLeave));
        assert!(!input.file_hover && !input.inside);
        let paths = vec![std::path::PathBuf::from("/k.wav")];
        input.apply(&Event::DragHover { x: 1.0, y: 1.0 });
        assert!(input.apply(&Event::FileDrop { x: 9.0, y: 8.0, paths: paths.clone() }));
        assert_eq!(input.dropped.as_deref(), Some(&paths[..]));
        assert!(!input.file_hover);
        input.begin_frame();
        assert_eq!(input.dropped, None, "a drop lasts one frame");
    }
}
