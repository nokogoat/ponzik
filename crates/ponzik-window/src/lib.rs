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
use std::path::PathBuf;
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

#[derive(Clone, Debug, PartialEq)]
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
    /// Files dragged from another application hover the window, at this
    /// position (X11 and macOS; Windows only reports the drop).
    DragHover { x: f32, y: f32 },
    /// The dragged files left the window, or the drag was cancelled.
    DragLeave,
    /// Files dropped on the window from another application.
    FileDrop { x: f32, y: f32, paths: Vec<PathBuf> },
}

/// Paths of a `text/uri-list` (RFC 2483): one URI per line, `#` comments.
/// Only `file:` URIs (local or `localhost`) are kept, percent-decoded; a bare
/// absolute path is taken as is.
pub fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(data);
    text.split(['\r', '\n', '\0'])
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(uri_to_path)
        .collect()
}

/// `file:///home/a%20b.wav` → `/home/a b.wav`.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = if let Some(rest) = strip_prefix_ignore_case(uri, "file://") {
        // An authority: empty or `localhost`, the path starts at the next `/`.
        let slash = rest.find('/')?;
        let host = &rest[..slash];
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        &rest[slash..]
    } else if let Some(rest) = strip_prefix_ignore_case(uri, "file:") {
        // `file:/home/x` (no authority).
        rest.starts_with('/').then_some(rest)?
    } else if uri.starts_with('/') {
        uri
    } else {
        return None;
    };
    let bytes = percent_decode(path);
    // `file:///C:/Music/x.wav`: a drive letter, no leading slash.
    let bytes = match bytes.as_slice() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() && cfg!(windows) => bytes[1..].to_vec(),
        _ => bytes,
    };
    (!bytes.is_empty()).then(|| path_from_bytes(bytes))
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

/// `%XX` escapes to bytes; a malformed escape is kept as written.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(hi), Some(lo)) = (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
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

    /// Comme [`Window::present`], mais seules les zones `rects` (x0, y0, x1, y1,
    /// bornes hautes exclues, en pixels physiques) ont changé depuis l'image
    /// précédente. Sous X11, seules ces zones sont copiées et envoyées ; les autres
    /// systèmes présentent toute l'image.
    pub fn present_rects(
        &mut self,
        pixels: &[u32],
        width: u32,
        height: u32,
        rects: &[(usize, usize, usize, usize)],
    ) -> Result<(), WindowError> {
        assert!(pixels.len() >= (width * height) as usize, "buffer too small");
        if rects.is_empty() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        return self.inner.present_rects(pixels, width, height, Some(rects));
        #[cfg(not(target_os = "linux"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_keeps_local_files_percent_decoded() {
        let list = b"# dragged from the file manager\r\nfile:///home/me/Kick%2001.wav\r\nfile://localhost/tmp/caf%C3%A9.wav\r\n\r\nhttp://example.com/x.wav\r\nfile://other-host/etc/passwd\r\nfile:/srv/loop.wav\r\n/abs/plain.mid\n";
        assert_eq!(
            parse_uri_list(list),
            [
                PathBuf::from("/home/me/Kick 01.wav"),
                PathBuf::from("/tmp/café.wav"),
                PathBuf::from("/srv/loop.wav"),
                PathBuf::from("/abs/plain.mid"),
            ]
        );
    }

    #[test]
    fn uri_list_edge_cases() {
        assert!(parse_uri_list(b"").is_empty());
        assert!(parse_uri_list(b"\0").is_empty());
        // A trailing NUL (some sources add one), no CRLF, an upper-case scheme.
        assert_eq!(parse_uri_list(b"FILE:///a/b.wav\0"), [PathBuf::from("/a/b.wav")]);
        // Malformed escapes stay as written; %2F decodes to a slash.
        assert_eq!(parse_uri_list(b"file:///a/100%25%zz%4"), [PathBuf::from("/a/100%%zz%4")]);
        assert_eq!(parse_uri_list(b"file:///a%2Fb"), [PathBuf::from("/a/b")]);
        // No path after the host, a relative name.
        assert!(parse_uri_list(b"file://localhost\r\nkick.wav").is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn uri_list_keeps_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt;
        let paths = parse_uri_list(b"file:///tmp/%FF.wav");
        assert_eq!(paths[0].as_os_str().as_bytes(), b"/tmp/\xff.wav");
    }
}

