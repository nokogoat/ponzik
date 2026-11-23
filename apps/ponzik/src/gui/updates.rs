//! Updates inside the app: a check at startup (in the background, never in
//! the way), a toast when a newer build exists, the download and the swap of
//! the AppImage or of `Ponzik.exe`, and the About tab of the settings.

use std::path::PathBuf;
use std::sync::mpsc;

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::json::Json;
use ponzik_project::update::{self, BuildVersion, Channel, FetchError, Release};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};
use crate::build_info;

/// Where the update is in its life.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    /// A newer build is published (see [`Updates::latest`]).
    Available,
    Downloading,
    /// The new file is in place: a restart runs it.
    Installed,
    Failed(String),
}

/// What a menu row, toast or button asks of the updater.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    Check,
    /// Download, verify and put in place; then restart.
    Install,
    Restart,
    /// The releases page in the browser (builds that cannot replace themselves).
    OpenReleases,
    SetAutoCheck(bool),
    SetAutoInstall(bool),
    SetChannel(Channel),
    /// The "What's new" popup.
    ShowNews,
    /// The same popup with the whole changelog.
    ShowChangelog,
}

enum Msg {
    Checked(Result<Release, FetchError>),
    Installed(Result<PathBuf, String>),
}

/// Settings and state of the updater.
pub struct Updates {
    pub phase: Phase,
    /// The newest release seen, and its build.
    pub latest: Option<(Release, BuildVersion)>,
    rx: Option<mpsc::Receiver<Msg>>,
    /// File being downloaded and its expected size (progress).
    download: Option<(PathBuf, u64)>,
    pub progress: f32,
    /// The check was asked for: "up to date" and errors are said out loud.
    manual: bool,
    /// Restart once the new file is in place.
    restart_after: bool,
    /// Set by "Restart": the event loop ends, then this file is started.
    pub relaunch: Option<PathBuf>,
    pub auto_check: bool,
    pub auto_install: bool,
    pub channel: Channel,
    /// Token for a private repository (`update_token` in settings.json).
    token: Option<String>,
    /// Heading of the changelog section last acknowledged in "What's new".
    pub news_seen: String,
    pub news_scroll: f32,
    pub news_full: bool,
    /// `news_seen` before the popup marked it, for an unchecked box.
    pub news_prev: String,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            latest: None,
            rx: None,
            download: None,
            progress: 0.0,
            manual: false,
            restart_after: false,
            relaunch: None,
            auto_check: true,
            auto_install: false,
            channel: if build_info::CHANNEL == "stable" { Channel::Stable } else { Channel::Nightly },
            token: None,
            news_seen: String::new(),
            news_scroll: 0.0,
            news_full: false,
            news_prev: String::new(),
        }
    }
}

impl Updates {
    pub fn load(&mut self, j: &Json) {
        let flag = |k: &str| j.get(k).and_then(Json::as_bool);
        self.auto_check = flag("update_check").unwrap_or(self.auto_check);
        self.auto_install = flag("update_install").unwrap_or(self.auto_install);
        if let Some(c) = j.get("update_channel").and_then(Json::as_str).and_then(Channel::from_id) {
            self.channel = c;
        }
        self.token = j.get("update_token").and_then(Json::as_str).filter(|t| !t.trim().is_empty()).map(|t| t.trim().to_string());
        self.news_seen = j.get("whats_new_seen").and_then(Json::as_str).unwrap_or("").to_string();
    }

    pub fn save(&self, fields: &mut Vec<(String, Json)>) {
        fields.push(("update_check".into(), self.auto_check.into()));
        fields.push(("update_install".into(), self.auto_install.into()));
        fields.push(("update_channel".into(), self.channel.id().into()));
        if let Some(t) = &self.token {
            fields.push(("update_token".into(), t.as_str().into()));
        }
        if !self.news_seen.is_empty() {
            fields.push(("whats_new_seen".into(), self.news_seen.as_str().into()));
        }
    }

    /// `PONZIK_GITHUB_TOKEN`, else the token of the settings.
    fn token(&self) -> Option<String> {
        std::env::var("PONZIK_GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty()).or_else(|| self.token.clone())
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Checking | Phase::Downloading)
    }

    /// Version of the newer release, `0.1.0 (build 1234)`.
    fn latest_label(&self) -> String {
        self.latest.as_ref().map_or_else(String::new, |(_, v)| label(v))
    }
}

fn label(v: &BuildVersion) -> String {
    if v.build > 0 { tr_fmt!("{} (build {})", v.semver(), v.build) } else { v.semver() }
}

/// The file an update replaces: the AppImage on Linux, the program on Windows.
/// `None` elsewhere (a build from source): updates then point to the releases page.
pub fn install_target() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::current_exe().ok();
    }
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file());
    }
    None
}

/// Opens an address in the default browser.
pub fn open_url(url: &str) {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("explorer.exe");
        c.arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// After the window closed: starts the new build with the same arguments.
pub fn relaunch(target: &std::path::Path) {
    let mut cmd = std::process::Command::new(target);
    cmd.args(std::env::args_os().skip(1));
    if let Err(e) = cmd.spawn() {
        eprintln!("restart failed ({}): {e}", target.display());
    }
}

/// Removes what the previous update left (`Ponzik.exe.old`), at startup.
pub fn cleanup_previous() {
    if let Some(target) = install_target() {
        update::cleanup_old(&target);
    }
}

impl App {
    /// Checks for a newer build in the background; `manual`: asked by the user.
    pub fn start_update_check(&mut self, manual: bool) {
        if self.updates.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (channel, token) = (self.updates.channel, self.updates.token());
        let scratch = std::env::temp_dir().join(format!("ponzik-release-{}.json", std::process::id()));
        let spawned = std::thread::Builder::new()
            .name("ponzik-update-check".into())
            .spawn(move || {
                let _ = tx.send(Msg::Checked(update::fetch_release(channel, token.as_deref(), &scratch)));
            });
        if spawned.is_ok() {
            self.updates.rx = Some(rx);
            self.updates.phase = Phase::Checking;
            self.updates.manual = manual;
        }
    }

    /// The check at startup: release builds only, when the setting allows it
    /// (`PONZIK_UPDATE_CHECK=1` forces it, `=0` prevents it).
    pub fn start_update_check_at_launch(&mut self) {
        cleanup_previous();
        let forced = std::env::var("PONZIK_UPDATE_CHECK").ok();
        let go = match forced.as_deref() {
            Some("0") => false,
            Some(_) => true,
            None => self.updates.auto_check && build_info::is_release(),
        };
        if go {
            self.start_update_check(false);
        }
    }

    pub fn update_op(&mut self, op: UpdateOp) {
        match op {
            UpdateOp::Check => self.start_update_check(true),
            UpdateOp::ShowNews => self.open_whats_new(),
            UpdateOp::ShowChangelog => {
                self.open_whats_new();
                self.updates.news_full = true;
            }
            UpdateOp::Install => self.install_update(true),
            UpdateOp::Restart => self.restart_for_update(),
            UpdateOp::OpenReleases => {
                let url = self.updates.latest.as_ref().map(|(r, _)| r.html_url.clone()).filter(|u| !u.is_empty());
                open_url(url.as_deref().unwrap_or(update::RELEASES_PAGE));
            }
            UpdateOp::SetAutoCheck(on) => {
                self.updates.auto_check = on;
                self.save_settings();
            }
            UpdateOp::SetAutoInstall(on) => {
                self.updates.auto_install = on;
                self.save_settings();
            }
            UpdateOp::SetChannel(c) => {
                if self.updates.channel != c {
                    self.updates.channel = c;
                    self.updates.latest = None;
                    self.save_settings();
                    self.start_update_check(true);
                }
            }
        }
    }

    /// Downloads the newer build, checks it and puts it in place of this one.
    fn install_update(&mut self, restart: bool) {
        if self.updates.busy() {
            return;
        }
        let Some((release, version)) = self.updates.latest.clone() else { return };
        let (Some(target), Some(asset)) = (install_target(), update::platform_asset()) else {
            self.update_op(UpdateOp::OpenReleases);
            return;
        };
        let Some(size) = release.asset(asset).map(|a| a.size) else {
            self.update_failed(tr_fmt!("{} is missing from the release", asset), true);
            return;
        };
        let dest = std::env::temp_dir().join(format!("ponzik-update-{}.download", std::process::id()));
        let token = self.updates.token();
        let (tx, rx) = mpsc::channel();
        let file = dest.clone();
        let spawned = std::thread::Builder::new().name("ponzik-update-install".into()).spawn(move || {
            let result = update::download_verified(&release, asset, token.as_deref(), &file).map_err(|e| e.message()).and_then(|()| {
                update::install_file(&file, &target, cfg!(windows))
                    .map(|()| target.clone())
                    .map_err(|e| tr_fmt!("Could not replace {}: {}", target.display().to_string(), e.to_string()))
            });
            let _ = std::fs::remove_file(&file);
            let _ = tx.send(Msg::Installed(result));
        });
        if spawned.is_ok() {
            self.updates.rx = Some(rx);
            self.updates.phase = Phase::Downloading;
            self.updates.download = Some((dest, size.max(1)));
            self.updates.progress = 0.0;
            self.updates.restart_after = restart;
            self.toast(&tr_fmt!("Downloading Ponzik {}…", label(&version)));
        }
    }

    /// Ends the program and starts the new file, the project saved first.
    fn restart_for_update(&mut self) {
        let Some(target) = install_target() else { return };
        if self.modified {
            self.save();
            if self.modified {
                return;
            }
        }
        self.updates.relaunch = Some(target);
    }

    fn update_failed(&mut self, msg: String, loud: bool) {
        if loud {
            self.toast(&msg);
        }
        self.updates.phase = Phase::Failed(msg);
    }

    /// Answers of the background threads; true when something changed on screen.
    pub fn updates_tick(&mut self) -> bool {
        let mut changed = false;
        if let Some((file, size)) = &self.updates.download {
            let p = std::fs::metadata(file).map_or(0.0, |m| (m.len() as f64 / *size as f64).min(1.0) as f32);
            if (p - self.updates.progress).abs() > 0.004 {
                self.updates.progress = p;
                changed = true;
            }
        }
        let Some(msg) = self.updates.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return changed };
        self.updates.rx = None;
        match msg {
            Msg::Checked(Ok(release)) => self.release_found(release),
            Msg::Checked(Err(e)) => {
                let loud = self.updates.manual;
                // Offline at startup: nothing to say.
                self.update_failed(e.message(), loud);
            }
            Msg::Installed(Ok(_)) => {
                self.updates.download = None;
                self.updates.phase = Phase::Installed;
                if self.updates.restart_after {
                    self.restart_for_update();
                }
                if self.updates.relaunch.is_none() {
                    let text = tr_fmt!("Ponzik {} installed · restart to use it", self.updates.latest_label());
                    self.toast_action_long(&text, tr("Restart"), Action::Update(UpdateOp::Restart));
                }
            }
            Msg::Installed(Err(e)) => {
                self.updates.download = None;
                self.update_failed(e, true);
            }
        }
        true
    }

    fn release_found(&mut self, release: Release) {
        let current = build_info::version();
        let Some(version) = release.version() else {
            self.update_failed(tr_fmt!("{}: no version in the release", release.tag.as_str()), self.updates.manual);
            return;
        };
        if !version.is_newer_than(&current) {
            self.updates.phase = Phase::UpToDate;
            self.updates.latest = Some((release, version));
            if self.updates.manual {
                self.toast(tr("Ponzik is up to date"));
            }
            return;
        }
        self.updates.latest = Some((release, version.clone()));
        self.updates.phase = Phase::Available;
        let can_install = install_target().is_some() && update::platform_asset().is_some();
        if can_install && self.updates.auto_install {
            self.install_update(false);
        } else {
            self.announce_update(&version);
        }
    }

    /// The toast of a newer build: install it here, or download it by hand.
    fn announce_update(&mut self, version: &BuildVersion) {
        let text = tr_fmt!("Ponzik {} is available", label(version));
        if install_target().is_some() && update::platform_asset().is_some() {
            self.toast_action_long(&text, tr("Install & restart"), Action::Update(UpdateOp::Install));
        } else {
            self.toast_action_long(&text, tr("Download"), Action::Update(UpdateOp::OpenReleases));
        }
    }

    /// Detail of the menu row "About and updates": what is waiting.
    pub fn updates_badge(&self) -> String {
        match self.updates.phase {
            Phase::Available => tr("update available").into(),
            Phase::Downloading => tr_fmt!("{}%", (self.updates.progress * 100.0).round() as u32),
            Phase::Installed => tr("restart to update").into(),
            _ => String::new(),
        }
    }

    /// The program should end now (restart into the update).
    pub fn quit_requested(&self) -> bool {
        self.updates.relaunch.is_some()
    }

    /// Fake states for the screenshots (`PONZIK_UPDATE=available|downloading|installed|uptodate|error|offline`).
    pub fn screenshot_update(&mut self, spec: &str) {
        let release = Release {
            tag: update::NIGHTLY_TAG.into(),
            name: "Ponzik nightly".into(),
            html_url: update::RELEASES_PAGE.into(),
            published_at: "2026-11-24T09:30:00Z".into(),
            prerelease: true,
            ..Default::default()
        };
        let mut v = build_info::version();
        v.build += 7;
        v.hash = "5f3c2a1".into();
        self.updates.latest = Some((release, v.clone()));
        self.updates.phase = match spec {
            "downloading" => {
                self.updates.progress = 0.42;
                Phase::Downloading
            }
            "installed" => Phase::Installed,
            "uptodate" => Phase::UpToDate,
            "error" => Phase::Failed(FetchError::Denied(403).message()),
            "offline" => Phase::Failed(FetchError::Offline(String::new()).message()),
            _ => Phase::Available,
        };
        match spec {
            "available" => self.announce_update(&v),
            "installed" => {
                let text = tr_fmt!("Ponzik {} installed · restart to use it", label(&v));
                self.toast_action_long(&text, tr("Restart"), Action::Update(UpdateOp::Restart));
            }
            _ => {}
        }
    }

    // --- Settings › About ---

    pub(super) fn about_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let left = Rect::new(body.x, body.y, (body.w * 0.42).round(), body.h);
        let right = Rect::new(left.right() + 32.0, body.y, body.right() - left.right() - 32.0, body.h);

        // Identity: the mark, the name, the build.
        let mark = Rect::new(left.x, left.y + 4.0, 96.0, 96.0);
        c.fill_rrect(mark.translate(0.0, 6.0).inset(-2.0), 26.0, ponzik_ui::Color::rgba(0, 0, 0, 70));
        ponzik_ui::brand::draw_mark(c, mark, true);
        let tx = mark.right() + 20.0;
        self.text(c, self.f.sans_med, 26.0, tx, mark.y + 28.0, "Ponzik", t.text);
        self.text_fit(c, self.f.sans, 13.0, tx, mark.y + 56.0, tr("The free DAW. No more gatekeeping."), left.right() - tx, t.text2);
        let v = build_info::version();
        let chip = |s: &str| s.to_string();
        let mut chips = vec![chip(&v.semver())];
        if v.build > 0 {
            chips.push(tr_fmt!("build {}", v.build));
        }
        let channel = match build_info::CHANNEL {
            "nightly" => tr("nightly").to_string(),
            "stable" => tr("stable").to_string(),
            _ => tr("local build").to_string(),
        };
        chips.push(channel);
        let mut x = tx;
        for (i, s) in chips.iter().enumerate() {
            let w = self.fonts.measure(self.f.mono_med, 11.0, s) + 16.0;
            let r = Rect::new(x, mark.y + 74.0, w, 22.0);
            let col = if i == 0 { t.accent } else { t.text2 };
            c.fill_rrect(r, 11.0, if i == 0 { t.accent.alpha(0.14) } else { t.raised });
            self.text_center(c, self.f.mono_med, 11.0, r.x + r.w * 0.5, r.y + 11.0, s, col);
            x += w + 6.0;
        }

        // Details, one per line.
        let mut y = mark.bottom() + 36.0;
        self.text(c, self.f.sans_med, 11.0, left.x + 2.0, y, tr("This build"), t.muted);
        y += 22.0;
        let rows = [
            (tr("Version"), v.id()),
            (tr("Revision"), if v.hash.is_empty() { "—".into() } else { v.hash.clone() }),
            (tr("Date"), build_info::DATE.to_string()),
            (tr("System"), format!("{} · {}", std::env::consts::OS, std::env::consts::ARCH)),
        ];
        for (k, val) in rows {
            self.text(c, self.f.sans, 12.0, left.x + 2.0, y, k, t.text2);
            self.text_fit(c, self.f.mono, 12.0, left.x + 96.0, y, &val, left.w - 98.0, t.text);
            y += 24.0;
        }
        y += 14.0;
        let w = (left.w - 8.0) * 0.5;
        if self.pill_button(c, input, wid("about-news", 0, 0), Rect::new(left.x, y, w, 32.0), Icon::Info, tr("What's new"), false) {
            self.open_whats_new();
        }
        if self.hovered(input, wid("about-news", 0, 0), Rect::new(left.x, y, w, 32.0)) {
            *hint = Some(tr("The changes of this version").into());
        }
        let rel = Rect::new(left.x + w + 8.0, y, w, 32.0);
        if self.pill_button(c, input, wid("about-rel", 0, 0), rel, Icon::Export, tr("All releases"), false) {
            open_url(update::RELEASES_PAGE);
        }
        if self.hovered(input, wid("about-rel", 0, 0), rel) {
            *hint = Some(update::RELEASES_PAGE.into());
        }

        // Updates.
        self.update_card(c, input, right, hint);
    }

    fn update_card(&mut self, c: &mut Canvas, input: &Input, right: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let mut y = right.y;
        self.text(c, self.f.sans_med, 11.0, right.x + 2.0, y + 8.0, tr("Updates"), t.muted);
        y += 22.0;

        // Status card: what the updater knows, and the one thing to do next.
        let card = Rect::new(right.x, y, right.w, 112.0);
        c.fill_rrect(card, 14.0, t.raised);
        c.stroke_rrect(card, 14.0, 1.0, t.hairline);
        let phase = self.updates.phase.clone();
        let latest = self.updates.latest_label();
        let can_install = install_target().is_some() && update::platform_asset().is_some();
        let (icon, title, detail, color): (Icon, String, String, ponzik_ui::Color) = match &phase {
            Phase::Idle => (Icon::Info, tr("Not checked yet").into(), tr("Check now, or let Ponzik look at startup").into(), t.text2),
            Phase::Checking => (Icon::Reset, tr("Checking…").into(), tr_fmt!("Asking GitHub for the {} channel", channel_name(self.updates.channel)), t.text2),
            Phase::UpToDate => (Icon::Check, tr("Ponzik is up to date").into(), tr_fmt!("Latest on the {} channel: {}", channel_name(self.updates.channel), latest), t.accent),
            Phase::Available => (
                Icon::Export,
                tr_fmt!("Ponzik {} is available", latest),
                if can_install { tr("Downloaded, checked (SHA-256), then Ponzik restarts").into() } else { tr("Download it from the releases page").into() },
                t.accent,
            ),
            Phase::Downloading => (Icon::Export, tr_fmt!("Downloading Ponzik {}…", latest), tr_fmt!("{}%", (self.updates.progress * 100.0).round() as u32), t.accent),
            Phase::Installed => (Icon::Check, tr_fmt!("Ponzik {} installed", latest), tr("Restart to use it").into(), t.accent),
            Phase::Failed(e) => (Icon::Info, tr("Update check failed").into(), e.clone(), t.negative),
        };
        let ic = ponzik_ui::Rect::new(card.x + 16.0, card.y + 18.0, 36.0, 36.0);
        c.fill_rrect(ic, 10.0, color.alpha(0.14));
        if phase == Phase::Checking {
            // A turning arc while GitHub answers.
            let a = self.time * 5.0;
            c.stroke_arc(ic.x + 18.0, ic.y + 18.0, 9.0, a, a + 4.2, 2.0, color);
            self.anim.set(wid("caret", 0, 0), self.time);
        } else {
            draw_icon(c, icon, ic.x + 18.0, ic.y + 18.0, 16.0, color);
        }
        let tx = ic.right() + 14.0;
        self.text_fit(c, self.f.sans_med, 15.0, tx, card.y + 28.0, &title, card.right() - tx - 16.0, t.text);
        let lines = self.wrap(self.f.sans, 12.0, &detail, card.right() - tx - 16.0);
        for (i, l) in lines.iter().take(2).enumerate() {
            self.text(c, self.f.sans, 12.0, tx, card.y + 50.0 + i as f32 * 17.0, l, if matches!(phase, Phase::Failed(_)) { t.negative.mix(t.text, 0.35) } else { t.text2 });
        }
        if phase == Phase::Downloading {
            let bar = Rect::new(tx, card.bottom() - 22.0, card.right() - tx - 16.0, 6.0);
            c.fill_rrect(bar, 3.0, t.well);
            c.fill_rrect(Rect::new(bar.x, bar.y, (bar.w * self.updates.progress).max(6.0), bar.h), 3.0, t.accent);
        }
        // The action of the card.
        let action: Option<(Icon, &str, UpdateOp, bool)> = match phase {
            Phase::Available if can_install => Some((Icon::Export, tr("Install & restart"), UpdateOp::Install, true)),
            Phase::Available => Some((Icon::Export, tr("Download"), UpdateOp::OpenReleases, true)),
            Phase::Installed => Some((Icon::Reset, tr("Restart"), UpdateOp::Restart, true)),
            _ => None,
        };
        if let Some((icon, name, op, accent)) = action {
            let w = self.fonts.measure(self.f.sans_med, 12.0, name) + 48.0;
            let b = Rect::new(card.right() - 16.0 - w, card.bottom() - 46.0, w, 32.0);
            if self.pill_button(c, input, wid("upd-act", 0, 0), b, icon, name, accent) {
                self.update_op(op);
            }
        }
        y = card.bottom() + 12.0;

        let check = Rect::new(right.x, y, 150.0, 32.0);
        if self.pill_button(c, input, wid("upd-check", 0, 0), check, Icon::Reset, tr("Check now"), false) {
            self.update_op(UpdateOp::Check);
        }
        if self.hovered(input, wid("upd-check", 0, 0), check) {
            *hint = Some(tr("Asks GitHub for a newer build, in the background").into());
        }
        y += 32.0 + 26.0;

        // Settings: two switches, then the channel.
        let toggles = [
            (tr("Check at startup"), tr("A quiet look at GitHub when Ponzik opens; nothing is sent about you"), self.updates.auto_check, 0),
            (tr("Install automatically"), tr("Downloads and installs in the background; the new version starts next time"), self.updates.auto_install, 1),
        ];
        for (name, tip, on, k) in toggles {
            let r = Rect::new(right.x, y, right.w, 40.0);
            let id = wid("upd-toggle", k, 0);
            if self.switch_row(c, input, id, r, name, on) {
                self.update_op(if k == 0 { UpdateOp::SetAutoCheck(!on) } else { UpdateOp::SetAutoInstall(!on) });
            }
            if self.hovered(input, id, r) {
                *hint = Some(tip.into());
            }
            y += 44.0;
        }
        y += 14.0;
        self.text(c, self.f.sans_med, 11.0, right.x + 2.0, y + 8.0, tr("Channel"), t.muted);
        y += 22.0;
        let w = (right.w - 8.0) * 0.5;
        for (k, ch, tip) in [
            (0, Channel::Nightly, tr("Every change, as soon as it is built")),
            (1, Channel::Stable, tr("Tagged versions only")),
        ] {
            let r = Rect::new(right.x + k as f32 * (w + 8.0), y, w, 32.0);
            let id = wid("upd-chan", k, 0);
            let on = self.updates.channel == ch;
            if self.pill_button(c, input, id, r, if on { Icon::Check } else { Icon::Globe }, channel_name(ch), on) {
                self.update_op(UpdateOp::SetChannel(ch));
            }
            if self.hovered(input, id, r) {
                *hint = Some(tip.into());
            }
        }
        y += 32.0 + 18.0;
        if install_target().is_none() {
            let note = tr("This build runs outside the AppImage: updates open the download page");
            for (i, l) in self.wrap(self.f.sans, 12.0, note, right.w).iter().take(2).enumerate() {
                self.text(c, self.f.sans, 12.0, right.x + 2.0, y + i as f32 * 17.0, l, t.muted);
            }
        }
    }

    /// A row with a name and a switch on the right.
    fn switch_row(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, name: &str, on: bool) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        c.fill_rrect(r, 12.0, t.well.mix(t.raised, 0.5 + 0.5 * hv));
        self.text(c, self.f.sans_med, 13.0, r.x + 14.0, r.y + r.h * 0.5, name, t.text2.mix(t.text, onv.max(hv)));
        let sw = Rect::new(r.right() - 14.0 - 34.0, r.y + r.h * 0.5 - 9.0, 34.0, 18.0);
        c.fill_rrect(sw, 9.0, t.raised_hi.mix(t.accent.alpha(0.45), onv));
        c.fill_circle(sw.x + 9.0 + onv * 16.0, sw.y + 9.0, 6.5, t.muted.mix(t.accent, onv).mix(WHITE, 0.15 * onv));
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }
}

fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::Nightly => tr("Nightly"),
        Channel::Stable => tr("Stable"),
    }
}
