//! Plugin windows: the plugin's own interface (CLAP `clap.gui`, VST 3
//! `IPlugView`) in a window of its own, kept above Ponzik's.
//!
//! Every CLAP or VST 3 instance hands out its editor before going to the
//! engine; [`PluginEditors::capture`] follows the commands sent to the engine
//! so each device's editor is known (and its window closed when the device is
//! replaced or removed). [`App::editors_tick`] pumps them every frame and turns
//! the edits made in a plugin's interface into project edits: undo,
//! automation and the device card follow.

use std::collections::HashMap;

use ponzik_engine::{AutoTarget, Command, Editor, EditorEvent, Engine, ParentWindow};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_window::{Event, NativeHandle, PluginWindow, PluginWindowOptions};

use super::app::{App, display_kind};
use super::devices::{Device, device_target, params_of};

struct OpenWindow {
    device: Device,
    editor: Editor,
    window: PluginWindow,
    /// Last size the plugin asked for and got: asking again is not a new
    /// request (some plugins ask again from the resize itself).
    granted: Option<(u32, u32)>,
}

/// Editors of the engine's plugin instances, and their open windows.
#[derive(Default)]
pub struct PluginEditors {
    /// By channel (`None`: not a plugin).
    channels: Vec<Option<Editor>>,
    inserts: HashMap<(usize, usize), Editor>,
    open: Vec<OpenWindow>,
    /// Where each device's window was last, for this session.
    positions: HashMap<Device, (i32, i32)>,
    /// Main window, which plugin windows stay above.
    owner: Option<NativeHandle>,
}

impl PluginEditors {
    /// Editors of an engine about to start.
    pub fn from_engine(engine: &mut Engine) -> Self {
        let channels = engine.channels().iter().map(|c| c.instrument.editor()).collect();
        let mut inserts = HashMap::new();
        for (t, track) in engine.mixer_mut().tracks.iter().enumerate() {
            for (s, fx) in track.inserts.iter().enumerate() {
                if let Some(e) = fx.as_ref().and_then(|fx| fx.editor()) {
                    inserts.insert((t, s), e);
                }
            }
        }
        Self { channels, inserts, ..Self::default() }
    }

    pub fn set_owner(&mut self, owner: NativeHandle) {
        self.owner = Some(owner);
    }

    /// Follows a command about to be sent to the engine.
    pub fn capture(&mut self, cmd: &Command) {
        match cmd {
            Command::AddChannel(ch) => self.channels.push(ch.instrument.editor()),
            Command::RemoveChannel(i) => {
                let i = *i;
                self.close(Device::Instrument(i));
                if i < self.channels.len() {
                    self.channels.remove(i);
                }
                for w in &mut self.open {
                    if let Device::Instrument(c) = &mut w.device
                        && *c > i
                    {
                        *c -= 1;
                    }
                }
                self.positions = std::mem::take(&mut self.positions)
                    .into_iter()
                    .filter_map(|(d, p)| match d {
                        Device::Instrument(c) if c == i => None,
                        Device::Instrument(c) if c > i => Some((Device::Instrument(c - 1), p)),
                        d => Some((d, p)),
                    })
                    .collect();
            }
            Command::ReplaceInstrument { channel, instrument } => {
                self.close(Device::Instrument(*channel));
                if self.channels.len() <= *channel {
                    self.channels.resize(*channel + 1, None);
                }
                self.channels[*channel] = instrument.editor();
            }
            Command::SetInsert { track, slot, effect } => {
                self.close(Device::Effect { track: *track, slot: *slot });
                match effect.as_ref().and_then(|fx| fx.editor()) {
                    Some(e) => self.inserts.insert((*track, *slot), e),
                    None => self.inserts.remove(&(*track, *slot)),
                };
            }
            Command::SetInstrumentParam { channel, id, value } => {
                if let Some(Some(e)) = self.channels.get(*channel) {
                    e.param_changed(*id, *value);
                }
            }
            Command::SetEffectParam { track, slot, id, value } => {
                if let Some(e) = self.inserts.get(&(*track, *slot)) {
                    e.param_changed(*id, *value);
                }
            }
            _ => {}
        }
    }

    pub fn editor(&self, device: Device) -> Option<&Editor> {
        match device {
            Device::Instrument(c) => self.channels.get(c)?.as_ref(),
            Device::Effect { track, slot } => self.inserts.get(&(track, slot)),
            // Les racks n'hébergent que des effets natifs : pas de fenêtre de plugin.
            Device::RackUnit { .. } => None,
        }
    }

    pub fn is_open(&self, device: Device) -> bool {
        self.open.iter().any(|w| w.device == device)
    }

    pub fn any_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Some plugin instance exists (its timers want pumping).
    pub fn has_editors(&self) -> bool {
        !self.inserts.is_empty() || self.channels.iter().any(Option::is_some)
    }

    /// Opens the device's plugin window, where it was last in this session.
    pub fn open(&mut self, device: Device, title: &str) -> Result<(), String> {
        if self.is_open(device) {
            return Ok(());
        }
        let editor = self.editor(device).cloned().ok_or_else(|| tr("this device is not a plugin").to_string())?;
        if !editor.has_gui() {
            return Err(tr("this plugin has no interface of its own").into());
        }
        if !editor.is_alive() {
            return Err(tr("the plugin is no longer loaded").into());
        }
        let options = PluginWindowOptions {
            title: title.into(),
            width: 640,
            height: 480,
            position: self.positions.get(&device).copied(),
            owner: self.owner,
            resizable: false,
        };
        let mut window = PluginWindow::open(&options).map_err(|e| e.to_string())?;
        let parent = match window.handle() {
            NativeHandle::X11(id) => ParentWindow::X11(id),
            NativeHandle::Win32(h) => ParentWindow::Win32(h),
            NativeHandle::Cocoa(v) => ParentWindow::Cocoa(v),
            NativeHandle::None => return Err(tr("no window system").into()),
        };
        let size = editor.open(parent, window.scale_factor() as f64)?;
        window.set_resizable(size.resizable);
        window.set_size(size.width, size.height);
        self.open.push(OpenWindow { device, editor, window, granted: Some((size.width, size.height)) });
        Ok(())
    }

    /// Closes the device's window: the interface goes first, then the window.
    pub fn close(&mut self, device: Device) {
        let Some(i) = self.open.iter().position(|w| w.device == device) else { return };
        let mut w = self.open.remove(i);
        if let Some(p) = w.window.position() {
            self.positions.insert(device, p);
        }
        w.editor.close();
        drop(w.window);
    }

    pub fn close_all(&mut self) {
        while let Some(d) = self.open.first().map(|w| w.device) {
            self.close(d);
        }
    }

    /// Pumps every editor and the windows; returns the edits for the project,
    /// and whether a window opened or closed by itself.
    pub fn tick(&mut self) -> (Vec<(Device, EditorEvent)>, bool) {
        let mut events = Vec::new();
        let mut buf = Vec::new();
        let editors = self.channels.iter().enumerate().filter_map(|(c, e)| Some((Device::Instrument(c), e.as_ref()?)));
        let inserts = self.inserts.iter().map(|(&(track, slot), e)| (Device::Effect { track, slot }, e));
        for (device, editor) in editors.chain(inserts) {
            if editor.is_alive() {
                editor.idle(&mut buf);
                events.extend(buf.drain(..).map(|e| (device, e)));
            }
        }
        let mut to_close = Vec::new();
        let mut wev = Vec::new();
        for w in &mut self.open {
            if !w.editor.is_alive() || !w.editor.is_open() {
                to_close.push(w.device);
                continue;
            }
            w.window.poll_events(&mut wev);
            for e in wev.drain(..) {
                match e {
                    Event::CloseRequested => to_close.push(w.device),
                    // The user resized: the plugin takes what fits its constraints.
                    Event::Resized { width, height } => {
                        w.granted = None;
                        if let Some(size) = w.editor.set_size(width, height)
                            && size != (width, height)
                        {
                            w.window.set_size(size.0, size.1);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut edits = Vec::new();
        for (device, e) in events {
            let w = self.open.iter_mut().find(|w| w.device == device);
            match (e, w) {
                (EditorEvent::Resize { width, height }, Some(w)) if w.granted != Some((width, height)) => {
                    w.granted = Some((width, height));
                    w.window.set_size(width, height);
                    w.editor.set_size(width, height);
                }
                (EditorEvent::HintsChanged, Some(w)) => w.window.set_resizable(w.editor.resizable()),
                (EditorEvent::Closed | EditorEvent::Hide, Some(_)) => to_close.push(device),
                (EditorEvent::Param { .. } | EditorEvent::GestureBegin(_) | EditorEvent::GestureEnd(_), _) => {
                    edits.push((device, e))
                }
                _ => {}
            }
        }
        let closed = !to_close.is_empty();
        for d in to_close {
            self.close(d);
        }
        (edits, closed)
    }
}

impl App {
    /// `<Plugin> — <Track>`.
    pub fn editor_title(&self, device: Device) -> String {
        let (kind, _) = self.device_kind(device);
        let plugin = self.plugin_name(&kind).unwrap_or_else(|| display_kind(&kind));
        let place = match device {
            Device::Instrument(c) => self.project.channels.get(c).map(|c| c.name.clone()),
            Device::Effect { track, .. } | Device::RackUnit { track, .. } => self.project.tracks.get(track).map(|t| t.name.clone()),
        };
        match place {
            Some(p) => format!("{plugin} — {p}"),
            None => plugin,
        }
    }

    /// Opens the device's plugin window, or closes it.
    pub fn toggle_editor(&mut self, device: Device) {
        if self.editors.is_open(device) {
            self.editors.close(device);
            return;
        }
        let title = self.editor_title(device);
        if let Err(e) = self.editors.open(device, &title) {
            self.toast(&tr_fmt!("Can't open the plugin window: {}", e));
        }
    }

    /// Pumps the plugin editors; edits made in their interfaces become project
    /// edits (one undo step per gesture). True when the interface must redraw.
    pub fn editors_tick(&mut self) -> bool {
        let (events, closed) = self.editors.tick();
        let mut edited = false;
        for (device, e) in &events {
            // Only a window the user works in edits the project (a closed
            // plugin moving its own values is not an edit).
            if !self.editors.is_open(*device) {
                continue;
            }
            match *e {
                EditorEvent::GestureBegin(_) => self.break_undo_merge(),
                EditorEvent::Param { id, value } => {
                    let (kind, inst) = self.device_kind(*device);
                    let Some(info) = params_of(&kind, inst).iter().find(|p| p.id == id) else { continue };
                    let v = info.clamp(value);
                    if (self.param_value(*device, info) - v).abs() < f32::EPSILON {
                        continue;
                    }
                    match *device {
                        Device::Instrument(ch) => self.project.channels[ch].instrument.set(id, v),
                        Device::Effect { track, slot } => {
                            if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
                                fx.set(id, v);
                            }
                        }
                        Device::RackUnit { .. } => {}
                    }
                    self.last_plugin_param = Some((device_target(*device, id), v));
                    edited = true;
                }
                _ => {}
            }
        }
        if edited {
            self.commit(tr("Edit in the plugin window"));
        }
        closed || !events.is_empty()
    }

    /// The plugin parameter last moved in a plugin window, for the palette.
    pub fn last_plugin_param(&self) -> Option<(AutoTarget, f32)> {
        self.last_plugin_param
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ponzik_engine::{Channel, Effect, EditorSize, Instrument, Params, PluginEditor};

    use super::*;

    /// Editor that only records what the host tells it.
    #[derive(Default)]
    struct Fake {
        changed: Mutex<Vec<(u32, f32)>>,
    }

    impl PluginEditor for Fake {
        fn has_gui(&self) -> bool {
            true
        }
        fn is_alive(&self) -> bool {
            true
        }
        fn is_open(&self) -> bool {
            false
        }
        fn open(&self, _: ParentWindow, _: f64) -> Result<EditorSize, String> {
            Err("no window in tests".into())
        }
        fn close(&self) {}
        fn set_size(&self, _: u32, _: u32) -> Option<(u32, u32)> {
            None
        }
        fn resizable(&self) -> bool {
            false
        }
        fn idle(&self, _: &mut Vec<EditorEvent>) {}
        fn param_changed(&self, id: u32, value: f32) {
            self.changed.lock().unwrap().push((id, value));
        }
    }

    struct Plug(Arc<Fake>);

    impl Params for Plug {}

    impl Instrument for Plug {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn note_on(&mut self, _: u8, _: f32) {}
        fn note_off(&mut self, _: u8) {}
        fn all_notes_off(&mut self) {}
        fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
        fn editor(&self) -> Option<Editor> {
            Some(self.0.clone())
        }
    }

    impl Effect for Plug {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
        fn editor(&self) -> Option<Editor> {
            Some(self.0.clone())
        }
    }

    fn same(a: Option<&Editor>, b: &Arc<Fake>) -> bool {
        a.is_some_and(|a| std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b)))
    }

    #[test]
    fn editors_follow_the_engine_commands() {
        let mut eds = PluginEditors::default();
        let (a, b, fx) = (Arc::new(Fake::default()), Arc::new(Fake::default()), Arc::new(Fake::default()));
        let native = ponzik_engine::registry::create_instrument("pulse").unwrap();
        eds.capture(&Command::AddChannel(Box::new(Channel::new("native", native))));
        eds.capture(&Command::AddChannel(Box::new(Channel::new("a", Box::new(Plug(a.clone()))))));
        eds.capture(&Command::AddChannel(Box::new(Channel::new("b", Box::new(Plug(b.clone()))))));
        eds.capture(&Command::SetInsert { track: 3, slot: 1, effect: Some(Box::new(Plug(fx.clone()))) });
        assert!(eds.editor(Device::Instrument(0)).is_none());
        assert!(same(eds.editor(Device::Instrument(1)), &a) && same(eds.editor(Device::Instrument(2)), &b));
        assert!(same(eds.editor(Device::Effect { track: 3, slot: 1 }), &fx));
        assert!(eds.has_editors() && !eds.any_open());

        // Host changes reach the right plugin.
        eds.capture(&Command::SetInstrumentParam { channel: 2, id: 4, value: 0.5 });
        eds.capture(&Command::SetEffectParam { track: 3, slot: 1, id: 9, value: 1.0 });
        eds.capture(&Command::SetInstrumentParam { channel: 0, id: 4, value: 0.5 });
        assert_eq!(*b.changed.lock().unwrap(), [(4, 0.5)]);
        assert_eq!(*fx.changed.lock().unwrap(), [(9, 1.0)]);
        assert!(a.changed.lock().unwrap().is_empty());

        // Removing a channel shifts the next ones; replacing or clearing forgets.
        eds.capture(&Command::RemoveChannel(1));
        assert!(same(eds.editor(Device::Instrument(1)), &b));
        assert!(eds.editor(Device::Instrument(2)).is_none());
        let native = ponzik_engine::registry::create_instrument("pulse").unwrap();
        eds.capture(&Command::ReplaceInstrument { channel: 1, instrument: native });
        assert!(eds.editor(Device::Instrument(1)).is_none());
        eds.capture(&Command::SetInsert { track: 3, slot: 1, effect: None });
        assert!(eds.editor(Device::Effect { track: 3, slot: 1 }).is_none());
        assert!(!eds.has_editors());
        assert!(eds.open(Device::Instrument(1), "x").is_err(), "not a plugin anymore");
    }
}
