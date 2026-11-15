//! Fenêtrage maison de Ponzik, sans dépendance.
//!
//! - Linux : on parle le protocole X11 directement sur le socket (pas de Xlib
//!   ni de libxcb), avec MIT-SHM pour envoyer les images sans copie réseau.
//!   Fonctionne aussi sous Wayland via XWayland.
//! - Windows : Win32 (`user32` / `gdi32`).
//!
//! La fenêtre affiche un tampon de pixels `0x00RRGGBB` fourni par l'appelant.

use std::fmt;
use std::time::Duration;

#[cfg(target_os = "linux")]
#[path = "x11.rs"]
mod backend;

#[cfg(windows)]
#[path = "win32.rs"]
mod backend;

#[cfg(not(any(target_os = "linux", windows)))]
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
    /// Caractère imprimable, en minuscule.
    Char(char),
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
    FocusLost,
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
            WindowError::NoDisplay(msg) => write!(f, "aucun affichage disponible : {msg}"),
            WindowError::Protocol(msg) => write!(f, "erreur du serveur graphique : {msg}"),
            WindowError::UnsupportedPlatform => write!(f, "plateforme non supportée (Linux et Windows uniquement)"),
            WindowError::System(msg) => write!(f, "erreur système : {msg}"),
        }
    }
}

impl std::error::Error for WindowError {}

pub struct Window {
    inner: backend::Window,
}

impl Window {
    pub fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        Ok(Self { inner: backend::Window::open(title, width, height)? })
    }

    /// Taille de la zone cliente, en pixels.
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
        assert!(pixels.len() >= (width * height) as usize, "tampon trop petit");
        self.inner.present(pixels, width, height)
    }

    /// Nom du chemin de rendu actif, pour le diagnostic.
    pub fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }
}
