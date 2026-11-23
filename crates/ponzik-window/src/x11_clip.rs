//! System clipboard over X11: the `CLIPBOARD` selection (Ctrl+C, Ctrl+V) and
//! `PRIMARY` (selected text, pasted with the middle button).
//!
//! It runs on a connection of its own with a hidden window, so that other
//! programs get our text even while the interface thread is busy: a reader
//! thread answers their `SelectionRequest`s (`TARGETS`, `UTF8_STRING`, `TEXT`,
//! `STRING`…), large texts going out in `INCR` chunks. Reading asks the owner
//! with `ConvertSelection` and waits for `SelectionNotify`, `INCR` included.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{
    connect, intern_atom, io_err, send_event_req, u32_at, Req, ATOM_ATOM, ATOM_STRING, CW_EVENT_MASK, OP_CHANGE_PROPERTY,
    OP_CHANGE_WINDOW_ATTRIBUTES, OP_CONVERT_SELECTION, OP_CREATE_WINDOW, OP_GET_PROPERTY, PROPERTY_CHANGE_MASK,
};
use crate::WindowError;

const OP_SET_SELECTION_OWNER: u8 = 22;
const OP_GET_SELECTION_OWNER: u8 = 23;
const ATOM_PRIMARY: u32 = 1;
const ATOM_INTEGER: u32 = 19;
/// How long the owner of a selection may take to answer.
const ANSWER_WAIT: Duration = Duration::from_millis(1500);
/// Largest property read at once, in 4-byte words (64 MB).
const READ_WORDS: u32 = 16 << 20;

#[derive(Clone, Copy, Debug, Default)]
struct Atoms {
    clipboard: u32,
    utf8: u32,
    targets: u32,
    text: u32,
    incr: u32,
    timestamp: u32,
    plain_utf8: u32,
    plain: u32,
    /// Property of our window that receives what we ask for.
    property: u32,
    /// Property touched to get a server timestamp.
    stamp: u32,
}

impl Atoms {
    const NAMES: [&'static str; 10] = [
        "CLIPBOARD",
        "UTF8_STRING",
        "TARGETS",
        "TEXT",
        "INCR",
        "TIMESTAMP",
        "text/plain;charset=utf-8",
        "text/plain",
        "PONZIK_CLIPBOARD",
        "PONZIK_TIME",
    ];

    fn from_list(a: [u32; 10]) -> Self {
        let [clipboard, utf8, targets, text, incr, timestamp, plain_utf8, plain, property, stamp] = a;
        Self { clipboard, utf8, targets, text, incr, timestamp, plain_utf8, plain, property, stamp }
    }

    /// The targets we hand our text out as.
    fn offered(&self) -> [u32; 7] {
        [self.targets, self.timestamp, self.utf8, self.plain_utf8, self.text, ATOM_STRING, self.plain]
    }
}

/// What the reader thread passes to the interface thread.
enum Msg {
    Reply(Vec<u8>),
    Error,
    /// SelectionNotify: the asked selection sits in `property` (0: refused).
    Notify { selection: u32, property: u32 },
    /// A property of our window changed (`new`: a new value, not a deletion).
    Property { atom: u32, time: u32, new: bool },
}

/// A text we own, and when we took the selection.
#[derive(Clone, Default)]
struct Owned {
    text: String,
    time: u32,
}

/// A large text going out to `requestor` in chunks: one per deletion of its property.
struct Transfer {
    requestor: u32,
    property: u32,
    ty: u32,
    data: Vec<u8>,
    sent: usize,
    started: Instant,
}

struct Shared {
    writer: Mutex<UnixStream>,
    clipboard: Mutex<Option<Owned>>,
    primary: Mutex<Option<Owned>>,
    atoms: Atoms,
    window: u32,
    /// Largest property written in one request; beyond, INCR.
    chunk: usize,
}

impl Shared {
    fn send(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
        }
    }

    fn owned(&self, selection: u32) -> Option<&Mutex<Option<Owned>>> {
        if selection == self.atoms.clipboard {
            Some(&self.clipboard)
        } else if selection == ATOM_PRIMARY {
            Some(&self.primary)
        } else {
            None
        }
    }
}

fn change_property_req(window: u32, prop: u32, ty: u32, format: u8, data: &[u8], append: bool) -> Vec<u8> {
    let mut r = Req::new(OP_CHANGE_PROPERTY, append as u8 * 2);
    r.u32(window).u32(prop).u32(ty).u8(format).u8(0).u16(0).u32((data.len() / (format as usize / 8)) as u32).bytes(data);
    r.finish().to_vec()
}

/// SelectionNotify for `requestor`: the data is in `property` (0: refused).
fn selection_notify(time: u32, requestor: u32, selection: u32, target: u32, property: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = 31;
    for (i, v) in [time, requestor, selection, target, property].into_iter().enumerate() {
        e[4 + 4 * i..8 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    e
}

/// Text as ISO 8859-1 (`STRING`), what this charset cannot hold as `?`.
fn to_latin1(text: &str) -> Vec<u8> {
    text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
}

fn from_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// What to put in the requestor's property for `target`: (type, format, bytes),
/// or `None` when we do not offer that target.
fn convert(atoms: &Atoms, target: u32, owned: &Owned) -> Option<(u32, u8, Vec<u8>)> {
    Some(if target == atoms.targets {
        (ATOM_ATOM, 32, atoms.offered().iter().flat_map(|a| a.to_le_bytes()).collect())
    } else if target == atoms.timestamp {
        (ATOM_INTEGER, 32, owned.time.to_le_bytes().to_vec())
    } else if target == atoms.utf8 || target == atoms.plain_utf8 || target == atoms.text {
        (if target == atoms.plain_utf8 { target } else { atoms.utf8 }, 8, owned.text.as_bytes().to_vec())
    } else if target == ATOM_STRING || target == atoms.plain {
        (target, 8, to_latin1(&owned.text))
    } else {
        return None;
    })
}

/// Answers another program asking for one of our selections.
fn on_request(shared: &Shared, transfers: &mut Vec<Transfer>, b: &[u8; 32]) {
    let (time, requestor, selection, target) = (u32_at(b, 4), u32_at(b, 12), u32_at(b, 16), u32_at(b, 20));
    // Obsolete clients give no property: the target's name is used.
    let property = match u32_at(b, 24) {
        0 => target,
        p => p,
    };
    let owned = shared.owned(selection).and_then(|m| m.lock().ok()?.clone());
    let answer = owned.as_ref().and_then(|o| convert(&shared.atoms, target, o));
    let Some((ty, format, data)) = answer else {
        shared.send(&send_event_req(requestor, &selection_notify(time, requestor, selection, target, 0)));
        return;
    };
    if data.len() > shared.chunk {
        // Too big for one request: announce the size, then a chunk each time
        // the requestor deletes the property.
        let mut r = Req::new(OP_CHANGE_WINDOW_ATTRIBUTES, 0);
        r.u32(requestor).u32(CW_EVENT_MASK).u32(PROPERTY_CHANGE_MASK);
        shared.send(r.finish());
        let size = (data.len() as u32).to_le_bytes();
        shared.send(&change_property_req(requestor, property, shared.atoms.incr, 32, &size, false));
        transfers.retain(|t| (t.requestor, t.property) != (requestor, property) && t.started.elapsed() < Duration::from_secs(30));
        transfers.push(Transfer { requestor, property, ty, data, sent: 0, started: Instant::now() });
    } else {
        shared.send(&change_property_req(requestor, property, ty, format, &data, false));
    }
    shared.send(&send_event_req(requestor, &selection_notify(time, requestor, selection, target, property)));
}

/// The requestor of an INCR transfer deleted its property: the next chunk,
/// or the empty one that ends the transfer.
fn on_requestor_property(shared: &Shared, transfers: &mut Vec<Transfer>, window: u32, atom: u32) {
    let Some(i) = transfers.iter().position(|t| t.requestor == window && t.property == atom) else { return };
    let t = &mut transfers[i];
    let end = (t.sent + shared.chunk).min(t.data.len());
    let chunk = &t.data[t.sent..end];
    shared.send(&change_property_req(t.requestor, t.property, t.ty, 8, chunk, false));
    if chunk.is_empty() {
        transfers.remove(i);
    } else {
        t.sent = end;
    }
}

fn reader_loop(mut stream: UnixStream, shared: Arc<Shared>, tx: Sender<Msg>) {
    let mut b = [0u8; 32];
    let mut transfers: Vec<Transfer> = Vec::new();
    loop {
        if stream.read_exact(&mut b).is_err() {
            return;
        }
        let msg = match b[0] {
            0 => Some(Msg::Error),
            1 | 35 => {
                let mut full = b.to_vec();
                full.resize(32 + u32_at(&b, 4) as usize * 4, 0);
                if stream.read_exact(&mut full[32..]).is_err() {
                    return;
                }
                (b[0] == 1).then_some(Msg::Reply(full))
            }
            c => match c & 0x7f {
                // PropertyNotify: ours goes to the reader, a requestor's moves an INCR transfer on.
                28 => {
                    let (window, atom, time, new) = (u32_at(&b, 4), u32_at(&b, 8), u32_at(&b, 12), b[16] == 0);
                    if window == shared.window {
                        Some(Msg::Property { atom, time, new })
                    } else {
                        if !new {
                            on_requestor_property(&shared, &mut transfers, window, atom);
                        }
                        None
                    }
                }
                // SelectionClear: another program took the selection.
                29 => {
                    if let Some(m) = shared.owned(u32_at(&b, 12))
                        && let Ok(mut owned) = m.lock()
                        && owned.as_ref().is_some_and(|o| o.time <= u32_at(&b, 4) || u32_at(&b, 4) == 0)
                    {
                        *owned = None;
                    }
                    None
                }
                30 => {
                    on_request(&shared, &mut transfers, &b);
                    None
                }
                31 => Some(Msg::Notify { selection: u32_at(&b, 12), property: u32_at(&b, 20) }),
                _ => None,
            },
        };
        if let Some(msg) = msg
            && tx.send(msg).is_err()
        {
            return;
        }
    }
}

pub(crate) struct Clipboard {
    shared: Arc<Shared>,
    rx: Receiver<Msg>,
}

impl Clipboard {
    pub(crate) fn open() -> Result<Self, WindowError> {
        let (mut stream, setup) = connect()?;
        let mut list = [0; 10];
        for (atom, name) in list.iter_mut().zip(Atoms::NAMES) {
            *atom = intern_atom(&mut stream, name)?;
        }
        let atoms = Atoms::from_list(list);
        // A hidden InputOnly window: owner of our selections, target of transfers.
        let window = setup.id_base | (1 & setup.id_mask);
        let mut r = Req::new(OP_CREATE_WINDOW, 0);
        r.u32(window)
            .u32(setup.root)
            .i16(-10)
            .i16(-10)
            .u16(1)
            .u16(1)
            .u16(0)
            .u16(2) // InputOnly
            .u32(0) // CopyFromParent
            .u32(CW_EVENT_MASK)
            .u32(PROPERTY_CHANGE_MASK);
        stream.write_all(r.finish()).map_err(io_err)?;
        // Request header and ChangeProperty fields: 24 bytes.
        let chunk = (setup.max_request_words * 4).saturating_sub(64).clamp(1024, 256 * 1024);
        let reader = stream.try_clone().map_err(io_err)?;
        let shared = Arc::new(Shared {
            writer: Mutex::new(stream),
            clipboard: Mutex::new(None),
            primary: Mutex::new(None),
            atoms,
            window,
            chunk,
        });
        let (tx, rx) = mpsc::channel();
        let for_reader = shared.clone();
        thread::Builder::new()
            .name("ponzik-clipboard".into())
            .spawn(move || reader_loop(reader, for_reader, tx))
            .map_err(|e| WindowError::System(format!("clipboard thread: {e}")))?;
        Ok(Self { shared, rx })
    }

    fn selection(&self, primary: bool) -> u32 {
        if primary { ATOM_PRIMARY } else { self.shared.atoms.clipboard }
    }

    /// Waits for a message `f` takes, dropping the others, until `deadline`.
    fn wait<T>(&self, deadline: Instant, mut f: impl FnMut(Msg) -> Option<T>) -> Option<T> {
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            if let Some(v) = f(self.rx.recv_timeout(left).ok()?) {
                return Some(v);
            }
        }
    }

    /// A server timestamp (ICCCM: never take a selection at `CurrentTime`):
    /// appending nothing to a property of ours sends back a PropertyNotify.
    fn timestamp(&self) -> u32 {
        let stamp = self.shared.atoms.stamp;
        self.shared.send(&change_property_req(self.shared.window, stamp, ATOM_INTEGER, 32, &[], true));
        let deadline = Instant::now() + Duration::from_millis(300);
        self.wait(deadline, |m| match m {
            Msg::Property { atom, time, .. } if atom == stamp => Some(time),
            _ => None,
        })
        .unwrap_or(0)
    }

    pub(crate) fn set(&mut self, primary: bool, text: &str) {
        while self.rx.try_recv().is_ok() {}
        let selection = self.selection(primary);
        let time = self.timestamp();
        if let Some(m) = self.shared.owned(selection)
            && let Ok(mut owned) = m.lock()
        {
            *owned = Some(Owned { text: text.to_string(), time });
        }
        let mut r = Req::new(OP_SET_SELECTION_OWNER, 0);
        r.u32(self.shared.window).u32(selection).u32(time);
        self.shared.send(r.finish());
        // Waits for the server to take it (other programs see it from now on).
        if !self.own(selection)
            && let Some(m) = self.shared.owned(selection)
            && let Ok(mut owned) = m.lock()
        {
            *owned = None;
        }
    }

    /// Whether our window owns `selection` now. The reply comes after any
    /// SelectionClear sent before it, which the reader has handled by then.
    fn own(&self, selection: u32) -> bool {
        let mut r = Req::new(OP_GET_SELECTION_OWNER, 0);
        r.u32(selection);
        self.shared.send(r.finish());
        self.wait(Instant::now() + ANSWER_WAIT, |m| match m {
            Msg::Reply(r) => Some(u32_at(&r, 8) == self.shared.window),
            Msg::Error => Some(false),
            _ => None,
        })
        .unwrap_or(false)
    }

    /// Reads our property (deleting it, which tells an INCR owner to go on).
    fn take(&self) -> Option<(u32, Vec<u8>)> {
        let mut r = Req::new(OP_GET_PROPERTY, 1);
        r.u32(self.shared.window).u32(self.shared.atoms.property).u32(0).u32(0).u32(READ_WORDS);
        self.shared.send(r.finish());
        let reply = self.wait(Instant::now() + ANSWER_WAIT, |m| match m {
            Msg::Reply(r) => Some(Some(r)),
            Msg::Error => Some(None),
            _ => None,
        })??;
        Some((u32_at(&reply, 8), super::property_value(&reply).to_vec()))
    }

    pub(crate) fn get(&mut self, primary: bool) -> Option<String> {
        let selection = self.selection(primary);
        while self.rx.try_recv().is_ok() {}
        if self.own(selection)
            && let Some(own) = self.shared.owned(selection).and_then(|m| m.lock().ok()?.clone())
        {
            return Some(own.text);
        }
        let atoms = self.shared.atoms;
        for target in [atoms.utf8, ATOM_STRING] {
            let mut r = Req::new(OP_CONVERT_SELECTION, 0);
            r.u32(self.shared.window).u32(selection).u32(target).u32(atoms.property).u32(0);
            self.shared.send(r.finish());
            let property = self.wait(Instant::now() + ANSWER_WAIT, |m| match m {
                Msg::Notify { selection: s, property } if s == selection => Some(property),
                _ => None,
            })?;
            if property == 0 {
                continue;
            }
            let (ty, mut data) = self.take()?;
            if ty == atoms.incr {
                data.clear();
                loop {
                    let deadline = Instant::now() + ANSWER_WAIT;
                    self.wait(deadline, |m| match m {
                        Msg::Property { atom, new: true, .. } if atom == atoms.property => Some(()),
                        _ => None,
                    })?;
                    let (_, chunk) = self.take()?;
                    if chunk.is_empty() {
                        break;
                    }
                    data.extend_from_slice(&chunk);
                }
            }
            return Some(if target == ATOM_STRING { from_latin1(&data) } else { String::from_utf8_lossy(&data).into_owned() });
        }
        None
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        if let Ok(w) = self.shared.writer.lock() {
            let _ = w.shutdown(Shutdown::Both);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms() -> Atoms {
        Atoms::from_list(std::array::from_fn(|i| 100 + i as u32))
    }

    #[test]
    fn atom_names_line_up() {
        let a = atoms();
        assert_eq!((a.clipboard, a.stamp), (100, 109));
        assert_eq!(Atoms::NAMES[2], "TARGETS");
        assert_eq!(a.targets, 102);
    }

    #[test]
    fn text_is_offered_in_every_usual_target() {
        let a = atoms();
        let own = Owned { text: "Kick é ✓".into(), time: 77 };
        let (ty, format, data) = convert(&a, a.targets, &own).unwrap();
        assert_eq!((ty, format), (ATOM_ATOM, 32));
        let targets: Vec<u32> = data.chunks(4).map(|c| u32_at(c, 0)).collect();
        for t in [a.utf8, a.text, ATOM_STRING, a.timestamp, a.plain_utf8] {
            assert!(targets.contains(&t));
        }
        assert_eq!(convert(&a, a.utf8, &own), Some((a.utf8, 8, "Kick é ✓".as_bytes().to_vec())));
        // TEXT answers in UTF-8; STRING in Latin-1, `?` for what it cannot hold.
        assert_eq!(convert(&a, a.text, &own).unwrap().0, a.utf8);
        assert_eq!(convert(&a, ATOM_STRING, &own), Some((ATOM_STRING, 8, b"Kick \xe9 ?".to_vec())));
        assert_eq!(convert(&a, a.timestamp, &own), Some((ATOM_INTEGER, 32, 77u32.to_le_bytes().to_vec())));
        assert_eq!(convert(&a, 9999, &own), None);
        assert_eq!(from_latin1(b"caf\xe9"), "café");
    }

    #[test]
    fn selection_notify_layout() {
        let e = selection_notify(1, 2, 3, 4, 5);
        assert_eq!(e[0], 31);
        assert_eq!([u32_at(&e, 4), u32_at(&e, 8), u32_at(&e, 12), u32_at(&e, 16), u32_at(&e, 20)], [1, 2, 3, 4, 5]);
        let req = change_property_req(9, 10, 11, 8, b"abc", true);
        assert_eq!((req[0], req[1], u32_at(&req, 20)), (OP_CHANGE_PROPERTY, 2, 3));
        assert_eq!(req.len(), 28);
    }

    /// By hand, with other programs: `PONZIK_CLIP_TEXT=hello cargo test -p
    /// ponzik-window holds_the_clipboard -- --ignored --nocapture`, then paste
    /// elsewhere (`xclip -o -selection clipboard`, `wl-paste`) within 5 s; at
    /// the end it prints what the clipboard holds then.
    #[test]
    #[ignore]
    fn holds_the_clipboard_for_other_programs() {
        let mut c = Clipboard::open().expect("a display");
        let text = std::env::var("PONZIK_CLIP_TEXT").unwrap_or_else(|_| "Ponzik ✓".into());
        c.set(false, &text);
        thread::sleep(Duration::from_secs(5));
        println!("clipboard now: {:?}", c.get(false));
    }

    /// Two clipboards on the running X server: one owns, the other reads.
    /// Skipped without a display.
    #[test]
    fn copy_and_paste_between_two_connections() {
        let (Ok(mut a), Ok(mut b)) = (Clipboard::open(), Clipboard::open()) else { return };
        a.set(false, "Ponzik clipboard ✓");
        assert_eq!(b.get(false).as_deref(), Some("Ponzik clipboard ✓"));
        a.set(true, "primary text");
        assert_eq!(b.get(true).as_deref(), Some("primary text"));
        // Taking the selection back: the first one now reads from the second.
        b.set(false, "back");
        let deadline = Instant::now() + Duration::from_secs(2);
        while a.get(false).as_deref() != Some("back") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(a.get(false).as_deref(), Some("back"));
        // A text larger than one request travels in INCR chunks.
        let big: String = (0..300_000).map(|i| char::from(b'a' + (i % 26) as u8)).chain("é".chars()).collect();
        a.set(false, &big);
        assert_eq!(b.get(false).map(|s| s.len()), Some(big.len()));
        assert_eq!(b.get(false).as_deref(), Some(big.as_str()));
    }
}
