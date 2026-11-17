//! Freeze, bounce in place, consolidate and render selection, in the
//! interface: the menu entries' work, the frost on frozen devices and the
//! prompt that offers to unfreeze before an edit of frozen content.

use std::path::PathBuf;
use std::sync::Arc;

use ponzik_engine::TICKS_PER_BAR;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::bounce::{render_bounce, render_freeze, render_sample};
use ponzik_project::{BounceSource, FreezeTarget, SourceAction};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, Focus, MenuItem, Popup, View};
use super::clip::ClipSel;
use super::compat;
use super::devices::Device;
use super::widgets::{Icon, draw_icon, wid};

/// Icy tint of frozen things.
pub const FROST: u32 = 0x8ee7f2;

/// Freeze and bounce operations, from menus and the palette.
#[derive(Clone, Debug)]
pub enum FreezeOp {
    Freeze(FreezeTarget),
    Unfreeze(FreezeTarget),
    /// Thaws, then applies the edit that was held back.
    UnfreezeAndApply,
    /// Stays frozen: the edit is dropped.
    KeepFrozen,
    /// Renders a source to audio on a new lane and mutes it; `fx`: through the mixer.
    Bounce { source: BounceSource, fx: bool },
    /// The clip, or every clip of its lane in the time selection, becomes one audio clip.
    Consolidate(ClipSel),
    /// The time selection, as heard, to a new audio track; what played there is muted.
    RenderSelection,
    /// Mutes or unmutes a pattern clip (or an audio clip).
    ToggleMute(ClipSel),
}

impl App {
    pub(super) fn run_freeze_op(&mut self, op: FreezeOp) {
        match op {
            FreezeOp::Freeze(target) => self.freeze(target),
            FreezeOp::Unfreeze(target) => self.unfreeze(target),
            FreezeOp::UnfreezeAndApply => {
                if let Some((target, edited)) = self.frozen_edit.take() {
                    self.project = edited;
                    self.project.set_frozen(target, None);
                    self.commit(tr("Unfreeze"));
                    self.toast(&tr_fmt!("{} unfrozen: its devices play again", self.project.target_name(target)));
                }
            }
            FreezeOp::KeepFrozen => self.frozen_edit = None,
            FreezeOp::Bounce { source, fx } => self.bounce(source, fx),
            FreezeOp::Consolidate(sel) => self.consolidate(sel),
            FreezeOp::RenderSelection => self.render_selection(),
            FreezeOp::ToggleMute(sel) => {
                match sel {
                    ClipSel::Pattern(i) => {
                        if let Some(c) = self.project.clips.get_mut(i) {
                            c.muted = !c.muted;
                        }
                    }
                    ClipSel::Audio(i) => {
                        if let Some(c) = self.project.audio_clips.get_mut(i) {
                            c.muted = !c.muted;
                        }
                    }
                }
                self.commit(tr("Mute a clip"));
            }
        }
    }

    /// Renders the target and plays the render instead of its devices.
    pub fn freeze(&mut self, target: FreezeTarget) {
        if self.project.is_frozen(target) {
            return;
        }
        if let FreezeTarget::Track(t) = target
            && !self.project.can_freeze_track(t)
        {
            self.toast(tr("The master and VCAs cannot be frozen"));
            return;
        }
        // The render builds its own instances: they start from the states of now.
        self.capture_plugin_states();
        let audio = {
            let loader = self.sample_loader();
            render_freeze(&self.project, target, self.sample_rate, &loader)
        };
        let name = self.project.target_name(target);
        match self.write_audio(&tr_fmt!("{} freeze", name), &audio) {
            Ok(path) => {
                self.project.set_frozen(target, Some(path));
                self.commit(tr("Freeze"));
                let secs = audio.len() as f32 / 2.0 / self.sample_rate as f32;
                self.toast_action(&tr_fmt!("{} frozen ({:.1} s): its devices are unloaded", name, secs), tr("Unfreeze"), Action::Freeze(FreezeOp::Unfreeze(target)));
            }
            Err(e) => self.toast(&e),
        }
    }

    pub fn unfreeze(&mut self, target: FreezeTarget) {
        if self.project.set_frozen(target, None).is_some() {
            self.commit(tr("Unfreeze"));
            self.toast(&tr_fmt!("{} unfrozen: its devices play again", self.project.target_name(target)));
        }
    }

    /// Writes a render next to the project ("Audio/<project>") and keeps it
    /// in memory; returns its path.
    fn write_audio(&mut self, name: &str, audio: &[f32]) -> Result<String, String> {
        let dir = if self.demo_audio {
            std::env::temp_dir().join("ponzik-audio")
        } else {
            let folder = self.project_path.as_ref().and_then(|p| p.parent().map(PathBuf::from));
            folder.unwrap_or_else(compat::projects_dir).join("Audio").join(compat::file_name(&self.project.name))
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let base = compat::file_name(name);
        let path = (1..)
            .map(|k| dir.join(if k == 1 { format!("{base}.wav") } else { format!("{base} {k}.wav") }))
            .find(|p| self.demo_audio || !p.exists())
            .unwrap_or_else(|| dir.join(format!("{base}.wav")));
        ponzik_project::render::write_render(&path, audio, self.sample_rate).map_err(|e| format!("{}: {e}", path.display()))?;
        let key = path.to_string_lossy().into_owned();
        self.samples.insert(key.clone(), Arc::new(render_sample(name, audio, self.sample_rate)));
        self.waveforms.remove(&key);
        Ok(key)
    }

    /// Renders a source and places the result; `None` when nothing was placed.
    fn place(&mut self, source: BounceSource, fx: bool, action: SourceAction, whole_file: bool, consolidate: bool) -> Option<usize> {
        self.capture_plugin_states();
        let render = {
            let loader = self.sample_loader();
            render_bounce(&self.project, &source, fx, self.sample_rate, &loader)
        };
        let name = self.project.bounce_name(&source);
        let label = if consolidate { tr_fmt!("{} consolidated", name) } else { tr_fmt!("{} bounce", name) };
        let path = match self.write_audio(&label, &render.audio) {
            Ok(p) => p,
            Err(e) => {
                self.toast(&e);
                return None;
            }
        };
        let secs = render.audio.len() as f64 / 2.0 / self.sample_rate as f64;
        let range = (render.start, render.end);
        let Some(clip) = self.project.place_bounce(&source, render.track, range, secs, path, action, whole_file, consolidate) else {
            self.toast(tr("No free mixer track for the bounce"));
            return None;
        };
        self.playlist.selected = None;
        self.select_clip(ClipSel::Audio(clip));
        Some(clip)
    }

    /// Bounce in place: the source becomes an audio clip on a new lane (its
    /// tail included) and is muted.
    pub fn bounce(&mut self, source: BounceSource, fx: bool) {
        let name = self.project.bounce_name(&source);
        if self.place(source, fx, SourceAction::Mute, true, false).is_some() {
            self.commit(tr("Bounce in place"));
            let how = if fx { tr("with its effects, on a new track") } else { tr("on its track") };
            self.toast_action(&tr_fmt!("{} bounced to audio {} · the source is muted", name, how), tr("Undo"), Action::Undo);
        }
    }

    /// Consolidate: the clip — or, with a time selection over it, every clip
    /// of its lane inside the selection (cut at its edges) — becomes one audio
    /// clip in its place.
    pub fn consolidate(&mut self, sel: ClipSel) {
        let lane = match sel {
            ClipSel::Pattern(i) => self.project.clips.get(i).map(|c| c.lane),
            ClipSel::Audio(i) => self.project.audio_clips.get(i).map(|c| c.lane),
        };
        let Some(lane) = lane else { return };
        let range = match self.time_selection {
            Some((View::Playlist, a, b)) if b > a => Some((a, b)),
            _ => None,
        };
        let source = match range {
            Some((a, b)) => {
                self.project.split_time(a);
                self.project.split_time(b);
                let inside = |start: u32, len: u32, l: usize| l == lane && start >= a && start + len <= b;
                let patterns = (0..self.project.clips.len()).filter(|&i| {
                    let c = &self.project.clips[i];
                    inside(c.start, c.len, c.lane) && !c.muted
                });
                let audio = (0..self.project.audio_clips.len()).filter(|&i| {
                    let c = &self.project.audio_clips[i];
                    inside(c.start, c.len, c.lane) && !c.muted
                });
                BounceSource::Clips { patterns: patterns.collect(), audio: audio.collect() }
            }
            None => match sel {
                ClipSel::Pattern(i) => BounceSource::Clips { patterns: vec![i], audio: Vec::new() },
                ClipSel::Audio(i) => BounceSource::Clips { patterns: Vec::new(), audio: vec![i] },
            },
        };
        let BounceSource::Clips { patterns, audio } = &source else { return };
        if patterns.is_empty() && audio.is_empty() {
            self.toast(tr("Nothing to consolidate on this lane"));
            return;
        }
        let count = patterns.len() + audio.len();
        // Patterns play through the mixer as heard; audio clips of one track merge on it, dry.
        let fx = !patterns.is_empty();
        if self.place(source, fx, SourceAction::Replace, false, true).is_some() {
            self.commit(tr("Consolidate"));
            let text = ponzik_i18n::tr_fmt_n!("{} clip consolidated into one audio clip", "{} clips consolidated into one audio clip", count, count);
            self.toast_action(&text, tr("Undo"), Action::Undo);
        }
    }

    /// The time selection, as heard, to a new audio track; the clips there are muted.
    pub fn render_selection(&mut self) {
        let Some((View::Playlist, a, b)) = self.time_selection.filter(|s| s.2 > s.1) else {
            self.toast(tr("Select a time range in the playlist ruler first"));
            return;
        };
        if self.place(BounceSource::Range(a, b), true, SourceAction::Mute, false, false).is_some() {
            self.commit(tr("Render the selection"));
            self.toast_action(tr("Selection rendered to a new audio track · what played there is muted"), tr("Undo"), Action::Undo);
        }
    }

    /// Called by every commit: an edit of frozen content is held back and
    /// the user is asked to unfreeze first. True when the edit was held.
    pub(super) fn hold_frozen_edit(&mut self) -> bool {
        let Some(target) = ponzik_project::frozen_content_changed(&self.synced, &self.project) else { return false };
        let edited = std::mem::replace(&mut self.project, self.synced.clone());
        self.frozen_edit = Some((target, edited));
        // The gesture stops here; the prompt opens once.
        self.drag = None;
        if !matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::FrozenEdit(_))) {
            let (x, y) = self.pointer;
            self.open_popup(Popup::FrozenEdit(target), Rect::new(x, y, 0.0, 0.0));
        }
        true
    }

    /// The prompt shown when frozen content is edited.
    pub(super) fn frozen_edit_menu(&self, target: FreezeTarget) -> (String, Vec<MenuItem>) {
        let name = self.project.target_name(target);
        (
            tr_fmt!("{} is frozen", name),
            vec![
                MenuItem::new(Icon::Snowflake, tr("Unfreeze and apply the edit"), Action::Freeze(FreezeOp::UnfreezeAndApply))
                    .tip(tr("Its devices are loaded again and play live")),
                MenuItem::new(Icon::Close, tr("Keep it frozen"), Action::Freeze(FreezeOp::KeepFrozen)).tip(tr("The edit is dropped")),
            ],
        )
    }

    /// Freeze entries of a channel's menu.
    pub(super) fn channel_freeze_items(&self, ch: usize) -> Vec<MenuItem> {
        let target = FreezeTarget::Channel(ch);
        let freeze = if self.project.is_frozen(target) {
            MenuItem::new(Icon::Snowflake, tr("Unfreeze"), Action::Freeze(FreezeOp::Unfreeze(target))).tip(tr("Loads its instrument and note effects again"))
        } else if self.project.channel_locked(ch) {
            let t = self.project.channels[ch].track;
            MenuItem::new(Icon::Snowflake, tr("Unfreeze its track"), Action::Freeze(FreezeOp::Unfreeze(FreezeTarget::Track(t))))
                .tip(tr("Frozen with its mixer track"))
        } else {
            MenuItem::new(Icon::Snowflake, tr("Freeze"), Action::Freeze(FreezeOp::Freeze(target)))
                .tip(tr("Renders it to audio and unloads its instrument and note effects to save CPU"))
        };
        vec![
            freeze.sep(),
            MenuItem::new(Icon::Export, tr("Bounce in place"), Action::Freeze(FreezeOp::Bounce { source: BounceSource::Channel(ch), fx: false }))
                .tip(tr("An audio clip of the whole song on its mixer track; the channel is muted")),
            MenuItem::new(Icon::Export, tr("Bounce with effects"), Action::Freeze(FreezeOp::Bounce { source: BounceSource::Channel(ch), fx: true }))
                .tip(tr("Through the mixer, to a new audio track; the channel is muted")),
        ]
    }

    /// Freeze entry of a mixer track's menu (none for the master and VCAs).
    pub(super) fn track_freeze_item(&self, t: usize) -> Option<MenuItem> {
        let target = FreezeTarget::Track(t);
        if !self.project.can_freeze_track(t) {
            return None;
        }
        Some(if self.project.is_frozen(target) {
            MenuItem::new(Icon::Snowflake, tr("Unfreeze the track"), Action::Freeze(FreezeOp::Unfreeze(target))).tip(tr("Loads its effects and channels again"))
        } else {
            MenuItem::new(Icon::Snowflake, tr("Freeze the track"), Action::Freeze(FreezeOp::Freeze(target)))
                .tip(tr("Renders everything that reaches it through its effects, then unloads them to save CPU"))
        })
    }

    /// Bounce entries of a clip's menu.
    pub(super) fn clip_bounce_items(&self, sel: ClipSel) -> Vec<MenuItem> {
        let selection = matches!(self.time_selection, Some((View::Playlist, a, b)) if b > a);
        let muted = match sel {
            ClipSel::Pattern(i) => self.project.clips.get(i).is_some_and(|c| c.muted),
            ClipSel::Audio(i) => self.project.audio_clips.get(i).is_some_and(|c| c.muted),
        };
        let mut v = Vec::new();
        if let ClipSel::Pattern(i) = sel {
            let source = BounceSource::Clips { patterns: vec![i], audio: Vec::new() };
            v.push(
                MenuItem::new(Icon::Export, tr("Bounce to audio"), Action::Freeze(FreezeOp::Bounce { source, fx: true }))
                    .tip(tr("Renders the clip with its effects to a new lane; the clip is muted"))
                    .sep(),
            );
        }
        let consolidate = MenuItem::new(Icon::Columns, tr("Consolidate"), Action::Freeze(FreezeOp::Consolidate(sel)));
        v.push(if selection {
            consolidate.detail(tr("selection").into()).tip(tr("Every clip of this lane in the time selection becomes one audio clip"))
        } else {
            consolidate.tip(tr("The clip becomes an audio clip · with a time selection, every clip of its lane in it"))
        });
        if selection {
            v.push(
                MenuItem::new(Icon::Playlist, tr("Render the selection"), Action::Freeze(FreezeOp::RenderSelection))
                    .tip(tr("The time selection, as heard, to a new audio track; what played there is muted")),
            );
        }
        let (icon, name) = if muted { (Icon::Mute, tr("Unmute the clip")) } else { (Icon::Mute, tr("Mute the clip")) };
        v.push(MenuItem::new(icon, name, Action::Freeze(FreezeOp::ToggleMute(sel))));
        v
    }

    /// Menu of an audio clip (right-click on its title).
    pub(super) fn audio_clip_menu(&self, i: usize) -> (String, Vec<MenuItem>) {
        let Some(c) = self.project.audio_clips.get(i) else { return (String::new(), Vec::new()) };
        let name = std::path::Path::new(&c.path).file_stem().map_or_else(|| c.path.clone(), |s| s.to_string_lossy().into_owned());
        if self.is_offline(&c.path) {
            // Nothing to bounce in a silent clip: find its file first.
            let locate = Action::Popup(Popup::LocateMissing(self.locate_start_dir()));
            let v = vec![
                MenuItem::new(Icon::Search, tr("Locate missing files…"), locate).tip(&c.path),
                MenuItem::new(Icon::Trash, tr("Delete the clip"), Action::DeleteAudioClip(i)).sep(),
            ];
            return (ponzik_project::flp::file_name_of(&c.path).to_string(), v);
        }
        let mut v = self.clip_bounce_items(ClipSel::Audio(i));
        v.push(
            MenuItem::new(Icon::Detect, tr("Detect the key"), Action::DetectKey(super::groove::KeySource::Audio(i)))
                .tip(tr("From the clip's pitches (chromagram), then apply it to the project"))
                .sep(),
        );
        v.push(MenuItem::new(Icon::Groove, tr("Extract a groove"), Action::ExtractAudioGroove(i)).tip(tr("Timing and accents of the clip's attacks, saved in the groove pool")));
        v.push(MenuItem::new(Icon::Trash, tr("Delete the clip"), Action::DeleteAudioClip(i)).tip(tr("Hold right-click: sweep to erase")).sep());
        (name, v)
    }

    /// The freeze that locks a device, if any.
    pub(super) fn device_lock(&self, d: Device) -> Option<FreezeTarget> {
        match d {
            Device::Instrument(ch) => self.channel_lock(ch),
            Device::Effect { track, .. } | Device::RackUnit { track, .. } => {
                self.project.is_frozen(FreezeTarget::Track(track)).then_some(FreezeTarget::Track(track))
            }
        }
    }

    /// The freeze that locks a channel's devices: its own, or its track's.
    pub(super) fn channel_lock(&self, ch: usize) -> Option<FreezeTarget> {
        let c = self.project.channels.get(ch)?;
        if c.frozen.is_some() {
            Some(FreezeTarget::Channel(ch))
        } else {
            self.project.is_frozen(FreezeTarget::Track(c.track)).then_some(FreezeTarget::Track(c.track))
        }
    }

    /// Frost over a frozen device card: read-only, with an Unfreeze button.
    pub(super) fn frost_card(&mut self, c: &mut Canvas, input: &Input, card: Rect, target: FreezeTarget, button: bool) {
        let t = self.theme;
        let frost = Color::hex(FROST);
        c.fill_rrect(card, 14.0, t.bg.alpha(0.62));
        c.fill_rrect(card, 14.0, frost.alpha(0.07));
        c.stroke_rrect(card, 14.0, 1.0, frost.alpha(0.35));
        // A few crystals in the corners, the big one in the middle.
        let (cx, cy) = card.center();
        draw_icon(c, Icon::Snowflake, cx, cy - if button { 18.0 } else { 0.0 }, 40.0, frost.alpha(0.5));
        for (dx, dy, s) in [(18.0, 50.0, 12.0), (card.w - 22.0, card.h - 26.0, 14.0), (card.w - 40.0, 54.0, 9.0)] {
            if dx < card.w && dy < card.h {
                draw_icon(c, Icon::Snowflake, card.x + dx, card.y + dy, s, frost.alpha(0.22));
            }
        }
        let inside = input.inside && card.contains(input.x, input.y);
        if inside {
            self.set_tip(tr("Frozen: it plays its render · edits ask to unfreeze it"), "");
        }
        if !button || card.h < 90.0 {
            return;
        }
        let label = tr("Unfreeze");
        let w = self.fonts.measure(self.f.sans_med, 13.0, label) + 44.0;
        let r = Rect::new(cx - w * 0.5, cy + 16.0, w, 30.0);
        let id = wid("frost-unfreeze", card.x as usize, card.y as usize);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 15.0, t.raised);
        c.fill_rrect(r, 15.0, frost.alpha(0.16 + hv * 0.14));
        c.stroke_rrect(r, 15.0, 1.0, frost.alpha(0.6 + hv * 0.3));
        draw_icon(c, Icon::Snowflake, r.x + 18.0, r.y + 15.0, 13.0, frost);
        self.text(c, self.f.sans_med, 13.0, r.x + 30.0, r.y + 15.0, label, t.text.mix(frost, 0.3));
        if hov {
            self.set_cursor(Cursor::Hand);
            self.set_tip(tr("Loads its devices again; they play live"), "");
            if input.pressed {
                self.unfreeze(target);
            }
        }
    }

    /// Snowflake badge, for rows and strips of frozen things.
    pub(super) fn frost_badge(&mut self, c: &mut Canvas, input: &Input, x: f32, y: f32, size: f32, target: FreezeTarget, id: usize) {
        let frost = Color::hex(FROST);
        let r = Rect::new(x - size * 0.5 - 2.0, y - size * 0.5 - 2.0, size + 4.0, size + 4.0);
        c.fill_rrect(r, size * 0.35, frost.alpha(0.14));
        draw_icon(c, Icon::Snowflake, x, y, size, frost);
        if self.hovered(input, wid("frost-badge", id, matches!(target, FreezeTarget::Track(_)) as usize), r) {
            let tip = match target {
                FreezeTarget::Channel(_) => tr("Frozen: plays its render, its instrument is unloaded · right-click: unfreeze"),
                FreezeTarget::Track(_) => tr("Frozen: plays its render, its effects are unloaded · right-click: unfreeze"),
            };
            self.set_tip(tip, "");
            if input.right_pressed {
                self.unfreeze(target);
            }
        }
    }

    /// `PONZIK_FREEZE=channel:4,track:3,prompt,bounce,consolidate,selection,menu`:
    /// freezes and bounces for a screenshot.
    pub fn screenshot_freeze(&mut self, spec: &str) {
        self.demo_audio = true;
        for part in spec.split(',').map(str::trim) {
            let (op, arg) = part.split_once(':').map_or((part, None), |(o, a)| (o, a.trim().parse::<usize>().ok()));
            match (op, arg) {
                ("channel", Some(ch)) if ch < self.project.channels.len() => {
                    self.freeze(FreezeTarget::Channel(ch));
                    self.focus = Focus::Channel(ch);
                }
                ("track", Some(t)) => {
                    self.freeze(FreezeTarget::Track(t));
                    self.focus = Focus::Track(t);
                }
                ("prompt", _) => {
                    // Editing a frozen channel's instrument.
                    if let Some(ch) = (0..self.project.channels.len()).find(|&c| self.project.is_frozen(FreezeTarget::Channel(c))) {
                        self.pointer = (620.0, 420.0);
                        self.project.channels[ch].instrument.set(1, 0.3);
                        self.commit(tr("Instrument"));
                    }
                }
                ("menu", Some(ch)) => {
                    self.focus = Focus::Channel(ch);
                    self.open_popup(Popup::ChannelMenu(ch), Rect::new(360.0, 260.0, 0.0, 0.0));
                }
                ("trackmenu", Some(t)) => {
                    self.focus = Focus::Track(t);
                    self.open_popup(Popup::TrackMenu(t), Rect::new(560.0, 146.0, 0.0, 0.0));
                }
                ("bounce", Some(ch)) => self.bounce(BounceSource::Channel(ch), true),
                ("clipbounce", Some(i)) => self.bounce(BounceSource::Clips { patterns: vec![i], audio: Vec::new() }, true),
                ("consolidate", Some(i)) => self.consolidate(ClipSel::Pattern(i)),
                ("selection", _) => {
                    self.time_selection = Some((View::Playlist, 4 * TICKS_PER_BAR, 8 * TICKS_PER_BAR));
                    self.render_selection();
                }
                ("clipmenu", Some(i)) => {
                    self.time_selection = Some((View::Playlist, 4 * TICKS_PER_BAR, 8 * TICKS_PER_BAR));
                    self.playlist.selected = Some(i);
                    self.open_popup(Popup::ClipMenu(i), Rect::new(700.0, 168.0, 0.0, 19.0));
                }
                _ => {}
            }
        }
    }
}
