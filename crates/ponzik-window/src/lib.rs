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
            WindowError::NoDisplay(msg) => write!(f, "aucun affichage disponible : {msg}"),
            WindowError::Protocol(msg) => write!(f, "erreur du serveur graphique : {msg}"),
            WindowError::UnsupportedPlatform => write!(f, "plateforme non supportée (Linux, Windows et macOS uniquement)"),
            WindowError::System(msg) => write!(f, "erreur système : {msg}"),
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
        assert!(pixels.len() >= (width * height) as usize, "tampon trop petit");
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
}
