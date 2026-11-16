//! Raccourcis remappables : une table action → combinaison de touches, avec les
//! valeurs de FL Studio par défaut, la détection des conflits et l'écriture en
//! texte (« ctrl+shift+s ») pour le fichier de réglages.

use ponzik_i18n::{tr, tr_noop};
use ponzik_window::{Key, Modifiers};

use super::app::View;
use super::widgets::Icon;

/// Combinaison de touches : une touche et ses modificateurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Combo {
    pub const fn key(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: false }
    }

    pub const fn ctrl(key: Key) -> Self {
        Self { key, ctrl: true, shift: false, alt: false }
    }

    pub const fn ctrl_shift(key: Key) -> Self {
        Self { key, ctrl: true, shift: true, alt: false }
    }

    pub const fn alt(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: true }
    }

    pub fn from_event(key: Key, mods: Modifiers) -> Self {
        Self { key, ctrl: mods.ctrl, shift: mods.shift, alt: mods.alt }
    }

    /// Les symboles (+, -, …) changent de touche selon la disposition : Maj n'y
    /// compte pas. Ailleurs, les modificateurs doivent être exactement ceux-là.
    pub fn matches(self, key: Key, mods: Modifiers) -> bool {
        let symbol = matches!(self.key, Key::Char(c) if !c.is_alphanumeric());
        self.key == key && self.ctrl == mods.ctrl && self.alt == mods.alt && (symbol || self.shift == mods.shift)
    }

    /// Une lettre sans Ctrl ni Alt : jouée comme une note quand le clavier musical est actif.
    pub fn is_plain_char(self) -> bool {
        matches!(self.key, Key::Char(_)) && !self.ctrl && !self.alt
    }

    /// Libellé affiché : « Ctrl Maj S », « Alt F8 », « Espace ».
    pub fn label(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push(tr("Shift").into());
        }
        parts.push(key_label(self.key));
        parts.join(" ")
    }

    /// Texte du fichier de réglages : « ctrl+shift+s », « alt+f8 », « plus ».
    pub fn to_text(self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        s.push_str(&key_text(self.key));
        s
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut combo = Combo::key(Key::Space);
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (last, mods) = parts.split_last()?;
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" => combo.ctrl = true,
                "shift" | "maj" => combo.shift = true,
                "alt" | "option" => combo.alt = true,
                _ => return None,
            }
        }
        combo.key = parse_key(last)?;
        Some(combo)
    }
}

fn key_label(key: Key) -> String {
    match key {
        Key::Space => tr("Space").into(),
        Key::Enter => tr("Enter").into(),
        Key::Escape => tr("Esc").into(),
        Key::Backspace => tr("Backspace").into(),
        Key::Tab => "Tab".into(),
        Key::Delete => tr("Del").into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::F(n) => format!("F{n}"),
        Key::PageUp => tr("Pg ↑").into(),
        Key::PageDown => tr("Pg ↓").into(),
        Key::Home => tr("Home").into(),
        Key::End => tr("End").into(),
        Key::Char('-') => "−".into(),
        Key::Char(c) => c.to_uppercase().collect(),
    }
}

fn key_text(key: Key) -> String {
    match key {
        Key::Space => "space".into(),
        Key::Enter => "enter".into(),
        Key::Escape => "escape".into(),
        Key::Backspace => "backspace".into(),
        Key::Tab => "tab".into(),
        Key::Delete => "delete".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::F(n) => format!("f{n}"),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::Char('+') => "plus".into(),
        Key::Char(c) => c.to_string(),
    }
}

fn parse_key(s: &str) -> Option<Key> {
    let lower = s.to_lowercase();
    Some(match lower.as_str() {
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "delete" | "del" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        f if f.len() > 1 && f.starts_with('f') => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            Key::F(n)
        }
        _ => {
            let mut chars = lower.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

/// Actions globales qu'on peut lier à une touche.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shortcut {
    PlayStop,
    ToggleMode,
    Record,
    TapTempo,
    Metronome,
    TypingKeyboard,
    ViewPlaylist,
    ViewRack,
    ViewPiano,
    ViewSession,
    ViewMixer,
    ViewLab,
    NextView,
    ToggleBrowser,
    TogglePanel,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewPattern,
    NextPattern,
    PrevPattern,
    Undo,
    Redo,
    History,
    Save,
    SaveAs,
    Open,
    NewProject,
    Export,
    Palette,
    Search,
    Settings,
}

/// Familles de raccourcis, dans l'ordre de l'écran de réglages.
pub const GROUPS: [&str; 5] = [tr_noop("Transport"), tr_noop("Views"), tr_noop("Pattern"), tr_noop("Edit"), tr_noop("File")];

impl Shortcut {
    pub const ALL: [Shortcut; 32] = [
        Shortcut::PlayStop,
        Shortcut::ToggleMode,
        Shortcut::Record,
        Shortcut::TapTempo,
        Shortcut::Metronome,
        Shortcut::TypingKeyboard,
        Shortcut::ViewPlaylist,
        Shortcut::ViewRack,
        Shortcut::ViewPiano,
        Shortcut::ViewSession,
        Shortcut::ViewMixer,
        Shortcut::ViewLab,
        Shortcut::NextView,
        Shortcut::ToggleBrowser,
        Shortcut::TogglePanel,
        Shortcut::ZoomIn,
        Shortcut::ZoomOut,
        Shortcut::ZoomReset,
        Shortcut::NewPattern,
        Shortcut::NextPattern,
        Shortcut::PrevPattern,
        Shortcut::Undo,
        Shortcut::Redo,
        Shortcut::History,
        Shortcut::Palette,
        Shortcut::Search,
        Shortcut::Settings,
        Shortcut::Save,
        Shortcut::SaveAs,
        Shortcut::Open,
        Shortcut::NewProject,
        Shortcut::Export,
    ];

    /// Identifiant stable du fichier de réglages.
    pub fn id(self) -> &'static str {
        match self {
            Shortcut::PlayStop => "play_stop",
            Shortcut::ToggleMode => "toggle_mode",
            Shortcut::Record => "record",
            Shortcut::TapTempo => "tap_tempo",
            Shortcut::Metronome => "metronome",
            Shortcut::TypingKeyboard => "typing_keyboard",
            Shortcut::ViewPlaylist => "view_playlist",
            Shortcut::ViewRack => "view_rack",
            Shortcut::ViewPiano => "view_piano",
            Shortcut::ViewSession => "view_session",
            Shortcut::ViewMixer => "view_mixer",
            Shortcut::ViewLab => "view_lab",
            Shortcut::NextView => "next_view",
            Shortcut::ToggleBrowser => "toggle_browser",
            Shortcut::TogglePanel => "toggle_panel",
            Shortcut::ZoomIn => "ui_zoom_in",
            Shortcut::ZoomOut => "ui_zoom_out",
            Shortcut::ZoomReset => "ui_zoom_reset",
            Shortcut::NewPattern => "new_pattern",
            Shortcut::NextPattern => "next_pattern",
            Shortcut::PrevPattern => "prev_pattern",
            Shortcut::Undo => "undo",
            Shortcut::Redo => "redo",
            Shortcut::History => "history",
            Shortcut::Save => "save",
            Shortcut::SaveAs => "save_as",
            Shortcut::Open => "open",
            Shortcut::NewProject => "new_project",
            Shortcut::Export => "export",
            Shortcut::Palette => "palette",
            Shortcut::Search => "search",
            Shortcut::Settings => "settings",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Shortcut::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Shortcut::PlayStop => tr("Play / stop"),
            Shortcut::ToggleMode => tr("Pattern / song"),
            Shortcut::Record => tr("Arm recording"),
            Shortcut::TapTempo => tr("Tap tempo"),
            Shortcut::Metronome => tr("Metronome"),
            Shortcut::TypingKeyboard => tr("Typing keyboard"),
            Shortcut::ViewPlaylist => tr("Playlist"),
            Shortcut::ViewRack => tr("Channel Rack"),
            Shortcut::ViewPiano => tr("Piano roll"),
            Shortcut::ViewSession => tr("Session"),
            Shortcut::ViewMixer => tr("Mixer"),
            Shortcut::ViewLab => tr("Sample Lab"),
            Shortcut::NextView => tr("Next view"),
            Shortcut::ToggleBrowser => tr("Browser"),
            Shortcut::TogglePanel => tr("Device panel"),
            Shortcut::ZoomIn => tr("Enlarge the interface"),
            Shortcut::ZoomOut => tr("Shrink the interface"),
            Shortcut::ZoomReset => tr("Interface at 100%"),
            Shortcut::NewPattern => tr("New pattern"),
            Shortcut::NextPattern => tr("Next pattern"),
            Shortcut::PrevPattern => tr("Previous pattern"),
            Shortcut::Undo => tr("Undo"),
            Shortcut::Redo => tr("Redo"),
            Shortcut::History => tr("History"),
            Shortcut::Save => tr("Save"),
            Shortcut::SaveAs => tr("Save as"),
            Shortcut::Open => tr("Open"),
            Shortcut::NewProject => tr("New project"),
            Shortcut::Export => tr("Export to WAV"),
            Shortcut::Palette => tr("Find anything"),
            Shortcut::Search => tr("Search sounds"),
            Shortcut::Settings => tr("Settings"),
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Shortcut::PlayStop => Icon::Play,
            Shortcut::ToggleMode => Icon::Swap,
            Shortcut::Record => Icon::Record,
            Shortcut::TapTempo => Icon::Metronome,
            Shortcut::Metronome => Icon::Metronome,
            Shortcut::TypingKeyboard => Icon::Keyboard,
            Shortcut::ViewPlaylist => Icon::Playlist,
            Shortcut::ViewRack => Icon::Rack,
            Shortcut::ViewPiano => Icon::Piano,
            Shortcut::ViewSession => Icon::Session,
            Shortcut::ViewMixer => Icon::Mixer,
            Shortcut::ViewLab => Icon::Lab,
            Shortcut::NextView => Icon::ChevronRight,
            Shortcut::ToggleBrowser => Icon::Sidebar,
            Shortcut::TogglePanel => Icon::Panel,
            Shortcut::ZoomIn | Shortcut::ZoomOut => Icon::Search,
            Shortcut::ZoomReset => Icon::Reset,
            Shortcut::NewPattern => Icon::Plus,
            Shortcut::NextPattern => Icon::ChevronRight,
            Shortcut::PrevPattern => Icon::ChevronLeft,
            Shortcut::Undo => Icon::Undo,
            Shortcut::Redo => Icon::Redo,
            Shortcut::History => Icon::History,
            Shortcut::Save => Icon::Save,
            Shortcut::SaveAs => Icon::Pencil,
            Shortcut::Open => Icon::Folder,
            Shortcut::NewProject => Icon::Plus,
            Shortcut::Export => Icon::Export,
            Shortcut::Palette => Icon::Search,
            Shortcut::Search => Icon::Sample,
            Shortcut::Settings => Icon::Layout,
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Shortcut::PlayStop
            | Shortcut::ToggleMode
            | Shortcut::Record
            | Shortcut::TapTempo
            | Shortcut::Metronome
            | Shortcut::TypingKeyboard => "Transport",
            Shortcut::ViewPlaylist
            | Shortcut::ViewRack
            | Shortcut::ViewPiano
            | Shortcut::ViewSession
            | Shortcut::ViewMixer
            | Shortcut::ViewLab
            | Shortcut::NextView
            | Shortcut::ToggleBrowser
            | Shortcut::TogglePanel
            | Shortcut::ZoomIn
            | Shortcut::ZoomOut
            | Shortcut::ZoomReset => "Views",
            Shortcut::NewPattern | Shortcut::NextPattern | Shortcut::PrevPattern => "Pattern",
            Shortcut::Undo
            | Shortcut::Redo
            | Shortcut::History
            | Shortcut::Palette
            | Shortcut::Search
            | Shortcut::Settings => "Edit",
            Shortcut::Save | Shortcut::SaveAs | Shortcut::Open | Shortcut::NewProject | Shortcut::Export => "File",
        }
    }

    /// Valeur par défaut : celle de FL Studio quand elle existe.
    pub fn default_combo(self) -> Option<Combo> {
        use Key::*;
        Some(match self {
            Shortcut::PlayStop => Combo::key(Space),
            Shortcut::ToggleMode => Combo::key(Char('l')),
            Shortcut::Record => Combo::key(Char('r')),
            Shortcut::TapTempo => Combo::key(Char('t')),
            Shortcut::Metronome => Combo::ctrl(Char('m')),
            Shortcut::TypingKeyboard => Combo::ctrl(Char('t')),
            Shortcut::ViewPlaylist => Combo::key(F(5)),
            Shortcut::ViewRack => Combo::key(F(6)),
            Shortcut::ViewPiano => Combo::key(F(7)),
            Shortcut::ViewSession => return None,
            Shortcut::ViewMixer => Combo::key(F(9)),
            Shortcut::ViewLab => Combo::key(F(8)),
            Shortcut::NextView => Combo::key(Tab),
            Shortcut::ToggleBrowser => Combo::alt(F(8)),
            Shortcut::TogglePanel => return None,
            Shortcut::ZoomIn => Combo::ctrl(Char('=')),
            Shortcut::ZoomOut => Combo::ctrl(Char('-')),
            Shortcut::ZoomReset => Combo::ctrl(Char('0')),
            Shortcut::NewPattern => Combo::key(F(4)),
            Shortcut::NextPattern => Combo::key(Char('+')),
            Shortcut::PrevPattern => Combo::key(Char('-')),
            Shortcut::Undo => Combo::ctrl(Char('z')),
            Shortcut::Redo => Combo::ctrl(Char('y')),
            Shortcut::History => return None,
            Shortcut::Save => Combo::ctrl(Char('s')),
            Shortcut::SaveAs => Combo::ctrl_shift(Char('s')),
            Shortcut::Open => Combo::ctrl(Char('o')),
            Shortcut::NewProject => Combo::ctrl(Char('n')),
            Shortcut::Export => Combo::ctrl(Char('r')),
            Shortcut::Palette => Combo::ctrl(Char('k')),
            Shortcut::Search => Combo::ctrl(Char('f')),
            Shortcut::Settings => Combo::ctrl(Char(',')),
        })
    }

    /// La vue qu'ouvre ce raccourci.
    pub fn view(self) -> Option<View> {
        match self {
            Shortcut::ViewPlaylist => Some(View::Playlist),
            Shortcut::ViewRack => Some(View::Rack),
            Shortcut::ViewPiano => Some(View::Piano),
            Shortcut::ViewSession => Some(View::Session),
            Shortcut::ViewMixer => Some(View::Mixer),
            Shortcut::ViewLab => Some(View::Lab),
            _ => None,
        }
    }

    pub fn for_view(view: View) -> Shortcut {
        match view {
            View::Playlist => Shortcut::ViewPlaylist,
            View::Rack => Shortcut::ViewRack,
            View::Piano => Shortcut::ViewPiano,
            View::Session => Shortcut::ViewSession,
            View::Mixer => Shortcut::ViewMixer,
            View::Lab => Shortcut::ViewLab,
        }
    }
}

/// Table des raccourcis : une combinaison (ou aucune) par action.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    binds: Vec<(Shortcut, Option<Combo>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self { binds: Shortcut::ALL.iter().map(|&s| (s, s.default_combo())).collect() }
    }
}

impl Keymap {
    pub fn get(&self, s: Shortcut) -> Option<Combo> {
        self.binds.iter().find(|(k, _)| *k == s).and_then(|(_, c)| *c)
    }

    /// Libellé de la touche d'une action, vide si elle n'en a pas.
    pub fn label(&self, s: Shortcut) -> String {
        self.get(s).map_or(String::new(), Combo::label)
    }

    pub fn set(&mut self, s: Shortcut, combo: Option<Combo>) {
        if let Some(slot) = self.binds.iter_mut().find(|(k, _)| *k == s) {
            slot.1 = combo;
        }
    }

    /// L'action déjà liée à cette combinaison, autre que `except`.
    pub fn conflict(&self, combo: Combo, except: Shortcut) -> Option<Shortcut> {
        self.binds.iter().find(|(s, c)| *s != except && *c == Some(combo)).map(|(s, _)| *s)
    }

    /// Lie `combo` à `s`. En cas de conflit : `swap` donne l'ancienne touche de
    /// `s` à l'autre action, sinon l'autre action perd la sienne.
    pub fn rebind(&mut self, s: Shortcut, combo: Combo, swap: bool) {
        let old = self.get(s);
        if let Some(other) = self.conflict(combo, s) {
            self.set(other, if swap { old } else { None });
        }
        self.set(s, Some(combo));
    }

    /// L'action déclenchée par une touche.
    pub fn lookup(&self, key: Key, mods: Modifiers) -> Option<Shortcut> {
        self.binds.iter().find(|(_, c)| c.is_some_and(|c| c.matches(key, mods))).map(|(s, _)| *s)
    }

    pub fn is_default(&self, s: Shortcut) -> bool {
        self.get(s) == s.default_combo()
    }

    /// Paires (identifiant, combinaison) qui diffèrent des valeurs par défaut.
    pub fn overrides(&self) -> Vec<(&'static str, String)> {
        self.binds
            .iter()
            .filter(|(s, c)| s.default_combo() != *c)
            .map(|(s, c)| (s.id(), c.map_or(String::new(), Combo::to_text)))
            .collect()
    }

    /// Applique des paires lues dans le fichier ; les inconnues sont ignorées,
    /// une chaîne vide retire la touche. Une touche reprise à une autre action
    /// lui est retirée : jamais deux actions sur la même combinaison.
    pub fn apply_overrides<'a>(&mut self, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) {
        for (id, text) in pairs {
            let Some(s) = Shortcut::from_id(id) else { continue };
            if text.trim().is_empty() {
                self.set(s, None);
            } else if let Some(combo) = Combo::parse(text) {
                self.rebind(s, combo, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };
    const CTRL: Modifiers = Modifiers { shift: false, ctrl: true, alt: false };
    const CTRL_SHIFT: Modifiers = Modifiers { shift: true, ctrl: true, alt: false };

    #[test]
    fn defaults_are_fl_studio_and_unique() {
        let k = Keymap::default();
        assert_eq!(k.lookup(Key::F(6), NONE), Some(Shortcut::ViewRack));
        assert_eq!(k.lookup(Key::F(9), NONE), Some(Shortcut::ViewMixer));
        assert_eq!(k.lookup(Key::Char('s'), CTRL), Some(Shortcut::Save));
        assert_eq!(k.lookup(Key::Char('s'), CTRL_SHIFT), Some(Shortcut::SaveAs));
        assert_eq!(k.lookup(Key::Char('s'), NONE), None);
        // Taille de l'interface : Ctrl =, Ctrl −, Ctrl 0 ; sans Ctrl, + et − changent de pattern.
        assert_eq!(k.lookup(Key::Char('='), CTRL), Some(Shortcut::ZoomIn));
        assert_eq!(k.lookup(Key::Char('='), CTRL_SHIFT), Some(Shortcut::ZoomIn));
        assert_eq!(k.lookup(Key::Char('-'), CTRL), Some(Shortcut::ZoomOut));
        assert_eq!(k.lookup(Key::Char('0'), CTRL), Some(Shortcut::ZoomReset));
        assert_eq!(k.lookup(Key::Char('-'), NONE), Some(Shortcut::PrevPattern));
        for s in Shortcut::ALL {
            if let Some(c) = s.default_combo() {
                assert_eq!(k.conflict(c, s), None, "{s:?}");
            }
            assert_eq!(Shortcut::from_id(s.id()), Some(s));
        }
    }

    #[test]
    fn symbols_ignore_shift() {
        let k = Keymap::default();
        let shift = Modifiers { shift: true, ..NONE };
        assert_eq!(k.lookup(Key::Char('+'), shift), Some(Shortcut::NextPattern));
        assert_eq!(k.lookup(Key::Char('l'), shift), None);
    }

    #[test]
    fn combo_text_round_trip() {
        for text in ["ctrl+shift+s", "alt+f8", "space", "plus", "-", "ctrl+,", "f12", "tab", "ctrl+=", "ctrl+-", "ctrl+0"] {
            let c = Combo::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(Combo::parse(&c.to_text()), Some(c), "{text}");
        }
        assert_eq!(Combo::parse("Ctrl+Maj+S"), Some(Combo::ctrl_shift(Key::Char('s'))));
        assert_eq!(Combo::parse("CTRL+F5"), Some(Combo::ctrl(Key::F(5))));
        for bad in ["", "ctrl+", "hyper+a", "f13", "f0", "abc", "ctrl+ab"] {
            assert_eq!(Combo::parse(bad), None, "{bad}");
        }
        assert_eq!(ponzik_i18n::with_lang(ponzik_i18n::Lang::EN, || Combo::ctrl_shift(Key::Char('s')).label()), "Ctrl Shift S");
        assert_eq!(Combo::alt(Key::F(8)).label(), "Alt F8");
        assert_eq!(Combo::key(Key::Char('-')).label(), "−");
    }

    #[test]
    fn rebind_replaces_or_swaps_on_conflict() {
        let mut k = Keymap::default();
        let f9 = Combo::key(Key::F(9));
        assert_eq!(k.conflict(f9, Shortcut::ViewRack), Some(Shortcut::ViewMixer));
        // Échange : le Mixer prend F6, l'ancienne touche du Channel Rack.
        k.rebind(Shortcut::ViewRack, f9, true);
        assert_eq!(k.get(Shortcut::ViewRack), Some(f9));
        assert_eq!(k.get(Shortcut::ViewMixer), Some(Combo::key(Key::F(6))));
        // Remplacement : l'autre action perd sa touche.
        k.rebind(Shortcut::Metronome, Combo::key(Key::F(6)), false);
        assert_eq!(k.get(Shortcut::ViewMixer), None);
        assert_eq!(k.lookup(Key::F(6), NONE), Some(Shortcut::Metronome));
        assert!(!k.is_default(Shortcut::Metronome));
    }

    #[test]
    fn overrides_round_trip_and_tolerate_junk() {
        let mut k = Keymap::default();
        k.rebind(Shortcut::ViewSession, Combo::key(Key::F(11)), false);
        k.set(Shortcut::TapTempo, None);
        let saved = k.overrides();
        assert_eq!(saved.len(), 2);
        let mut back = Keymap::default();
        back.apply_overrides(saved.iter().map(|(a, b)| (*a, b.as_str())));
        assert_eq!(back, k);
        // Inconnu, illisible : ignorés. Doublon : retiré à l'action précédente.
        let mut j = Keymap::default();
        j.apply_overrides([("nope", "f1"), ("save", "???"), ("undo", "ctrl+s")]);
        assert_eq!(j.get(Shortcut::Undo), Some(Combo::ctrl(Key::Char('s'))));
        assert_eq!(j.get(Shortcut::Save), None);
        assert_eq!(j.lookup(Key::Char('s'), CTRL), Some(Shortcut::Undo));
    }
}
