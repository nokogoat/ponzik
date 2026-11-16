//! Fenêtrage maison de Ponzik, sans dépendance.
//!
//! - Linux : on parle le protocole X11 directement sur le socket (pas de Xlib
//!   ni de libxcb), avec MIT-SHM pour envoyer les images sans copie réseau.
//!   Fonctionne aussi sous Wayland via XWayland.
//! - Windows : Win32 (`user32` / `gdi32`).
//! - macOS (expérimental) : Cocoa via le runtime Objective-C, image posée sur
//!   le calque de la vue.
//!
//! La fenêtre affiche un tampon de pixels `0x00RRGGBB` fourni par l'appelant.
//!
//! Tailles et positions de souris sont en pixels physiques ; [`Window::scale_factor`]
//! donne l'échelle du système (1 = 96 ppp, 2 = écran 4K « Retina ») et
//! [`Event::ScaleChanged`] signale qu'elle a changé.

use std::fmt;
use std::time::Duration;

#[cfg(target_os = "linux")]
#[path = "x11.rs"]
mod backend;

#[cfg(windows)]
#[path = "win32.rs"]
mod backend;

#[cfg(target_os = "macos")]
#[path = "cocoa.rs"]
mod backend;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[path = "unsupported.rs"]
mod backend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Space,
    Enter,
    Escape,
    Backspace,
    Tab,
    Delete,
    Left,
    Right,
    Up,
    Down,
    /// Touche de fonction F1 à F12.
    F(u8),
    PageUp,
    PageDown,
    Home,
    End,
    /// Caractère imprimable, en minuscule.
    Char(char),
}

/// Forme du curseur de la souris au-dessus de la fenêtre.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    /// Flèche ordinaire.
    #[default]
    Arrow,
    /// Main : élément cliquable.
    Hand,
    /// Barre de saisie : champ de texte.
    Text,
    /// Double flèche horizontale : bord à étirer, séparateur vertical.
    ResizeH,
    /// Double flèche verticale : séparateur horizontal, valeur à glisser.
    ResizeV,
    /// Double flèche en diagonale : coin à tirer (haut-gauche / bas-droite).
    ResizeNwSe,
    /// Quatre flèches : objet déplaçable.
    Move,
    /// Réticule : pose précise.
    Crosshair,
    /// Action impossible ici.
    NotAllowed,
    /// Objet saisi, en cours de déplacement.
    Grab,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    CloseRequested,
    Resized { width: u32, height: u32 },
    /// La fenêtre doit être redessinée (exposition, retour au premier plan…).
    Redraw,
    MouseMove { x: f32, y: f32 },
    MouseDown { x: f32, y: f32, button: MouseButton, mods: Modifiers },
    MouseUp { x: f32, y: f32, button: MouseButton },
    /// Molette : positif vers le haut, en crans.
    Wheel { x: f32, y: f32, delta: f32, mods: Modifiers },
    MouseLeave,
    KeyDown { key: Key, mods: Modifiers },
    KeyUp { key: Key },
    /// Caractère saisi (disposition du clavier, majuscules et AltGr compris).
    Text(char),
    /// Touche physique enfoncée ou relâchée, par position (code evdev, identique
    /// au code de balayage PC sous Windows, traduit depuis `kVK_*` sous macOS) :
    /// indépendant de la disposition.
    Physical { code: u16, down: bool },
    FocusLost,
    /// L'échelle du système a changé (réglage de l'écran, fenêtre passée sur un
    /// autre moniteur) : pixels physiques par pixel logique.
    ScaleChanged { scale: f32 },
}

#[derive(Debug)]
pub enum WindowError {
    NoDisplay(String),
    Protocol(String),
    UnsupportedPlatform,
    System(String),
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WindowError::NoDisplay(msg) => write!(f, "no display available: {msg}"),
            WindowError::Protocol(msg) => write!(f, "display server error: {msg}"),
            WindowError::UnsupportedPlatform => write!(f, "unsupported platform (Linux, Windows and macOS only)"),
            WindowError::System(msg) => write!(f, "system error: {msg}"),
        }
    }
}

impl std::error::Error for WindowError {}

pub struct Window {
    inner: backend::Window,
}

impl Window {
    /// Ouvre une fenêtre de `width` × `height` pixels logiques : la taille réelle
    /// suit l'échelle du système.
    pub fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        Ok(Self { inner: backend::Window::open(title, width, height)? })
    }

    /// Échelle du système : pixels physiques par pixel logique (1 = 96 ppp).
    pub fn scale_factor(&self) -> f32 {
        self.inner.scale_factor()
    }

    /// Taille de la zone cliente, en pixels physiques.
    pub fn size(&self) -> (u32, u32) {
        self.inner.size()
    }

    /// Récupère les événements en attente sans bloquer.
    pub fn poll_events(&mut self, out: &mut Vec<Event>) {
        self.inner.poll_events(out);
    }

    /// Attend au moins un événement, au plus `timeout`.
    pub fn wait_events(&mut self, timeout: Duration, out: &mut Vec<Event>) {
        self.inner.wait_events(timeout, out);
    }

    /// Affiche `pixels` (`0x00RRGGBB`, lignes de `width` pixels).
    pub fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        assert!(pixels.len() >= (width * height) as usize, "buffer too small");
        self.inner.present(pixels, width, height)
    }

    /// Change la forme du curseur au-dessus de la zone cliente. Peu coûteux :
    /// rien n'est envoyé au système si la forme ne change pas.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.inner.set_cursor(cursor);
    }

    /// Nom du chemin de rendu actif, pour le diagnostic.
    pub fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    /// Native handle, to make other windows its transients.
    pub fn native_handle(&self) -> NativeHandle {
        self.inner.native_handle()
    }
}

/// Native handle of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHandle {
    /// X11 window id.
    X11(u32),
    /// `HWND`.
    Win32(usize),
    /// `NSView*` (the window's content view).
    Cocoa(usize),
    None,
}

/// How to open a [`PluginWindow`].
#[derive(Clone, Debug)]
pub struct PluginWindowOptions {
    pub title: String,
    /// Client area, in the platform's window units (pixels on X11 and Win32,
    /// points on macOS), the unit plugin editors use.
    pub width: u32,
    pub height: u32,
    /// Top-left corner of the frame on the desktop (as given by
    /// [`PluginWindow::position`]); `None` lets the system place it.
    pub position: Option<(i32, i32)>,
    /// Window to stay above (`WM_TRANSIENT_FOR`, owned window, child window).
    pub owner: Option<NativeHandle>,
    pub resizable: bool,
}

/// Plain top-level window hosting a foreign view: a plugin editor creates its
/// own child inside [`PluginWindow::handle`]. Nothing is drawn by Ponzik; only
/// closing and resizing are reported.
pub struct PluginWindow {
    inner: backend::PluginWindow,
}

impl PluginWindow {
    pub fn open(options: &PluginWindowOptions) -> Result<Self, WindowError> {
        Ok(Self { inner: backend::PluginWindow::open_plugin(options)? })
    }

    /// Parent to give the plugin (X11 window, `HWND`, `NSView*`).
    pub fn handle(&self) -> NativeHandle {
        self.inner.native_handle()
    }

    /// Client area size, in the units of [`PluginWindowOptions::width`].
    pub fn size(&self) -> (u32, u32) {
        self.inner.size()
    }

    /// System scale (1 = 96 dpi), for the plugin's own scaling.
    pub fn scale_factor(&self) -> f32 {
        self.inner.scale_factor()
    }

    /// Resizes the client area (the plugin asked for it); no `Resized` event
    /// comes back for this size.
    pub fn set_size(&mut self, width: u32, height: u32) {
        self.inner.set_plugin_size(width, height);
    }

    /// Lets the user resize the window, or locks it to its current size.
    pub fn set_resizable(&mut self, resizable: bool) {
        self.inner.set_resizable(resizable);
    }

    /// Top-left corner of the frame on the desktop, to reopen the window there.
    pub fn position(&mut self) -> Option<(i32, i32)> {
        self.inner.position()
    }

    /// `CloseRequested` and `Resized` (by the user) since the last call.
    pub fn poll_events(&mut self, out: &mut Vec<Event>) {
        self.inner.poll_plugin_events(out);
    }
}

