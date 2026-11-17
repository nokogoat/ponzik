//! Plugin list of the browser: what the cache knows at start, then a scan in
//! child processes in the background (`ponzik scan-plugin`), so a broken
//! plugin can neither crash nor freeze Ponzik.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::plugins::{self, ScanReport};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::compat;
use super::widgets::{Icon, wid};

/// Scan results kept between runs.
pub fn cache_path() -> PathBuf {
    compat::config_dir().join("plugins.cache")
}

/// What the cache knows of the installed plugins, without loading any.
pub fn cached() -> ScanReport {
    plugins::cached(&cache_path())
}

/// Background scan in progress.
pub struct PluginScan {
    rx: mpsc::Receiver<ScanReport>,
    /// Asked by the user (rescan): the result is announced.
    forced: bool,
}

impl App {
    /// Scans the plugins in the background; `force` loads every file again
    /// instead of trusting the cache.
    pub fn start_plugin_scan(&mut self, force: bool) {
        if self.plugin_scan.is_some() || std::env::var_os("PONZIK_NO_PLUGIN_SCAN").is_some() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("ponzik-plugin-scan".into()).spawn(move || {
            let scanner = |path: &Path| {
                let mut c = Command::new(&exe);
                c.arg("scan-plugin").arg(path);
                c
            };
            let _ = tx.send(plugins::scan(&scanner, Some(&cache_path()), force, plugins::SCAN_TIMEOUT));
        });
        if spawned.is_ok() {
            self.plugin_scan = Some(PluginScan { rx, forced: force });
            if force {
                self.toast(tr("Scanning the plugins…"));
            }
        }
    }

    pub fn plugin_scan_running(&self) -> bool {
        self.plugin_scan.is_some()
    }

    /// Picks up a finished scan; true when the interface must redraw.
    pub fn plugin_scan_tick(&mut self) -> bool {
        let Some(scan) = &self.plugin_scan else { return false };
        let report = match scan.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.plugin_scan = None;
                return true;
            }
        };
        let forced = scan.forced;
        self.plugin_scan = None;
        let (n, failed) = (report.plugin_count(), report.errors.len());
        let changed = report.scanned > 0;
        self.set_plugins(report);
        if forced || changed {
            let msg = if failed == 0 {
                tr_fmt!("Plugins: {} found", n)
            } else {
                tr_fmt!("Plugins: {} found, {} could not be loaded (see the browser)", n, failed)
            };
            self.toast(&msg);
        }
        true
    }

    /// Plugins of the browser, CLAP first, by name.
    pub fn set_plugins(&mut self, mut report: ScanReport) {
        report.clap.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        report.vst3.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        report.errors.sort_by(|a, b| a.path.cmp(&b.path));
        self.plugins = report.clap;
        self.vst3_plugins = report.vst3;
        self.scan_errors = report.errors;
        // Same counts after a rescan can still hide new plugins: rebuild the list.
        self.browser_cache.0 = None;
    }

    /// Below the plugins of the browser: the files that could not be loaded
    /// (why, in the tooltip) and the rescan button. Returns the next row's `y`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plugin_scan_rows(&mut self, c: &mut Canvas, input: &Input, list: Rect, x: f32, w: f32, mut y: f32) -> f32 {
        let t = self.theme;
        let visible = |r: Rect| r.bottom() > list.y && r.y < list.bottom();
        let errors = self.scan_errors.clone();
        if !errors.is_empty() {
            y += 10.0;
            self.text(c, self.f.sans_med, 11.5, x + 12.0, y + 8.0, tr("Could not be loaded"), t.muted);
            y += 20.0;
            for (k, e) in errors.iter().enumerate() {
                let row = Rect::new(x, y, w, 34.0);
                y += 36.0;
                if !visible(row) {
                    continue;
                }
                let id = wid("br-err", k, 0);
                let hov = self.hovered(input, id, row);
                let hv = self.hover_anim(id, hov);
                if hv > 0.01 {
                    c.fill_rrect(row, 8.0, t.raised.alpha(hv));
                }
                let cy = row.y + row.h * 0.5;
                // A red stub where a plugin's line would be.
                c.fill_rrect(Rect::new(row.x + 12.0, cy - 6.0, 3.0, 12.0), 1.5, t.negative.alpha(0.8));
                let name = e.path.file_stem().map_or_else(|| e.path.display().to_string(), |s| s.to_string_lossy().into_owned());
                let fw = self.fonts.measure(self.f.mono_med, 9.0, e.format.label());
                self.text_right(c, self.f.mono_med, 9.0, row.right() - 12.0, cy, e.format.label(), t.muted);
                self.text_fit(c, self.f.sans, 13.0, row.x + 24.0, cy, &name, row.w - 48.0 - fw, t.muted.mix(t.text2, hv));
                if hov {
                    let tip = tr_fmt!("{}\n{}\nFix it, then rescan the plugins", e.message.as_str(), e.path.display().to_string());
                    self.set_tip(&tip, "");
                }
            }
        }
        y += 6.0;
        let row = Rect::new(x, y, w, 32.0);
        if visible(row) {
            let id = wid("br-rescan", 0, 0);
            let scanning = self.plugin_scan_running();
            let hov = self.hovered(input, id, row) && !scanning && !self.dnd_active();
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(row, 8.0, t.raised.alpha(hv * 0.8));
            c.stroke_rrect(row, 8.0, 1.0, t.hairline.alpha(1.0 + hv));
            let cy = row.y + row.h * 0.5;
            let col = if scanning { t.muted } else { t.text2.mix(t.accent, hv) };
            // While scanning, the icon turns.
            let turn = if scanning { (self.time * 3.0).fract() } else { 0.0 };
            super::widgets::draw_icon(c, if turn < 0.5 { Icon::Reset } else { Icon::Plugin }, row.x + 18.0, cy, 13.0, col);
            let label = if scanning { tr("Scanning the plugins…") } else { tr("Rescan plugins") };
            self.text(c, self.f.sans_med, 12.5, row.x + 34.0, cy, label, col);
            if hov {
                self.set_tip(tr("Loads every plugin again, each in its own process"), "");
                if input.pressed {
                    self.start_plugin_scan(true);
                }
            }
        }
        y + 42.0
    }

    /// Name of a plugin from the scan, for a device kind.
    pub fn plugin_name(&self, kind: &str) -> Option<String> {
        let clap = self.plugins.iter().find(|p| p.kind() == kind).map(|p| p.name.clone());
        clap.or_else(|| self.vst3_plugins.iter().find(|p| p.kind() == kind).map(|p| p.name.clone()))
    }
}
