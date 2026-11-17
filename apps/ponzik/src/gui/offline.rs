//! Offline files (missing, or in a format not readable yet): what shows them,
//! the "Locate missing files" dialog and the FL Studio import report.

use std::path::{Path, PathBuf};

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::ReportKind;
use ponzik_project::flp::{file_name_of, wrap};
use ponzik_ui::Rect;

use super::app::{Action, App, MenuItem, Popup};
use super::compat;
use super::theme::TOP_BAR;
use super::widgets::Icon;

/// Where the import report and the locate dialog open: under the top bar,
/// on the left, so that folding a category or opening a folder keeps them in place.
pub(super) const REPORT_ANCHOR: Rect = Rect::new(12.0, TOP_BAR, 0.0, 0.0);
/// Characters per row of a report entry.
const REPORT_WIDTH: usize = 46;
/// Offline files listed by name in the locate dialog (the others are counted).
const LISTED: usize = 6;
/// Sub-folders listed in the locate dialog.
const MAX_FOLDERS: usize = 60;

impl App {
    /// True when a file of the project did not load: it plays silence.
    pub(super) fn is_offline(&self, path: &str) -> bool {
        !self.samples.contains_key(path)
    }

    /// The offline files of the project.
    pub(super) fn offline_files(&self) -> Vec<String> {
        self.project.offline_paths(&|p| self.samples.contains_key(p))
    }

    /// The sample of a channel's instrument that is offline, if any.
    pub(super) fn channel_missing_sample(&self, ch: usize) -> Option<String> {
        let c = self.project.channels.get(ch)?;
        c.instrument.sample_paths().find(|p| self.is_offline(p)).cloned()
    }

    /// The folder the locate dialog starts in: the project's, the imported
    /// FL Studio project's, else the home folder.
    pub(super) fn locate_start_dir(&self) -> PathBuf {
        self.project_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.import_dir.clone())
            .filter(|d| d.is_dir())
            .unwrap_or_else(compat::home)
    }

    /// Rows of the import report: one per category (click: fold or unfold),
    /// its entries under it when unfolded.
    pub(super) fn report_items(&self) -> Vec<MenuItem> {
        let Some(report) = &self.import_report else {
            return vec![MenuItem::plain(tr("No FL Studio import yet"), Action::None)];
        };
        let mut v = Vec::new();
        for (i, g) in report.groups().into_iter().enumerate() {
            let open = self.report_open.contains(&g.kind);
            let icon = match g.kind {
                ReportKind::Mapped => Icon::Check,
                ReportKind::Approximated => Icon::Swap,
                ReportKind::Offline => Icon::Missing,
                ReportKind::Skipped => Icon::Close,
            };
            let fold = if open { tr("Hide") } else { tr("Show") };
            let head = MenuItem::new(icon, &g.title, Action::ToggleReportGroup(g.kind)).detail(fold.into());
            v.push(if i > 0 { head.sep() } else { head });
            if g.kind == ReportKind::Offline && !self.offline_files().is_empty() {
                let locate = Action::Popup(Popup::LocateMissing(self.locate_start_dir()));
                v.push(
                    MenuItem::new(Icon::Search, tr("Locate missing files…"), locate)
                        .tip(tr("Search a folder for them by name, relink and save")),
                );
            }
            if open {
                for entry in g.entries {
                    for (k, row) in wrap(entry, REPORT_WIDTH).into_iter().enumerate() {
                        // A bullet and a little room above each entry; its other rows indented.
                        let (text, h) = if k == 0 { (format!("•  {row}"), 28.0) } else { (format!("    {row}"), 20.0) };
                        v.push(MenuItem::line(&text, h));
                    }
                }
            }
        }
        v
    }

    /// Rows of the locate dialog: the offline files, "Search here", then the
    /// folders to move to (parent, shortcuts, sub-folders).
    pub(super) fn locate_items(&self, dir: &Path) -> Vec<MenuItem> {
        let missing = self.offline_files();
        if missing.is_empty() {
            return vec![MenuItem::new(Icon::Check, tr("No offline files: every file is found"), Action::None)];
        }
        let mut v = vec![MenuItem::new(Icon::Missing, &tr_fmt!("{} files missing", missing.len()), Action::None)];
        for path in missing.iter().take(LISTED) {
            v.push(MenuItem::line(file_name_of(path), 24.0).dim(true).tip(path));
        }
        if missing.len() > LISTED {
            v.push(MenuItem::line(&tr_fmt!("… and {} more", missing.len() - LISTED), 24.0).dim(true));
        }
        let shown = dir.display().to_string();
        let name = dir.file_name().map_or_else(|| shown.clone(), |n| n.to_string_lossy().into_owned());
        v.push(
            MenuItem::new(Icon::Search, &tr_fmt!("Search in {}", name), Action::LocateIn(dir.to_path_buf()))
                .on(true)
                .tip(&tr_fmt!("{} and its sub-folders, by file name: every file found is relinked, then the project is saved", shown))
                .sep(),
        );
        let goto = |d: &Path| Action::Popup(Popup::LocateMissing(d.to_path_buf()));
        let folder_name = |d: &Path| d.file_name().map_or_else(|| d.display().to_string(), |n| n.to_string_lossy().into_owned());
        if let Some(parent) = dir.parent() {
            v.push(MenuItem::new(Icon::ChevronLeft, tr("Parent folder"), goto(parent)).detail(folder_name(parent)).tip(&parent.display().to_string()));
        }
        // Shortcuts: the imported project's folder and the usual places.
        let h = compat::home();
        let mut places: Vec<PathBuf> = self.import_dir.iter().cloned().collect();
        places.extend([h.clone(), compat::music_dir(), h.join("Téléchargements"), h.join("Downloads"), compat::library_dir()]);
        let mut seen = vec![dir.to_path_buf()];
        for (i, p) in places.into_iter().filter(|p| p.is_dir()).enumerate() {
            if seen.contains(&p) {
                continue;
            }
            let it = MenuItem::new(Icon::Folder, &folder_name(&p), goto(&p)).detail(tr("Place").into()).tip(&p.display().to_string());
            v.push(if i == 0 { it.sep() } else { it });
            seen.push(p);
        }
        // Sub-folders, by name.
        let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        subs.retain(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')));
        subs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        for (i, p) in subs.iter().take(MAX_FOLDERS).enumerate() {
            let it = MenuItem::new(Icon::Folder, &folder_name(p), goto(p)).tip(&p.display().to_string());
            v.push(if i == 0 { it.sep() } else { it });
        }
        v
    }

    /// Searches `dir` for the offline files, relinks those found, loads them
    /// and saves the project.
    pub(super) fn locate_in(&mut self, dir: &Path) {
        let missing = self.offline_files();
        let found = ponzik_project::media::find_files(dir, &missing, &ponzik_engine::wav::is_audio_file);
        if found.is_empty() {
            let again = Action::Popup(Popup::LocateMissing(dir.to_path_buf()));
            self.toast_action(&tr_fmt!("None of the {} missing files is in {}", missing.len(), dir.display().to_string()), tr("Another folder…"), again);
            return;
        }
        for path in found.values() {
            self.try_load_sample(path);
        }
        self.project.relink(&found);
        self.commit(tr("Locate missing files"));
        self.save();
        let left = self.offline_files().len();
        if left == 0 {
            self.toast(&tr_fmt!("{} files relinked: nothing is missing any more", found.len()));
        } else {
            let more = Action::Popup(Popup::LocateMissing(dir.to_path_buf()));
            self.toast_action(&tr_fmt!("{} files relinked, {} still missing", found.len(), left), tr("Locate…"), more);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_project::{AudioClipDef, InstrumentDef};

    #[test]
    fn offline_files_are_kept_then_located() {
        let root = std::env::temp_dir().join(format!("ponzik-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("found/Audio")).unwrap();
        let mut project = ponzik_project::Project::empty();
        let mut sampler = InstrumentDef::new("sampler");
        sampler.sample = Some("G:\\sons\\Kick.wav".into());
        project.add_channel("Kick", sampler, 60);
        project.audio_clips.push(AudioClipDef { path: "G:\\sons\\Audio\\Take 1.wav".into(), len: 384, ..Default::default() });
        project.audio_clips.push(AudioClipDef { path: "G:\\sons\\Gone.wav".into(), start: 384, len: 96, ..Default::default() });
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, ponzik_project::Project::empty(), 48_000);
        app.replace_project(project, Some(root.join("song.ponzik")));
        // Nothing is dropped: every file stays, offline.
        assert_eq!(app.project.audio_clips.len(), 2);
        assert_eq!(app.offline_files().len(), 3);
        assert_eq!(app.channel_missing_sample(0).as_deref(), Some("G:\\sons\\Kick.wav"));
        let names: Vec<String> = app.locate_items(&root).iter().map(|i| i.name.clone()).collect();
        assert!(names.contains(&"3 files missing".to_string()), "{names:?}");
        assert!(names.contains(&"Kick.wav".to_string()) && names.contains(&"found".to_string()), "{names:?}");

        // Two of them are found under another folder: relinked, loaded and saved.
        let wav = [0.0f32, 0.5, -0.5, 0.0];
        ponzik_project::write_wav(root.join("found/kick.wav"), &wav, 48_000, ponzik_project::BitDepth::Pcm16).unwrap();
        ponzik_project::write_wav(root.join("found/Audio/Take 1.wav"), &wav, 48_000, ponzik_project::BitDepth::Pcm16).unwrap();
        app.locate_in(&root);
        assert_eq!(app.offline_files(), ["G:\\sons\\Gone.wav"]);
        assert_eq!(app.channel_missing_sample(0), None);
        let take = root.join("found/Audio/Take 1.wav").to_string_lossy().into_owned();
        assert_eq!(app.project.audio_clips[0].path, take);
        let saved = ponzik_project::load_from_file(root.join("song.ponzik")).unwrap();
        assert_eq!(saved.audio_clips[0].path, take);
        assert_eq!(saved.audio_clips[1].path, "G:\\sons\\Gone.wav", "still offline, still kept");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
