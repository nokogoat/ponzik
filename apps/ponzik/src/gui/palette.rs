//! Palette « touche-à-tout » (Ctrl+K) : toutes les actions de Ponzik, trouvées
//! en quelques lettres.

use ponzik_engine::mixer::MASTER;
use ponzik_engine::registry;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{Action, App, Popup, Rename, View};
use super::keymap::Shortcut as S;
use super::theme::*;
use super::widgets::wid;

const ROW_H: f32 = 44.0;
const VISIBLE: usize = 9;

pub struct Item {
    pub title: String,
    pub category: &'static str,
    /// Touche actuelle de l'action (table des raccourcis), ou vide.
    pub shortcut: String,
    pub action: Action,
}

/// Minuscules sans accents, pour chercher « reverbe » et trouver « Réverbe ».
pub fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Score de correspondance floue : les lettres de la requête dans l'ordre, avec
/// un bonus pour les suites contiguës et les débuts de mots. `None` si absent.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = normalize(query).chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = normalize(text).chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            score += 10;
            if last == Some(i.wrapping_sub(1)) {
                score += 15;
            }
            if i == 0 || !t[i - 1].is_alphanumeric() {
                score += 20;
            }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score - t.len() as i32 / 4)
}

impl App {
    fn palette_items(&self) -> Vec<Item> {
        let mut v = Vec::new();
        let k = |s: S| self.key_label(s);
        let mut add = |title: String, category: &'static str, shortcut: String, action: Action| {
            v.push(Item { title, category, shortcut, action });
        };
        let none = String::new;
        add(tr("Play / stop").into(), tr("Transport"), k(S::PlayStop), Action::PlayStop);
        add(tr("Take the tour: a beat, a bass, an export").into(), tr("Help"), none(), Action::StartTour);
        add(tr("Switch pattern / song").into(), tr("Transport"), k(S::ToggleMode), Action::ToggleMode);
        add(tr("Metronome").into(), tr("Transport"), k(S::Metronome), Action::ToggleMetronome);
        add(tr("Arm recording").into(), tr("Transport"), k(S::Record), Action::ToggleRecord);
        add(tr("Tap tempo").into(), tr("Transport"), k(S::TapTempo), Action::TapTempo);
        let count_in = if self.count_in { tr("Turn off the count-in") } else { tr("One-bar count-in before recording") };
        add(count_in.into(), tr("Transport"), none(), Action::ToggleCountIn);
        add(tr("Computer typing keyboard").into(), tr("Transport"), k(S::TypingKeyboard), Action::ToggleTyping);
        add(tr("Capture MIDI: the last phrase played becomes a pattern").into(), tr("Transport"), k(S::Capture), Action::CaptureMidi);
        add(tr("Record automation: Touch, Latch, Write").into(), tr("Transport"), none(), Action::Popup(Popup::AutoMode));
        for (title, port) in self.midi_out_palette() {
            add(title, "MIDI", none(), Action::ToggleMidiClock(port));
        }
        for view in self.shown_views() {
            add(tr_fmt!("Go to: {}", view.label()), tr("View"), k(S::for_view(view)), Action::SetView(view));
        }
        add(tr("Show or hide the browser").into(), tr("View"), k(S::ToggleBrowser), Action::ToggleSidebar);
        add(tr("Show or hide the devices").into(), tr("View"), k(S::TogglePanel), Action::TogglePanel);
        add(tr("Keyboard focus: devices and transport (Tab, arrows, Enter)").into(), tr("Accessibility"), "Ctrl Tab".into(), Action::FocusControls);
        add(tr("Collapse all the devices").into(), tr("Devices"), none(), Action::CollapseAll(true));
        add(tr("Expand all the devices").into(), tr("Devices"), none(), Action::CollapseAll(false));
        for d in self.panel_devices() {
            use super::devstrip::DeviceOp;
            let name = self.device_name(d);
            add(tr_fmt!("Go to device: {}", name.as_str()), tr("Devices"), "Ctrl ← →".into(), Action::Device(d, DeviceOp::Show));
            add(tr_fmt!("Collapse or expand: {}", name.as_str()), tr("Devices"), none(), Action::Device(d, DeviceOp::Collapse));
            if let super::devices::Device::Effect { .. } = d {
                add(tr_fmt!("Bypass or enable: {}", name.as_str()), tr("Devices"), none(), Action::Device(d, DeviceOp::Bypass));
                add(tr_fmt!("Detach into a window: {}", name.as_str()), tr("Devices"), none(), Action::Device(d, DeviceOp::Float));
            }
        }
        // Réglages : une vue masquée se retrouve toujours ici.
        add(tr("Settings: workspace").into(), tr("Settings"), k(S::Settings), Action::Popup(Popup::Settings { tab: 0 }));
        add(tr("Settings: keyboard shortcuts").into(), tr("Settings"), none(), Action::Popup(Popup::Settings { tab: 1 }));
        add(tr("Settings: MIDI controller links").into(), tr("Settings"), none(), Action::Popup(Popup::Settings { tab: 2 }));
        add(tr("Settings: about and updates").into(), tr("Settings"), none(), Action::Popup(Popup::Settings { tab: 3 }));
        {
            use super::updates::UpdateOp;
            add(tr("Check for updates").into(), tr("Help"), none(), Action::Update(UpdateOp::Check));
            add(tr("What's new in this version").into(), tr("Help"), none(), Action::Update(UpdateOp::ShowNews));
            add(tr("Full changelog").into(), tr("Help"), none(), Action::Update(UpdateOp::ShowChangelog));
            if self.updates.phase == super::updates::Phase::Available {
                add(tr("Install the update and restart").into(), tr("Help"), none(), Action::Update(UpdateOp::Install));
            }
        }
        for (i, (name, desc, _)) in super::settings::presets().into_iter().enumerate() {
            add(tr_fmt!("Profile: {} · {}", name, desc.to_lowercase()), tr("Settings"), none(), Action::ApplyPreset(i));
        }
        for lang in ponzik_i18n::Lang::all() {
            add(ponzik_i18n::tr_fmt!("Language: {}", lang.name()), tr("Settings"), none(), Action::SetLanguage(if lang == ponzik_i18n::Lang::EN { None } else { Some(lang) }));
        }
        let translate = Action::Popup(Popup::Rename { target: Rename::Translation, text: String::new() });
        add(tr("Create a translation of Ponzik").into(), tr("Settings"), none(), translate);
        add(tr("Reload translations").into(), tr("Settings"), none(), Action::ReloadTranslations);
        add(tr("Open the translations folder").into(), tr("Settings"), none(), Action::OpenTranslationsFolder);
        for view in View::ALL {
            let title = if self.view_shown(view) { tr_fmt!("Hide the {} view", view.label()) } else { tr_fmt!("Show the {} view", view.label()) };
            add(title, tr("Settings"), none(), Action::ToggleViewShown(view));
        }
        add(tr("New pattern").into(), tr("Pattern"), k(S::NewPattern), Action::NewPattern);
        add(tr("Duplicate pattern").into(), tr("Pattern"), none(), Action::DuplicatePattern);
        let cur = self.project.current_pattern;
        add(
            tr("Rename pattern").into(),
            tr("Pattern"),
            none(),
            Action::Popup(Popup::Rename { target: Rename::Pattern(cur), text: self.project.patterns[cur].name.clone() }),
        );
        add(tr("Duplicate time selection").into(), tr("Playlist"), "Ctrl B".into(), Action::DuplicateTime);
        add(tr("Add a section marker at the playhead").into(), tr("Arrangement"), none(), Action::AddMarkerHere);
        for (i, m) in self.project.markers.iter().enumerate() {
            let key = if i < 9 { (i + 1).to_string() } else { String::new() };
            add(tr_fmt!("Go to marker: {}", m.name.as_str()), tr("Arrangement"), key, Action::Marker(i, super::arrange::MarkerOp::Jump));
            add(tr_fmt!("Loop section: {}", m.name.as_str()), tr("Arrangement"), none(), Action::Marker(i, super::arrange::MarkerOp::Loop));
        }
        if let Some(clip) = self.playlist.selected.filter(|&i| i < self.project.clips.len()) {
            add(tr("Make the selected clip unique").into(), tr("Playlist"), none(), Action::MakeUnique(clip));
        }
        let paint = if self.playlist.paint { tr("Paint brush off") } else { tr("Paint brush: drag along a lane to lay copies of a clip") };
        add(paint.into(), tr("Playlist"), "B".into(), Action::TogglePaint);
        if let Some(ch) = self.selected_channel().filter(|&c| self.project.channels[c].instrument.kind == "sampler" && self.project.channels[c].instrument.sample.is_some()) {
            add(tr_fmt!("Fit loop to pattern: {}", self.project.channels[ch].name.as_str()), tr("Pattern"), none(), Action::FitLoop(ch));
        }
        // Loops of the clip shown in the clip panel (or the selected pattern clip).
        let sel = self.clip_panel_shown().or_else(|| self.playlist.selected.filter(|&i| i < self.project.clips.len()).map(super::clip::ClipSel::Pattern));
        if let Some(sel) = sel {
            use super::loops::LoopOp;
            add(tr("Loop the selected clip to the end of the song").into(), tr("Playlist"), none(), Action::Loop(sel, LoopOp::SongEnd));
            add(tr("Loop the selected clip over the time selection").into(), tr("Playlist"), none(), Action::Loop(sel, LoopOp::Selection));
            for n in [2, 4, 8] {
                add(tr_fmt!("Repeat the selected clip ×{}", n), tr("Playlist"), none(), Action::Loop(sel, LoopOp::Repeat(n)));
            }
            add(tr("Duplicate the selected clip").into(), tr("Playlist"), "Ctrl B".into(), Action::Loop(sel, LoopOp::Duplicate));
        }
        // Freeze, bounce, consolidate, render selection.
        {
            use super::clip::ClipSel;
            use super::freeze::FreezeOp;
            use ponzik_project::{BounceSource, FreezeTarget};
            if let Some(ch) = self.selected_channel().filter(|&c| c < self.project.channels.len()) {
                let name = self.project.channels[ch].name.clone();
                let target = FreezeTarget::Channel(ch);
                if self.project.is_frozen(target) {
                    add(tr_fmt!("Unfreeze {}", name.as_str()), tr("Freeze"), none(), Action::Freeze(FreezeOp::Unfreeze(target)));
                } else if !self.project.channel_locked(ch) {
                    add(tr_fmt!("Freeze {}", name.as_str()), tr("Freeze"), none(), Action::Freeze(FreezeOp::Freeze(target)));
                }
                let bounce = |fx| Action::Freeze(FreezeOp::Bounce { source: BounceSource::Channel(ch), fx });
                add(tr_fmt!("Bounce {} in place", name.as_str()), tr("Freeze"), none(), bounce(false));
                add(tr_fmt!("Bounce {} with its effects to a new track", name.as_str()), tr("Freeze"), none(), bounce(true));
            }
            let track = self.focused_track();
            if self.project.can_freeze_track(track) {
                let name = self.project.tracks[track].name.clone();
                let target = FreezeTarget::Track(track);
                if self.project.is_frozen(target) {
                    add(tr_fmt!("Unfreeze the track {}", name.as_str()), tr("Freeze"), none(), Action::Freeze(FreezeOp::Unfreeze(target)));
                } else {
                    add(tr_fmt!("Freeze the track {}", name.as_str()), tr("Freeze"), none(), Action::Freeze(FreezeOp::Freeze(target)));
                }
            }
            let clip = match self.clip_panel_shown() {
                Some(sel) => Some(sel),
                None => self.playlist.selected.filter(|&i| i < self.project.clips.len()).map(ClipSel::Pattern),
            };
            if let Some(sel) = clip {
                add(tr("Consolidate the selected clip").into(), tr("Freeze"), none(), Action::Freeze(FreezeOp::Consolidate(sel)));
                if let ClipSel::Pattern(i) = sel {
                    let source = BounceSource::Clips { patterns: vec![i], audio: Vec::new() };
                    add(tr("Bounce the selected clip to audio").into(), tr("Freeze"), none(), Action::Freeze(FreezeOp::Bounce { source, fx: true }));
                }
            }
            add(tr("Render the time selection to a new audio track").into(), tr("Freeze"), none(), Action::Freeze(FreezeOp::RenderSelection));
        }
        add(tr("Pattern color").into(), tr("Pattern"), none(), Action::Popup(Popup::PatternColor(cur)));
        for (i, p) in self.project.patterns.iter().enumerate() {
            add(tr_fmt!("Pattern: {}", p.name.as_str()), tr("Pattern"), none(), Action::SelectPattern(i));
        }
        for (i, c) in self.project.channels.iter().enumerate() {
            add(tr_fmt!("Channel: {}", c.name.as_str()), tr("Channel"), none(), Action::FocusChannel(i));
        }
        for k in registry::instruments() {
            add(tr_fmt!("Add {}", tr(k.name)), tr("Instrument"), none(), Action::AddInstrument(k.id, None));
        }
        let track = self.focused_track();
        let group = self.group_for(track);
        let title = match group.len() {
            0 => tr("New bus").to_string(),
            1 => tr_fmt!("Send {} to a new bus", self.project.tracks[track].name.as_str()),
            n => tr_fmt!("Group {} tracks into a bus", n),
        };
        add(title, tr("Mixer"), "Ctrl G".into(), Action::NewBus(group.clone()));
        for (kind, name, _) in ponzik_project::RETURN_EFFECTS {
            let from = (track != MASTER).then_some(track);
            add(tr_fmt!("New send bus: {}", tr(name)), tr("Mixer"), String::new(), Action::NewReturn(kind, from));
        }
        add(tr("New VCA").into(), tr("Mixer"), String::new(), Action::NewVca(group));
        if self.project.tracks[track].kind == ponzik_project::TrackKind::Bus {
            add(tr_fmt!("Fold or unfold {}", self.project.tracks[track].name.as_str()), tr("Mixer"), String::new(), Action::ToggleFold(track));
        }
        for k in registry::effects() {
            add(tr_fmt!("{} on {}", tr(k.name), self.project.tracks[track].name.as_str()), tr("Effect"), none(), Action::AddEffect(track, k.id));
        }
        // Plugins: scan, their own windows, the parameter last moved in one.
        add(tr("Rescan plugins").into(), tr("Plugins"), none(), Action::RescanPlugins);
        for d in self.panel_devices() {
            if super::app::plugin_badge(&self.device_kind(d).0).is_some() {
                let name = self.editor_title(d);
                let title = if self.editors.is_open(d) {
                    tr_fmt!("Close the plugin window: {}", name)
                } else {
                    tr_fmt!("Open the plugin window: {}", name)
                };
                add(title, tr("Plugins"), none(), Action::ToggleEditor(d));
            }
        }
        if let Some((target, value)) = self.last_plugin_param()
            && let Some((name, _)) = self.target_info(target)
        {
            add(tr_fmt!("Automate {}", name), tr("Plugins"), none(), Action::Automate(target, value));
        }
        if let Some(ch) = self.selected_channel()
            && let Some(slot) = (0..ponzik_engine::NOTE_FX_SLOTS).find(|&s| self.project.channels[ch].note_fx.get(s).is_none_or(Option::is_none))
        {
            for k in registry::note_fx() {
                let title = tr_fmt!("{} on {}", tr(k.name), self.project.channels[ch].name.as_str());
                add(title, tr("Note effect"), none(), Action::AddNoteFx(ch, slot, k.id));
            }
        }
        add(tr("Save").into(), tr("File"), k(S::Save), Action::Save);
        add(tr("Open a project").into(), tr("File"), k(S::Open), Action::OpenMenu);
        for path in super::compat::list_projects().into_iter().take(6) {
            let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            add(tr_fmt!("Open: {}", name), tr("Recent"), none(), Action::Open(path));
        }
        add(tr("New project").into(), tr("File"), k(S::NewProject), Action::NewProject);
        for (i, (name, _)) in ponzik_project::templates().into_iter().enumerate() {
            add(tr_fmt!("New project: {}", tr(name)), tr("Template"), none(), Action::NewFromTemplate(i));
        }
        add(tr("Export to 24-bit WAV").into(), tr("File"), k(S::Export), Action::Export);
        {
            use ponzik_project::{BitDepth, ExportOptions, Normalize};
            let wav = ExportOptions::default();
            let exports = [
                (tr("Export to 16-bit WAV (TPDF dither)"), ExportOptions { depth: BitDepth::Pcm16, ..wav }),
                (tr("Export to 32-bit float WAV"), ExportOptions { depth: BitDepth::Float32, ..wav }),
                (tr("Export FLAC (lossless, 24-bit)"), ExportOptions::flac()),
                (tr("Export FLAC in 16 bits (TPDF dither)"), ExportOptions { depth: BitDepth::Pcm16, ..ExportOptions::flac() }),
                (
                    tr("Export FLAC for streaming (−14 LUFS, −1 dBTP)"),
                    ExportOptions { normalize: Normalize::Loudness { lufs: -14.0, ceiling: -1.0 }, ..ExportOptions::flac() },
                ),
                (tr("Export normalized (peak at −0.3 dBFS)"), ExportOptions { normalize: Normalize::Peak(-0.3), ..wav }),
                (
                    tr("Export for streaming (−14 LUFS, −1 dBTP)"),
                    ExportOptions { normalize: Normalize::Loudness { lufs: -14.0, ceiling: -1.0 }, ..wav },
                ),
                (
                    tr("Export loud for the club (−9 LUFS, −1 dBTP)"),
                    ExportOptions { normalize: Normalize::Loudness { lufs: -9.0, ceiling: -1.0 }, ..wav },
                ),
            ];
            for (title, options) in exports {
                add(title.into(), tr("File"), none(), Action::ExportWith(options));
            }
        }
        add(tr("Export separate tracks (stems)").into(), tr("File"), none(), Action::ExportStems(ponzik_project::FileFormat::Wav));
        add(tr("Export separate tracks (stems) as FLAC").into(), tr("File"), none(), Action::ExportStems(ponzik_project::FileFormat::Flac));
        add(tr("Export each section as its own WAV (render by regions)").into(), tr("File"), none(), Action::ExportSections(Default::default()));
        add(
            tr("Export each section as its own FLAC (render by regions)").into(),
            tr("File"),
            none(),
            Action::ExportSections(ponzik_project::ExportOptions::flac()),
        );
        add(tr("Import a MIDI file").into(), tr("File"), none(), Action::ImportMidiMenu);
        add(tr("Import an FL Studio project (.flp)").into(), tr("File"), none(), Action::ImportFlpMenu);
        add(tr("Show the FL Studio import report").into(), tr("File"), none(), Action::Popup(Popup::ImportReport));
        add(
            tr("Locate missing files (relink offline samples and audio)").into(),
            tr("File"),
            none(),
            Action::Popup(Popup::LocateMissing(self.locate_start_dir())),
        );
        add(tr("Export the pattern to MIDI").into(), tr("File"), none(), Action::ExportMidi);
        add(tr("Save as").into(), tr("File"), k(S::SaveAs), Action::SaveAs);
        add(
            tr("Rename project").into(),
            tr("File"),
            none(),
            Action::Popup(Popup::Rename { target: Rename::Project, text: self.project.name.clone() }),
        );
        for (name, ticks, vel) in super::piano::HUMANIZE {
            add(tr_fmt!("Humanize notes: {}", tr(name).to_lowercase()), tr("Piano roll"), none(), Action::Humanize(ticks, vel));
        }
        for t in super::tools::Tool::TRANSFORMS {
            add(tr_fmt!("Transform notes: {}", tr(t.name()).to_lowercase()), tr("Piano roll"), none(), Action::OpenTool(t));
        }
        for t in super::tools::Tool::GENERATORS {
            add(tr_fmt!("Generate: {}", tr(t.name()).to_lowercase()), tr("Piano roll"), none(), Action::OpenTool(t));
        }
        add(tr("Undo").into(), tr("Edit"), k(S::Undo), Action::Undo);
        add(tr("Redo").into(), tr("Edit"), k(S::Redo), Action::Redo);
        add(tr("History").into(), tr("Edit"), k(S::History), Action::Popup(Popup::History));
        add(tr("Demo: house groove").into(), tr("Demo"), none(), Action::LoadDemo(0));
        add(tr("Demo: Amen rave (breakcore)").into(), tr("Demo"), none(), Action::LoadDemo(1));
        add(tr("Demo: Zalgo rave (rage × techno)").into(), tr("Demo"), none(), Action::LoadDemo(2));
        for i in 0..self.themes.entries.len() {
            add(tr_fmt!("Theme: {}", self.theme_label(i)), tr("Theme"), none(), Action::SetTheme(i));
        }
        v
    }

    /// Dessine la palette et gère ses touches ; renvoie l'action choisie et la
    /// palette mise à jour (ou `None` si elle se ferme).
    pub(super) fn palette(&mut self, c: &mut Canvas, input: &Input, screen: Rect, mut query: String, mut selected: usize) -> (Option<Action>, Option<Popup>) {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        query.push_str(&input.text);

        let all = self.palette_items();
        let mut found: Vec<(i32, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy_score(&query, &format!("{} {}", it.title, it.category)).map(|s| (s, i)))
            .collect();
        if !query.trim().is_empty() {
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        let count = found.len();

        let mut run = None;
        let mut close = false;
        for &(key, _) in &input.keys {
            match key {
                Key::Escape => close = true,
                Key::Backspace => {
                    query.pop();
                    selected = 0;
                }
                Key::Down => selected = (selected + 1).min(count.saturating_sub(1)),
                Key::Up => selected = selected.saturating_sub(1),
                Key::PageDown => selected = (selected + VISIBLE).min(count.saturating_sub(1)),
                Key::PageUp => selected = selected.saturating_sub(VISIBLE),
                Key::Enter => {
                    if let Some(&(_, i)) = found.get(selected) {
                        run = Some(all[i].action.clone());
                    }
                }
                _ => {}
            }
        }
        if !input.text.is_empty() {
            selected = 0;
        }
        selected = selected.min(count.saturating_sub(1));

        // Fond assombri, puis la palette en haut de l'écran.
        c.fill_rect(screen, t.shade(120.0 * open));
        let w = 620.0f32.min(screen.w - 48.0);
        let rows = count.clamp(1, VISIBLE);
        let r = Rect::new((screen.w - w) * 0.5, screen.h * 0.14 + (1.0 - open) * 10.0, w, 64.0 + rows as f32 * ROW_H + 12.0);
        c.fill_rrect(r.translate(0.0, 8.0).inset(-4.0), 20.0, t.shade(80.0 * open));
        c.fill_rrect(r, 18.0, t.panel);
        c.stroke_rrect(r, 18.0, 1.0, t.hairline.alpha(1.8));

        let field = Rect::new(r.x + 12.0, r.y + 12.0, r.w - 24.0, 44.0);
        c.fill_rrect(field, 12.0, t.well);
        // Loupe.
        c.stroke_circle(field.x + 22.0, field.y + 20.0, 6.5, 1.6, t.muted);
        c.stroke_polyline(&[(field.x + 27.0, field.y + 25.0), (field.x + 31.0, field.y + 29.0)], 1.6, t.muted);
        let shown = if query.is_empty() { tr("Search for an action, an instrument, an effect…").to_string() } else { query.clone() };
        let ink = if query.is_empty() { t.muted } else { t.text };
        let tw = self.text(c, self.f.sans, 15.0, field.x + 42.0, field.y + 22.0, &shown, ink);
        if (self.time * 2.0).fract() < 0.6 {
            let cx = if query.is_empty() { field.x + 42.0 } else { field.x + 43.0 + tw };
            c.fill_rect(Rect::new(cx, field.y + 12.0, 1.5, 20.0), t.select);
        }
        self.anim.set(wid("caret", 0, 0), self.time);
        self.text_right(c, self.f.mono, 11.0, field.right() - 12.0, field.y + 22.0, &format!("{count}"), t.muted);

        // Résultats, avec défilement qui suit la sélection.
        let first = selected.saturating_sub(VISIBLE - 1).min(count.saturating_sub(VISIBLE));
        let list = Rect::new(r.x + 6.0, field.bottom() + 8.0, r.w - 12.0, rows as f32 * ROW_H);
        if count == 0 {
            self.text(c, self.f.sans, 13.0, list.x + 16.0, list.y + ROW_H * 0.5, tr("Nothing found"), t.muted);
        }
        for (k, &(_, i)) in found.iter().enumerate().skip(first).take(VISIBLE) {
            let row = Rect::new(list.x, list.y + (k - first) as f32 * ROW_H, list.w, ROW_H - 4.0);
            let hov = input.inside && row.contains(input.x, input.y);
            if hov && input.pressed {
                run = Some(all[i].action.clone());
            }
            let sel = k == selected;
            let sv = self.anim.to(wid("pal-sel", k, 0), sel as u8 as f32, T_PRESS);
            if sv > 0.01 || hov {
                c.fill_rrect(row, 10.0, t.raised.mix(t.raised_hi, 0.4).alpha(sv.max(hov as u8 as f32 * 0.5)));
            }
            if sel {
                c.fill_rrect(Rect::new(row.x + 4.0, row.y + 12.0, 3.0, row.h - 24.0), 1.5, t.accent);
            }
            let item = &all[i];
            let right = row.right() - 12.0;
            let mut x = right;
            if !item.shortcut.is_empty() {
                let kw = self.fonts.measure(self.f.mono, 11.0, &item.shortcut) + 14.0;
                let cap = Rect::new(x - kw, row.y + 10.0, kw, 20.0);
                c.fill_rrect(cap, 5.0, t.raised_hi);
                c.fill_rect(Rect::new(cap.x + 3.0, cap.bottom() - 1.0, cap.w - 6.0, 1.0), t.shade(90.0));
                self.text_center(c, self.f.mono, 11.0, cap.x + cap.w * 0.5, cap.y + 10.0, &item.shortcut, t.text2);
                x = cap.x - 12.0;
            }
            let cat_w = self.fonts.measure(self.f.sans, 11.0, item.category);
            self.text(c, self.f.sans, 11.0, x - cat_w, row.y + 20.0, item.category, t.muted);
            let title = item.title.clone();
            self.text_fit(c, self.f.sans_med, 14.0, row.x + 18.0, row.y + 20.0, &title, x - cat_w - row.x - 34.0, t.text);
        }
        if input.pressed && !r.contains(input.x, input.y) {
            close = true;
        }
        if run.is_some() || close {
            (run, None)
        } else {
            (None, Some(Popup::Palette { query, selected }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching() {
        assert!(fuzzy_score("rev", "Réverbe").is_some());
        assert!(fuzzy_score("reverbe", "Ajouter Réverbe").is_some());
        assert!(fuzzy_score("xyz", "Réverbe").is_none());
        let start = fuzzy_score("pl", "Aller à : Playlist").unwrap();
        let scattered = fuzzy_score("pl", "Aller à : Piano roll").unwrap();
        assert!(start > scattered, "{start} > {scattered}");
        assert_eq!(normalize("Ça Été"), "ca ete");
    }
}
