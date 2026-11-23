//! The install screens: the first screen of a downloaded copy ("Install
//! Ponzik" or "Just run it"), the uninstaller of `Ponzik.exe --uninstall`, and
//! the Installation card of Settings › About. The work itself is in
//! `crate::install`; this file only asks and shows.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::json::Json;
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Event, Key, Window};

use super::app::{App, Popup};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};
use super::{ClickTracker, FRAME, Input, system_scale, to_logical};
use crate::install::{self, Options, Place, Platform, Report, Status};

/// What the install sheet shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Sheet {
    /// Install, or just run it.
    Welcome,
    Working,
    /// Installed; the program to start from now on.
    Done(PathBuf),
    Failed(String),
    /// "Uninstall Ponzik?"
    Uninstall,
    Removing,
    Removed,
}

/// What happens once a standalone screen closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Go on into Ponzik, in this process.
    Continue,
    /// Start this program (the installed copy) and leave.
    Relaunch(PathBuf),
    Quit,
}

enum Msg {
    Installed(Result<Report, String>),
    Removed(Result<Vec<String>, String>),
}

/// State of the install screens.
pub struct InstallUi {
    pub sheet: Sheet,
    /// The sheet fills the window on its own (first screen, `--uninstall`).
    pub standalone: bool,
    /// "Don't ask again" (`install_prompt: false` in settings.json).
    pub dont_ask: bool,
    /// Windows: a desktop shortcut too.
    pub desktop: bool,
    /// Linux: move the AppImage to `~/Applications`.
    pub move_apps: bool,
    /// Uninstall: also remove the settings folder.
    pub purge: bool,
    pub status: Status,
    /// Set when a standalone screen is done.
    pub exit: Option<Exit>,
    /// Ponzik was uninstalled: settings are not saved again, the program goes.
    pub uninstalled: bool,
    /// Leave Ponzik now (after an uninstall from the settings).
    pub quit: bool,
    pub warnings: Vec<String>,
    /// The last work started was an uninstall.
    removal: bool,
    /// Screenshots: the install folder shown instead of this machine's.
    pretend_dest: Option<PathBuf>,
    rx: Option<mpsc::Receiver<Msg>>,
    /// `App::time` when the install finished (the next step waits a beat).
    done_at: f32,
}

impl Default for InstallUi {
    fn default() -> Self {
        let status = install::status();
        let move_apps = status.platform == Platform::Linux && install::linux::looks_temporary(&status.program);
        Self {
            sheet: Sheet::Welcome,
            standalone: false,
            dont_ask: false,
            desktop: true,
            move_apps,
            purge: false,
            status,
            exit: None,
            uninstalled: false,
            quit: false,
            warnings: Vec::new(),
            removal: false,
            pretend_dest: None,
            rx: None,
            done_at: 0.0,
        }
    }
}

impl InstallUi {
    pub fn load(&mut self, j: &Json) {
        self.dont_ask = j.get("install_prompt").and_then(Json::as_bool) == Some(false);
    }

    pub fn save(&self, fields: &mut Vec<(String, Json)>) {
        if self.dont_ask {
            fields.push(("install_prompt".into(), false.into()));
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.sheet, Sheet::Working | Sheet::Removing)
    }
}

/// A path as people read it: `~/…` on Linux, `%LOCALAPPDATA%\…` on Windows.
pub fn short_path(p: &Path) -> String {
    let s = p.display().to_string();
    let swap = |var: &str, label: &str| -> Option<String> {
        let v = std::env::var(var).ok().filter(|v| v.len() > 1)?;
        let rest = s.strip_prefix(v.trim_end_matches(['/', '\\']))?;
        (rest.is_empty() || rest.starts_with(['/', '\\'])).then(|| format!("{label}{rest}"))
    };
    if cfg!(windows) {
        swap("LOCALAPPDATA", "%LOCALAPPDATA%").or_else(|| swap("USERPROFILE", "%USERPROFILE%")).unwrap_or(s)
    } else {
        swap("HOME", "~").unwrap_or(s)
    }
}

/// An App without sound or project, for the screens shown before (or instead of) Ponzik.
fn light_app() -> (ponzik_engine::Engine, App) {
    let project = ponzik_project::Project::empty();
    let none = |_: &str| None;
    let (engine, handle) = project.build_engine(48_000.0, &none);
    let mut app = App::new(handle, project, 48_000);
    app.load_settings();
    app.theme_from_env();
    (engine, app)
}

/// At launch: the first screen, when this copy can be installed and the user
/// did not decline for good. `PONZIK_INSTALL=welcome` shows it anyway.
pub fn first_screen() -> Result<Exit, String> {
    let forced = std::env::var("PONZIK_INSTALL").is_ok_and(|v| v.starts_with("welcome"));
    let status = install::status();
    if !forced && status.place != Place::Loose {
        return Ok(Exit::Continue);
    }
    let (engine, mut app) = light_app();
    if !install::should_offer(&status, app.install.dont_ask, crate::build_info::is_release(), forced) {
        return Ok(Exit::Continue);
    }
    app.install.status = status;
    app.install.sheet = Sheet::Welcome;
    app.install.standalone = true;
    let mut window = Window::open("Ponzik", 1040, 680).map_err(|e| e.to_string())?;
    let exit = sheet_loop(&mut window, &mut app);
    drop(app);
    drop(window);
    drop(engine);
    exit
}

/// `Ponzik.exe --uninstall` ("Installed apps" › Uninstall): asks, removes,
/// then the program deletes itself once this process ends.
pub fn run_uninstall() -> Result<(), String> {
    let (engine, mut app) = light_app();
    app.install.sheet = Sheet::Uninstall;
    app.install.standalone = true;
    let mut window = Window::open(tr("Uninstall Ponzik"), 760, 520).map_err(|e| e.to_string())?;
    let result = sheet_loop(&mut window, &mut app);
    let uninstalled = app.install.uninstalled;
    drop(app);
    drop(window);
    drop(engine);
    result?;
    if uninstalled { install::delete_program_after_exit() } else { Ok(()) }
}

/// The event loop of a standalone screen: redrawn when something moves or
/// happens, until it sets its exit.
fn sheet_loop(window: &mut Window, app: &mut App) -> Result<Exit, String> {
    let (w, h) = window.size();
    let mut canvas = Canvas::new(w as usize, h as usize);
    let mut scale = system_scale(Some(window)) * app.ui_scale();
    canvas.set_scale(scale);
    let mut input = Input::default();
    let mut clicks = ClickTracker::default();
    let mut events = Vec::new();
    let mut last = Instant::now();
    let mut dirty = true;
    loop {
        let moving = dirty || app.anim.is_moving() || app.install.busy() || matches!(app.install.sheet, Sheet::Done(_));
        window.wait_events(if moving { FRAME } else { Duration::from_millis(250) }, &mut events);
        window.poll_events(&mut events);
        let k = system_scale(Some(window)) * app.ui_scale();
        if k != scale {
            scale = k;
            canvas.set_scale(k);
        }
        input.begin_frame();
        dirty = !events.is_empty();
        for ev in events.drain(..) {
            if ev == Event::CloseRequested && !app.install.busy() {
                return Ok(app.install.exit.take().unwrap_or(Exit::Quit));
            }
            input.apply(&to_logical(ev, scale));
        }
        clicks.check(&mut input);
        dirty |= app.install_tick();
        let (w, h) = window.size();
        if (w as usize, h as usize) != (canvas.width(), canvas.height()) {
            canvas.resize(w as usize, h as usize);
            canvas.set_scale(scale);
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.1);
        last = now;
        canvas.begin_frame();
        app.frame(&mut canvas, &input, dt);
        canvas.end_frame();
        window.set_cursor(app.cursor);
        window.present(canvas.pixels(), w, h).map_err(|e| e.to_string())?;
        if let Some(exit) = app.install.exit.take() {
            return Ok(exit);
        }
    }
}

/// Look of a sheet button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Primary,
    Danger,
    Plain,
}

impl App {
    /// A frame of a standalone screen: the sheet alone on the window.
    pub(super) fn install_frame(&mut self, c: &mut Canvas, input: &Input, dt: f32) {
        self.anim.begin_frame(dt);
        self.time += dt;
        self.cursor = ponzik_window::Cursor::Arrow;
        self.apply_theme_style(c);
        let (w, h) = c.logical_size();
        c.set_clip(None);
        c.clear(self.theme.bg_deep);
        self.install_sheet(c, input, Rect::new(0.0, 0.0, w, h));
    }

    /// Opens a sheet over Ponzik (from Settings › About).
    pub fn open_install_sheet(&mut self, sheet: Sheet) {
        self.install.status = install::status();
        self.install.sheet = sheet;
        self.install.standalone = false;
        self.anim.set(wid("inst-open", 0, 0), 0.0);
        self.open_popup(Popup::Install, Rect::default());
    }

    fn start_install(&mut self) {
        let opts = Options { desktop: self.install.desktop, move_to_applications: self.install.move_apps };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("ponzik-install".into()).spawn(move || {
            let _ = tx.send(Msg::Installed(install::install(opts)));
        });
        if spawned.is_ok() {
            self.install.rx = Some(rx);
            self.install.removal = false;
            self.install.sheet = Sheet::Working;
        }
    }

    fn start_uninstall(&mut self) {
        let (tx, rx) = mpsc::channel();
        let purge = self.install.purge;
        let config = super::config_dir();
        let spawned = std::thread::Builder::new().name("ponzik-uninstall".into()).spawn(move || {
            let _ = tx.send(Msg::Removed(install::uninstall(purge, &config)));
        });
        if spawned.is_ok() {
            self.install.rx = Some(rx);
            self.install.removal = true;
            self.install.sheet = Sheet::Removing;
        }
    }

    /// Answers of the install thread, and the step after "installed"; true
    /// when something changed on screen.
    pub fn install_tick(&mut self) -> bool {
        if let Sheet::Done(program) = &self.install.sheet
            && self.time - self.install.done_at > 1.2
        {
            let program = program.clone();
            self.after_install(program);
            return true;
        }
        let Some(msg) = self.install.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return false };
        self.install.rx = None;
        match msg {
            Msg::Installed(Ok(report)) => {
                for w in &report.warnings {
                    eprintln!("install: {w}");
                }
                self.install.warnings = report.warnings;
                self.install.status = install::status();
                self.install.done_at = self.time;
                if report.relaunch {
                    self.install.sheet = Sheet::Done(report.program);
                } else {
                    // Same file, nothing to restart (Linux, AppImage left in place).
                    self.install.sheet = Sheet::Done(PathBuf::new());
                }
            }
            Msg::Installed(Err(e)) => self.install.sheet = Sheet::Failed(e),
            Msg::Removed(Ok(warnings)) => {
                for w in &warnings {
                    eprintln!("uninstall: {w}");
                }
                self.install.warnings = warnings;
                self.install.uninstalled = true;
                self.install.status = install::status();
                self.install.sheet = Sheet::Removed;
            }
            Msg::Removed(Err(e)) => self.install.sheet = Sheet::Failed(e),
        }
        true
    }

    /// Installed: start the installed copy (Windows, moved AppImage), or go on here.
    fn after_install(&mut self, program: PathBuf) {
        let relaunch = !program.as_os_str().is_empty();
        if self.install.standalone {
            self.install.exit = Some(if relaunch { Exit::Relaunch(program) } else { Exit::Continue });
            return;
        }
        self.popup = None;
        self.install.sheet = Sheet::Welcome;
        if relaunch {
            // The project is saved, as for an update, then the new copy starts.
            if self.modified {
                self.save();
            }
            if !self.modified {
                self.updates.relaunch = Some(program);
            }
        } else {
            self.toast(tr("Ponzik is in the applications menu"));
        }
    }

    /// "Just run it": on into Ponzik (the choice remembered if asked).
    fn run_portable(&mut self) {
        if self.install.dont_ask {
            self.save_settings();
        }
        if self.install.standalone {
            self.install.exit = Some(Exit::Continue);
        } else {
            self.popup = None;
        }
    }

    /// The sheet; false once it closes (over Ponzik).
    pub(super) fn install_sheet(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("inst-open", 0, 0), 1.0, T_LAYOUT);
        if !self.install.standalone {
            c.fill_rect(screen, t.shade(120.0 * open));
        }
        let sheet = self.install.sheet.clone();
        let (cw, ch) = match sheet {
            Sheet::Uninstall | Sheet::Removing | Sheet::Removed => (620.0, 400.0),
            Sheet::Failed(_) if self.removing() => (620.0, 400.0),
            _ => (920.0, 552.0),
        };
        let w = f32::min(cw, screen.w - 32.0);
        let h = f32::min(ch, screen.h - 32.0);
        let card = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.5 + (1.0 - open) * 14.0).round(), w, h);
        c.fill_rrect(card.translate(0.0, 14.0).inset(-4.0), 24.0, t.shade(90.0 * open));
        c.fill_rrect(card, 20.0, t.panel);
        c.stroke_rrect(card, 20.0, 1.0, t.hairline.alpha(1.8));
        // The states after a choice: the header stays, the state below it.
        let mut area = card;
        if !matches!(sheet, Sheet::Welcome | Sheet::Uninstall) {
            let title = if self.removing() { tr("Uninstall Ponzik") } else if self.install.standalone { tr("Welcome to Ponzik") } else { tr("Install Ponzik") };
            let top = self.sheet_header(c, card, title, tr("The free DAW. No more gatekeeping."));
            let foot = if matches!(sheet, Sheet::Failed(_) | Sheet::Removed) { 76.0 } else { 24.0 };
            area = Rect::new(card.x, top + 12.0, card.w, card.bottom() - foot - top - 12.0);
        }
        let mut keep = true;
        match sheet {
            Sheet::Welcome => keep = self.welcome_body(c, input, card),
            Sheet::Working => self.busy_body(c, area, tr("Installing Ponzik…"), &short_path(&self.install_destination())),
            Sheet::Removing => self.busy_body(c, area, tr("Removing Ponzik…"), ""),
            Sheet::Done(_) => self.done_body(c, area),
            Sheet::Failed(e) => keep = self.failed_body(c, input, card, area, &e),
            Sheet::Uninstall => keep = self.uninstall_body(c, input, card),
            Sheet::Removed => keep = self.removed_body(c, input, card, area),
        }
        // Over Ponzik, a click outside closes (not while working).
        if !self.install.standalone && input.pressed && !card.contains(input.x, input.y) && !self.install.busy() {
            keep = false;
        }
        keep
    }

    /// The sheet is about removing Ponzik (an error then belongs to the uninstall).
    fn removing(&self) -> bool {
        self.install.removal || matches!(self.install.sheet, Sheet::Uninstall | Sheet::Removing | Sheet::Removed)
    }

    /// Where the install goes: the install folder (Windows), the menu (Linux).
    fn install_destination(&self) -> PathBuf {
        if let Some(d) = &self.install.pretend_dest {
            return d.clone();
        }
        match self.install.status.platform {
            Platform::Windows => install::windows::install_dir(),
            _ if self.install.move_apps => install::linux::Layout::user().applications,
            _ => install::linux::Layout::user().data.join("applications"),
        }
    }

    /// The mark, a title and a line under it; returns the bottom of the header.
    fn sheet_header(&mut self, c: &mut Canvas, card: Rect, title: &str, sub: &str) -> f32 {
        let t = self.theme;
        let mark = Rect::new(card.x + 40.0, card.y + 38.0, 60.0, 60.0);
        ponzik_ui::brand::draw_mark(c, mark);
        let tx = mark.right() + 22.0;
        self.text_fit(c, self.f.sans_med, 26.0, tx, mark.y + 20.0, title, card.right() - tx - 40.0, t.text);
        self.text_fit(c, self.f.sans, 14.0, tx, mark.y + 48.0, sub, card.right() - tx - 40.0, t.text2);
        mark.bottom()
    }

    fn welcome_body(&mut self, c: &mut Canvas, input: &Input, card: Rect) -> bool {
        let t = self.theme;
        let windows = self.install.status.platform == Platform::Windows;
        let standalone = self.install.standalone;
        let title = if standalone { tr("Welcome to Ponzik") } else if windows { tr("Install Ponzik") } else { tr("Add Ponzik to the applications menu") };
        let top = self.sheet_header(c, card, title, tr("The free DAW. No more gatekeeping."));
        // The version, top right.
        let v = crate::build_info::version();
        let chip = if v.build > 0 { tr_fmt!("{} · build {}", v.semver(), v.build) } else { v.semver() };
        let cw = self.fonts.measure(self.f.mono_med, 11.0, &chip) + 18.0;
        let chip_r = Rect::new(card.right() - 40.0 - cw, card.y + 40.0, cw, 24.0);
        c.fill_rrect(chip_r, 12.0, t.accent.alpha(0.14));
        self.text_center(c, self.f.mono_med, 11.0, chip_r.x + cw * 0.5, chip_r.y + 12.0, &chip, t.accent);

        let q = if windows { tr("How do you want to use it?") } else { tr("Where should Ponzik live?") };
        let qy = top + 34.0;
        self.text(c, self.f.sans_med, 12.0, card.x + 42.0, qy, q, t.muted);

        // The two choices.
        let pad = 40.0;
        let gap = 16.0;
        let tiles_y = qy + 20.0;
        let foot_h = 64.0;
        let tile_h = (card.bottom() - foot_h - 64.0 - tiles_y).clamp(170.0, 260.0);
        let tile_w = ((card.w - pad * 2.0 - gap) * 0.5).floor();
        let a = Rect::new(card.x + pad, tiles_y, tile_w, tile_h);
        let b = Rect::new(a.right() + gap, tiles_y, tile_w, tile_h);
        let dest = short_path(&self.install_destination());
        let here = short_path(&self.install.status.program);
        let (a_title, a_lines): (&str, [&str; 3]) = if windows {
            (
                tr("Install Ponzik"),
                [
                    tr("Start menu shortcut, desktop if you like"),
                    tr("Opens .ponzik and FL Studio .flp files"),
                    tr("Updates itself · remove it from Windows Settings"),
                ],
            )
        } else {
            (
                tr("Add to applications menu"),
                [
                    tr("In your launcher, with its icon"),
                    tr("Opens .ponzik and FL Studio .flp files"),
                    tr("Still one AppImage that updates itself"),
                ],
            )
        };
        let b_lines: [&str; 3] = if windows {
            [tr("Nothing is installed: it runs from this file"), tr("Settings stay in your user profile"), tr("Install it later from Settings › About")]
        } else {
            [tr("Nothing is added to your system"), tr("Start the AppImage whenever you like"), tr("Add it later from Settings › About")]
        };
        let b_title = if standalone { tr("Just run it") } else { tr("Keep it portable") };
        let mut keep = true;
        if self.choice_tile(c, input, wid("inst-tile", 0, 0), a, true, Icon::Install, a_title, tr("Recommended"), &a_lines, &dest) {
            self.start_install();
        }
        if self.choice_tile(c, input, wid("inst-tile", 1, 0), b, false, Icon::Play, b_title, tr("Portable"), &b_lines, &here) {
            self.run_portable();
            keep = false;
        }

        // The option of the install, under its tile.
        let oy = a.bottom() + 26.0;
        let (label, on) = if windows {
            (tr("Also put a shortcut on the desktop"), self.install.desktop)
        } else {
            (tr("Move the AppImage to ~/Applications first"), self.install.move_apps)
        };
        if self.check_row(c, input, wid("inst-opt", 0, 0), a.x + 4.0, oy, label, on) {
            if windows {
                self.install.desktop = !on;
            } else {
                self.install.move_apps = !on;
            }
        }

        // Footer: don't ask again, and the keys.
        let fy = card.bottom() - foot_h;
        c.fill_rect(Rect::new(card.x + 1.0, fy, card.w - 2.0, 1.0), t.hairline);
        let cy = fy + foot_h * 0.5;
        if standalone {
            let dont = self.install.dont_ask;
            if self.check_row(c, input, wid("inst-dont", 0, 0), card.x + pad + 4.0, cy, tr("Don't ask again"), dont) {
                self.install.dont_ask = !dont;
            }
        }
        let keys = if standalone { tr("Enter installs · Esc just runs it") } else { tr("Enter installs · Esc closes") };
        self.text_right(c, self.f.sans, 12.0, card.right() - pad, cy, keys, t.muted);
        for &(key, _) in &input.keys {
            match key {
                Key::Enter => self.start_install(),
                Key::Escape => {
                    self.run_portable();
                    keep = false;
                }
                _ => {}
            }
        }
        keep
    }

    /// A big choice: icon, title and badge, three lines, and where it goes.
    fn choice_tile(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        id: u64,
        r: Rect,
        primary: bool,
        icon: Icon,
        title: &str,
        badge: &str,
        lines: &[&str],
        foot: &str,
    ) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.985 } else { 1.0 }, T_PRESS);
        let r = r.scale(press);
        let lift = (hv * 3.0).round();
        let r = r.translate(0.0, -lift);
        c.fill_rrect(r.translate(0.0, 4.0 + lift), 16.0, t.shade(40.0 + 50.0 * hv));
        c.fill_rrect(r, 16.0, t.raised.mix(t.raised_hi, hv * 0.8));
        let edge = if primary { t.accent.alpha(0.55 + 0.45 * hv) } else { t.hairline.mix(t.muted, hv * 0.6) };
        c.stroke_rrect(r, 16.0, if primary { 1.5 } else { 1.0 }, edge);

        // Icon.
        let ic = Rect::new(r.x + 24.0, r.y + 24.0, 52.0, 52.0);
        c.fill_rrect(ic, 14.0, if primary { t.accent } else { t.raised_hi.mix(t.text2, 0.08) });
        draw_icon(c, icon, ic.x + 26.0, ic.y + 26.0, 24.0, if primary { t.on_accent } else { t.text });
        // Badge.
        let bw = self.fonts.measure(self.f.sans_med, 11.0, badge) + 20.0;
        let br = Rect::new(r.right() - 20.0 - bw, r.y + 24.0, bw, 24.0);
        c.fill_rrect(br, 12.0, if primary { t.accent.alpha(0.16) } else { t.well });
        self.text_center(c, self.f.sans_med, 11.0, br.x + bw * 0.5, br.y + 12.0, badge, if primary { t.accent } else { t.text2 });
        // Title.
        let tx = r.x + 24.0;
        self.text_fit(c, self.f.sans_med, 19.0, tx, ic.bottom() + 28.0, title, r.w - 48.0, t.text);
        // Lines.
        let mut y = ic.bottom() + 60.0;
        for l in lines {
            if y > r.bottom() - 54.0 {
                break;
            }
            draw_icon(c, Icon::Check, tx + 7.0, y, 12.0, if primary { t.accent } else { t.muted });
            self.text_fit(c, self.f.sans, 13.0, tx + 22.0, y, l, r.w - 70.0, t.text2.mix(t.text, 0.3));
            y += 25.0;
        }
        // Where it goes, and the arrow.
        let fy = r.bottom() - 26.0;
        c.fill_rect(Rect::new(r.x + 16.0, fy - 18.0, r.w - 32.0, 1.0), t.hairline);
        let arrow_x = r.right() - 30.0 + hv * 4.0;
        draw_icon(c, Icon::Folder, tx + 7.0, fy, 13.0, t.muted);
        self.text_fit(c, self.f.mono, 11.0, tx + 22.0, fy, foot, r.w - 90.0, t.muted.mix(t.text2, hv));
        draw_icon(c, Icon::ChevronRight, arrow_x, fy, 15.0, if primary { t.accent } else { t.text2.mix(t.text, hv) });
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    /// A check box and its label; true when clicked.
    fn check_row(&mut self, c: &mut Canvas, input: &Input, id: u64, x: f32, cy: f32, label: &str, checked: bool) -> bool {
        let t = self.theme;
        let lw = self.fonts.measure(self.f.sans, 13.0, label);
        let row = Rect::new(x, cy - 16.0, 30.0 + lw, 32.0);
        let hov = self.hovered(input, id, row);
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(id ^ 0x4f4e, checked as u8 as f32, T_HOVER);
        let bx = Rect::new(x, cy - 9.0, 18.0, 18.0);
        c.fill_rrect(bx, 5.0, t.well.mix(t.accent, on));
        c.stroke_rrect(bx, 5.0, 1.0, t.hairline.mix(t.muted, hv).mix(t.accent, on));
        if on > 0.05 {
            draw_icon(c, Icon::Check, bx.x + 9.0, bx.y + 9.0, 12.0, t.on_accent.alpha(on));
        }
        self.text(c, self.f.sans, 13.0, bx.right() + 10.0, cy, label, t.text2.mix(t.text, hv.max(on * 0.5)));
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    /// A rounded button of the sheets.
    fn sheet_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Icon, label: &str, look: Look) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.95 } else { 1.0 }, T_PRESS);
        let b = r.scale(press);
        let (fill, ink) = match look {
            Look::Primary => (t.accent.mix(t.highlight, 0.12 * hv), t.on_accent),
            Look::Danger => (t.negative.mix(t.highlight, 0.12 * hv), ensure_contrast(t.highlight, t.negative, 3.0)),
            Look::Plain => (t.raised.mix(t.raised_hi, hv), t.text2.mix(t.text, hv)),
        };
        c.fill_rrect(b, b.h * 0.5, fill);
        let tw = self.fonts.measure(self.f.sans_med, 13.0, label);
        let x0 = b.x + (b.w - tw - 24.0) * 0.5;
        let cy = b.y + b.h * 0.5;
        draw_icon(c, icon, x0 + 7.0, cy, 15.0, ink);
        self.text(c, self.f.sans_med, 13.0, x0 + 24.0, cy, label, ink);
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    fn sheet_button_width(&self, label: &str) -> f32 {
        (self.fonts.measure(self.f.sans_med, 13.0, label) + 64.0).max(120.0)
    }

    /// A centered state: a big round icon (or a turning arc), a title, a line.
    fn status_block(&mut self, c: &mut Canvas, area: Rect, icon: Option<Icon>, color: Color, title: &str, detail: &str) -> f32 {
        let t = self.theme;
        let lines = self.wrap(self.f.sans, 13.0, detail, area.w - 120.0);
        let shown = lines.len().min(3);
        let block = 80.0 + 34.0 + 20.0 * shown as f32;
        let cx = area.x + area.w * 0.5;
        let cy = (area.y + (area.h - block) * 0.5 + 40.0).round();
        c.fill_circle(cx, cy, 40.0, color.alpha(0.14));
        match icon {
            Some(icon) => draw_icon(c, icon, cx, cy, 34.0, color),
            None => {
                let a = self.time * 5.0;
                c.stroke_arc(cx, cy, 22.0, a, a + 4.2, 3.0, color);
                self.anim.set(wid("caret", 0, 0), self.time);
            }
        }
        self.text_center(c, self.f.sans_med, 20.0, cx, cy + 66.0, title, t.text);
        let mut y = cy + 94.0;
        for l in lines.iter().take(3) {
            self.text_center(c, self.f.sans, 13.0, cx, y, l, t.text2);
            y += 20.0;
        }
        y
    }

    fn busy_body(&mut self, c: &mut Canvas, card: Rect, title: &str, detail: &str) {
        let accent = self.theme.accent;
        self.status_block(c, card, None, accent, title, detail);
    }

    fn done_body(&mut self, c: &mut Canvas, card: Rect) {
        let accent = self.theme.accent;
        let relaunch = matches!(&self.install.sheet, Sheet::Done(p) if !p.as_os_str().is_empty());
        let (title, detail) = match (self.install.status.platform, relaunch) {
            (Platform::Windows, _) => (tr("Ponzik is installed"), tr("Starting it from its new home…").to_string()),
            (_, true) => (tr("Ponzik is in your applications menu"), tr("Starting it from ~/Applications…").to_string()),
            _ => (tr("Ponzik is in your applications menu"), tr("Find it with your other apps").to_string()),
        };
        let detail = match self.install.warnings.first() {
            Some(w) => tr_fmt!("{} (note: {})", detail.as_str(), w.as_str()),
            None => detail,
        };
        self.status_block(c, card, Some(Icon::Check), accent, title, &detail);
    }

    /// The line above the buttons of a sheet; returns their top.
    fn sheet_footer(&mut self, c: &mut Canvas, card: Rect) -> f32 {
        let fy = card.bottom() - 76.0;
        c.fill_rect(Rect::new(card.x + 1.0, fy, card.w - 2.0, 1.0), self.theme.hairline);
        fy + 18.0
    }

    fn failed_body(&mut self, c: &mut Canvas, input: &Input, card: Rect, area: Rect, error: &str) -> bool {
        let t = self.theme;
        let removing = self.removing();
        let title = if removing { tr("Ponzik could not be removed") } else { tr("Ponzik could not be installed") };
        self.status_block(c, area, Some(Icon::Missing), t.negative, title, error);
        let by = self.sheet_footer(c, card);
        let (again, other) = (tr("Try again"), if removing { tr("Close") } else if self.install.standalone { tr("Just run it") } else { tr("Close") });
        let (wa, wo) = (self.sheet_button_width(again), self.sheet_button_width(other));
        let x = card.right() - 40.0 - wa - 12.0 - wo;
        let mut keep = true;
        if self.sheet_button(c, input, wid("inst-other", 0, 0), Rect::new(x, by, wo, 40.0), if removing { Icon::Close } else { Icon::Play }, other, Look::Plain) {
            if removing {
                keep = self.close_sheet();
            } else {
                self.run_portable();
                keep = false;
            }
        }
        if self.sheet_button(c, input, wid("inst-again", 0, 0), Rect::new(x + wo + 12.0, by, wa, 40.0), Icon::Reset, again, Look::Primary) {
            if removing {
                self.start_uninstall();
            } else {
                self.start_install();
            }
        }
        keep
    }

    /// Closes the sheet: the window of a standalone screen, the popup otherwise.
    fn close_sheet(&mut self) -> bool {
        if self.install.standalone {
            self.install.exit = Some(Exit::Quit);
        }
        false
    }

    fn uninstall_body(&mut self, c: &mut Canvas, input: &Input, card: Rect) -> bool {
        let t = self.theme;
        let pad = 40.0;
        // Header: a red-tinted bin, the question.
        let ic = Rect::new(card.x + pad, card.y + 38.0, 60.0, 60.0);
        c.fill_rrect(ic, 16.0, t.negative.alpha(0.14));
        draw_icon(c, Icon::Trash, ic.x + 30.0, ic.y + 30.0, 28.0, t.negative.mix(t.text, 0.2));
        let tx = ic.right() + 22.0;
        self.text_fit(c, self.f.sans_med, 24.0, tx, ic.y + 20.0, tr("Uninstall Ponzik?"), card.right() - tx - pad, t.text);
        let v = crate::build_info::version();
        let sub = if v.build > 0 { tr_fmt!("Version {} · build {}", v.semver(), v.build) } else { tr_fmt!("Version {}", v.semver()) };
        self.text_fit(c, self.f.sans, 13.0, tx, ic.y + 46.0, &sub, card.right() - tx - pad, t.text2);

        let what = if self.install.status.platform == Platform::Linux {
            tr("Removes Ponzik from the applications menu, with its icons and file types. The AppImage stays where it is.")
        } else {
            tr("Removes the program, its shortcuts, its file types and its entry in Windows Settings.")
        };
        let mut y = ic.bottom() + 34.0;
        let tw = card.w - pad * 2.0;
        for l in self.wrap(self.f.sans, 14.0, what, tw) {
            self.text(c, self.f.sans, 14.0, card.x + pad, y, &l, t.text2.mix(t.text, 0.4));
            y += 22.0;
        }
        // What stays: the projects.
        y += 10.0;
        let keep_r = Rect::new(card.x + pad, y, tw, 52.0);
        c.fill_rrect(keep_r, 12.0, t.accent.alpha(0.08));
        draw_icon(c, Icon::Check, keep_r.x + 22.0, keep_r.y + 26.0, 15.0, t.accent);
        let projects = short_path(&super::compat::projects_dir());
        self.text_fit(c, self.f.sans_med, 13.0, keep_r.x + 42.0, keep_r.y + 17.0, tr("Your projects and sounds stay"), tw - 60.0, t.text);
        self.text_fit(c, self.f.mono, 11.0, keep_r.x + 42.0, keep_r.y + 36.0, &projects, tw - 60.0, t.text2);
        y = keep_r.bottom() + 28.0;
        let purge = self.install.purge;
        if self.check_row(c, input, wid("inst-purge", 0, 0), card.x + pad + 2.0, y, tr("Also remove my settings (themes, shortcuts, translations)"), purge) {
            self.install.purge = !purge;
        }
        self.text_fit(c, self.f.mono, 11.0, card.x + pad + 30.0, y + 22.0, &short_path(&super::config_dir()), tw - 30.0, t.muted);

        // Buttons.
        let by = self.sheet_footer(c, card);
        let (rm, cancel) = (tr("Uninstall"), tr("Cancel"));
        let (wr, wc) = (self.sheet_button_width(rm), self.sheet_button_width(cancel));
        let rr = Rect::new(card.right() - pad - wr, by, wr, 40.0);
        let cr = Rect::new(rr.x - 12.0 - wc, by, wc, 40.0);
        let mut keep = true;
        if self.sheet_button(c, input, wid("inst-cancel", 0, 0), cr, Icon::Close, cancel, Look::Plain) {
            keep = self.close_sheet();
        }
        if self.sheet_button(c, input, wid("inst-remove", 0, 0), rr, Icon::Trash, rm, Look::Danger) {
            self.start_uninstall();
        }
        if input.keys.iter().any(|(k, _)| *k == Key::Escape) {
            keep = self.close_sheet();
        }
        keep
    }

    fn removed_body(&mut self, c: &mut Canvas, input: &Input, card: Rect, area: Rect) -> bool {
        let accent = self.theme.accent;
        let linux = self.install.status.platform == Platform::Linux;
        let title = if linux { tr("Ponzik left the applications menu") } else { tr("Ponzik is uninstalled") };
        let detail = tr_fmt!("Thanks for making music with it. Your projects are still in {}.", short_path(&super::compat::projects_dir()));
        self.status_block(c, area, Some(Icon::Check), accent, title, &detail);
        let label = if self.install.standalone || linux { tr("Close") } else { tr("Quit Ponzik") };
        let w = self.sheet_button_width(label);
        let by = self.sheet_footer(c, card);
        let r = Rect::new(card.right() - 40.0 - w, by, w, 40.0);
        let mut keep = true;
        let enter = input.keys.iter().any(|(k, _)| matches!(k, Key::Enter | Key::Escape));
        if self.sheet_button(c, input, wid("inst-close", 0, 0), r, Icon::Check, label, Look::Primary) || enter {
            if !self.install.standalone && !linux {
                // The program is gone: Ponzik ends, then deletes itself.
                if self.modified {
                    self.save();
                }
                self.install.quit = true;
            }
            keep = self.close_sheet();
        }
        keep
    }

    // --- Settings › About ---

    /// The Installation card: where this copy runs, and what can be done.
    pub(super) fn install_card(&mut self, c: &mut Canvas, input: &Input, area: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        self.text(c, self.f.sans_med, 11.0, area.x + 2.0, area.y + 8.0, tr("Installation"), t.muted);
        let card = Rect::new(area.x, area.y + 22.0, area.w, 96.0);
        c.fill_rrect(card, 14.0, t.raised);
        c.stroke_rrect(card, 14.0, 1.0, t.hairline);
        let st = self.install.status.clone();
        let (icon, color, title, tip): (Icon, Color, &str, &str) = match (st.platform, &st.place) {
            (Platform::Windows, Place::Installed) => (Icon::Check, t.accent, tr("Installed"), tr("With shortcuts and file types")),
            (Platform::Windows, Place::Portable) => (Icon::Play, t.text2, tr("Portable"), tr("Nothing is installed")),
            (Platform::Windows, _) => (Icon::Play, t.text2, tr("Not installed"), tr("Runs from the downloaded file")),
            (Platform::Linux, Place::Installed) => (Icon::Check, t.accent, tr("In the applications menu"), tr("The menu starts this AppImage")),
            (Platform::Linux, Place::Loose) => (Icon::Play, t.text2, tr("Portable AppImage"), tr("Not in the applications menu")),
            _ => (Icon::Info, t.text2, tr("Built from source"), tr("Release builds can install themselves")),
        };
        let ic = Rect::new(card.x + 16.0, card.y + 16.0, 36.0, 36.0);
        c.fill_rrect(ic, 10.0, color.alpha(0.14));
        draw_icon(c, icon, ic.x + 18.0, ic.y + 18.0, 16.0, color);
        let tx = ic.right() + 14.0;
        // The action, on the right of the title.
        let action: Option<(Icon, &str, &str)> = match (st.platform, &st.place) {
            (Platform::Windows, Place::Installed) => Some((Icon::Trash, tr("Uninstall"), tr("Removes Ponzik from this computer; your projects stay"))),
            (Platform::Windows, Place::Portable | Place::Loose) => {
                Some((Icon::Install, tr("Install"), tr("Copies Ponzik to your programs, with shortcuts and file types")))
            }
            (Platform::Linux, Place::Installed) => Some((Icon::Close, tr("Remove from menu"), tr("Removes the menu entry, icons and file types; the AppImage stays"))),
            (Platform::Linux, Place::Loose) => Some((Icon::Install, tr("Add to menu"), tr("Puts Ponzik in the applications menu, opening .ponzik and .flp files"))),
            _ => None,
        };
        let mut text_right = card.right() - 16.0;
        if let Some((aicon, name, atip)) = action {
            let w = self.fonts.measure(self.f.sans_med, 12.0, name) + 48.0;
            let b = Rect::new(card.right() - 16.0 - w, card.y + 18.0, w, 32.0);
            text_right = b.x - 12.0;
            let id = wid("about-install", 0, 0);
            if self.pill_button(c, input, id, b, aicon, name, false) {
                self.install_action(&st);
            }
            if self.hovered(input, id, b) {
                *hint = Some(atip.into());
            }
        }
        self.text_fit(c, self.f.sans_med, 15.0, tx, card.y + 26.0, title, text_right - tx, t.text);
        self.text_fit(c, self.f.sans, 12.0, tx, card.y + 46.0, tip, text_right - tx, t.text2);
        let loc = short_path(&st.location());
        self.text_fit(c, self.f.mono, 11.0, card.x + 16.0, card.bottom() - 18.0, &loc, card.w - 32.0, t.muted);
        if self.hovered(input, wid("about-install-path", 0, 0), Rect::new(card.x, card.bottom() - 32.0, card.w, 32.0)) {
            *hint = Some(st.location().display().to_string());
        }
    }

    /// The button of the Installation card.
    fn install_action(&mut self, st: &Status) {
        match (st.platform, &st.place) {
            (Platform::Windows, Place::Installed) => self.open_install_sheet(Sheet::Uninstall),
            (Platform::Windows, _) => self.open_install_sheet(Sheet::Welcome),
            (Platform::Linux, Place::Installed) => {
                match install::linux::uninstall(&install::linux::Layout::user()) {
                    Ok(_) => self.toast(tr("Ponzik left the applications menu")),
                    Err(e) => self.toast(&e),
                }
                self.install.status = install::status();
            }
            (Platform::Linux, _) => {
                let opts = Options { desktop: false, move_to_applications: false };
                match install::linux::install(&st.program, opts, &install::linux::Layout::user()) {
                    Ok(_) => self.toast(tr("Ponzik is in the applications menu")),
                    Err(e) => self.toast(&e),
                }
                self.install.status = install::status();
            }
            _ => {}
        }
    }

    /// Screenshots (`PONZIK_INSTALL=<sheet>[,windows|linux][,installed|portable|loose|unmanaged][,app]`):
    /// sheets: welcome, working, done, failed, uninstall, removed, about.
    pub fn screenshot_install(&mut self, spec: &str) {
        let mut parts = spec.split(',').map(str::trim);
        let sheet = parts.next().unwrap_or("welcome");
        let mut platform = self.install.status.platform;
        let mut place = None;
        let mut in_app = false;
        for p in parts {
            match p {
                "windows" => platform = Platform::Windows,
                "linux" => platform = Platform::Linux,
                "installed" => place = Some(Place::Installed),
                "portable" => place = Some(Place::Portable),
                "loose" => place = Some(Place::Loose),
                "unmanaged" => place = Some(Place::Unmanaged),
                "app" => in_app = true,
                _ => {}
            }
        }
        let (program, installed) = match platform {
            Platform::Windows => {
                let dl = r"C:\Users\Ana\Downloads\Ponzik-windows-x86_64.exe";
                let inst = r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe";
                match place {
                    Some(Place::Installed) => (inst, Some(inst)),
                    Some(Place::Portable) => (r"D:\Tools\Ponzik\Ponzik.exe", None),
                    _ => (dl, None),
                }
            }
            _ => {
                let dl = "/home/ana/Downloads/Ponzik-x86_64.AppImage";
                match place {
                    Some(Place::Installed) => ("/home/ana/Applications/Ponzik-x86_64.AppImage", Some("/home/ana/Applications/Ponzik-x86_64.AppImage")),
                    Some(Place::Unmanaged) => ("/home/ana/src/ponzik/target/release/ponzik", None),
                    _ => (dl, None),
                }
            }
        };
        let default_place = if sheet == "uninstall" || sheet == "removed" { Place::Installed } else { Place::Loose };
        self.install.status = Status::pretend(platform, place.unwrap_or(default_place), program, installed);
        self.install.move_apps = platform == Platform::Linux && install::linux::looks_temporary(Path::new(program));
        if platform == Platform::Windows {
            self.install.pretend_dest = Some(PathBuf::from(r"%LOCALAPPDATA%\Programs\Ponzik"));
        }
        if sheet == "about" {
            self.open_popup(Popup::Settings { tab: 4 }, Rect::default());
            return;
        }
        let dest = match platform {
            Platform::Windows => PathBuf::from(r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe"),
            _ => PathBuf::from("/home/ana/Applications/Ponzik-x86_64.AppImage"),
        };
        self.install.sheet = match sheet {
            "working" => Sheet::Working,
            "done" => Sheet::Done(dest),
            "failed" => Sheet::Failed(tr_fmt!("Could not copy Ponzik to {}: {}", r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe", "Access is denied. (os error 5)")),
            "uninstall" => Sheet::Uninstall,
            "removed" => Sheet::Removed,
            _ => Sheet::Welcome,
        };
        // Done waits forever in a capture.
        self.install.done_at = f32::INFINITY;
        if in_app {
            self.install.standalone = false;
            self.open_popup(Popup::Install, Rect::default());
        } else {
            self.install.standalone = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_start_from_home() {
        let home = std::env::var("HOME").ok().filter(|h| h.len() > 1);
        if let (Some(h), false) = (home, cfg!(windows)) {
            assert_eq!(short_path(&Path::new(&h).join("Applications/P.AppImage")), "~/Applications/P.AppImage");
            // A sibling folder that only starts like home is not shortened.
            assert_eq!(short_path(Path::new(&format!("{h}x/a"))), format!("{h}x/a"));
        }
        assert_eq!(short_path(Path::new("/opt/ponzik")), "/opt/ponzik");
    }

    #[test]
    fn the_choice_is_remembered_only_when_asked() {
        let mut ui = InstallUi::default();
        let mut fields = Vec::new();
        ui.save(&mut fields);
        assert!(fields.is_empty());
        ui.dont_ask = true;
        ui.save(&mut fields);
        let j = Json::Obj(fields);
        let mut back = InstallUi::default();
        back.load(&j);
        assert!(back.dont_ask);
        back.load(&Json::Obj(Vec::new()));
        assert!(!back.dont_ask);
    }
}
