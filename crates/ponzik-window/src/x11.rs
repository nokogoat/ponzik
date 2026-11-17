//! Client X11 maison : on parle le protocole binaire directement sur le socket Unix.
//!
//! Un thread lit les événements et les réponses du serveur ; le thread principal
//! envoie les requêtes. Les images passent par MIT-SHM (mémoire partagée) quand
//! le serveur le permet, sinon par `PutImage` découpé en bandes.

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Cursor, Event, Key, Modifiers, MouseButton, NativeHandle, PluginWindowOptions, WindowError};

/// A plugin window is a plain window of its own connection.
pub(crate) type PluginWindow = Window;

unsafe extern "C" {
    fn shmget(key: c_int, size: usize, flags: c_int) -> c_int;
    fn shmat(id: c_int, addr: *const c_void, flags: c_int) -> *mut c_void;
    fn shmdt(addr: *const c_void) -> c_int;
    fn shmctl(id: c_int, cmd: c_int, buf: *mut c_void) -> c_int;
}

const IPC_PRIVATE: c_int = 0;
const IPC_CREAT: c_int = 0o1000;
const IPC_RMID: c_int = 0;

const OP_CREATE_WINDOW: u8 = 1;
const OP_CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
const OP_MAP_WINDOW: u8 = 8;
const OP_CONFIGURE_WINDOW: u8 = 12;
const OP_INTERN_ATOM: u8 = 16;
const OP_CONVERT_SELECTION: u8 = 24;
const OP_SEND_EVENT: u8 = 25;
const OP_CHANGE_PROPERTY: u8 = 18;
const OP_GET_PROPERTY: u8 = 20;
const OP_TRANSLATE_COORDINATES: u8 = 40;
const OP_GET_INPUT_FOCUS: u8 = 43;
const OP_OPEN_FONT: u8 = 45;
const OP_CREATE_GC: u8 = 55;
const OP_PUT_IMAGE: u8 = 72;
const OP_CREATE_GLYPH_CURSOR: u8 = 94;
const OP_QUERY_EXTENSION: u8 = 98;
const OP_GET_KEYBOARD_MAPPING: u8 = 101;
const SHM_ATTACH: u8 = 1;
const SHM_DETACH: u8 = 2;
const SHM_PUT_IMAGE: u8 = 3;

const ATOM_ATOM: u32 = 4;
const ATOM_STRING: u32 = 31;
const ATOM_WM_NAME: u32 = 39;
const ATOM_WM_NORMAL_HINTS: u32 = 40;
const ATOM_WM_SIZE_HINTS: u32 = 41;
const ATOM_WM_CLASS: u32 = 67;
const ATOM_WM_TRANSIENT_FOR: u32 = 68;
const ATOM_WINDOW: u32 = 33;

/// Events of a plugin window: only its own geometry (the plugin's child
/// window takes the input).
const PLUGIN_EVENT_MASK: u32 = 0x2_0000; // StructureNotify

const EVENT_MASK: u32 = 0x1 // KeyPress
    | 0x2 // KeyRelease
    | 0x4 // ButtonPress
    | 0x8 // ButtonRelease
    | 0x20 // LeaveWindow
    | 0x40 // PointerMotion
    | 0x8000 // Exposure
    | 0x2_0000 // StructureNotify
    | 0x20_0000; // FocusChange

/// Attribut de fenêtre CWEventMask, et l'événement PropertyChange.
const CW_EVENT_MASK: u32 = 0x800;
const PROPERTY_CHANGE_MASK: u32 = 0x40_0000;
/// Taille maximale lue de la base de ressources, en mots de 4 octets.
const RESOURCES_MAX_WORDS: u32 = 64 * 1024;

const MIN_SIZE: (u32, u32) = (960, 640);
/// Attribut de fenêtre CWCursor.
const CW_CURSOR: u32 = 0x4000;
/// Nombre de formes de [`Cursor`].
const CURSOR_SHAPES: usize = 10;
const BACKGROUND: u32 = 0x0f0f10;

fn proto(msg: impl Into<String>) -> WindowError {
    WindowError::Protocol(msg.into())
}

fn io_err(e: std::io::Error) -> WindowError {
    WindowError::System(format!("socket X11 : {e}"))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn i16_at(b: &[u8], i: usize) -> i16 {
    i16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// Glyphe de la police de curseurs standard (`cursorfont.h`) pour une forme.
/// Le masque est le glyphe suivant.
fn cursor_glyph(cursor: Cursor) -> u16 {
    match cursor {
        Cursor::Arrow => 68,      // XC_left_ptr
        Cursor::Hand => 60,       // XC_hand2
        Cursor::Text => 152,      // XC_xterm
        Cursor::ResizeH => 108,   // XC_sb_h_double_arrow
        Cursor::ResizeV => 116,   // XC_sb_v_double_arrow
        Cursor::ResizeNwSe => 14, // XC_bottom_right_corner
        Cursor::Move => 52,       // XC_fleur
        Cursor::Crosshair => 34,  // XC_crosshair
        Cursor::NotAllowed => 0,  // XC_X_cursor
        Cursor::Grab => 58,       // XC_hand1 : main pleine, distincte de la main à cliquer
    }
}

fn cursor_index(cursor: Cursor) -> usize {
    match cursor {
        Cursor::Arrow => 0,
        Cursor::Hand => 1,
        Cursor::Text => 2,
        Cursor::ResizeH => 3,
        Cursor::ResizeV => 4,
        Cursor::Move => 5,
        Cursor::Crosshair => 6,
        Cursor::NotAllowed => 7,
        Cursor::Grab => 8,
        Cursor::ResizeNwSe => 9,
    }
}

/// OpenFont : ouvre la police `name` sous l'identifiant `fid`.
fn open_font_req(fid: u32, name: &str) -> Vec<u8> {
    let mut r = Req::new(OP_OPEN_FONT, 0);
    r.u32(fid).u16(name.len() as u16).u16(0).bytes(name.as_bytes());
    r.finish().to_vec()
}

/// CreateGlyphCursor : curseur noir bordé de blanc tiré de la police `font`.
fn glyph_cursor_req(cid: u32, font: u32, glyph: u16) -> Vec<u8> {
    let mut r = Req::new(OP_CREATE_GLYPH_CURSOR, 0);
    r.u32(cid).u32(font).u32(font).u16(glyph).u16(glyph + 1);
    r.u16(0).u16(0).u16(0); // avant-plan : noir
    r.u16(0xffff).u16(0xffff).u16(0xffff); // fond : blanc
    r.finish().to_vec()
}

/// ChangeWindowAttributes limité au curseur.
fn window_cursor_req(window: u32, cursor: u32) -> Vec<u8> {
    let mut r = Req::new(OP_CHANGE_WINDOW_ATTRIBUTES, 0);
    r.u32(window).u32(CW_CURSOR).u32(cursor);
    r.finish().to_vec()
}

/// GetProperty : toute la valeur de `prop` sur `window`, quel que soit son type.
fn get_property_req(window: u32, prop: u32) -> Vec<u8> {
    let mut r = Req::new(OP_GET_PROPERTY, 0);
    r.u32(window).u32(prop).u32(0).u32(0).u32(RESOURCES_MAX_WORDS);
    r.finish().to_vec()
}

/// GetProperty that deletes the property once read (a selection transfer).
fn take_property_req(window: u32, prop: u32) -> Vec<u8> {
    let mut r = Req::new(OP_GET_PROPERTY, 1);
    r.u32(window).u32(prop).u32(0).u32(0).u32(RESOURCES_MAX_WORDS);
    r.finish().to_vec()
}

// --- XDND: files dragged from other applications --------------------------------
//
// The source sends ClientMessages to our window: XdndEnter (the types on
// offer), XdndPosition (root coordinates, each answered by an XdndStatus),
// XdndLeave or XdndDrop. On a drop we ask the XdndSelection for
// `text/uri-list`; the server answers with SelectionNotify once the data sits
// in a property of our window, and we close with XdndFinished.

/// Protocol version we speak (and announce in `XdndAware`).
const XDND_VERSION: u32 = 5;

/// Atoms of the XDND protocol (all zero on windows that do not take drops).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct XdndAtoms {
    aware: u32,
    enter: u32,
    position: u32,
    status: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    selection: u32,
    action_copy: u32,
    uri_list: u32,
    /// Property of our window that receives the dropped data.
    property: u32,
}

impl XdndAtoms {
    const NAMES: [&'static str; 11] = [
        "XdndAware",
        "XdndEnter",
        "XdndPosition",
        "XdndStatus",
        "XdndLeave",
        "XdndDrop",
        "XdndFinished",
        "XdndSelection",
        "XdndActionCopy",
        "text/uri-list",
        "PONZIK_XDND_DATA",
    ];

    fn from_list(a: [u32; 11]) -> Self {
        let [aware, enter, position, status, leave, drop, finished, selection, action_copy, uri_list, property] = a;
        Self { aware, enter, position, status, leave, drop, finished, selection, action_copy, uri_list, property }
    }
}

/// An XDND message from the drag source.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Xdnd {
    /// `files`: `text/uri-list` is on offer (or the source has more than three
    /// types and keeps them in `XdndTypeList`: we try anyway).
    Enter { source: u32, version: u32, files: bool },
    /// Pointer in root coordinates.
    Position { source: u32, x: i16, y: i16, time: u32 },
    Leave { source: u32 },
    Drop { source: u32, time: u32 },
}

/// Reads an XDND ClientMessage (`b`: the 32-byte event).
fn parse_xdnd(b: &[u8; 32], atoms: &XdndAtoms) -> Option<Xdnd> {
    if b[0] & 0x7f != 33 || b[1] != 32 || atoms.enter == 0 {
        return None;
    }
    let ty = u32_at(b, 8);
    let l = |i: usize| u32_at(b, 12 + 4 * i);
    Some(if ty == atoms.enter {
        let more = l(1) & 1 != 0;
        Xdnd::Enter { source: l(0), version: l(1) >> 24, files: more || (2..5).any(|i| l(i) == atoms.uri_list) }
    } else if ty == atoms.position {
        Xdnd::Position { source: l(0), x: (l(2) >> 16) as u16 as i16, y: l(2) as u16 as i16, time: l(3) }
    } else if ty == atoms.leave {
        Xdnd::Leave { source: l(0) }
    } else if ty == atoms.drop {
        Xdnd::Drop { source: l(0), time: l(2) }
    } else {
        return None;
    })
}

/// A ClientMessage event (format 32) for `window`.
fn client_message(window: u32, ty: u32, data: [u32; 5]) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = 33;
    e[1] = 32;
    e[4..8].copy_from_slice(&window.to_le_bytes());
    e[8..12].copy_from_slice(&ty.to_le_bytes());
    for (i, v) in data.iter().enumerate() {
        e[12 + 4 * i..16 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    e
}

/// SendEvent of `event` to `dest`, with no event mask (straight to its owner).
fn send_event_req(dest: u32, event: &[u8; 32]) -> Vec<u8> {
    let mut r = Req::new(OP_SEND_EVENT, 0);
    r.u32(dest).u32(0).bytes(event);
    r.finish().to_vec()
}

/// XdndStatus: whether we take the drop here, and the action (copy).
fn xdnd_status(atoms: &XdndAtoms, ours: u32, source: u32, accept: bool) -> Vec<u8> {
    // Bit 1: send a position for every move, our targets change all over the window.
    let flags = accept as u32 | 2;
    let action = if accept { atoms.action_copy } else { 0 };
    send_event_req(source, &client_message(source, atoms.status, [ours, flags, 0, 0, action]))
}

/// XdndFinished: the drop is over (`accepted` with the copy action).
fn xdnd_finished(atoms: &XdndAtoms, ours: u32, source: u32, accepted: bool) -> Vec<u8> {
    let action = if accepted { atoms.action_copy } else { 0 };
    send_event_req(source, &client_message(source, atoms.finished, [ours, accepted as u32, action, 0, 0]))
}

/// ConvertSelection: the selection `selection` as `target`, into `property` of `requestor`.
fn convert_selection_req(requestor: u32, selection: u32, target: u32, property: u32, time: u32) -> Vec<u8> {
    let mut r = Req::new(OP_CONVERT_SELECTION, 0);
    r.u32(requestor).u32(selection).u32(target).u32(property).u32(time);
    r.finish().to_vec()
}

/// A drag in progress over our window.
#[derive(Debug)]
struct DragIn {
    source: u32,
    version: u32,
    accept: bool,
    /// Our window's origin on the root, to turn root positions into ours.
    origin: Option<(i32, i32)>,
    pos: (f32, f32),
    /// Dropped: the data was asked for at this time.
    dropped: Option<Instant>,
}

/// How long a source may take to hand the dropped data over.
const DROP_WAIT: Duration = Duration::from_secs(3);

/// Valeur d'une réponse GetProperty en octets (vide si la propriété n'existe pas).
fn property_value(reply: &[u8]) -> &[u8] {
    if reply.len() < 32 || reply[1] == 0 {
        return &[];
    }
    let len = u32_at(reply, 16) as usize * (reply[1] as usize / 8);
    &reply[32..(32 + len).min(reply.len())]
}

/// Échelle tirée de la base de ressources X (`RESOURCE_MANAGER`) : `Xft.dpi: 192` → 2.
/// La dernière définition l'emporte, comme dans xrdb.
fn scale_from_resources(resources: &str) -> Option<f32> {
    resources
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == "Xft.dpi").then(|| value.trim().parse::<f32>().ok()).flatten()
        })
        .next_back()
        .and_then(|dpi| sane_scale(dpi / 96.0))
}

/// Repli sans Xft.dpi : `GDK_SCALE` (entier) puis `QT_SCALE_FACTOR`.
fn scale_from_env(var: impl Fn(&str) -> Option<String>) -> Option<f32> {
    ["GDK_SCALE", "QT_SCALE_FACTOR"].into_iter().find_map(|name| sane_scale(var(name)?.trim().parse().ok()?))
}

fn sane_scale(scale: f32) -> Option<f32> {
    (scale.is_finite() && scale > 0.0).then(|| scale.clamp(0.5, 4.0))
}

fn system_scale(resources: &[u8]) -> f32 {
    scale_from_resources(&String::from_utf8_lossy(resources))
        .or_else(|| scale_from_env(|name| std::env::var(name).ok()))
        .unwrap_or(1.0)
}

/// Constructeur de requête : longueur en mots de 4 octets écrite à la fin.
struct Req(Vec<u8>);

impl Req {
    fn new(opcode: u8, data: u8) -> Self {
        Req(vec![opcode, data, 0, 0])
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i16(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self.0.resize(pad4(self.0.len()), 0);
        self
    }
    fn finish(&mut self) -> &[u8] {
        self.0.resize(pad4(self.0.len()), 0);
        let words = (self.0.len() / 4) as u16;
        self.0[2..4].copy_from_slice(&words.to_le_bytes());
        &self.0
    }
}

enum Incoming {
    Event(Event),
    ShmCompletion,
    /// Réponse complète (en-tête de 32 octets compris).
    Reply(Vec<u8>),
    /// La base de ressources X (donc peut-être Xft.dpi) a changé.
    ResourcesChanged,
    /// A message of a drag from another application.
    Xdnd(Xdnd),
    /// SelectionNotify: the requested selection is in `property` (0: refused).
    Selection { property: u32 },
    Error { code: u8, major: u8 },
    Closed,
}

struct Keymap {
    min_keycode: u8,
    per_keycode: usize,
    syms: Vec<u32>,
}

impl Keymap {
    fn sym(&self, keycode: u8, column: usize) -> Option<u32> {
        let i = (keycode.checked_sub(self.min_keycode)? as usize) * self.per_keycode + column;
        self.syms.get(i).copied().filter(|&s| s != 0 && column < self.per_keycode)
    }

    fn key(&self, keycode: u8) -> Option<Key> {
        // Rangée des chiffres en AZERTY : « à » seul, « 0 » avec Maj. Comme sous
        // Windows (VK_0…VK_9), la touche est rapportée par son chiffre.
        if let Some(digit @ 0x30..=0x39) = self.sym(keycode, 1) {
            return Some(Key::Char(digit as u8 as char));
        }
        keysym_to_key(self.sym(keycode, 0)?)
    }

    /// Caractère produit par la touche selon Maj, Verr. Maj et AltGr (Mod5).
    fn text(&self, keycode: u8, state: u16) -> Option<char> {
        let shift = state & 0x1 != 0;
        let caps = state & 0x2 != 0;
        let altgr = state & 0x80 != 0;
        let base = if altgr { 4 } else { 0 };
        let sym = if shift { self.sym(keycode, base + 1).or_else(|| self.sym(keycode, base)) } else { self.sym(keycode, base) }
            .or_else(|| self.sym(keycode, 0))?;
        let c = match sym {
            0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym)?,
            0x0100_0000..=0x0110_ffff => char::from_u32(sym - 0x0100_0000)?,
            0x20ac => '€',
            _ => return None,
        };
        Some(if caps && !shift { c.to_uppercase().next()? } else { c })
    }
}

fn keysym_to_key(sym: u32) -> Option<Key> {
    Some(match sym {
        0x20 => Key::Space,
        0xff0d | 0xff8d => Key::Enter,
        0xff1b => Key::Escape,
        0xff08 => Key::Backspace,
        0xff09 => Key::Tab,
        0xffff => Key::Delete,
        0xff51 => Key::Left,
        0xff52 => Key::Up,
        0xff53 => Key::Right,
        0xff54 => Key::Down,
        0xffbe..=0xffc9 => Key::F((sym - 0xffbe + 1) as u8),
        0xff55 => Key::PageUp,
        0xff56 => Key::PageDown,
        0xff50 => Key::Home,
        0xff57 => Key::End,
        0xffab => Key::Char('+'),
        0xffad => Key::Char('-'),
        0x21..=0x7e | 0xa0..=0xff => Key::Char(char::from_u32(sym)?.to_lowercase().next()?),
        _ => return None,
    })
}

fn modifiers(state: u16) -> Modifiers {
    Modifiers { shift: state & 0x1 != 0, ctrl: state & 0x4 != 0, alt: state & 0x8 != 0 }
}

struct ReaderCtx {
    keymap: Keymap,
    root: u32,
    resource_manager: u32,
    wm_protocols: u32,
    wm_delete: u32,
    shm_event: Option<u8>,
    xdnd: XdndAtoms,
}

fn parse_event(code: u8, b: &[u8; 32], ctx: &ReaderCtx) -> Option<Event> {
    let pos = || (i16_at(b, 24) as f32, i16_at(b, 26) as f32);
    let mods = || modifiers(u16_at(b, 28));
    Some(match code {
        2 => Event::KeyDown { key: ctx.keymap.key(b[1])?, mods: mods() },
        3 => Event::KeyUp { key: ctx.keymap.key(b[1])? },
        4 => {
            let (x, y) = pos();
            match b[1] {
                1 => Event::MouseDown { x, y, button: MouseButton::Left, mods: mods() },
                2 => Event::MouseDown { x, y, button: MouseButton::Middle, mods: mods() },
                3 => Event::MouseDown { x, y, button: MouseButton::Right, mods: mods() },
                4 => Event::Wheel { x, y, delta: 1.0, mods: mods() },
                5 => Event::Wheel { x, y, delta: -1.0, mods: mods() },
                _ => return None,
            }
        }
        5 => {
            let (x, y) = pos();
            let button = match b[1] {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                _ => return None,
            };
            Event::MouseUp { x, y, button }
        }
        6 => {
            let (x, y) = pos();
            Event::MouseMove { x, y }
        }
        8 => Event::MouseLeave,
        10 => Event::FocusLost,
        12 if u16_at(b, 16) == 0 => Event::Redraw,
        22 => Event::Resized { width: u16_at(b, 20) as u32, height: u16_at(b, 22) as u32 },
        33 if u32_at(b, 8) == ctx.wm_protocols && u32_at(b, 12) == ctx.wm_delete => Event::CloseRequested,
        _ => return None,
    })
}

fn reader_loop(mut stream: UnixStream, tx: Sender<Incoming>, ctx: ReaderCtx) {
    let mut b = [0u8; 32];
    loop {
        if stream.read_exact(&mut b).is_err() {
            let _ = tx.send(Incoming::Closed);
            return;
        }
        let msg = match b[0] {
            0 => Some(Incoming::Error { code: b[1], major: b[10] }),
            1 | 35 => {
                // Réponse ou événement générique : longueur additionnelle à lire.
                let extra = u32_at(&b, 4) as usize * 4;
                let mut full = b.to_vec();
                full.resize(32 + extra, 0);
                if stream.read_exact(&mut full[32..]).is_err() {
                    let _ = tx.send(Incoming::Closed);
                    return;
                }
                (b[0] == 1).then_some(Incoming::Reply(full))
            }
            // PropertyNotify sur la racine : seul RESOURCE_MANAGER nous intéresse.
            28 | 156 => (u32_at(&b, 4) == ctx.root && u32_at(&b, 8) == ctx.resource_manager)
                .then_some(Incoming::ResourcesChanged),
            c => {
                let code = c & 0x7f;
                if Some(code) == ctx.shm_event {
                    Some(Incoming::ShmCompletion)
                } else if let Some(m) = parse_xdnd(&b, &ctx.xdnd) {
                    Some(Incoming::Xdnd(m))
                } else if code == 31 {
                    // SelectionNotify: requestor, selection, target, property.
                    (u32_at(&b, 12) == ctx.xdnd.selection && ctx.xdnd.selection != 0)
                        .then(|| Incoming::Selection { property: u32_at(&b, 20) })
                } else {
                    // Position physique de la touche (code X11 = code evdev + 8).
                    if (code == 2 || code == 3)
                        && b[1] >= 8
                        && tx.send(Incoming::Event(Event::Physical { code: b[1] as u16 - 8, down: code == 2 })).is_err()
                    {
                        return;
                    }
                    // Saisie de texte, sauf quand Ctrl ou Alt font un raccourci.
                    let state = u16_at(&b, 28);
                    if code == 2
                        && state & 0xc == 0
                        && let Some(ch) = ctx.keymap.text(b[1], state)
                        && tx.send(Incoming::Event(Event::Text(ch))).is_err()
                    {
                        return;
                    }
                    parse_event(code, &b, &ctx).map(Incoming::Event)
                }
            }
        };
        if let Some(msg) = msg
            && tx.send(msg).is_err()
        {
            return;
        }
    }
}

struct Setup {
    id_base: u32,
    id_mask: u32,
    max_request_words: usize,
    min_keycode: u8,
    max_keycode: u8,
    root: u32,
    root_visual: u32,
    depth: u8,
    screen_size: (u32, u32),
}

/// Cookie MIT-MAGIC-COOKIE-1 depuis le fichier Xauthority, s'il existe.
fn read_xauth(display: &str) -> Option<Vec<u8>> {
    let path = std::env::var_os("XAUTHORITY")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".Xauthority")))?;
    let data = std::fs::read(path).ok()?;
    let mut i = 0;
    let field = |i: &mut usize| -> Option<Vec<u8>> {
        let len = u16::from_be_bytes([*data.get(*i)?, *data.get(*i + 1)?]) as usize;
        let v = data.get(*i + 2..*i + 2 + len)?.to_vec();
        *i += 2 + len;
        Some(v)
    };
    while i + 2 <= data.len() {
        i += 2; // famille
        let _address = field(&mut i)?;
        let number = field(&mut i)?;
        let name = field(&mut i)?;
        let cookie = field(&mut i)?;
        if name == b"MIT-MAGIC-COOKIE-1" && (number.is_empty() || number == display.as_bytes()) {
            return Some(cookie);
        }
    }
    None
}

fn connect() -> Result<(UnixStream, Setup), WindowError> {
    let display = std::env::var("DISPLAY").map_err(|_| WindowError::NoDisplay("DISPLAY not set".into()))?;
    let number = display
        .rsplit(':')
        .next()
        .and_then(|s| s.split('.').next())
        .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| WindowError::NoDisplay(format!("DISPLAY invalide : {display}")))?
        .to_string();
    let path = format!("/tmp/.X11-unix/X{number}");
    let mut stream =
        UnixStream::connect(&path).map_err(|e| WindowError::NoDisplay(format!("{path} : {e}")))?;

    let cookie = read_xauth(&number);
    let auth_name: &[u8] = if cookie.is_some() { b"MIT-MAGIC-COOKIE-1" } else { b"" };
    let auth_data = cookie.unwrap_or_default();
    let mut hello = vec![b'l', 0];
    hello.extend_from_slice(&11u16.to_le_bytes());
    hello.extend_from_slice(&0u16.to_le_bytes());
    hello.extend_from_slice(&(auth_name.len() as u16).to_le_bytes());
    hello.extend_from_slice(&(auth_data.len() as u16).to_le_bytes());
    hello.extend_from_slice(&[0, 0]);
    hello.extend_from_slice(auth_name);
    hello.resize(pad4(hello.len()), 0);
    hello.extend_from_slice(&auth_data);
    hello.resize(pad4(hello.len()), 0);
    stream.write_all(&hello).map_err(io_err)?;

    let mut head = [0u8; 8];
    stream.read_exact(&mut head).map_err(io_err)?;
    let mut d = vec![0u8; u16_at(&head, 6) as usize * 4];
    stream.read_exact(&mut d).map_err(io_err)?;
    match head[0] {
        1 => {}
        0 => {
            let reason = String::from_utf8_lossy(&d[..(head[1] as usize).min(d.len())]).into_owned();
            return Err(proto(format!("connection refused: {reason}")));
        }
        _ => return Err(proto("the server asks for an unsupported authentication")),
    }

    let vendor_len = u16_at(&d, 16) as usize;
    let num_formats = d[21] as usize;
    if d[22] != 0 {
        return Err(proto("unsupported image byte order (MSB)"));
    }
    let formats_at = 32 + pad4(vendor_len);
    let screen = formats_at + 8 * num_formats;
    let root = u32_at(&d, screen);
    let root_visual = u32_at(&d, screen + 32);
    let depth = d[screen + 38];

    let bpp = (0..num_formats).map(|i| &d[formats_at + 8 * i..]).find(|f| f[0] == depth).map(|f| f[1]);
    if bpp != Some(32) {
        return Err(proto(format!("unsupported {depth}-bit depth (24/32 bits needed)")));
    }
    // Vérifie que le visuel racine est un TrueColor 0xRRGGBB.
    let mut at = screen + 40;
    let mut masks = None;
    for _ in 0..d[screen + 39] {
        let visuals = u16_at(&d, at + 2) as usize;
        at += 8;
        for v in 0..visuals {
            let vis = &d[at + 24 * v..];
            if u32_at(vis, 0) == root_visual {
                masks = Some((u32_at(vis, 8), u32_at(vis, 12), u32_at(vis, 16)));
            }
        }
        at += 24 * visuals;
    }
    if masks != Some((0xff0000, 0xff00, 0xff)) {
        return Err(proto("unsupported visual (8-bit RGB TrueColor needed)"));
    }

    let setup = Setup {
        id_base: u32_at(&d, 4),
        id_mask: u32_at(&d, 8),
        max_request_words: u16_at(&d, 18) as usize,
        min_keycode: d[26],
        max_keycode: d[27],
        root,
        root_visual,
        depth,
        screen_size: (u16_at(&d, screen + 20) as u32, u16_at(&d, screen + 22) as u32),
    };
    Ok((stream, setup))
}

/// Aller-retour synchrone, uniquement avant le lancement du thread lecteur.
fn roundtrip(stream: &mut UnixStream, req: &[u8]) -> Result<Vec<u8>, WindowError> {
    stream.write_all(req).map_err(io_err)?;
    let mut b = [0u8; 32];
    loop {
        stream.read_exact(&mut b).map_err(io_err)?;
        match b[0] {
            0 => return Err(proto(format!("error {} on request {}", b[1], b[10]))),
            1 => {
                let mut reply = b.to_vec();
                reply.resize(32 + u32_at(&b, 4) as usize * 4, 0);
                stream.read_exact(&mut reply[32..]).map_err(io_err)?;
                return Ok(reply);
            }
            _ => {} // aucun événement attendu à ce stade
        }
    }
}

fn intern_atom(stream: &mut UnixStream, name: &str) -> Result<u32, WindowError> {
    let mut r = Req::new(OP_INTERN_ATOM, 0);
    r.u16(name.len() as u16).u16(0).bytes(name.as_bytes());
    Ok(u32_at(&roundtrip(stream, r.finish())?, 8))
}

/// Taille d'une requête ShmPutImage, en octets.
const SHM_PUT_IMAGE_LEN: usize = 40;
/// Octet `send_event` d'une requête ShmPutImage : en-tête (4), drawable et gc
/// (8), tailles et zone source (12), destination (4), depth et format (2).
const SHM_SEND_EVENT: usize = 30;

struct Shm {
    seg: u32,
    addr: *mut u8,
    size: usize,
}

pub(crate) struct Window {
    stream: UnixStream,
    rx: Receiver<Incoming>,
    pending: VecDeque<Event>,
    setup: Setup,
    next_id: u32,
    window: u32,
    gc: u32,
    size: (u32, u32),
    shm_major: Option<u8>,
    shm: Option<Shm>,
    shm_busy: bool,
    shm_failed: bool,
    /// Taille de la dernière image complète copiée dans le segment : une image
    /// partielle n'y copie que ses zones, le reste doit déjà être à jour.
    shm_image: (u32, u32),
    /// Police « cursor » (0 tant qu'elle n'est pas ouverte), curseurs déjà créés
    /// (0 = pas encore) et forme appliquée à la fenêtre.
    cursor_font: u32,
    cursors: [u32; CURSOR_SHAPES],
    cursor: Option<Cursor>,
    /// Échelle du système (Xft.dpi / 96) et atome de la base de ressources.
    scale: f32,
    resource_manager: u32,
    resources_changed: bool,
    /// Plugin window: `_NET_FRAME_EXTENTS` atom (0 for the main window) and
    /// whether the user may resize it.
    frame_extents: u32,
    resizable: bool,
    /// Size asked by `set_plugin_size` and when, until the server confirms it.
    awaiting: Option<((u32, u32), Instant)>,
    /// XDND atoms (zero on plugin windows), the drag over the window and the
    /// messages waiting to be handled outside the reply loop.
    xdnd: XdndAtoms,
    drag: Option<DragIn>,
    xdnd_inbox: VecDeque<Incoming>,
}

/// How long the sizes reported after our own resize are taken as stale.
const RESIZE_WAIT: Duration = Duration::from_millis(500);

/// `WM_NORMAL_HINTS` of a plugin window: position given by the user (so the
/// window manager keeps it), fixed size unless resizable.
fn plugin_hints(position: Option<(i32, i32)>, size: (u32, u32), resizable: bool) -> Vec<u8> {
    let mut hints = [0u32; 18];
    if let Some((x, y)) = position {
        hints[0] |= 1 | 4; // USPosition | PPosition
        hints[1] = x as u32;
        hints[2] = y as u32;
    }
    hints[3] = size.0;
    hints[4] = size.1;
    if !resizable {
        hints[0] |= 16 | 32; // PMinSize | PMaxSize
        hints[5] = size.0;
        hints[6] = size.1;
        hints[7] = size.0;
        hints[8] = size.1;
    }
    hints.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// ConfigureWindow limited to the size.
fn resize_req(window: u32, width: u32, height: u32) -> Vec<u8> {
    let mut r = Req::new(OP_CONFIGURE_WINDOW, 0);
    r.u32(window).u16(0x4 | 0x8).u16(0).u32(width.max(1)).u32(height.max(1));
    r.finish().to_vec()
}

impl Window {
    pub(crate) fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        let (mut stream, setup) = connect()?;

        let wm_protocols = intern_atom(&mut stream, "WM_PROTOCOLS")?;
        let wm_delete = intern_atom(&mut stream, "WM_DELETE_WINDOW")?;
        let net_wm_name = intern_atom(&mut stream, "_NET_WM_NAME")?;
        let utf8_string = intern_atom(&mut stream, "UTF8_STRING")?;
        let resource_manager = intern_atom(&mut stream, "RESOURCE_MANAGER")?;
        let resources = roundtrip(&mut stream, &get_property_req(setup.root, resource_manager))?;
        let scale = system_scale(property_value(&resources));
        let px = |v: u32| (v as f32 * scale).round() as u32;
        let mut atoms = [0; 11];
        for (atom, name) in atoms.iter_mut().zip(XdndAtoms::NAMES) {
            *atom = intern_atom(&mut stream, name)?;
        }
        let xdnd = XdndAtoms::from_list(atoms);
        let (width, height) = (px(width), px(height));

        let count = setup.max_keycode - setup.min_keycode + 1;
        let mut r = Req::new(OP_GET_KEYBOARD_MAPPING, 0);
        r.u8(setup.min_keycode).u8(count).u16(0);
        let reply = roundtrip(&mut stream, r.finish())?;
        let keymap = Keymap {
            min_keycode: setup.min_keycode,
            per_keycode: reply[1].max(1) as usize,
            syms: reply[32..].chunks_exact(4).map(|c| u32_at(c, 0)).collect(),
        };

        let mut r = Req::new(OP_QUERY_EXTENSION, 0);
        r.u16(7).u16(0).bytes(b"MIT-SHM");
        let reply = roundtrip(&mut stream, r.finish())?;
        let (shm_major, shm_event) = if reply[8] != 0 { (Some(reply[9]), Some(reply[10])) } else { (None, None) };

        let mut win = Window {
            rx: mpsc::channel().1,
            stream,
            pending: VecDeque::new(),
            next_id: 1,
            window: 0,
            gc: 0,
            size: (width, height),
            shm_major,
            shm: None,
            shm_busy: false,
            shm_failed: false,
            shm_image: (0, 0),
            cursor_font: 0,
            cursors: [0; CURSOR_SHAPES],
            cursor: None,
            scale,
            resource_manager,
            resources_changed: false,
            frame_extents: 0,
            resizable: true,
            awaiting: None,
            xdnd: XdndAtoms::default(),
            drag: None,
            xdnd_inbox: VecDeque::new(),
            setup,
        };
        win.window = win.alloc_id();
        win.gc = win.alloc_id();

        let mut r = Req::new(OP_CREATE_WINDOW, win.setup.depth);
        r.u32(win.window)
            .u32(win.setup.root)
            .i16(0)
            .i16(0)
            .u16(width as u16)
            .u16(height as u16)
            .u16(0)
            .u16(1) // InputOutput
            .u32(win.setup.root_visual)
            .u32(0x2 | 0x800) // BackPixel | EventMask
            .u32(BACKGROUND)
            .u32(EVENT_MASK);
        win.send(r.finish())?;

        let ascii: String = title.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect();
        win.set_property(ATOM_WM_NAME, ATOM_STRING, 8, ascii.as_bytes())?;
        win.set_property(net_wm_name, utf8_string, 8, title.as_bytes())?;
        win.set_property(ATOM_WM_CLASS, ATOM_STRING, 8, b"ponzik\0Ponzik\0")?;
        win.set_property(wm_protocols, ATOM_ATOM, 32, &wm_delete.to_le_bytes())?;
        // Files can be dropped here from other applications.
        win.xdnd = xdnd;
        win.set_property(xdnd.aware, ATOM_ATOM, 32, &XDND_VERSION.to_le_bytes())?;
        let mut hints = [0u32; 18];
        hints[0] = 16; // PMinSize
        hints[5] = px(MIN_SIZE.0);
        hints[6] = px(MIN_SIZE.1);
        let hints: Vec<u8> = hints.iter().flat_map(|v| v.to_le_bytes()).collect();
        win.set_property(ATOM_WM_NORMAL_HINTS, ATOM_WM_SIZE_HINTS, 32, &hints)?;

        // Écoute la base de ressources de la racine : un changement de Xft.dpi
        // (réglage d'échelle du bureau) arrive sans redémarrer.
        let mut r = Req::new(OP_CHANGE_WINDOW_ATTRIBUTES, 0);
        r.u32(win.setup.root).u32(CW_EVENT_MASK).u32(PROPERTY_CHANGE_MASK);
        win.send(r.finish())?;

        let mut r = Req::new(OP_CREATE_GC, 0);
        r.u32(win.gc).u32(win.window).u32(0x1_0000).u32(0); // GraphicsExposures = faux
        win.send(r.finish())?;
        let mut r = Req::new(OP_MAP_WINDOW, 0);
        r.u32(win.window);
        win.send(r.finish())?;

        let (tx, rx) = mpsc::channel();
        win.rx = rx;
        let reader = win.stream.try_clone().map_err(io_err)?;
        let ctx = ReaderCtx { keymap, root: win.setup.root, resource_manager, wm_protocols, wm_delete, shm_event, xdnd };
        thread::Builder::new()
            .name("ponzik-x11".into())
            .spawn(move || reader_loop(reader, tx, ctx))
            .map_err(|e| WindowError::System(format!("thread X11 : {e}")))?;

        if win.shm_major.is_some() {
            let (sw, sh) = win.setup.screen_size;
            let size = (sw.max(width) * sh.max(height) * 4) as usize;
            win.shm = win.create_shm(size);
        }
        Ok(win)
    }

    /// Plain top-level window on its own connection, for a plugin editor.
    pub(crate) fn open_plugin(o: &PluginWindowOptions) -> Result<Self, WindowError> {
        let (mut stream, setup) = connect()?;
        let wm_protocols = intern_atom(&mut stream, "WM_PROTOCOLS")?;
        let wm_delete = intern_atom(&mut stream, "WM_DELETE_WINDOW")?;
        let net_wm_name = intern_atom(&mut stream, "_NET_WM_NAME")?;
        let utf8_string = intern_atom(&mut stream, "UTF8_STRING")?;
        let frame_extents = intern_atom(&mut stream, "_NET_FRAME_EXTENTS")?;
        let resource_manager = intern_atom(&mut stream, "RESOURCE_MANAGER")?;
        let resources = roundtrip(&mut stream, &get_property_req(setup.root, resource_manager))?;
        let scale = system_scale(property_value(&resources));
        let size = (o.width.clamp(1, 16_384), o.height.clamp(1, 16_384));
        let keymap = Keymap { min_keycode: setup.min_keycode, per_keycode: 1, syms: Vec::new() };
        let mut win = Window {
            rx: mpsc::channel().1,
            stream,
            pending: VecDeque::new(),
            next_id: 1,
            window: 0,
            gc: 0,
            size,
            shm_major: None,
            shm: None,
            shm_busy: false,
            shm_failed: false,
            shm_image: (0, 0),
            cursor_font: 0,
            cursors: [0; CURSOR_SHAPES],
            cursor: None,
            scale,
            resource_manager,
            resources_changed: false,
            frame_extents,
            resizable: o.resizable,
            awaiting: None,
            xdnd: XdndAtoms::default(),
            drag: None,
            xdnd_inbox: VecDeque::new(),
            setup,
        };
        win.window = win.alloc_id();
        let (x, y) = o.position.unwrap_or((0, 0));
        let mut r = Req::new(OP_CREATE_WINDOW, win.setup.depth);
        r.u32(win.window)
            .u32(win.setup.root)
            .i16(x.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .i16(y.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .u16(size.0 as u16)
            .u16(size.1 as u16)
            .u16(0)
            .u16(1) // InputOutput
            .u32(win.setup.root_visual)
            .u32(0x2 | 0x800) // BackPixel | EventMask
            .u32(BACKGROUND)
            .u32(PLUGIN_EVENT_MASK);
        win.send(r.finish())?;
        let ascii: String = o.title.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect();
        win.set_property(ATOM_WM_NAME, ATOM_STRING, 8, ascii.as_bytes())?;
        win.set_property(net_wm_name, utf8_string, 8, o.title.as_bytes())?;
        win.set_property(ATOM_WM_CLASS, ATOM_STRING, 8, b"ponzik-plugin\0Ponzik\0")?;
        win.set_property(wm_protocols, ATOM_ATOM, 32, &wm_delete.to_le_bytes())?;
        if let Some(NativeHandle::X11(owner)) = o.owner {
            win.set_property(ATOM_WM_TRANSIENT_FOR, ATOM_WINDOW, 32, &owner.to_le_bytes())?;
        }
        win.set_property(ATOM_WM_NORMAL_HINTS, ATOM_WM_SIZE_HINTS, 32, &plugin_hints(o.position, size, o.resizable))?;
        let mut r = Req::new(OP_MAP_WINDOW, 0);
        r.u32(win.window);
        win.send(r.finish())?;

        let (tx, rx) = mpsc::channel();
        win.rx = rx;
        let reader = win.stream.try_clone().map_err(io_err)?;
        let ctx = ReaderCtx {
            keymap,
            root: win.setup.root,
            resource_manager,
            wm_protocols,
            wm_delete,
            shm_event: None,
            xdnd: XdndAtoms::default(),
        };
        thread::Builder::new()
            .name("ponzik-x11-plugin".into())
            .spawn(move || reader_loop(reader, tx, ctx))
            .map_err(|e| WindowError::System(format!("thread X11 : {e}")))?;
        // The window must exist on the server before the plugin parents to it.
        win.sync()?;
        Ok(win)
    }

    pub(crate) fn native_handle(&self) -> NativeHandle {
        NativeHandle::X11(self.window)
    }

    pub(crate) fn set_plugin_size(&mut self, width: u32, height: u32) {
        let size = (width.clamp(1, 16_384), height.clamp(1, 16_384));
        if size == self.size {
            return;
        }
        self.size = size;
        self.awaiting = Some((size, Instant::now()));
        if !self.resizable {
            let hints = plugin_hints(None, size, false);
            let _ = self.set_property(ATOM_WM_NORMAL_HINTS, ATOM_WM_SIZE_HINTS, 32, &hints);
        }
        let _ = self.send(&resize_req(self.window, size.0, size.1));
    }

    pub(crate) fn set_resizable(&mut self, resizable: bool) {
        if resizable != self.resizable {
            self.resizable = resizable;
            let hints = plugin_hints(None, self.size, resizable);
            let _ = self.set_property(ATOM_WM_NORMAL_HINTS, ATOM_WM_SIZE_HINTS, 32, &hints);
        }
    }

    /// Frame corner: client origin on the root, minus the decorations
    /// (`_NET_FRAME_EXTENTS`, when the window manager sets it).
    pub(crate) fn position(&mut self) -> Option<(i32, i32)> {
        let mut r = Req::new(OP_TRANSLATE_COORDINATES, 0);
        r.u32(self.window).u32(self.setup.root).i16(0).i16(0);
        let req = r.finish().to_vec();
        let reply = self.request(&req).ok()?;
        if reply.len() < 16 {
            return None;
        }
        let (mut x, mut y) = (i16_at(&reply, 12) as i32, i16_at(&reply, 14) as i32);
        if self.frame_extents != 0
            && let Ok(ext) = self.request(&get_property_req(self.window, self.frame_extents))
        {
            let v = property_value(&ext);
            if v.len() >= 16 {
                x -= u32_at(v, 0) as i32;
                y -= u32_at(v, 8) as i32;
            }
        }
        Some((x, y))
    }

    pub(crate) fn poll_plugin_events(&mut self, out: &mut Vec<Event>) {
        self.drain();
        out.extend(self.pending.drain(..).filter(|e| matches!(e, Event::CloseRequested | Event::Resized { .. })));
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.setup.id_base | (self.next_id & self.setup.id_mask);
        self.next_id += 1;
        id
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), WindowError> {
        self.stream.write_all(bytes).map_err(io_err)
    }

    fn set_property(&mut self, prop: u32, ty: u32, format: u8, data: &[u8]) -> Result<(), WindowError> {
        let mut r = Req::new(OP_CHANGE_PROPERTY, 0);
        r.u32(self.window)
            .u32(prop)
            .u32(ty)
            .u8(format)
            .u8(0)
            .u16(0)
            .u32((data.len() / (format as usize / 8)) as u32)
            .bytes(data);
        self.send(r.finish())
    }

    /// Traite un message du lecteur ; renvoie le contenu d'une réponse.
    fn handle(&mut self, msg: Incoming) -> Option<Vec<u8>> {
        match msg {
            Incoming::Event(ev) => {
                if let Event::Resized { width, height } = ev {
                    // Our own resize: sizes the server reports before it are stale.
                    if let Some((target, since)) = self.awaiting {
                        if (width, height) == target || since.elapsed() >= RESIZE_WAIT {
                            self.awaiting = None;
                        }
                        if (width, height) == target || self.awaiting.is_some() {
                            return None;
                        }
                    }
                    if (width, height) == self.size {
                        return None;
                    }
                    self.size = (width, height);
                }
                self.pending.push_back(ev);
            }
            Incoming::ShmCompletion => self.shm_busy = false,
            Incoming::Reply(data) => return Some(data),
            Incoming::ResourcesChanged => self.resources_changed = true,
            // Answered outside: it may need a request of its own.
            msg @ (Incoming::Xdnd(_) | Incoming::Selection { .. }) => self.xdnd_inbox.push_back(msg),
            Incoming::Error { code, major } => {
                if Some(major) == self.shm_major {
                    self.shm_failed = true;
                    self.shm_busy = false;
                } else {
                    eprintln!("ponzik-window: X11 error {code} (request {major})");
                }
            }
            Incoming::Closed => self.pending.push_back(Event::CloseRequested),
        }
        None
    }

    /// Envoie une requête à réponse et attend celle-ci.
    fn request(&mut self, req: &[u8]) -> Result<Vec<u8>, WindowError> {
        self.send(req)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => {
                    if let Some(reply) = self.handle(msg) {
                        return Ok(reply);
                    }
                }
                Err(_) => return Err(proto("the server stopped responding")),
            }
        }
    }

    /// Attend que le serveur ait traité toutes nos requêtes.
    fn sync(&mut self) -> Result<(), WindowError> {
        let mut r = Req::new(OP_GET_INPUT_FOCUS, 0);
        let req = r.finish().to_vec();
        self.request(&req).map(drop)
    }

    /// Relit Xft.dpi après un changement de la base de ressources.
    fn refresh_scale(&mut self) {
        self.resources_changed = false;
        let req = get_property_req(self.setup.root, self.resource_manager);
        let Ok(reply) = self.request(&req) else { return };
        let scale = system_scale(property_value(&reply));
        if (scale - self.scale).abs() > 1e-3 {
            self.scale = scale;
            self.pending.push_back(Event::ScaleChanged { scale });
        }
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.scale
    }

    fn create_shm(&mut self, size: usize) -> Option<Shm> {
        let major = self.shm_major?;
        // SAFETY: segment privé, attaché ici puis marqué pour destruction dès que
        // le serveur l'a attaché à son tour (il disparaît avec le dernier détachement).
        unsafe {
            let id = shmget(IPC_PRIVATE, size, IPC_CREAT | 0o600);
            if id < 0 {
                return None;
            }
            let addr = shmat(id, null(), 0);
            if addr as isize == -1 {
                shmctl(id, IPC_RMID, null_mut());
                return None;
            }
            let seg = self.alloc_id();
            let mut r = Req::new(major, SHM_ATTACH);
            r.u32(seg).u32(id as u32).u8(1).u8(0).u16(0);
            self.shm_failed = false;
            let ok = self.send(r.finish()).is_ok() && self.sync().is_ok() && !self.shm_failed;
            shmctl(id, IPC_RMID, null_mut());
            if !ok {
                shmdt(addr);
                return None;
            }
            Some(Shm { seg, addr: addr.cast(), size })
        }
    }

    fn drop_shm(&mut self) {
        if let (Some(shm), Some(major)) = (self.shm.take(), self.shm_major) {
            let mut r = Req::new(major, SHM_DETACH);
            r.u32(shm.seg);
            let _ = self.send(r.finish());
            let _ = self.sync();
            // SAFETY: adresse obtenue par shmat, plus utilisée ensuite.
            unsafe { shmdt(shm.addr.cast()) };
        }
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        if self.resources_changed {
            self.refresh_scale();
        }
        self.handle_xdnd();
    }

    /// Answers the drag messages received so far.
    fn handle_xdnd(&mut self) {
        while let Some(msg) = self.xdnd_inbox.pop_front() {
            match msg {
                Incoming::Xdnd(m) => self.on_xdnd(m),
                Incoming::Selection { property } => self.on_dropped_data(property),
                _ => {}
            }
        }
        // A source that never hands its data over.
        if self.drag.as_ref().and_then(|d| d.dropped).is_some_and(|t| t.elapsed() > DROP_WAIT) {
            self.end_drag(None);
        }
    }

    fn on_xdnd(&mut self, m: Xdnd) {
        let atoms = self.xdnd;
        match m {
            Xdnd::Enter { source, version, files } => {
                let origin = self.root_origin();
                self.drag = Some(DragIn { source, version, accept: files, origin, pos: (0.0, 0.0), dropped: None });
            }
            Xdnd::Position { source, x, y, .. } => {
                let Some(drag) = self.drag.as_mut().filter(|d| d.source == source && d.dropped.is_none()) else { return };
                let (ox, oy) = drag.origin.unwrap_or((0, 0));
                drag.pos = ((x as i32 - ox) as f32, (y as i32 - oy) as f32);
                let (accept, pos) = (drag.accept, drag.pos);
                let _ = self.send(&xdnd_status(&atoms, self.window, source, accept));
                if accept {
                    self.pending.push_back(Event::DragHover { x: pos.0, y: pos.1 });
                }
            }
            Xdnd::Leave { source } => {
                if self.drag.as_ref().is_some_and(|d| d.source == source) {
                    self.drag = None;
                    self.pending.push_back(Event::DragLeave);
                }
            }
            Xdnd::Drop { source, time } => {
                let Some(drag) = self.drag.as_mut().filter(|d| d.source == source) else { return };
                if !drag.accept {
                    self.end_drag(None);
                    return;
                }
                drag.dropped = Some(Instant::now());
                let req = convert_selection_req(self.window, atoms.selection, atoms.uri_list, atoms.property, time);
                if self.send(&req).is_err() {
                    self.end_drag(None);
                }
            }
        }
    }

    /// The dropped `text/uri-list` arrived in `property` (0: the source refused).
    fn on_dropped_data(&mut self, property: u32) {
        if self.drag.as_ref().is_none_or(|d| d.dropped.is_none()) {
            return;
        }
        let paths = if property == 0 {
            Vec::new()
        } else {
            let req = take_property_req(self.window, property);
            self.request(&req).map(|reply| crate::parse_uri_list(property_value(&reply))).unwrap_or_default()
        };
        self.end_drag(Some(paths));
    }

    /// Closes the drag: tells the source (after a drop) and the application,
    /// with the dropped files, if any.
    fn end_drag(&mut self, paths: Option<Vec<std::path::PathBuf>>) {
        let Some(drag) = self.drag.take() else { return };
        let paths = paths.filter(|p| !p.is_empty());
        // Version 2+ expects XdndFinished after a drop.
        if drag.version >= 2 {
            let _ = self.send(&xdnd_finished(&self.xdnd, self.window, drag.source, paths.is_some()));
        }
        self.pending.push_back(match paths {
            Some(paths) => Event::FileDrop { x: drag.pos.0, y: drag.pos.1, paths },
            None => Event::DragLeave,
        });
    }

    /// Our window's top-left corner on the root.
    fn root_origin(&mut self) -> Option<(i32, i32)> {
        let mut r = Req::new(OP_TRANSLATE_COORDINATES, 0);
        r.u32(self.window).u32(self.setup.root).i16(0).i16(0);
        let req = r.finish().to_vec();
        let reply = self.request(&req).ok()?;
        (reply.len() >= 16).then(|| (i16_at(&reply, 12) as i32, i16_at(&reply, 14) as i32))
    }

    pub(crate) fn poll_events(&mut self, out: &mut Vec<Event>) {
        self.drain();
        out.extend(self.pending.drain(..));
    }

    pub(crate) fn wait_events(&mut self, timeout: Duration, out: &mut Vec<Event>) {
        self.drain();
        if self.pending.is_empty() {
            match self.rx.recv_timeout(timeout) {
                Ok(msg) => {
                    self.handle(msg);
                    self.drain();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.pending.push_back(Event::CloseRequested),
            }
        }
        out.extend(self.pending.drain(..));
    }

    pub(crate) fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        self.present_rects(pixels, width, height, None)
    }

    /// Envoie seulement les zones `rects` (x0, y0, x1, y1) de l'image, ou toute
    /// l'image avec `None`. Le segment partagé garde le reste de l'image précédente.
    pub(crate) fn present_rects(
        &mut self,
        pixels: &[u32],
        width: u32,
        height: u32,
        rects: Option<&[(usize, usize, usize, usize)]>,
    ) -> Result<(), WindowError> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let bytes = (width * height * 4) as usize;
        if self.shm.as_ref().is_some_and(|s| s.size < bytes) {
            self.drop_shm();
            self.shm = self.create_shm(bytes + bytes / 4);
            self.shm_image = (0, 0);
        }
        // Nouveau segment ou nouvelle taille : tout est envoyé.
        let full = [(0, 0, width as usize, height as usize)];
        let rects = match rects {
            Some(r) if self.shm_image == (width, height) => r,
            _ => &full[..],
        };
        if rects.is_empty() {
            return Ok(());
        }

        // On attend que le serveur ait fini de lire l'image précédente.
        let deadline = Instant::now() + Duration::from_millis(100);
        while self.shm_busy {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => {
                    self.handle(msg);
                }
                Err(_) => self.shm_busy = false,
            }
        }
        if self.shm_failed && self.shm.take().is_some() {
            eprintln!("ponzik-window: MIT-SHM refused, images now go through the socket (slower)");
        }

        if let (Some((seg, addr)), Some(major)) = (self.shm.as_ref().map(|s| (s.seg, s.addr)), self.shm_major) {
            let w = width as usize;
            let mut reqs = Vec::with_capacity(rects.len() * SHM_PUT_IMAGE_LEN);
            for &(x0, y0, x1, y1) in rects {
                let (x1, y1) = (x1.min(w), y1.min(height as usize));
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                for y in y0..y1 {
                    let row = &pixels[y * w + x0..y * w + x1];
                    // SAFETY: le segment fait au moins `bytes` octets (la ligne y est
                    // dans l'image) et le serveur a fini de le lire.
                    unsafe {
                        std::ptr::copy_nonoverlapping(row.as_ptr().cast::<u8>(), addr.add((y * w + x0) * 4), row.len() * 4)
                    };
                }
                let mut r = Req::new(major, SHM_PUT_IMAGE);
                r.u32(self.window)
                    .u32(self.gc)
                    .u16(width as u16)
                    .u16(height as u16)
                    .u16(x0 as u16)
                    .u16(y0 as u16)
                    .u16((x1 - x0) as u16)
                    .u16((y1 - y0) as u16)
                    .i16(x0 as i16)
                    .i16(y0 as i16)
                    .u8(self.setup.depth)
                    .u8(2) // ZPixmap
                    .u8(0) // ShmCompletion : posé plus bas sur la dernière zone
                    .u8(0)
                    .u32(seg)
                    .u32(0);
                reqs.extend_from_slice(r.finish());
            }
            let Some(last) = reqs.len().checked_sub(SHM_PUT_IMAGE_LEN) else { return Ok(()) };
            // Le serveur prévient quand il a lu la dernière zone (send_event, octet 30 : après depth et format).
            reqs[last + SHM_SEND_EVENT] = 1;
            self.shm_busy = true;
            self.shm_image = (width, height);
            return self.send(&reqs);
        }

        // Repli : PutImage par bandes qui tiennent dans une requête.
        let rows_per = ((self.setup.max_request_words.saturating_sub(6)) / width as usize).max(1);
        let mut y = 0;
        while y < height as usize {
            let rows = rows_per.min(height as usize - y);
            let mut head = Req::new(OP_PUT_IMAGE, 2);
            head.u32(self.window)
                .u32(self.gc)
                .u16(width as u16)
                .u16(rows as u16)
                .i16(0)
                .i16(y as i16)
                .u8(0)
                .u8(self.setup.depth)
                .u16(0);
            let words = (6 + rows * width as usize) as u16;
            head.0[2..4].copy_from_slice(&words.to_le_bytes());
            let strip = &pixels[y * width as usize..(y + rows) * width as usize];
            // SAFETY: réinterprétation d'un &[u32] en octets (même mémoire, alignement plus faible).
            let raw = unsafe { std::slice::from_raw_parts(strip.as_ptr().cast::<u8>(), strip.len() * 4) };
            let header = head.0.clone();
            self.send(&header)?;
            self.send(raw)?;
            y += rows;
        }
        Ok(())
    }

    /// Les curseurs sont créés à la première demande de chaque forme puis
    /// gardés ; la fenêtre n'est modifiée que si la forme change.
    pub(crate) fn set_cursor(&mut self, cursor: Cursor) {
        if self.cursor == Some(cursor) {
            return;
        }
        if self.cursor_font == 0 {
            let fid = self.alloc_id();
            if self.send(&open_font_req(fid, "cursor")).is_err() {
                return;
            }
            self.cursor_font = fid;
        }
        let i = cursor_index(cursor);
        if self.cursors[i] == 0 {
            let cid = self.alloc_id();
            if self.send(&glyph_cursor_req(cid, self.cursor_font, cursor_glyph(cursor))).is_err() {
                return;
            }
            self.cursors[i] = cid;
        }
        if self.send(&window_cursor_req(self.window, self.cursors[i])).is_ok() {
            self.cursor = Some(cursor);
        }
    }

    pub(crate) fn backend_name(&self) -> &'static str {
        if self.shm.is_some() { "X11 · MIT-SHM" } else { "X11 · PutImage" }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some(shm) = self.shm.take() {
            // SAFETY: adresse obtenue par shmat ; le serveur détache à la fermeture.
            unsafe { shmdt(shm.addr.cast()) };
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_put_image_layout() {
        let mut r = Req::new(130, SHM_PUT_IMAGE);
        r.u32(7).u32(8).u16(100).u16(50).u16(1).u16(2).u16(3).u16(4).i16(5).i16(6);
        r.u8(24).u8(2).u8(1).u8(0).u32(0xabcd).u32(0);
        let bytes = r.finish().to_vec();
        assert_eq!(bytes.len(), SHM_PUT_IMAGE_LEN);
        assert_eq!(bytes[SHM_SEND_EVENT], 1, "send_event");
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 0xabcd, "shmseg untouched");
    }

    const ALL: [Cursor; CURSOR_SHAPES] = [
        Cursor::Arrow,
        Cursor::Hand,
        Cursor::Text,
        Cursor::ResizeH,
        Cursor::ResizeV,
        Cursor::Move,
        Cursor::Crosshair,
        Cursor::NotAllowed,
        Cursor::Grab,
        Cursor::ResizeNwSe,
    ];

    #[test]
    fn open_font_pads_name_and_counts_words() {
        let req = open_font_req(0x0040_0001, "cursor");
        // 12 octets d'en-tête + « cursor » (6) complété à 8.
        assert_eq!(req.len(), 20);
        assert_eq!(&req[..4], &[OP_OPEN_FONT, 0, 5, 0]);
        assert_eq!(u32_at(&req, 4), 0x0040_0001);
        assert_eq!(u16_at(&req, 8), 6);
        assert_eq!(&req[12..18], b"cursor");
        assert_eq!(&req[18..], &[0, 0]);
    }

    #[test]
    fn glyph_cursor_uses_next_glyph_as_mask() {
        let req = glyph_cursor_req(7, 3, 60);
        assert_eq!(req.len(), 32);
        assert_eq!(&req[..4], &[OP_CREATE_GLYPH_CURSOR, 0, 8, 0]);
        assert_eq!((u32_at(&req, 4), u32_at(&req, 8), u32_at(&req, 12)), (7, 3, 3));
        assert_eq!((u16_at(&req, 16), u16_at(&req, 18)), (60, 61));
        assert!(req[20..26].iter().all(|&b| b == 0), "avant-plan noir");
        assert!(req[26..32].iter().all(|&b| b == 0xff), "fond blanc");
    }

    #[test]
    fn window_cursor_sets_only_cw_cursor() {
        let req = window_cursor_req(0x10, 0x20);
        assert_eq!(req.len(), 16);
        assert_eq!(&req[..4], &[OP_CHANGE_WINDOW_ATTRIBUTES, 0, 4, 0]);
        assert_eq!((u32_at(&req, 4), u32_at(&req, 8), u32_at(&req, 12)), (0x10, CW_CURSOR, 0x20));
    }

    #[test]
    fn digit_row_reports_digits_on_azerty() {
        // Keycodes 10, 11, 12 : AZERTY « à/0 », QWERTY « 0/) », lettre « a/A ».
        let keymap = Keymap { min_keycode: 10, per_keycode: 2, syms: vec![0xe0, 0x30, 0x30, 0x29, 0x61, 0x41] };
        assert_eq!(keymap.key(10), Some(Key::Char('0')));
        assert_eq!(keymap.key(11), Some(Key::Char('0')));
        assert_eq!(keymap.key(12), Some(Key::Char('a')));
        // Le texte saisi, lui, suit toujours la disposition.
        assert_eq!(keymap.text(10, 0), Some('à'));
    }

    #[test]
    fn xft_dpi_sets_the_scale() {
        assert_eq!(scale_from_resources("Xft.dpi:\t96\n"), Some(1.0));
        assert_eq!(scale_from_resources("Xcursor.size:\t48\nXft.dpi:\t192\nXft.antialias:\t1\n"), Some(2.0));
        assert_eq!(scale_from_resources("Xft.dpi: 144"), Some(1.5));
        // La dernière définition l'emporte ; les autres ressources sont ignorées.
        assert_eq!(scale_from_resources("Xft.dpi:\t96\nXft.dpi:\t120\n"), Some(1.25));
        assert_eq!(scale_from_resources("*dpi:\t192\nXft.dpis:\t192\n"), None);
        assert_eq!(scale_from_resources("Xft.dpi:\tbeaucoup\n"), None);
        assert_eq!(scale_from_resources("Xft.dpi:\t0\n"), None);
        assert_eq!(scale_from_resources("Xft.dpi:\t9600\n"), Some(4.0));
        assert_eq!(scale_from_resources(""), None);
    }

    #[test]
    fn env_scale_is_a_fallback() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(scale_from_env(env(&[])), None);
        assert_eq!(scale_from_env(env(&[("GDK_SCALE", "2")])), Some(2.0));
        assert_eq!(scale_from_env(env(&[("QT_SCALE_FACTOR", "1.5")])), Some(1.5));
        assert_eq!(scale_from_env(env(&[("GDK_SCALE", "2"), ("QT_SCALE_FACTOR", "1.5")])), Some(2.0));
        assert_eq!(scale_from_env(env(&[("GDK_SCALE", "x"), ("QT_SCALE_FACTOR", "1.25")])), Some(1.25));
    }

    #[test]
    fn get_property_reply_value() {
        let req = get_property_req(0x100, 23);
        assert_eq!(&req[..4], &[OP_GET_PROPERTY, 0, 6, 0]);
        assert_eq!((u32_at(&req, 4), u32_at(&req, 8), u32_at(&req, 12)), (0x100, 23, 0));
        assert_eq!(u32_at(&req, 20), RESOURCES_MAX_WORDS);
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        assert_eq!(property_value(&reply), b"", "propriété absente");
        reply[1] = 8;
        reply[16..20].copy_from_slice(&11u32.to_le_bytes());
        reply.extend_from_slice(b"Xft.dpi:192\0");
        assert_eq!(property_value(&reply), b"Xft.dpi:192");
    }

    #[test]
    fn plugin_hints_lock_the_size_unless_resizable() {
        let words = |b: Vec<u8>| -> Vec<u32> { b.chunks_exact(4).map(|c| u32_at(c, 0)).collect() };
        let fixed = words(plugin_hints(Some((10, -5)), (800, 600), false));
        assert_eq!(fixed.len(), 18);
        assert_eq!(fixed[0], 1 | 4 | 16 | 32);
        assert_eq!((fixed[1] as i32, fixed[2] as i32), (10, -5));
        assert_eq!(&fixed[5..9], &[800, 600, 800, 600]);
        let free = words(plugin_hints(None, (800, 600), true));
        assert_eq!(free[0], 0);
        let req = resize_req(0x42, 300, 0);
        assert_eq!(&req[..4], &[OP_CONFIGURE_WINDOW, 0, 5, 0]);
        assert_eq!((u32_at(&req, 4), u16_at(&req, 8)), (0x42, 0xc));
        assert_eq!((u32_at(&req, 12), u32_at(&req, 16)), (300, 1));
    }

    fn atoms() -> XdndAtoms {
        XdndAtoms::from_list([100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110])
    }

    #[test]
    fn xdnd_atom_names_line_up() {
        let a = atoms();
        assert_eq!(XdndAtoms::NAMES[2], "XdndPosition");
        assert_eq!(a.position, 102);
        assert_eq!(XdndAtoms::NAMES[9], "text/uri-list");
        assert_eq!(a.uri_list, 109);
        assert_eq!(a.property, 110);
    }

    #[test]
    fn xdnd_messages_are_decoded() {
        let a = atoms();
        // Enter, version 5, three types listed, uri-list among them.
        let enter = client_message(0x42, a.enter, [0x0c00_0001, 5 << 24, 77, a.uri_list, 0]);
        assert_eq!(parse_xdnd(&enter, &a), Some(Xdnd::Enter { source: 0x0c00_0001, version: 5, files: true }));
        // Only text: no files. More than three types: kept in XdndTypeList, tried anyway.
        let text = client_message(0x42, a.enter, [9, 5 << 24, 77, 78, 0]);
        assert_eq!(parse_xdnd(&text, &a), Some(Xdnd::Enter { source: 9, version: 5, files: false }));
        let more = client_message(0x42, a.enter, [9, (4 << 24) | 1, 0, 0, 0]);
        assert_eq!(parse_xdnd(&more, &a), Some(Xdnd::Enter { source: 9, version: 4, files: true }));
        // Position: root x in the high half, y in the low one (signed).
        let pos = client_message(0x42, a.position, [9, 0, (300 << 16) | 0xfffe, 1234, a.action_copy]);
        assert_eq!(parse_xdnd(&pos, &a), Some(Xdnd::Position { source: 9, x: 300, y: -2, time: 1234 }));
        assert_eq!(parse_xdnd(&client_message(0x42, a.leave, [9, 0, 0, 0, 0]), &a), Some(Xdnd::Leave { source: 9 }));
        assert_eq!(
            parse_xdnd(&client_message(0x42, a.drop, [9, 0, 777, 0, 0]), &a),
            Some(Xdnd::Drop { source: 9, time: 777 })
        );
        // Sent events carry the high bit; other messages and formats are not ours.
        let mut sent = pos;
        sent[0] |= 0x80;
        assert!(parse_xdnd(&sent, &a).is_some());
        assert_eq!(parse_xdnd(&client_message(0x42, 555, [0; 5]), &a), None);
        let mut format8 = pos;
        format8[1] = 8;
        assert_eq!(parse_xdnd(&format8, &a), None);
        // A window without XDND atoms ignores everything.
        assert_eq!(parse_xdnd(&pos, &XdndAtoms::default()), None);
    }

    #[test]
    fn xdnd_replies_are_encoded() {
        let a = atoms();
        let status = xdnd_status(&a, 0x50, 0x0c00_0001, true);
        assert_eq!(status.len(), 44);
        assert_eq!(&status[..4], &[OP_SEND_EVENT, 0, 11, 0]);
        assert_eq!((u32_at(&status, 4), u32_at(&status, 8)), (0x0c00_0001, 0), "destination, no event mask");
        let ev = &status[12..];
        assert_eq!((ev[0], ev[1]), (33, 32));
        assert_eq!((u32_at(ev, 4), u32_at(ev, 8)), (0x0c00_0001, a.status));
        let l: Vec<u32> = (0..5).map(|i| u32_at(ev, 12 + 4 * i)).collect();
        assert_eq!(l, [0x50, 3, 0, 0, a.action_copy]);
        // Refused: no accept bit, no action.
        let no = xdnd_status(&a, 0x50, 7, false);
        assert_eq!((u32_at(&no, 12 + 16), u32_at(&no, 12 + 28)), (2, 0));

        let done = xdnd_finished(&a, 0x50, 7, true);
        assert_eq!(u32_at(&done, 12 + 8), a.finished);
        assert_eq!((u32_at(&done, 12 + 12), u32_at(&done, 12 + 16), u32_at(&done, 12 + 20)), (0x50, 1, a.action_copy));
        let failed = xdnd_finished(&a, 0x50, 7, false);
        assert_eq!((u32_at(&failed, 12 + 16), u32_at(&failed, 12 + 20)), (0, 0));

        let conv = convert_selection_req(0x50, a.selection, a.uri_list, a.property, 999);
        assert_eq!(&conv[..4], &[OP_CONVERT_SELECTION, 0, 6, 0]);
        let words: Vec<u32> = (1..6).map(|i| u32_at(&conv, 4 * i)).collect();
        assert_eq!(words, [0x50, a.selection, a.uri_list, a.property, 999]);

        let take = take_property_req(0x50, a.property);
        assert_eq!(&take[..4], &[OP_GET_PROPERTY, 1, 6, 0], "delete once read");
    }

    #[test]
    fn every_shape_has_its_own_slot_and_an_even_glyph() {
        let mut seen = [false; CURSOR_SHAPES];
        for c in ALL {
            let i = cursor_index(c);
            assert!(!seen[i], "{c:?} partage sa case");
            seen[i] = true;
            // Dans la police « cursor », un glyphe pair est suivi de son masque.
            assert_eq!(cursor_glyph(c) % 2, 0, "{c:?}");
        }
    }
}
