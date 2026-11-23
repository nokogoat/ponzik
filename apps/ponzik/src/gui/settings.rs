//! Réglages : l'espace de travail (vues, panneaux, éléments de la barre du haut,
//! profils en un clic) et les raccourcis remappables, enregistrés dans
//! `settings.json` du dossier de configuration de l'utilisateur.

use ponzik_i18n::{Lang, tr, tr_fmt};
use ponzik_project::json::{self, Json};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{Action, App, Popup, Rename, View};
use super::compat;
use super::keymap::{Combo, GROUPS, Keymap, Shortcut};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// 2: the calm top bar (new items, a smaller default).
const VERSION: f64 = 2.0;

/// Éléments facultatifs de la barre du haut. Lecture, arrêt, tempo, recherche
/// et menu y restent toujours ; le reste se montre ou se range ici.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Mode,
    Pattern,
    Tap,
    Position,
    Scale,
    Metronome,
    Record,
    /// Panneau d'enregistrement : entrée, niveau, latence.
    Mic,
    /// Voix vers notes.
    Voice,
    /// Armement de l'enregistrement d'automation.
    AutoArm,
    Keyboard,
    /// Annuler et rétablir.
    Undo,
    History,
    Save,
    /// Bouton du panneau d'appareils.
    Panel,
    Learn,
    Load,
}

/// Saved settings before the calm top bar: every item was shown by default.
const OLD_BARS: [&str; 11] = ["mode", "pattern", "tap", "position", "scale", "metronome", "record", "keyboard", "undo", "history", "load"];

impl Bar {
    pub const ALL: [Bar; 17] = [
        Bar::Mode,
        Bar::Pattern,
        Bar::Tap,
        Bar::Position,
        Bar::Scale,
        Bar::Metronome,
        Bar::Record,
        Bar::Mic,
        Bar::Voice,
        Bar::AutoArm,
        Bar::Keyboard,
        Bar::Undo,
        Bar::History,
        Bar::Save,
        Bar::Panel,
        Bar::Learn,
        Bar::Load,
    ];

    fn index(self) -> usize {
        Bar::ALL.iter().position(|b| *b == self).unwrap_or(0)
    }

    /// Shown in the calm default bar ("Simple"): the transport, undo and the load.
    pub fn simple(self) -> bool {
        matches!(self, Bar::Mode | Bar::Position | Bar::Metronome | Bar::Record | Bar::Undo | Bar::Load)
    }

    fn id(self) -> &'static str {
        match self {
            Bar::Mode => "mode",
            Bar::Pattern => "pattern",
            Bar::Tap => "tap",
            Bar::Position => "position",
            Bar::Scale => "scale",
            Bar::Metronome => "metronome",
            Bar::Record => "record",
            Bar::Mic => "mic",
            Bar::Voice => "voice",
            Bar::AutoArm => "auto_arm",
            Bar::Keyboard => "keyboard",
            Bar::Undo => "undo",
            Bar::History => "history",
            Bar::Save => "save",
            Bar::Panel => "panel",
            Bar::Learn => "learn",
            Bar::Load => "load",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Bar::Mode => tr("Mode"),
            Bar::Pattern => tr("Pattern"),
            Bar::Tap => tr("Tap"),
            Bar::Position => tr("Position"),
            Bar::Scale => tr("Scale"),
            Bar::Metronome => tr("Metronome"),
            Bar::Record => tr("Rec"),
            Bar::Mic => tr("Input"),
            Bar::Voice => tr("Voice"),
            Bar::AutoArm => tr("Automation"),
            Bar::Keyboard => tr("Keyboard"),
            Bar::Undo => tr("Undo"),
            Bar::History => tr("History"),
            Bar::Save => tr("Save"),
            Bar::Panel => tr("Panel"),
            Bar::Learn => tr("Learn"),
            Bar::Load => tr("DSP"),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Bar::Mode => tr("Play the pattern, the song or the session"),
            Bar::Pattern => tr("Current pattern picker"),
            Bar::Tap => tr("Tap tempo: tap the beat"),
            Bar::Position => tr("Bar, beat and step of playback"),
            Bar::Scale => tr("Project scale"),
            Bar::Metronome => tr("Metronome"),
            Bar::Record => tr("Record notes and audio"),
            Bar::Mic => tr("Recording panel: input, level, monitoring, latency"),
            Bar::Voice => tr("Voice to notes: sing or hum to play an instrument"),
            Bar::AutoArm => tr("Record automation: Touch, Latch, Write"),
            Bar::Keyboard => tr("Computer keyboard played like a piano"),
            Bar::Undo => tr("Undo and redo buttons"),
            Bar::History => tr("Edit history"),
            Bar::Save => tr("Save button"),
            Bar::Panel => tr("Button that shows or hides the devices"),
            Bar::Learn => tr("Short hands-on lessons"),
            Bar::Load => tr("Audio processor load"),
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Bar::Mode => Glyph::Icon(Icon::Swap),
            Bar::Pattern => Glyph::Icon(Icon::Rack),
            Bar::Tap => Glyph::Text(ponzik_i18n::tr_noop("TAP")),
            Bar::Position => Glyph::Text("1.1.1"),
            Bar::Scale => Glyph::Icon(Icon::Scale),
            Bar::Metronome => Glyph::Icon(Icon::Metronome),
            Bar::Record => Glyph::Rec,
            Bar::Mic => Glyph::Icon(Icon::Mic),
            Bar::Voice => Glyph::Icon(Icon::Voice),
            Bar::AutoArm => Glyph::Icon(Icon::Automation),
            Bar::Keyboard => Glyph::Icon(Icon::Keyboard),
            Bar::Undo => Glyph::Icon(Icon::Undo),
            Bar::History => Glyph::Icon(Icon::History),
            Bar::Save => Glyph::Icon(Icon::Save),
            Bar::Panel => Glyph::Icon(Icon::Panel),
            Bar::Learn => Glyph::Icon(Icon::Learn),
            Bar::Load => Glyph::Gauge,
        }
    }

    /// Largeur dans la vraie barre (px), pour l'aperçu.
    fn width(self) -> f32 {
        match self {
            Bar::Mode => 76.0,
            Bar::Pattern => 198.0,
            Bar::Tap => 44.0,
            Bar::Position => 132.0,
            Bar::Scale => 134.0,
            Bar::Undo => 72.0,
            Bar::Load => 34.0,
            _ => 38.0,
        }
    }

    /// Where the item is when it is not in the top bar.
    fn elsewhere(self) -> &'static str {
        match self {
            Bar::Mode => tr("Ctrl+K: switch pattern / song"),
            Bar::Pattern => tr("Also in the Channel Rack and the piano roll"),
            Bar::Tap => tr("Also when hovering the tempo"),
            Bar::Scale => tr("Also in the Channel Rack and the piano roll"),
            Bar::Record | Bar::Position | Bar::Load | Bar::Undo => tr("Ctrl+K finds every action"),
            Bar::Metronome | Bar::Mic | Bar::Voice | Bar::AutoArm | Bar::Keyboard => tr("Also in the menu of the record button"),
            Bar::History => tr("Also with a right-click on undo"),
            Bar::Save | Bar::Learn => tr("Also in the main menu"),
            Bar::Panel => tr("Also on the devices panel itself"),
        }
    }

}

/// Ce qu'affiche une tuile.
#[derive(Clone, Copy)]
pub(super) enum Glyph {
    Icon(Icon),
    Text(&'static str),
    Rec,
    Gauge,
    CountIn,
}

/// Espace de travail : vues proposées et éléments affichés.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    /// Dans l'ordre de [`View::ALL`].
    pub views: [bool; 6],
    pub browser: bool,
    pub panel: bool,
    pub count_in: bool,
    pub bar: [bool; Bar::ALL.len()],
}

impl Default for Workspace {
    /// The "Simple" profile: every view, a calm top bar.
    fn default() -> Self {
        Self { views: [true; 6], browser: true, panel: true, count_in: true, bar: Bar::ALL.map(Bar::simple) }
    }
}

fn view_id(v: View) -> &'static str {
    match v {
        View::Rack => "rack",
        View::Piano => "piano",
        View::Playlist => "playlist",
        View::Session => "session",
        View::Mixer => "mixer",
        View::Lab => "lab",
    }
}

fn view_hint(v: View) -> &'static str {
    match v {
        View::Rack => tr("Steps and channels: the heart of the beat"),
        View::Piano => tr("Melodies and chords, note by note"),
        View::Playlist => tr("The arrangement of the song"),
        View::Session => tr("Clips launched live"),
        View::Mixer => tr("Tracks, effects and sends"),
        View::Lab => tr("Chop, edit and flip samples"),
    }
}

impl Workspace {
    pub fn view(&self, v: View) -> bool {
        self.views[v.index()]
    }

    pub fn bar(&self, b: Bar) -> bool {
        self.bar[b.index()]
    }

    /// Au moins une vue reste proposée.
    fn fix(&mut self) {
        if !self.views.iter().any(|&v| v) {
            self.views = [true; 6];
        }
    }

    /// The simple bar, with `show` added and `hide` taken away.
    fn with(views: &[View], browser: bool, panel: bool, show: &[Bar], hide: &[Bar]) -> Self {
        let mut ws = Workspace { browser, panel, ..Default::default() };
        ws.views = View::ALL.map(|v| views.contains(&v));
        for b in show {
            ws.bar[b.index()] = true;
        }
        for b in hide {
            ws.bar[b.index()] = false;
        }
        ws
    }

    /// Everything shown, as before the calm top bar.
    fn full() -> Self {
        Self { bar: [true; Bar::ALL.len()], ..Default::default() }
    }
}

/// Profils en un clic : nom, description, espace de travail.
pub fn presets() -> [(&'static str, &'static str, Workspace); 6] {
    use View::*;
    [
        (tr("Simple"), tr("Every view, a calm top bar"), Workspace::default()),
        (tr("Full"), tr("All of Ponzik, every button in the top bar"), Workspace::full()),
        (tr("Beatmaker"), tr("Rack, playlist and mixer"), Workspace::with(&[Rack, Playlist, Mixer], true, true, &[Bar::Pattern, Bar::Tap], &[])),
        (tr("Sampling"), tr("Everything but the piano roll"), Workspace::with(&[Rack, Playlist, Session, Mixer, Lab], true, true, &[Bar::Mic], &[])),
        (
            tr("Live"),
            tr("Session and mixer, to play live"),
            Workspace::with(&[Session, Mixer], true, true, &[Bar::Tap, Bar::Mic, Bar::AutoArm], &[Bar::Position]),
        ),
        (tr("Minimal"), tr("Rack and playlist, nothing else"), Workspace::with(&[Rack, Playlist], true, false, &[], &[Bar::Load, Bar::Metronome])),
    ]
}

/// Taille de l'interface choisie par l'utilisateur (75 % à 200 %), multipliée
/// par l'échelle du système (écran HiDPI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiScale(f32);

impl Default for UiScale {
    fn default() -> Self {
        UiScale(1.0)
    }
}

impl UiScale {
    pub const MIN: f32 = 0.75;
    pub const MAX: f32 = 2.0;

    /// Bornée et arrondie au pas de 5 %.
    pub fn new(v: f32) -> Self {
        if !v.is_finite() {
            return Self::default();
        }
        UiScale((v.clamp(Self::MIN, Self::MAX) * 20.0).round() / 20.0)
    }

    pub fn get(self) -> f32 {
        self.0
    }

    pub fn percent(self) -> u32 {
        (self.0 * 100.0).round() as u32
    }

    /// Cran suivant pour Ctrl = / Ctrl − : le multiple de 10 % au-dessus ou en dessous.
    pub fn step(self, up: bool) -> Self {
        let tenths = self.0 * 10.0;
        let next = if up { (tenths + 1e-3).floor() + 1.0 } else { (tenths - 1e-3).ceil() - 1.0 };
        Self::new(next / 10.0)
    }

    /// Position 0..1 sur la glissière, et retour.
    fn fraction(self) -> f32 {
        (self.0 - Self::MIN) / (Self::MAX - Self::MIN)
    }

    fn from_fraction(t: f32) -> Self {
        Self::new(Self::MIN + t.clamp(0.0, 1.0) * (Self::MAX - Self::MIN))
    }

    fn from_json(j: &Json) -> Self {
        j.get("ui_scale").and_then(Json::as_f64).map_or_else(Self::default, |v| Self::new(v as f32))
    }
}

/// Réglages enregistrés et état de leur écran.
#[derive(Default)]
pub struct Prefs {
    pub ws: Workspace,
    pub keymap: Keymap,
    pub ui_scale: UiScale,
    /// Action dont on attend la nouvelle touche.
    capture: Option<Shortcut>,
    /// Touche demandée déjà prise : (action, touche, action qui l'a).
    conflict: Option<(Shortcut, Combo, Shortcut)>,
    scroll: f32,
    /// Langue choisie ; `None` suit celle du système.
    pub lang: Option<Lang>,
    /// The first-run tour was finished or skipped.
    pub tour_done: bool,
    /// First-time hints already seen (one bit per hint).
    pub hints_seen: u32,
    /// Tampon audio en millisecondes (0 : celui par défaut, 10 ms).
    pub buffer_ms: u32,
    /// Tampon choisi à la main : jamais relevé automatiquement.
    pub buffer_manual: bool,
    /// Smart disable, eco mode, export quality.
    pub perf: super::performance::Perf,
    /// Action that closes the sheet to open its own popup ("Create a translation").
    pending: Option<Action>,
}

/// Tampon audio enregistré : (millisecondes, choisi à la main).
pub fn buffer_from_json(j: &Json) -> (u32, bool) {
    let ms = j.get("audio_buffer_ms").and_then(Json::as_f64).map_or(0, |v| if v.is_finite() { v.clamp(0.0, 100.0) as u32 } else { 0 });
    (ms, j.get("audio_buffer_manual").and_then(Json::as_bool).unwrap_or(false))
}

/// Tampon audio des sessions précédentes, appliqué avant d'ouvrir la sortie.
pub fn init_audio_buffer() {
    let saved = std::fs::read_to_string(settings_path()).ok().and_then(|t| json::parse(&t).ok());
    if let Some((ms, _)) = saved.as_ref().map(buffer_from_json)
        && ms > 0
    {
        ponzik_audio::set_target_latency_ms(ms);
    }
}

/// Texte JSON des réglages.
pub fn to_json(ws: &Workspace, keymap: &Keymap, theme: &str, lang: Option<Lang>) -> Json {
    let views = View::ALL.iter().map(|&v| (view_id(v).to_string(), Json::Bool(ws.view(v)))).collect();
    let bar = Bar::ALL.iter().map(|&b| (b.id().to_string(), Json::Bool(ws.bar(b)))).collect();
    let keys = keymap.overrides().into_iter().map(|(id, text)| (id.to_string(), Json::Str(text))).collect();
    let mut fields = vec![
        ("version".into(), VERSION.into()),
        ("theme".into(), theme.into()),
        ("views".into(), Json::Obj(views)),
        ("browser".into(), ws.browser.into()),
        ("panel".into(), ws.panel.into()),
        ("count_in".into(), ws.count_in.into()),
        ("bar".into(), Json::Obj(bar)),
        ("keys".into(), Json::Obj(keys)),
    ];
    if let Some(lang) = lang {
        fields.push(("language".into(), lang.code().into()));
    }
    Json::Obj(fields)
}

/// Langue enregistrée (absente : celle du système).
pub fn lang_from_json(j: &Json) -> Option<Lang> {
    j.get("language").and_then(Json::as_str).and_then(Lang::from_code)
}

/// Langue au démarrage, avant de construire quoi que ce soit : `PONZIK_LANG`,
/// sinon celle des réglages, sinon l'anglais (les traductions se choisissent).
/// Les traductions de l'utilisateur (`<config>/lang`) sont lues d'abord.
pub fn init_language() {
    ponzik_i18n::set_user_dir(Some(compat::translations_dir()));
    let env = std::env::var("PONZIK_LANG").ok().and_then(|v| Lang::from_code(&v));
    let saved = || std::fs::read_to_string(settings_path()).ok().and_then(|t| json::parse(&t).ok()).and_then(|j| lang_from_json(&j));
    ponzik_i18n::set_lang(env.or_else(saved).unwrap_or(Lang::EN));
}

/// Lecture tolérante : tout ce qui manque ou ne se lit pas garde sa valeur par défaut.
pub fn from_json(j: &Json) -> (Workspace, Keymap, Option<String>) {
    let mut ws = Workspace::default();
    let flag = |obj: Option<&Json>, key: &str| obj.and_then(|o| o.get(key)).and_then(Json::as_bool);
    for (i, &v) in View::ALL.iter().enumerate() {
        if let Some(on) = flag(j.get("views"), view_id(v)) {
            ws.views[i] = on;
        }
    }
    // Before version 2 every item was shown: an untouched bar takes the calm
    // default, a customised one keeps its choices and shows what it showed.
    let old = j.get("version").and_then(Json::as_f64).unwrap_or(0.0) < 2.0;
    let customised = OLD_BARS.iter().any(|id| flag(j.get("bar"), id) == Some(false));
    if old && customised {
        ws.bar = [true; Bar::ALL.len()];
        let record = flag(j.get("bar"), "record").unwrap_or(true);
        for b in [Bar::Mic, Bar::Voice, Bar::AutoArm] {
            ws.bar[b.index()] = record;
        }
    }
    if !old || customised {
        for (i, &b) in Bar::ALL.iter().enumerate() {
            if let Some(on) = flag(j.get("bar"), b.id()) {
                ws.bar[i] = on;
            }
        }
    }
    ws.browser = j.get("browser").and_then(Json::as_bool).unwrap_or(ws.browser);
    ws.panel = j.get("panel").and_then(Json::as_bool).unwrap_or(ws.panel);
    ws.count_in = j.get("count_in").and_then(Json::as_bool).unwrap_or(ws.count_in);
    ws.fix();
    let mut keymap = Keymap::default();
    if let Some(pairs) = j.get("keys").and_then(Json::as_obj) {
        keymap.apply_overrides(pairs.iter().filter_map(|(k, v)| Some((k.as_str(), v.as_str()?))));
    }
    let theme = j.get("theme").and_then(Json::as_str).map(str::to_string);
    (ws, keymap, theme)
}

fn settings_path() -> std::path::PathBuf {
    compat::config_dir().join("settings.json")
}

/// Rangées de l'écran des raccourcis : en-têtes de famille et actions, par colonne.
enum KeyRow {
    Head(&'static str),
    Item(Shortcut),
}

fn key_columns() -> [Vec<KeyRow>; 2] {
    let mut cols: [Vec<KeyRow>; 2] = [Vec::new(), Vec::new()];
    for (g, name) in GROUPS.iter().enumerate() {
        let col = &mut cols[usize::from(g >= 2)];
        col.push(KeyRow::Head(name));
        col.extend(Shortcut::ALL.iter().filter(|s| s.group() == *name).map(|&s| KeyRow::Item(s)));
    }
    cols
}

impl App {
    // --- Fichier ---

    /// Charge `settings.json` ; un fichier absent ou abîmé laisse les valeurs par défaut.
    pub fn load_settings(&mut self) {
        let path = settings_path();
        self.load_browser_prefs();
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let j = match json::parse(&text) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("settings ignored ({}): {e}", path.display());
                return;
            }
        };
        let (ws, keymap, theme) = from_json(&j);
        self.prefs.lang = lang_from_json(&j);
        self.themes.load_user();
        if let Some(id) = theme {
            self.select_theme(&id);
        }
        self.prefs.keymap = keymap;
        self.prefs.ui_scale = UiScale::from_json(&j);
        self.prefs.tour_done = j.get("tour_done").and_then(Json::as_bool).unwrap_or(false);
        self.lessons.progress = super::lessons::Progress::from_json(j.get("lessons"));
        self.prefs.hints_seen = j.get("hints_seen").and_then(Json::as_f64).map_or(0, |v| if v.is_finite() { v.clamp(0.0, u32::MAX as f64) as u32 } else { 0 });
        (self.prefs.buffer_ms, self.prefs.buffer_manual) = buffer_from_json(&j);
        self.prefs.perf.load(&j);
        self.ctl.global = ponzik_project::midi_map::load_mappings(j.get("midi_map"), usize::MAX);
        self.updates.load(&j);
        self.install.load(&j);
        self.apply_workspace(ws, false);
    }

    pub fn save_settings(&mut self) {
        self.sync_workspace();
        let mut j = to_json(&self.prefs.ws, &self.prefs.keymap, self.themes.active_id(), self.prefs.lang);
        if let Json::Obj(fields) = &mut j {
            fields.push(("ui_scale".into(), (self.prefs.ui_scale.percent() as f64 / 100.0).into()));
            fields.push(("tour_done".into(), self.prefs.tour_done.into()));
            if self.prefs.hints_seen != 0 {
                fields.push(("hints_seen".into(), (self.prefs.hints_seen as f64).into()));
            }
            if !self.lessons.progress.is_empty() {
                fields.push(("lessons".into(), self.lessons.progress.to_json()));
            }
            if !self.ctl.global.is_empty() {
                fields.push(("midi_map".into(), ponzik_project::midi_map::mappings_json(&self.ctl.global)));
            }
            self.updates.save(fields);
            self.install.save(fields);
            self.prefs.perf.save(fields);
            if self.prefs.buffer_ms > 0 {
                fields.push(("audio_buffer_ms".into(), (self.prefs.buffer_ms as f64).into()));
                fields.push(("audio_buffer_manual".into(), self.prefs.buffer_manual.into()));
            }
        }
        let text = j.pretty();
        let path = settings_path();
        let tmp = path.with_extension("json.tmp");
        let res = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&tmp, text))
            .and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = res {
            eprintln!("settings not saved ({}): {e}", path.display());
        }
    }

    /// Les bascules faites ailleurs (Alt F8, pré-compte…) rejoignent l'espace de travail.
    fn sync_workspace(&mut self) {
        let ws = &mut self.prefs.ws;
        (ws.browser, ws.panel, ws.count_in) = (self.sidebar, self.panel, self.count_in);
    }

    /// Applique un espace de travail ; la vue en cours change si elle disparaît.
    pub fn apply_workspace(&mut self, mut ws: Workspace, save: bool) {
        ws.fix();
        (self.sidebar, self.panel, self.count_in) = (ws.browser, ws.panel, ws.count_in);
        self.prefs.ws = ws;
        if !self.view_shown(self.view)
            && let Some(&v) = self.shown_views().first()
        {
            self.set_view(v);
        }
        if save {
            self.save_settings();
        }
    }

    /// Capture: a profile by its English id, and `nopanel` to hide the devices.
    pub fn screenshot_workspace(&mut self, spec: &str) {
        const IDS: [&str; 6] = ["simple", "full", "beatmaker", "sampling", "live", "minimal"];
        for part in spec.split(',').map(str::trim) {
            if part == "nopanel" {
                self.panel = false;
            } else if let Some(i) = IDS.iter().position(|id| *id == part)
                && let Some((_, _, ws)) = presets().into_iter().nth(i)
            {
                self.apply_workspace(ws, false);
            }
        }
    }

    pub fn apply_profile(&mut self, i: usize) {
        if let Some((name, _, ws)) = presets().into_iter().nth(i) {
            self.apply_workspace(ws, true);
            self.toast(&tr_fmt!("Profile: {}", name));
        }
    }

    // --- Taille de l'interface ---

    /// Taille choisie, à multiplier par l'échelle du système.
    pub fn ui_scale(&self) -> f32 {
        self.prefs.ui_scale.get()
    }

    /// Change la taille de l'interface et l'enregistre ; `announce` la montre en bulle.
    pub fn set_ui_scale(&mut self, s: UiScale, announce: bool) {
        if s != self.prefs.ui_scale {
            self.prefs.ui_scale = s;
            self.save_settings();
        }
        if announce {
            self.toast(&tr_fmt!("Interface size: {}%", s.percent()));
        }
    }

    /// Change la langue de l'interface tout de suite et l'enregistre
    /// (`None` : suivre le système).
    pub fn set_language(&mut self, lang: Option<Lang>) {
        self.prefs.lang = lang;
        let lang = lang.unwrap_or(Lang::EN);
        ponzik_i18n::set_lang(lang);
        self.save_settings();
        self.toast(&tr_fmt!("Language: {}", lang.name()));
    }

    // --- Traductions de l'utilisateur ---

    /// Reads the user's translation files again: edits show at once.
    pub fn reload_translations(&mut self) {
        let reg = ponzik_i18n::reload();
        let mine = reg.langs.iter().filter(|l| l.file.is_some()).count();
        let broken = reg.langs.iter().filter(|l| !l.errors.is_empty()).count();
        let mut msg = tr_fmt!("Translations reloaded · {} from your folder", mine);
        if broken > 0 {
            msg = tr_fmt!("{} · {} with problems (see Settings)", msg, broken);
        }
        self.toast(&msg);
    }

    /// Opens the user's translation folder (created if needed).
    pub fn open_translations_folder(&mut self) {
        let dir = compat::translations_dir();
        let res = std::fs::create_dir_all(&dir).and_then(|_| compat::open_in_file_manager(&dir));
        if res.is_err() {
            self.toast(&tr_fmt!("Could not open the folder: {}", dir.display().to_string()));
        }
    }

    /// "Create a translation": writes `<code>.po` ready to fill in (or adds the
    /// missing strings to an existing one), switches to it and shows its folder.
    pub fn create_translation(&mut self, input: &str) {
        let dir = compat::translations_dir();
        let made = match ponzik_i18n::create_translation(&dir, input) {
            Ok(made) => made,
            Err(_) if ponzik_i18n::parse_new_language(input).is_none() => {
                self.toast(tr("Type a language code, such as es or pt_BR, or a language name"));
                return;
            }
            Err(e) => {
                self.toast(&tr_fmt!("Could not write the translation: {}", e));
                return;
            }
        };
        ponzik_i18n::reload();
        if let Some(lang) = ponzik_i18n::Lang::from_code(&made.code) {
            self.prefs.lang = (lang != Lang::EN).then_some(lang);
            ponzik_i18n::set_lang(lang);
            self.save_settings();
        }
        let file = made.path.file_name().map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        let msg = if !made.existed {
            tr_fmt!("{} created: fill in the msgstr lines, save, then reload the translations", file)
        } else if made.added > 0 {
            tr_fmt!("{} already exists: {} new strings added at the end", file, made.added)
        } else {
            tr_fmt!("{} already exists and is up to date", file)
        };
        self.toast_action(&msg, tr("Reload"), Action::ReloadTranslations);
        let _ = compat::open_in_file_manager(&dir);
    }

    // --- Requêtes ---

    pub fn view_shown(&self, v: View) -> bool {
        self.prefs.ws.view(v)
    }

    pub fn shown_views(&self) -> Vec<View> {
        View::ALL.into_iter().filter(|&v| self.view_shown(v)).collect()
    }

    pub fn bar_shown(&self, b: Bar) -> bool {
        self.prefs.ws.bar(b)
    }

    /// Touche d'une action, pour les infobulles et la palette.
    pub fn key_label(&self, s: Shortcut) -> String {
        self.prefs.keymap.label(s)
    }

    /// Affiche ou masque une vue (jamais la dernière).
    pub fn toggle_view_shown(&mut self, v: View) {
        let mut ws = self.prefs.ws.clone();
        let i = v.index();
        ws.views[i] = !ws.views[i];
        if !ws.views.iter().any(|&on| on) {
            self.toast(tr("Keep at least one view"));
            return;
        }
        self.apply_workspace(ws, true);
    }

    /// Capture d'écran : touches changées et un conflit en attente.
    pub fn screenshot_conflict(&mut self) {
        self.prefs.keymap.rebind(Shortcut::ViewSession, Combo::key(Key::F(11)), false);
        self.prefs.keymap.set(Shortcut::TapTempo, None);
        let combo = Combo::ctrl(Key::Char('m'));
        self.prefs.conflict = Some((Shortcut::Record, combo, Shortcut::Metronome));
        self.open_popup(Popup::Settings { tab: 1 }, Rect::default());
    }

    /// Les réglages tiennent Échap pour annuler une saisie de touche.
    pub fn settings_open(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| matches!(p.kind, Popup::Settings { .. }))
    }

    // --- Raccourcis ---

    /// Exécute l'action d'une touche.
    pub(super) fn run_shortcut(&mut self, s: Shortcut) {
        let anchor = Rect::new(60.0, 8.0, 0.0, TOP_BAR - 12.0);
        if let Some(v) = s.view() {
            self.set_view(v);
            return;
        }
        let action = match s {
            Shortcut::PlayStop => Action::PlayStop,
            Shortcut::ToggleMode => Action::ToggleMode,
            Shortcut::Record => {
                self.record = !self.record;
                self.toast(if self.record { tr("Recording armed") } else { tr("Recording disarmed") });
                return;
            }
            Shortcut::TapTempo => Action::TapTempo,
            Shortcut::Metronome => Action::ToggleMetronome,
            Shortcut::TypingKeyboard => Action::ToggleTyping,
            Shortcut::Capture => Action::CaptureMidi,
            Shortcut::NextView => {
                let views = self.shown_views();
                let i = views.iter().position(|v| *v == self.view).map_or(0, |i| (i + 1) % views.len());
                self.set_view(views[i]);
                return;
            }
            Shortcut::ToggleBrowser => Action::ToggleSidebar,
            Shortcut::TogglePanel => Action::TogglePanel,
            Shortcut::ZoomIn | Shortcut::ZoomOut | Shortcut::ZoomReset => {
                let s = match s {
                    Shortcut::ZoomReset => UiScale::default(),
                    _ => self.prefs.ui_scale.step(s == Shortcut::ZoomIn),
                };
                self.set_ui_scale(s, true);
                return;
            }
            Shortcut::NewPattern => {
                let p = self.project.add_pattern();
                self.select_pattern(p);
                self.toast(tr("New pattern"));
                return;
            }
            Shortcut::NextPattern => {
                let next = (self.project.current_pattern + 1).min(self.project.patterns.len() - 1);
                self.select_pattern(next);
                return;
            }
            Shortcut::PrevPattern => {
                self.select_pattern(self.project.current_pattern.saturating_sub(1));
                return;
            }
            Shortcut::Undo => Action::Undo,
            Shortcut::Redo => Action::Redo,
            Shortcut::History => Action::Popup(Popup::History),
            Shortcut::Save => Action::Save,
            Shortcut::SaveAs => Action::SaveAs,
            Shortcut::Open => {
                self.open_popup(Popup::OpenProject(compat::list_projects()), Rect::new(12.0, TOP_BAR, 0.0, 0.0));
                return;
            }
            Shortcut::NewProject => Action::NewProject,
            Shortcut::Export => Action::Export,
            Shortcut::Palette => {
                self.open_palette();
                return;
            }
            Shortcut::Search => {
                self.sidebar = true;
                self.sidebar_focus = true;
                return;
            }
            Shortcut::Settings => Action::Popup(Popup::Settings { tab: 0 }),
            Shortcut::ViewPlaylist | Shortcut::ViewRack | Shortcut::ViewPiano | Shortcut::ViewSession | Shortcut::ViewMixer | Shortcut::ViewLab => {
                return;
            }
        };
        self.run_action(action, anchor);
    }

    // --- Écran des réglages ---

    /// Feuille des réglages ; renvoie l'onglet à garder ouvert, ou `None` pour fermer.
    pub(super) fn settings_sheet(&mut self, c: &mut Canvas, input: &Input, screen: Rect, tab: usize) -> Option<usize> {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        let mut tab = tab.min(5);
        let mut close = false;

        // Clavier : saisie d'une touche, conflit en attente, ou navigation.
        for &(key, mods) in &input.keys {
            if let Some(s) = self.prefs.capture {
                self.prefs.capture = None;
                if key == Key::Escape {
                    continue;
                }
                let combo = Combo::from_event(key, mods);
                match self.prefs.keymap.conflict(combo, s) {
                    Some(other) => self.prefs.conflict = Some((s, combo, other)),
                    None => self.bind(s, combo, false),
                }
            } else if let Some((s, combo, _)) = self.prefs.conflict {
                match key {
                    Key::Escape => self.prefs.conflict = None,
                    Key::Enter => self.bind(s, combo, true),
                    _ => {}
                }
            } else if tab == 3 && self.themes.editor.hex.is_some() {
                // Typed into the theme editor's color field.
            } else {
                match key {
                    Key::Escape => close = true,
                    Key::Tab => tab = (tab + 1) % 6,
                    _ => {}
                }
            }
        }
        if self.prefs.capture.is_some() || self.prefs.conflict.is_some() {
            // Tient l'image éveillée : la touche en attente pulse.
            self.anim.set(wid("caret", 0, 0), self.time);
        }

        // Appearance: a lighter veil, the app behind is the live preview.
        c.fill_rect(screen, t.shade(if tab == 3 { 40.0 } else { 110.0 } * open));
        let w = 960.0f32.min(screen.w - 40.0);
        let h = 690.0f32.min(screen.h - 40.0);
        let r = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.5 + (1.0 - open) * 10.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 10.0).inset(-4.0), 22.0, t.shade(90.0 * open));
        c.fill_rrect(r, 18.0, t.panel);
        c.stroke_rrect(r, 18.0, 1.0, t.hairline.alpha(1.8));

        // En-tête : deux onglets, et la croix.
        let tabs = Rect::new(r.x + 16.0, r.y + 14.0, (r.w - 32.0 - 46.0).min(820.0), 38.0);
        c.fill_rrect(tabs, 11.0, t.bg_deep);
        c.stroke_rrect(tabs, 11.0, 1.0, t.hairline);
        let pos = self.anim.to(wid("set-tabs", 0, 0), tab as f32, T_LAYOUT);
        let seg_w = (tabs.w - 6.0) / 6.0;
        let knob = Rect::new(tabs.x + 3.0 + pos * seg_w, tabs.y + 3.0, seg_w, tabs.h - 6.0);
        c.fill_rrect(knob, 8.0, t.raised_hi);
        let names = [
            (Icon::Layout, tr("Workspace"), tr("Views, panels and top bar")),
            (Icon::Keyboard, tr("Shortcuts"), tr("Change the keys")),
            (Icon::Midi, tr("MIDI"), tr("Controller links: list, modes, ranges")),
            (Icon::Theme, tr("Appearance"), tr("Themes: colors, corners, text size, density, animations")),
            (Icon::Cpu, ponzik_i18n::tr_ctx("settings tab", "Performance"), tr("Processor load, idle effects, eco mode, audio buffer")),
            (Icon::Info, tr("About"), tr("Version, build and update settings")),
        ];
        let mut hint: Option<String> = None;
        for (i, (icon, name, tip)) in names.into_iter().enumerate() {
            let seg = Rect::new(tabs.x + 3.0 + i as f32 * seg_w, tabs.y, seg_w, tabs.h);
            let id = wid("set-tab", i, 0);
            let hov = self.hovered(input, id, seg);
            let hv = self.hover_anim(id, hov);
            let sel = 1.0 - (pos - i as f32).abs().min(1.0);
            let tw = self.fonts.measure(self.f.sans_med, 13.0, name);
            let x0 = seg.x + (seg.w - tw - 24.0) * 0.5;
            let cy = seg.y + seg.h * 0.5;
            draw_icon(c, icon, x0 + 8.0, cy, 15.0, t.muted.mix(t.text, hv * 0.7).mix(t.accent, sel));
            self.text(c, self.f.sans_med, 13.0, x0 + 24.0, cy, name, t.muted.mix(t.text2, hv).mix(t.text, sel));
            if hov {
                hint = Some(tip.to_string());
                if input.pressed && i != tab {
                    tab = i;
                }
            }
        }
        let close_r = Rect::new(r.right() - 16.0 - 34.0, r.y + 16.0, 34.0, 34.0);
        if self.icon_button(c, input, wid("set-close", 0, 0), close_r, Icon::Close, false, (tr("Close"), tr("Esc"))) {
            close = true;
        }
        if self.prefs.capture.is_none() && input.pressed && !r.contains(input.x, input.y) {
            close = true;
        }
        let was = self.anim.get(wid("set-tab-was", 0, 0));
        if was != Some(tab as f32) {
            self.anim.set(wid("set-tab-was", 0, 0), tab as f32);
            self.anim.set(wid("set-enter", 0, 0), 0.0);
            if was.is_some() {
                self.prefs.capture = None;
                self.prefs.conflict = None;
            }
        }

        // Corps, avec un fondu à chaque changement d'onglet.
        let enter = self.anim.to(wid("set-enter", 0, 0), 1.0, T_LAYOUT);
        let body = Rect::new(r.x + 20.0, r.y + 68.0 + ((1.0 - enter) * 8.0).round(), r.w - 40.0, r.h - 68.0 - 52.0);
        let foot = Rect::new(r.x + 20.0, r.bottom() - 46.0, r.w - 40.0, 34.0);
        c.fill_rect(Rect::new(r.x + 1.0, foot.y - 8.0, r.w - 2.0, 1.0), t.hairline);
        match tab {
            0 => self.workspace_tab(c, input, body, &mut hint),
            1 => self.keys_tab(c, input, body, foot, &mut hint),
            2 => self.midi_tab(c, input, body, &mut hint),
            3 => self.appearance_tab(c, input, body, &mut hint),
            4 => self.performance_tab(c, input, body, &mut hint),
            _ => self.about_tab(c, input, body, &mut hint),
        }
        if let Some(action) = self.prefs.pending.take() {
            self.prefs.capture = None;
            self.prefs.conflict = None;
            self.run_action(action, Rect::default());
            return None;
        }
        if enter < 0.999 {
            c.fill_rect(Rect::new(r.x + 1.0, r.y + 60.0, r.w - 2.0, body.h + 8.0), t.panel.alpha(1.0 - enter));
        }
        // Barre d'aide de la feuille : ce que fait l'élément survolé.
        let showing_banner = tab == 1 && self.prefs.conflict.is_some();
        if !showing_banner {
            let line = hint.unwrap_or_else(|| {
                match tab {
                    0 => tr("Click a tile to show or hide it").into(),
                    1 => tr("Click a key, then the new combination · Esc cancels").into(),
                    3 => tr("Built-in themes are read-only: editing one makes a copy in your themes folder").into(),
                    4 => tr("Live costs of the audio engine; the switches act at once and are saved").into(),
                    5 => tr("Updates come from the GitHub releases of Ponzik; nothing is sent about you").into(),
                    _ => tr("Global links follow the controller in every project; project links are saved with the song").into(),
                }
            });
            let right = if tab == 1 { foot.x + 190.0 } else { foot.x };
            self.text_fit(c, self.f.sans, 12.0, right, foot.y + foot.h * 0.5, &line, foot.right() - right, t.text2);
        }
        if close {
            self.prefs.capture = None;
            self.prefs.conflict = None;
            return None;
        }
        Some(tab)
    }

    fn bind(&mut self, s: Shortcut, combo: Combo, swap: bool) {
        self.prefs.keymap.rebind(s, combo, swap);
        self.prefs.conflict = None;
        self.anim.set(wid("set-flash", s as usize, 0), 1.0);
        self.save_settings();
    }

    pub(super) fn section(&mut self, c: &mut Canvas, x: f32, y: f32, label: &str) {
        let t = self.theme;
        self.text(c, self.f.sans_med, 11.0, x + 2.0, y + 8.0, label, t.muted);
    }

    /// Tuile à bascule : pictogramme, nom, petit interrupteur dans le coin.
    pub(super) fn tile(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, glyph: Glyph, label: &str, on: bool, key: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.95 } else { 1.0 }, T_PRESS);
        let body = r.scale(press);
        let fill = t.well.mix(t.raised, onv).mix(t.raised_hi, hv * 0.5);
        c.fill_rrect(body, 12.0, fill);
        c.stroke_rrect(body, 12.0, 1.0, t.hairline.mix(t.accent.alpha(0.55), onv));
        // Interrupteur : une pastille qui glisse vers la droite et s'allume.
        let sw = Rect::new(body.right() - 26.0, body.y + 9.0, 16.0, 9.0);
        c.fill_rrect(sw, 4.5, t.raised_hi.mix(t.accent.alpha(0.35), onv));
        c.fill_circle(sw.x + 4.5 + onv * 7.0, sw.y + 4.5, 3.2, t.muted.mix(t.accent, onv));
        let ink = t.muted.alpha(0.55).mix(t.accent, onv).mix(t.text, hv * 0.25);
        let big = r.h >= 80.0;
        let gy = body.y + if big { 34.0 } else { 26.0 };
        let (gx, size) = (body.x + body.w * 0.5, if big { 24.0 } else { 18.0 });
        self.glyph(c, glyph, gx, gy, size, ink);
        let ly = body.y + if big { 62.0 } else { 48.0 };
        let txt = t.muted.mix(t.text, onv.max(hv * 0.6));
        let fs = if big { 12.0 } else { 11.0 };
        let label = self.fit(self.f.sans_med, fs, label, body.w - 8.0);
        self.text_center(c, self.f.sans_med, fs, gx, ly, &label, txt);
        if big && !key.is_empty() {
            let kw = self.fonts.measure(self.f.mono, 10.0, key);
            self.text(c, self.f.mono, 10.0, gx - kw * 0.5, body.bottom() - 11.0, key, t.muted.alpha(0.5 + 0.5 * onv));
        }
        hov && input.pressed
    }

    fn glyph(&mut self, c: &mut Canvas, g: Glyph, cx: f32, cy: f32, size: f32, col: Color) {
        let t = self.theme;
        match g {
            Glyph::Icon(icon) => draw_icon(c, icon, cx, cy, size, col),
            Glyph::Text(s) => self.text_center(c, self.f.mono_med, size * 0.6, cx, cy, tr(s), col),
            Glyph::Rec => c.fill_circle(cx, cy, size * 0.3, t.negative.alpha(col.a as f32 / 255.0)),
            Glyph::CountIn => {
                c.stroke_circle(cx, cy, size * 0.42, 1.4, col);
                c.stroke_arc(cx, cy, size * 0.42, -std::f32::consts::FRAC_PI_2, 0.6, 2.4, col);
                c.fill_circle(cx, cy, size * 0.2, col);
            }
            Glyph::Gauge => {
                let g = Rect::new(cx - size * 0.6, cy - 3.0, size * 1.2, 6.0);
                c.fill_rrect(g, 3.0, col.alpha(0.3 * col.a as f32 / 255.0));
                c.fill_rrect(Rect::new(g.x, g.y, g.w * 0.45, g.h), 3.0, col);
            }
        }
    }

    fn workspace_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let gap = 8.0;
        let left = Rect::new(body.x, body.y, (body.w * 0.62).round() - 12.0, body.h);
        let right = Rect::new(left.right() + 24.0, body.y, body.right() - left.right() - 24.0, body.h);
        let mut y = left.y;

        // Profils.
        self.section(c, left.x, y, tr("Profiles"));
        y += 20.0;
        let current = {
            let mut ws = self.prefs.ws.clone();
            (ws.browser, ws.panel, ws.count_in) = (self.sidebar, self.panel, self.count_in);
            ws
        };
        let n_cards = presets().len() as f32;
        let card_w = ((left.w - (n_cards - 1.0) * gap) / n_cards).floor();
        for (i, (name, desc, ws)) in presets().into_iter().enumerate() {
            let card = Rect::new(left.x + i as f32 * (card_w + gap), y, card_w, 70.0);
            let id = wid("set-preset", i, 0);
            let hov = self.hovered(input, id, card);
            let hv = self.hover_anim(id, hov);
            let active = ws == current;
            let on = self.anim.to(id ^ 0x4f4e, active as u8 as f32, T_HOVER);
            let lift = (hv * 2.0).round();
            let body = card.translate(0.0, -lift);
            c.fill_rrect(body, 12.0, t.raised.mix(t.raised_hi, hv * 0.6).mix(t.accent.alpha(0.18), on * 0.6));
            c.stroke_rrect(body, 12.0, 1.0 + on * 0.5, t.hairline.mix(t.accent, on));
            // Les vues du profil, en petites icônes.
            let views: Vec<View> = View::ALL.into_iter().filter(|&v| ws.view(v)).collect();
            let step = ((card_w - 20.0) / views.len().max(1) as f32).min(16.0);
            let x0 = body.x + body.w * 0.5 - (views.len() as f32 - 1.0) * step * 0.5;
            for (k, v) in views.iter().enumerate() {
                draw_icon(c, v.icon(), x0 + k as f32 * step, body.y + 24.0, (step - 3.0).min(12.0), t.muted.mix(t.accent, on.max(hv * 0.5)));
            }
            self.text_center(c, self.f.sans_med, 12.0, body.x + body.w * 0.5, body.y + 50.0, name, t.text2.mix(t.text, on.max(hv)));
            if hov {
                *hint = Some(format!("{name} · {desc}"));
                if input.pressed {
                    self.apply_profile(i);
                }
            }
        }
        y += 70.0 + 18.0;

        // Vues.
        self.section(c, left.x, y, tr("Views"));
        y += 20.0;
        // Every view on one row, however many there are.
        let n_views = View::ALL.len().max(1) as f32;
        let tile_w = ((left.w - (n_views - 1.0) * gap) / n_views).floor();
        for (i, v) in View::ALL.into_iter().enumerate() {
            let r = Rect::new(left.x + i as f32 * (tile_w + gap), y, tile_w, 92.0);
            let key = self.key_label(Shortcut::for_view(v));
            let on = self.view_shown(v);
            let id = wid("set-view", i, 0);
            if self.tile(c, input, id, r, Glyph::Icon(v.icon()), v.label(), on, &key) {
                self.toggle_view_shown(v);
            }
            if self.hovered(input, id, r) {
                *hint = Some(format!("{} · {}", v.label(), view_hint(v)));
            }
        }
        y += 92.0 + 18.0;

        // Panneaux, puis la barre du haut : chaque élément rangé peut y revenir.
        self.section(c, left.x, y, tr("Panels and top bar"));
        y += 20.0;
        #[derive(Clone, Copy)]
        enum Cell {
            Browser,
            Devices,
            CountIn,
            Bar(Bar),
        }
        let cols = 7;
        let small_w = ((left.w - (cols - 1) as f32 * gap) / cols as f32).floor();
        let mut cells: Vec<(Cell, Glyph, &str, &str, bool)> = vec![
            (Cell::Browser, Glyph::Icon(Icon::Sidebar), tr("Browser"), tr("Sounds, instruments and effects"), self.sidebar),
            (Cell::Devices, Glyph::Icon(Icon::Panel), tr("Devices"), tr("Details of the selected channel or track"), self.panel),
        ];
        for b in Bar::ALL {
            cells.push((Cell::Bar(b), b.glyph(), b.label(), b.hint(), self.bar_shown(b)));
            if b == Bar::Record {
                cells.push((Cell::CountIn, Glyph::CountIn, tr("Count-in"), tr("One bar of metronome before recording"), self.count_in));
            }
        }
        let tile_h = 56.0;
        let mut toggled = None;
        for (k, &(cell, glyph, label, desc, on)) in cells.iter().enumerate() {
            let r = Rect::new(left.x + (k % cols) as f32 * (small_w + gap), y + (k / cols) as f32 * (tile_h + gap), small_w, tile_h);
            let id = wid("set-cell", k, 0);
            if self.tile(c, input, id, r, glyph, label, on, "") {
                toggled = Some(cell);
            }
            if self.hovered(input, id, r) {
                // Ce qui n'est pas dans la barre reste à portée : on dit où.
                let place = match cell {
                    Cell::Bar(b) if !on => format!(" · {}", b.elsewhere()),
                    _ => String::new(),
                };
                *hint = Some(format!("{label} · {desc}{place}"));
            }
        }
        let rows = cells.len().div_ceil(cols);
        y += rows as f32 * (tile_h + gap) - gap + 18.0;
        // Taille de l'interface et tampon audio côte à côte.
        let size_w = (left.w * 0.38).floor();
        self.section(c, left.x, y, tr("Interface size"));
        self.ui_scale_slider(c, input, Rect::new(left.x, y + 20.0, size_w, 34.0), hint);
        self.audio_buffer_choice(c, input, left.x + size_w + 24.0, y, left.w - size_w - 24.0, hint);

        if let Some(cell) = toggled {
            let mut ws = current.clone();
            match cell {
                Cell::Browser => ws.browser = !ws.browser,
                Cell::Devices => ws.panel = !ws.panel,
                Cell::CountIn => ws.count_in = !ws.count_in,
                Cell::Bar(b) => ws.bar[b.index()] = !ws.bar[b.index()],
            }
            self.apply_workspace(ws, true);
        }

        // Aperçu en direct.
        self.section(c, right.x, right.y, tr("Preview"));
        let fw = right.w;
        let fh = (fw * 0.66).round();
        self.workspace_preview(c, Rect::new(right.x, right.y + 20.0, fw, fh));

        // Sous l'aperçu : le profil reconnu, ou « Sur mesure ».
        let found = presets().into_iter().find(|(_, _, ws)| *ws == current);
        let (name, desc) = found.map_or((tr("Custom"), tr("Your own workspace")), |(n, d, _)| (n, d));
        let y = right.y + 20.0 + fh + 28.0;
        self.text(c, self.f.sans_med, 16.0, right.x + 2.0, y, name, t.text);
        self.text_fit(c, self.f.sans, 12.0, right.x + 2.0, y + 22.0, desc, right.w - 4.0, t.text2);
        let shown = self.shown_views();
        for (i, v) in shown.iter().enumerate() {
            let cx = right.x + 14.0 + i as f32 * 34.0;
            c.fill_rrect(Rect::new(cx - 14.0, y + 42.0, 28.0, 28.0), 8.0, t.raised);
            draw_icon(c, v.icon(), cx, y + 56.0, 15.0, t.accent);
        }

        self.language_choice(c, input, right, y + 96.0, hint);
    }

    /// Langue de l'interface (effet immédiat) : l'anglais par défaut, les
    /// traductions livrées et celles de l'utilisateur, avec leur avancement ;
    /// à droite du titre, créer une traduction, la recharger, ouvrir son dossier.
    fn language_choice(&mut self, c: &mut Canvas, input: &Input, right: Rect, y: f32, hint: &mut Option<String>) {
        let t = self.theme;
        struct Choice {
            lang: Option<Lang>,
            label: String,
            tip: String,
            broken: bool,
            w: f32,
        }
        let mut choices = Vec::new();
        for info in &ponzik_i18n::registry().langs {
            // Rounded down, but a started translation never reads 0%.
            let pct = ((info.completion * 100.0).floor() as u32).max(u32::from(info.completion > 0.0));
            let label = if info.lang == Lang::EN || pct >= 100 { info.name.clone() } else { format!("{} · {}%", info.name, pct) };
            let file = info.file.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().into_owned());
            let tip = match (&file, info.errors.first()) {
                (Some(f), Some(e)) => tr_fmt!("{}: {} problems, the readable lines are used · first: {}", f, info.errors.len(), e.as_str()),
                (Some(f), None) if info.embedded => tr_fmt!("Interface in {} · {}% translated · corrected by your file {}", info.name.as_str(), pct, f),
                (Some(f), None) => tr_fmt!("Interface in {} · {}% translated · your file {}", info.name.as_str(), pct, f),
                (None, _) if info.lang == Lang::EN => tr_fmt!("Interface in {} · takes effect immediately", info.name.as_str()),
                (None, _) => tr_fmt!("Interface in {} · {}% translated · takes effect immediately", info.name.as_str(), pct),
            };
            let w = self.fonts.measure(self.f.sans_med, 12.0, &label) + 44.0;
            choices.push(Choice { lang: (info.lang != Lang::EN).then_some(info.lang), label, tip, broken: !info.errors.is_empty(), w });
        }
        // Rangées nécessaires : la section remonte pour tenir dans la colonne.
        let (mut x, mut rows) = (right.x, 1);
        for ch in &choices {
            if x + ch.w > right.right() && x > right.x {
                x = right.x;
                rows += 1;
            }
            x += ch.w + 8.0;
        }
        let y = y.min(right.bottom() - 22.0 - rows as f32 * 36.0);
        self.section(c, right.x, y, tr("Language"));
        let tools = [
            (Icon::Plus, tr("Create a translation"), tr("A file with every text of Ponzik to translate, in your translations folder"), 0),
            (Icon::Reset, tr("Reload translations"), tr("Reads your translation files again: no restart needed"), 1),
            (Icon::Folder, tr("Open the translations folder"), tr("Where your .po translation files live"), 2),
        ];
        let mut tool = None;
        for (i, (icon, name, desc, k)) in tools.into_iter().enumerate() {
            let r = Rect::new(right.right() - (3 - i) as f32 * 28.0, y - 6.0, 24.0, 24.0);
            let id = wid("set-lang-tool", k, 0);
            if self.icon_button(c, input, id, r, icon, false, (name, "")) {
                tool = Some(k);
            }
            if self.hovered(input, id, r) {
                *hint = Some(format!("{name} · {desc}"));
            }
        }
        let (mut x, mut row_y) = (right.x, y + 20.0);
        for (k, ch) in choices.iter().enumerate() {
            if x + ch.w > right.right() && x > right.x {
                x = right.x;
                row_y += 36.0;
            }
            let r = Rect::new(x, row_y, ch.w, 30.0);
            let id = wid("set-lang", k, 0);
            // The language in use (PONZIK_LANG may differ from the saved one).
            let active = ch.lang.unwrap_or(Lang::EN) == ponzik_i18n::lang();
            let icon = if active { Icon::Check } else { Icon::Globe };
            if self.pill_button(c, input, id, r, icon, &ch.label, active) && !active {
                self.set_language(ch.lang);
            }
            if ch.broken {
                c.fill_circle(r.right() - 7.0, r.y + 7.0, 4.5, t.panel);
                c.fill_circle(r.right() - 7.0, r.y + 7.0, 3.0, t.negative);
            }
            if self.hovered(input, id, r) {
                *hint = Some(ch.tip.clone());
                if ch.broken {
                    self.set_tip(&ch.tip, "");
                }
            }
            x += ch.w + 8.0;
        }
        match tool {
            Some(0) => self.prefs.pending = Some(Action::Popup(Popup::Rename { target: Rename::Translation, text: String::new() })),
            Some(1) => self.reload_translations(),
            Some(_) => self.open_translations_folder(),
            None => {}
        }
    }

    /// Tampon audio : automatique (10 ms, relevé après des craquements) ou fixé.
    pub(super) fn audio_buffer_choice(&mut self, c: &mut Canvas, input: &Input, x: f32, y: f32, w: f32, hint: &mut Option<String>) {
        self.section(c, x, y, tr("Audio buffer"));
        let current = ponzik_audio::target_latency_ms();
        let auto = !self.prefs.buffer_manual;
        let choices = std::iter::once(None).chain(super::dropouts::BUFFER_STEPS.iter().map(|&ms| Some(ms)));
        let (mut cx, mut cy) = (x, y + 20.0);
        let mut picked = None;
        for (k, choice) in choices.enumerate() {
            let label = match choice {
                None => tr_fmt!("Auto · {} ms", current),
                Some(ms) => tr_fmt!("{} ms", ms),
            };
            let bw = self.fonts.measure(self.f.sans_med, 12.0, &label) + 40.0;
            if cx + bw > x + w && cx > x {
                cx = x;
                cy += 36.0;
            }
            let r = Rect::new(cx, cy, bw, 30.0);
            let id = wid("set-buffer", k, 0);
            let active = match choice {
                None => auto,
                Some(ms) => !auto && ms == current,
            };
            let icon = if active { Icon::Check } else { Icon::History };
            if self.pill_button(c, input, id, r, icon, &label, active) && !active {
                picked = Some(choice);
            }
            if self.hovered(input, id, r) {
                *hint = Some(match choice {
                    None => tr("Starts at 10 ms and goes up a step when the sound crackles").to_string(),
                    Some(ms) => tr_fmt!("{} ms: a larger buffer crackles less but answers later", ms),
                });
            }
            cx += bw + 6.0;
        }
        if let Some(choice) = picked {
            self.set_audio_buffer(choice, self.time);
        }
    }

    /// Glissière de la taille de l'interface, appliquée au relâchement : changer
    /// l'échelle pendant le glisser déplacerait la glissière sous la souris.
    fn ui_scale_slider(&mut self, c: &mut Canvas, input: &Input, r: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let id = wid("set-ui-scale", 0, 0);
        let track = Rect::new(r.x + 10.0, r.y, r.w - 10.0 - 96.0, r.h);
        let at = |x: f32| UiScale::from_fraction((x - track.x) / track.w);
        let hov = self.hovered(input, id, track);
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
        }
        let mut value = self.prefs.ui_scale;
        if self.is_dragging(id) {
            value = at(input.x);
            self.set_cursor(ponzik_window::Cursor::ResizeH);
            if input.released || !input.down {
                self.set_ui_scale(value, false);
            }
        }
        if hov && input.double {
            self.drag = None;
            value = UiScale::default();
            self.set_ui_scale(value, false);
        }
        let pos = self.anim.to(id ^ 0x504f53, value.fraction(), T_PRESS);
        let cy = track.y + track.h * 0.5;
        let bar = Rect::new(track.x, cy - 3.0, track.w, 6.0);
        c.fill_rrect(bar, 3.0, t.well.mix(t.raised_hi, hv * 0.5));
        c.fill_rrect(Rect::new(bar.x, bar.y, bar.w * pos, bar.h), 3.0, t.accent.alpha(0.55 + 0.45 * hv));
        // Repère de 100 %.
        let x100 = track.x + UiScale::default().fraction() * track.w;
        c.fill_rect(Rect::new(x100 - 0.5, cy + 6.0, 1.0, 5.0), t.muted.alpha(0.6));
        let kx = track.x + pos * track.w;
        c.fill_circle(kx, cy, 8.0 + hv, t.text);
        c.fill_circle(kx, cy, 3.0, t.accent);
        let label = tr_fmt!("{}%", value.percent());
        self.text_right(c, self.f.mono_med, 14.0, r.right() - 34.0, cy, &label, t.text);
        // Retour à 100 %.
        let reset = Rect::new(r.right() - 28.0, cy - 14.0, 28.0, 28.0);
        let changed = value != UiScale::default();
        let key = self.key_label(Shortcut::ZoomReset);
        if changed && self.icon_button(c, input, wid("set-ui-scale-reset", 0, 0), reset, Icon::Reset, false, (tr("Back to 100%"), &key)) {
            self.set_ui_scale(UiScale::default(), false);
        }
        if hov || self.is_dragging(id) {
            let (up, down) = (self.key_label(Shortcut::ZoomIn), self.key_label(Shortcut::ZoomOut));
            *hint = Some(tr_fmt!("Size of text and controls, on top of the screen scale · {} / {}", up, down));
        }
    }

    /// Miniature de l'interface : barre du haut, navigateur, onglets, panneau du bas.
    fn workspace_preview(&mut self, c: &mut Canvas, f: Rect) {
        let t = self.theme;
        c.fill_rrect(f.translate(0.0, 4.0), 12.0, t.shade(60.0));
        c.fill_rrect(f, 12.0, t.bg);
        let prev = c.push_clip(f.inset(1.0));
        let k = f.w / 1600.0;
        let top = (TOP_BAR * k * 1.4).round();
        c.fill_rect(Rect::new(f.x, f.y, f.w, top), t.bg_deep);
        c.fill_rect(Rect::new(f.x, f.y + top, f.w, 1.0), t.hairline);
        let cy = f.y + top * 0.5;
        // Barre : la capsule du transport au centre (lecture, arrêt et tempo
        // toujours là), motif et gamme à côté, le reste à droite ; tout glisse en place.
        let play = (None, 84.0);
        let tempo = (None, 118.0);
        let mut widths: Vec<(Option<Bar>, f32)> = vec![(Some(Bar::Mode), Bar::Mode.width()), play];
        for b in [Bar::Record, Bar::Mic, Bar::Voice, Bar::AutoArm, Bar::Keyboard] {
            widths.push((Some(b), b.width()));
        }
        widths.push(tempo);
        for b in [Bar::Tap, Bar::Metronome, Bar::Position] {
            widths.push((Some(b), b.width()));
        }
        let mut items = Vec::new();
        let mut total = 0.0;
        for (i, (b, wpx)) in widths.iter().enumerate() {
            let on = b.is_none_or(|b| self.bar_shown(b));
            let v = self.anim.to(wid("set-pv-bar", i, 0), on as u8 as f32, T_LAYOUT);
            items.push((*b, wpx * k * v, v));
            total += wpx * k * v;
        }
        let side: Vec<f32> = [Bar::Pattern, Bar::Scale]
            .into_iter()
            .enumerate()
            .map(|(i, b)| b.width() * k * self.anim.to(wid("set-pv-side", i, 0), self.bar_shown(b) as u8 as f32, T_LAYOUT))
            .collect();
        let side_w: f32 = side.iter().sum();
        let x0 = f.x + ((f.w - total - side_w) * 0.5).max(4.0);
        let cap = Rect::new(x0 - 2.0, cy - top * 0.36, total + 4.0, top * 0.72);
        c.fill_rrect(cap, 4.0, t.raised.alpha(0.45));
        c.stroke_rrect(cap, 4.0, 1.0, t.hairline);
        let mut x = x0;
        for (i, (b, w, v)) in items.into_iter().enumerate() {
            if w > 1.0 {
                let blk = Rect::new(x + 1.0, cy - top * 0.26, (w - 2.0).max(0.0), top * 0.52);
                let col = match b {
                    None if i == 1 => t.accent,
                    Some(Bar::Record) => t.negative.alpha(0.8),
                    _ => t.raised_hi,
                };
                c.fill_rrect(blk, 2.5, col.alpha(v));
            }
            x += w;
        }
        for w in side {
            if w > 1.0 {
                c.fill_rrect(Rect::new(x + 4.0, cy - top * 0.26, w - 4.0, top * 0.52), 2.5, t.raised);
            }
            x += w;
        }
        // Droite : menu et recherche toujours là, puis ce que l'espace de travail y garde.
        let mut rx = f.right() - 6.0;
        for _ in 0..2 {
            let w = 36.0 * k * 0.8;
            c.fill_rrect(Rect::new(rx - w, cy - top * 0.2, w, top * 0.4), 2.0, t.raised_hi);
            rx -= w + 3.0;
        }
        let right_items = [Bar::Undo, Bar::History, Bar::Save, Bar::Panel, Bar::Learn, Bar::Load];
        for (i, b) in right_items.into_iter().enumerate() {
            let v = self.anim.to(wid("set-pv-right", i, 0), self.bar_shown(b) as u8 as f32, T_LAYOUT);
            if v > 0.01 {
                let w = b.width() * k * 0.8 * v;
                c.fill_rrect(Rect::new(rx - w, cy - top * 0.2, w, top * 0.4), 2.0, t.raised.alpha(v));
                rx -= w + 3.0;
            }
        }

        // Navigateur et panneau.
        let sb = self.anim.to(wid("set-pv-sb", 0, 0), if self.sidebar { f.w * 0.16 } else { 0.0 }, T_LAYOUT);
        let below = f.bottom() - f.y - top;
        let pn = self.anim.to(wid("set-pv-pn", 0, 0), if self.panel { below * 0.28 } else { 0.0 }, T_LAYOUT);
        if sb > 0.5 {
            let s = Rect::new(f.x, f.y + top + 1.0, sb, below);
            c.fill_rect(s, t.panel);
            for i in 0..7 {
                let row = Rect::new(s.x + 6.0, s.y + 10.0 + i as f32 * 9.0, (s.w - 12.0) * if i % 3 == 0 { 0.7 } else { 0.9 }, 4.0);
                c.fill_rrect(row, 2.0, t.raised_hi.alpha(if i == 0 { 1.0 } else { 0.6 }));
            }
        }
        if pn > 0.5 {
            let p = Rect::new(f.x + sb, f.bottom() - pn, f.w - sb, pn);
            c.fill_rect(p, t.bg_deep);
            c.fill_rect(Rect::new(p.x, p.y, p.w, 1.0), t.hairline);
            for i in 0..3 {
                let card = Rect::new(p.x + 8.0 + i as f32 * 62.0, p.y + 7.0, 56.0, (p.h - 14.0).max(0.0));
                c.fill_rrect(card, 5.0, t.raised.alpha(0.9));
                c.fill_circle(card.x + 14.0, card.y + card.h * 0.55, 6.0, t.raised_hi);
                c.fill_circle(card.x + 34.0, card.y + card.h * 0.55, 6.0, t.raised_hi);
            }
        }

        // Zone principale : les onglets des vues proposées, et la vue en cours.
        let main = Rect::new(f.x + sb, f.y + top + 1.0, f.w - sb, below - pn - 1.0);
        let views = self.shown_views();
        let seg = 18.0;
        let tabs = Rect::new(main.x + 8.0, main.y + 7.0, views.len() as f32 * seg + 4.0, 18.0);
        let tw = self.anim.to(wid("set-pv-tabs", 0, 0), tabs.w, T_LAYOUT);
        let tabs = Rect::new(tabs.x, tabs.y, tw, tabs.h);
        c.fill_rrect(tabs, 5.0, t.bg_deep);
        c.stroke_rrect(tabs, 5.0, 1.0, t.hairline);
        let cur = views.iter().position(|v| *v == self.view).unwrap_or(0);
        let kx = self.anim.to(wid("set-pv-knob", 0, 0), cur as f32, T_LAYOUT);
        c.fill_rrect(Rect::new(tabs.x + 2.0 + kx * seg, tabs.y + 2.0, seg, tabs.h - 4.0), 4.0, t.raised_hi);
        for (i, v) in views.iter().enumerate() {
            let col = if i == cur { t.accent } else { t.muted };
            draw_icon(c, v.icon(), tabs.x + 2.0 + seg * (i as f32 + 0.5), tabs.y + tabs.h * 0.5, 10.0, col);
        }
        let (mx, my) = (main.x + main.w * 0.5, main.y + 16.0 + (main.h - 16.0) * 0.5);
        if main.h > 40.0 {
            c.fill_circle(mx, my, 18.0, t.raised.alpha(0.6));
            draw_icon(c, self.view.icon(), mx, my, 18.0, t.muted);
        }
        c.set_clip(Some(prev));
        c.stroke_rrect(f, 12.0, 1.0, t.hairline.alpha(1.8));
    }

    fn keys_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, foot: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        const ROW: f32 = 31.0;
        const HEAD: f32 = 28.0;
        let cols = key_columns();
        let content = cols.iter().map(|col| col.iter().map(|r| if matches!(r, KeyRow::Head(_)) { HEAD } else { ROW }).sum::<f32>()).fold(0.0, f32::max);
        if input.inside && body.contains(input.x, input.y) && input.wheel != 0.0 {
            self.prefs.scroll -= input.wheel * 48.0;
        }
        self.prefs.scroll = self.prefs.scroll.clamp(0.0, (content - body.h).max(0.0));
        let gap = 28.0;
        let col_w = ((body.w - gap) * 0.5).floor();
        let prev = c.push_clip(Rect::new(body.x - 8.0, body.y, body.w + 16.0, body.h));
        let conflict = self.prefs.conflict;
        for (ci, col) in cols.iter().enumerate() {
            let x = body.x + ci as f32 * (col_w + gap);
            let mut y = body.y - self.prefs.scroll;
            for row in col {
                match *row {
                    KeyRow::Head(name) => {
                        self.section(c, x, y + 6.0, tr(name));
                        y += HEAD;
                    }
                    KeyRow::Item(s) => {
                        let r = Rect::new(x, y, col_w, ROW - 4.0);
                        if r.bottom() > body.y && r.y < body.bottom() {
                            let visible = input.inside && body.contains(input.x, input.y);
                            let input = if visible { input } else { &super::float::masked(input) };
                            self.key_row(c, input, r, s, conflict, hint);
                        }
                        y += ROW;
                    }
                }
            }
        }
        // Bords qui s'estompent quand la liste dépasse.
        let fade = 24.0;
        if self.prefs.scroll > 0.5 {
            c.fill_rrect_vgrad(Rect::new(body.x - 8.0, body.y, body.w + 16.0, fade), 0.0, t.panel, t.panel.alpha(0.0));
        }
        if self.prefs.scroll < content - body.h - 0.5 {
            c.fill_rrect_vgrad(Rect::new(body.x - 8.0, body.bottom() - fade, body.w + 16.0, fade), 0.0, t.panel.alpha(0.0), t.panel);
        }
        c.set_clip(Some(prev));

        // Pied : conflit à régler, ou remise à zéro.
        if let Some((s, combo, other)) = conflict {
            let a = self.anim.to(wid("set-banner", 0, 0), 1.0, T_LAYOUT);
            let cy = foot.y + foot.h * 0.5 + (1.0 - a) * 6.0;
            draw_icon(c, Icon::Info, foot.x + 8.0, cy, 15.0, t.negative.alpha(a));
            let x = foot.x + 24.0;
            let kx = x + self.fonts.measure(self.f.mono, 11.0, &combo.label()) + 12.0;
            self.keycap(c, kx, cy, &combo.label(), a);
            let x = self.text(c, self.f.sans, 12.0, kx + 8.0, cy, tr("is already used by"), t.text2.alpha(a)) + kx + 14.0;
            draw_icon(c, other.icon(), x + 8.0, cy, 14.0, t.text.alpha(a));
            self.text(c, self.f.sans_med, 12.0, x + 22.0, cy, other.label(), t.text.alpha(a));
            let mut bx = foot.right();
            let buttons = [
                (Icon::Close, tr("Cancel"), tr("Keep the current keys"), 0),
                (Icon::Check, tr("Replace"), tr("Take the key away from the other action"), 1),
                (Icon::Swap, tr("Swap"), tr("The other action takes the old key · Enter"), 2),
            ];
            for (icon, label, tip, k) in buttons {
                let w = self.fonts.measure(self.f.sans_med, 12.0, label) + 40.0;
                let br = Rect::new(bx - w, foot.y + 2.0, w, foot.h - 4.0);
                bx -= w + 8.0;
                let id = wid("set-conflict", k, 0);
                if self.pill_button(c, input, id, br, icon, label, k == 2) {
                    match k {
                        0 => self.prefs.conflict = None,
                        1 => self.bind(s, combo, false),
                        _ => self.bind(s, combo, true),
                    }
                }
                if self.hovered(input, id, br) {
                    *hint = Some(tip.into());
                }
            }
        } else {
            self.anim.set(wid("set-banner", 0, 0), 0.0);
            let custom = Shortcut::ALL.iter().any(|&s| !self.prefs.keymap.is_default(s));
            let br = Rect::new(foot.x, foot.y + 2.0, 170.0, foot.h - 4.0);
            let id = wid("set-reset-all", 0, 0);
            let a = self.anim.to(id ^ 0x5649, custom as u8 as f32, T_HOVER);
            if a > 0.01 {
                let input = if custom { input } else { &super::float::masked(input) };
                if self.pill_button(c, input, id, br, Icon::Reset, tr("FL Studio keys"), false) && custom {
                    self.prefs.keymap = Keymap::default();
                    self.save_settings();
                    self.toast(tr("Default shortcuts"));
                }
                if self.hovered(input, id, br) {
                    *hint = Some(tr("Go back to all the default keys").into());
                }
            }
        }
    }

    fn key_row(&mut self, c: &mut Canvas, input: &Input, r: Rect, s: Shortcut, conflict: Option<(Shortcut, Combo, Shortcut)>, hint: &mut Option<String>) {
        let t = self.theme;
        let i = s as usize;
        let id = wid("set-key", i, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let capturing = self.prefs.capture == Some(s);
        let involved = conflict.is_some_and(|(a, _, b)| a == s || b == s);
        let flash = self.anim.to(wid("set-flash", i, 0), 0.0, 0.25);
        // Une vue masquée garde sa touche, mais en retrait.
        let dim = s.view().is_some_and(|v| !self.view_shown(v));
        let alpha = if dim { 0.45 } else { 1.0 };
        if hv > 0.01 || capturing || involved || flash > 0.01 {
            let base = t.raised.alpha(hv.max(capturing as u8 as f32) * 0.8);
            c.fill_rrect(r, 9.0, base.mix(t.accent.alpha(0.25), flash));
        }
        if involved {
            c.stroke_rrect(r, 9.0, 1.0, t.negative.alpha(0.7));
        }
        let cy = r.y + r.h * 0.5;
        draw_icon(c, s.icon(), r.x + 16.0, cy, 15.0, t.muted.mix(t.accent, hv.max(flash)).alpha(alpha));
        self.text_fit(c, self.f.sans_med, 13.0, r.x + 34.0, cy, s.label(), r.w * 0.5, t.text2.mix(t.text, hv).alpha(alpha));

        // La touche : un bouton ; clic pour en saisir une nouvelle.
        let label = if capturing { "…".to_string() } else { self.key_label(s) };
        let kw = (self.fonts.measure(self.f.mono, 11.0, &label) + 18.0).max(54.0);
        let cap = Rect::new(r.right() - 6.0 - kw, cy - 11.0, kw, 22.0);
        let cid = wid("set-cap", i, 0);
        let chov = self.hovered(input, cid, cap);
        let chv = self.hover_anim(cid, chov);
        let pulse = if capturing { 0.6 + 0.4 * (self.time * 5.0).sin().abs() } else { 0.0 };
        c.fill_rrect(cap, 6.0, t.bg_deep.mix(t.raised_hi, 0.5 + 0.5 * chv));
        c.fill_rect(Rect::new(cap.x + 3.0, cap.bottom() - 1.0, cap.w - 6.0, 1.0), t.shade(90.0));
        c.stroke_rrect(cap, 6.0, 1.0 + pulse, t.hairline.mix(t.accent, pulse.max(chv * 0.6)));
        let ink = if label.is_empty() { t.muted } else { t.text2.mix(t.text, chv) };
        let shown = if label.is_empty() { "—" } else { label.as_str() };
        self.text_center(c, self.f.mono, 11.0, cap.x + cap.w * 0.5, cy, shown, if capturing { t.accent } else { ink.alpha(alpha) });
        if chov {
            *hint = Some(if capturing { tr("Press the new combination · Esc cancels").into() } else { tr_fmt!("{} · click: change the key", s.label()) });
            if input.pressed {
                self.prefs.capture = if capturing { None } else { Some(s) };
                self.prefs.conflict = None;
            }
        }

        // Au survol : retirer la touche, revenir à celle par défaut.
        let mut bx = cap.x - 4.0;
        let tools = [(Icon::Reset, !self.prefs.keymap.is_default(s), tr("Default key")), (Icon::Close, hov && self.prefs.keymap.get(s).is_some(), tr("Remove the key"))];
        for (k, (icon, show, tip)) in tools.into_iter().enumerate() {
            let v = self.anim.to(wid("set-tool", i, k), show as u8 as f32, T_HOVER);
            if v < 0.01 {
                continue;
            }
            let br = Rect::new(bx - 24.0, cy - 12.0, 24.0, 24.0);
            bx -= 26.0;
            let tid = wid("set-tool-b", i, k);
            let thov = self.hovered(input, tid, br) && show;
            let thv = self.hover_anim(tid, thov);
            if thv > 0.01 {
                c.fill_rrect(br, 6.0, t.raised_hi.alpha(thv * v));
            }
            draw_icon(c, icon, br.x + 12.0, cy, 13.0, t.muted.mix(t.text, thv).alpha(v));
            if thov {
                *hint = Some(tip.into());
                if input.pressed {
                    match icon {
                        Icon::Reset => {
                            if let Some(d) = s.default_combo() {
                                self.prefs.keymap.rebind(s, d, true);
                            } else {
                                self.prefs.keymap.set(s, None);
                            }
                        }
                        _ => self.prefs.keymap.set(s, None),
                    }
                    self.anim.set(wid("set-flash", i, 0), 1.0);
                    self.save_settings();
                }
            }
        }
        if hov && !chov && hint.is_none() && dim {
            *hint = Some(tr("Hidden view: the key stays in reserve").into());
        }
    }

    /// Bouton pilule : pictogramme et mot.
    pub(super) fn pill_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Icon, label: &str, accent: bool) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.95 } else { 1.0 }, T_PRESS);
        let b = r.scale(press);
        let fill = if accent { t.accent.mix(t.highlight, 0.12 * hv) } else { t.raised.mix(t.raised_hi, hv) };
        c.fill_rrect(b, b.h * 0.5, fill);
        let ink = if accent { t.on_accent } else { t.text2.mix(t.text, hv) };
        let cy = b.y + b.h * 0.5;
        draw_icon(c, icon, b.x + 18.0, cy, 14.0, ink);
        self.text(c, self.f.sans_med, 12.0, b.x + 32.0, cy, label, ink);
        hov && input.pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let mut ws = presets()[4].2.clone();
        ws.browser = false;
        ws.count_in = false;
        let mut keymap = Keymap::default();
        keymap.rebind(Shortcut::ViewSession, Combo::key(Key::F(11)), false);
        keymap.set(Shortcut::TapTempo, None);
        let text = to_json(&ws, &keymap, "Glace", Lang::from_code("fr")).pretty();
        let j = json::parse(&text).expect("JSON valide");
        let (ws2, keymap2, theme) = from_json(&j);
        assert_eq!(lang_from_json(&j).map(Lang::code), Some("fr"));
        assert_eq!(ws2, ws);
        assert_eq!(keymap2, keymap);
        assert_eq!(theme.as_deref(), Some("Glace"));
    }

    #[test]
    fn loading_is_tolerant() {
        // Vide : tout par défaut.
        let (ws, keymap, theme) = from_json(&json::parse("{}").unwrap());
        assert_eq!(ws, Workspace::default());
        assert_eq!(keymap, Keymap::default());
        assert_eq!(theme, None);
        // Types faux, clés inconnues, aucune vue : on garde ce qui se lit.
        let text = r#"{"views": {"rack": false, "piano": false, "playlist": false, "session": false, "mixer": false, "lab": false},
            "bar": {"tap": false, "scale": "oui", "zzz": true}, "panel": 3, "keys": {"save": 12, "undo": "ctrl+u", "nope": "f1"}}"#;
        let (ws, keymap, _) = from_json(&json::parse(text).unwrap());
        assert_eq!(ws.views, [true; 6], "au moins une vue");
        assert!(!ws.bar(Bar::Tap));
        assert!(ws.bar(Bar::Scale));
        assert!(ws.panel);
        assert_eq!(keymap.get(Shortcut::Undo), Some(Combo::ctrl(Key::Char('u'))));
        assert_eq!(keymap.get(Shortcut::Save), Shortcut::Save.default_combo());
    }

    #[test]
    fn ui_scale_snaps_steps_and_loads() {
        assert_eq!(UiScale::new(1.234).get(), 1.25);
        assert_eq!(UiScale::new(0.1).get(), UiScale::MIN);
        assert_eq!(UiScale::new(9.0).get(), UiScale::MAX);
        assert_eq!(UiScale::new(f32::NAN), UiScale::default());
        // Crans de 10 %, raccrochés à la dizaine depuis une valeur de 5 %.
        assert_eq!(UiScale::new(1.0).step(true).percent(), 110);
        assert_eq!(UiScale::new(1.0).step(false).percent(), 90);
        assert_eq!(UiScale::new(1.15).step(true).percent(), 120);
        assert_eq!(UiScale::new(1.15).step(false).percent(), 110);
        assert_eq!(UiScale::new(0.8).step(false).percent(), 75);
        assert_eq!(UiScale::new(0.75).step(false).percent(), 75);
        assert_eq!(UiScale::new(2.0).step(true).percent(), 200);
        // Glissière : bornes et pas de 5 %.
        assert_eq!(UiScale::from_fraction(0.0).percent(), 75);
        assert_eq!(UiScale::from_fraction(1.0).percent(), 200);
        assert_eq!(UiScale::from_fraction(UiScale::default().fraction()), UiScale::default());
        // Fichier : absent ou illisible → 100 %.
        assert_eq!(UiScale::from_json(&json::parse(r#"{"ui_scale": 1.5}"#).unwrap()).percent(), 150);
        assert_eq!(UiScale::from_json(&json::parse(r#"{"ui_scale": "grand"}"#).unwrap()), UiScale::default());
        assert_eq!(UiScale::from_json(&json::parse("{}").unwrap()), UiScale::default());
    }

    #[test]
    fn presets_are_distinct_and_keep_a_view() {
        let p = presets();
        for (i, (_, _, a)) in p.iter().enumerate() {
            assert!(a.views.iter().any(|&v| v));
            for (_, _, b) in &p[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(p[0].2, Workspace::default(), "Simple : l'espace par défaut");
        assert!(p[1].2.bar.iter().all(|&on| on), "Complet : toute la barre");
        assert!(!p[3].2.view(View::Piano), "Sampling : sans piano roll");
        assert!(p[4].2.view(View::Session) && !p[4].2.view(View::Rack), "Live : la session");
    }

    #[test]
    fn the_default_top_bar_is_calm() {
        let ws = Workspace::default();
        for b in [Bar::Record, Bar::Undo, Bar::Mode] {
            assert!(ws.bar(b), "{b:?}");
        }
        for b in [Bar::Pattern, Bar::Scale, Bar::Tap, Bar::Mic, Bar::Voice, Bar::AutoArm, Bar::Keyboard, Bar::History, Bar::Save, Bar::Panel, Bar::Learn] {
            assert!(!ws.bar(b), "{b:?}");
        }
    }

    #[test]
    fn old_settings_migrate_to_the_calm_bar() {
        // Untouched (every item on, as the old default): the new default.
        let untouched = r#"{"version": 1, "bar": {"mode": true, "pattern": true, "tap": true, "position": true, "scale": true,
            "metronome": true, "record": true, "keyboard": true, "undo": true, "history": true, "load": true}}"#;
        let (ws, ..) = from_json(&json::parse(untouched).unwrap());
        assert_eq!(ws.bar, Workspace::default().bar);
        // Customised: the choices stay, what was shown stays shown.
        let custom = r#"{"version": 1, "bar": {"tap": false, "history": false, "record": true}}"#;
        let (ws, ..) = from_json(&json::parse(custom).unwrap());
        assert!(!ws.bar(Bar::Tap) && !ws.bar(Bar::History));
        for b in [Bar::Pattern, Bar::Scale, Bar::Mic, Bar::Voice, Bar::AutoArm, Bar::Save, Bar::Panel, Bar::Learn] {
            assert!(ws.bar(b), "{b:?}");
        }
        // The record button hid the input, voice and automation buttons with it.
        let no_rec = r#"{"version": 1, "bar": {"record": false}}"#;
        let (ws, ..) = from_json(&json::parse(no_rec).unwrap());
        assert!(!ws.bar(Bar::Record) && !ws.bar(Bar::Mic) && !ws.bar(Bar::Voice) && !ws.bar(Bar::AutoArm));
        // Version 2 files are read as they are.
        let full = to_json(&presets()[1].2, &Keymap::default(), "x", None).pretty();
        let (ws, ..) = from_json(&json::parse(&full).unwrap());
        assert_eq!(ws, presets()[1].2);
    }

    #[test]
    fn key_columns_list_every_shortcut_once() {
        let n: usize = key_columns().iter().map(|c| c.iter().filter(|r| matches!(r, KeyRow::Item(_))).count()).sum();
        assert_eq!(n, Shortcut::ALL.len());
    }
}
