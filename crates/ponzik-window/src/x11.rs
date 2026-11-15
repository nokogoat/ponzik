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

use crate::{Event, Key, Modifiers, MouseButton, WindowError};

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
const OP_MAP_WINDOW: u8 = 8;
const OP_INTERN_ATOM: u8 = 16;
const OP_CHANGE_PROPERTY: u8 = 18;
const OP_GET_INPUT_FOCUS: u8 = 43;
const OP_CREATE_GC: u8 = 55;
const OP_PUT_IMAGE: u8 = 72;
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

const EVENT_MASK: u32 = 0x1 // KeyPress
    | 0x2 // KeyRelease
    | 0x4 // ButtonPress
    | 0x8 // ButtonRelease
    | 0x20 // LeaveWindow
    | 0x40 // PointerMotion
    | 0x8000 // Exposure
    | 0x2_0000 // StructureNotify
    | 0x20_0000; // FocusChange

const MIN_SIZE: (u32, u32) = (960, 640);
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
    Reply,
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
        0x21..=0x7e | 0xa0..=0xff => Key::Char(char::from_u32(sym)?.to_lowercase().next()?),
        _ => return None,
    })
}

fn modifiers(state: u16) -> Modifiers {
    Modifiers { shift: state & 0x1 != 0, ctrl: state & 0x4 != 0, alt: state & 0x8 != 0 }
}

struct ReaderCtx {
    keymap: Keymap,
    wm_protocols: u32,
    wm_delete: u32,
    shm_event: Option<u8>,
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
                let mut rest = vec![0u8; extra];
                if stream.read_exact(&mut rest).is_err() {
                    let _ = tx.send(Incoming::Closed);
                    return;
                }
                (b[0] == 1).then_some(Incoming::Reply)
            }
            c => {
                let code = c & 0x7f;
                if Some(code) == ctx.shm_event {
                    Some(Incoming::ShmCompletion)
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
    let display = std::env::var("DISPLAY").map_err(|_| WindowError::NoDisplay("DISPLAY non défini".into()))?;
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
            return Err(proto(format!("connexion refusée : {reason}")));
        }
        _ => return Err(proto("le serveur demande une authentification non gérée")),
    }

    let vendor_len = u16_at(&d, 16) as usize;
    let num_formats = d[21] as usize;
    if d[22] != 0 {
        return Err(proto("ordre des octets d'image non géré (MSB)"));
    }
    let formats_at = 32 + pad4(vendor_len);
    let screen = formats_at + 8 * num_formats;
    let root = u32_at(&d, screen);
    let root_visual = u32_at(&d, screen + 32);
    let depth = d[screen + 38];

    let bpp = (0..num_formats).map(|i| &d[formats_at + 8 * i..]).find(|f| f[0] == depth).map(|f| f[1]);
    if bpp != Some(32) {
        return Err(proto(format!("profondeur {depth} bits non gérée (il faut 24/32 bits)")));
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
        return Err(proto("visuel non géré (il faut du TrueColor RGB 8 bits)"));
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
            0 => return Err(proto(format!("erreur {} sur la requête {}", b[1], b[10]))),
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
}

impl Window {
    pub(crate) fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        let (mut stream, setup) = connect()?;

        let wm_protocols = intern_atom(&mut stream, "WM_PROTOCOLS")?;
        let wm_delete = intern_atom(&mut stream, "WM_DELETE_WINDOW")?;
        let net_wm_name = intern_atom(&mut stream, "_NET_WM_NAME")?;
        let utf8_string = intern_atom(&mut stream, "UTF8_STRING")?;

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
        let mut hints = [0u32; 18];
        hints[0] = 16; // PMinSize
        hints[5] = MIN_SIZE.0;
        hints[6] = MIN_SIZE.1;
        let hints: Vec<u8> = hints.iter().flat_map(|v| v.to_le_bytes()).collect();
        win.set_property(ATOM_WM_NORMAL_HINTS, ATOM_WM_SIZE_HINTS, 32, &hints)?;

        let mut r = Req::new(OP_CREATE_GC, 0);
        r.u32(win.gc).u32(win.window).u32(0x1_0000).u32(0); // GraphicsExposures = faux
        win.send(r.finish())?;
        let mut r = Req::new(OP_MAP_WINDOW, 0);
        r.u32(win.window);
        win.send(r.finish())?;

        let (tx, rx) = mpsc::channel();
        win.rx = rx;
        let reader = win.stream.try_clone().map_err(io_err)?;
        let ctx = ReaderCtx { keymap, wm_protocols, wm_delete, shm_event };
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

    fn handle(&mut self, msg: Incoming) -> bool {
        match msg {
            Incoming::Event(ev) => {
                if let Event::Resized { width, height } = ev {
                    if (width, height) == self.size {
                        return false;
                    }
                    self.size = (width, height);
                }
                self.pending.push_back(ev);
            }
            Incoming::ShmCompletion => self.shm_busy = false,
            Incoming::Reply => return true,
            Incoming::Error { code, major } => {
                if Some(major) == self.shm_major {
                    self.shm_failed = true;
                    self.shm_busy = false;
                } else {
                    eprintln!("ponzik-window : erreur X11 {code} (requête {major})");
                }
            }
            Incoming::Closed => self.pending.push_back(Event::CloseRequested),
        }
        false
    }

    /// Attend que le serveur ait traité toutes nos requêtes.
    fn sync(&mut self) -> Result<(), WindowError> {
        let mut r = Req::new(OP_GET_INPUT_FOCUS, 0);
        self.send(r.finish())?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => {
                    if self.handle(msg) {
                        return Ok(());
                    }
                }
                Err(_) => return Err(proto("le serveur ne répond plus")),
            }
        }
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
        if width == 0 || height == 0 {
            return Ok(());
        }
        let bytes = (width * height * 4) as usize;
        if self.shm.as_ref().is_some_and(|s| s.size < bytes) {
            self.drop_shm();
            self.shm = self.create_shm(bytes + bytes / 4);
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
        if self.shm_failed {
            self.shm = None;
        }

        if let (Some((seg, addr)), Some(major)) = (self.shm.as_ref().map(|s| (s.seg, s.addr)), self.shm_major) {
            // SAFETY: le segment fait au moins `bytes` octets et le serveur a fini de le lire.
            unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr().cast::<u8>(), addr, bytes) };
            let mut r = Req::new(major, SHM_PUT_IMAGE);
            r.u32(self.window)
                .u32(self.gc)
                .u16(width as u16)
                .u16(height as u16)
                .u16(0)
                .u16(0)
                .u16(width as u16)
                .u16(height as u16)
                .i16(0)
                .i16(0)
                .u8(self.setup.depth)
                .u8(2) // ZPixmap
                .u8(1) // ShmCompletion
                .u8(0)
                .u32(seg)
                .u32(0);
            let req = r.finish().to_vec();
            self.shm_busy = true;
            return self.send(&req);
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
