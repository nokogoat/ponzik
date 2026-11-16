//! Sortie MIDI horodatée : du thread audio vers un thread d'envoi.
//!
//! Le thread audio pousse des [`TimedMessage`] dans une file SPSC sans verrou
//! ni allocation ([`MessageSender::push`]). Le thread d'envoi de
//! [`OutputScheduler`] les trie par échéance et écrit chacun sur son port au
//! plus près de son heure, mesurée sur l'horloge monotone [`now_ns`].
//!
//! # Gigue
//!
//! L'échéance d'un message est calculée par le moteur à partir de l'instant de
//! début de son bloc audio, plus la latence de sortie : elle hérite donc de la
//! gigue de réveil du rappel audio (typiquement quelques dizaines à quelques
//! centaines de µs). Le thread d'envoi dort jusqu'à ~200 µs de l'échéance
//! (au plus 1 ms d'affilée, pour relever la file), puis attend activement :
//! sous Linux et macOS l'écart à l'échéance reste sous ~100 µs, sous Windows
//! le sommeil a une granularité de ~1 à 15 ms mais l'attente active finale
//! rattrape l'essentiel. Viennent ensuite la file du pilote et le câble (une
//! liaison DIN à 31 250 bauds transmet un message de trois octets en ~1 ms).
//! Un message dont l'échéance est passée part immédiatement ; l'ordre des
//! messages de même échéance est conservé.
//!
//! Les ports sont ouverts au premier message qui les vise (pas de port
//! réservé inutilement). Un port qui refuse un envoi (débranché) est fermé
//! et rouvert au besoin, au plus une fois par seconde. À l'arrêt, chaque port
//! ouvert reçoit Stop (si l'horloge tournait) et « All Notes Off » sur les
//! canaux utilisés.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::message::{CONTINUE, STOP, START, ShortMessage};
use crate::{MidiError, MidiPortInfo};

/// Ports visés au plus (indice de port dans un `u8`, masque d'horloge sur 32 bits).
pub const MAX_PORTS: usize = 32;
/// Capacité par défaut de la file : plusieurs blocs audio de notes et d'horloge.
pub const DEFAULT_QUEUE: usize = 8_192;
/// Sommeil maximal du thread d'envoi entre deux relevés de la file.
const POLL: Duration = Duration::from_millis(1);
/// En deçà, on attend activement l'échéance plutôt que de dormir.
const SPIN_NS: u64 = 200_000;
/// Délai avant de retenter l'ouverture d'un port qui a échoué.
const RETRY_NS: u64 = 1_000_000_000;

/// Nanosecondes écoulées depuis l'origine de l'horloge des sorties MIDI
/// (fixée au premier appel). Sans allocation ni verrou après le premier appel :
/// utilisable depuis le thread audio.
pub fn now_ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Message à envoyer sur un port à une échéance donnée.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimedMessage {
    /// Indice du port dans la liste du planificateur.
    pub port: u8,
    /// Échéance sur l'horloge [`now_ns`].
    pub at_ns: u64,
    /// Échantillon du moteur auquel le message correspond (diagnostic, tests).
    pub frame: u64,
    pub message: ShortMessage,
}

struct Ring {
    slots: Box<[UnsafeCell<TimedMessage>]>,
    mask: usize,
    /// Prochain indice à lire ; écrit uniquement par le consommateur.
    head: AtomicUsize,
    /// Prochain indice à écrire ; écrit uniquement par le producteur.
    tail: AtomicUsize,
    /// Messages perdus faute de place.
    dropped: AtomicU64,
}

// SAFETY: chaque case est écrite par le producteur avant publication de `tail`
// et lue par le consommateur avant publication de `head`, jamais les deux à la fois.
unsafe impl Sync for Ring {}
// SAFETY: idem ; les cases ne contiennent que des valeurs `Copy`.
unsafe impl Send for Ring {}

/// Côté producteur (thread audio) de la file.
pub struct MessageSender {
    ring: Arc<Ring>,
}

/// Côté consommateur de la file.
pub struct MessageReceiver {
    ring: Arc<Ring>,
}

/// File SPSC d'au moins `capacity` messages (arrondi à la puissance de 2).
pub fn queue(capacity: usize) -> (MessageSender, MessageReceiver) {
    let capacity = capacity.max(2).next_power_of_two();
    let ring = Arc::new(Ring {
        slots: (0..capacity).map(|_| UnsafeCell::new(TimedMessage::default())).collect(),
        mask: capacity - 1,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        dropped: AtomicU64::new(0),
    });
    (MessageSender { ring: ring.clone() }, MessageReceiver { ring })
}

impl MessageSender {
    /// Ajoute un message sans bloquer ni allouer ; renvoie `false` (et le compte
    /// comme perdu) si la file est pleine.
    pub fn push(&mut self, message: TimedMessage) -> bool {
        let r = &*self.ring;
        let tail = r.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(r.head.load(Ordering::Acquire)) == r.slots.len() {
            r.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // SAFETY: case libre (hors de [head, tail)), seul le producteur y écrit.
        unsafe { *r.slots[tail & r.mask].get() = message };
        r.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Messages perdus faute de place depuis la création de la file.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

impl MessageReceiver {
    pub fn pop(&mut self) -> Option<TimedMessage> {
        let r = &*self.ring;
        let head = r.head.load(Ordering::Relaxed);
        if head == r.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: case publiée par le producteur via `tail` (Release/Acquire).
        let message = unsafe { *r.slots[head & r.mask].get() };
        r.head.store(head.wrapping_add(1), Ordering::Release);
        Some(message)
    }

    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

/// Destination d'octets MIDI : port réel ([`crate::MidiOutput`]) ou factice (tests).
pub trait MidiSink: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError>;
}

/// Ouvre le port d'indice donné ; appelé par le thread d'envoi, au besoin.
pub type PortOpener = Box<dyn FnMut(u8) -> Result<Box<dyn MidiSink>, MidiError> + Send>;

#[derive(Default)]
struct Slot {
    sink: Option<Box<dyn MidiSink>>,
    /// Pas de nouvelle tentative d'ouverture avant cette heure.
    retry_at: u64,
    /// Canaux qui ont reçu un message (bit par canal).
    channels: u16,
    /// Start ou Continue envoyé, pas encore Stop.
    clock_running: bool,
}

/// Thread d'envoi ; le détruire envoie les messages de fin puis ferme les ports.
pub struct OutputScheduler {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    ports: Vec<MidiPortInfo>,
}

impl OutputScheduler {
    /// Démarre un thread d'envoi qui ouvre ses ports avec `opener`. Renvoie le
    /// côté producteur de la file, à confier au moteur.
    pub fn start(capacity: usize, opener: PortOpener) -> Result<(MessageSender, OutputScheduler), MidiError> {
        Self::start_named(capacity, opener, Vec::new())
    }

    /// Planificateur sur les ports de sortie du système : l'indice d'un port
    /// est sa place dans [`OutputScheduler::ports`] (liste relevée au démarrage).
    pub fn start_system(capacity: usize) -> Result<(MessageSender, OutputScheduler), MidiError> {
        let ports = crate::list_outputs()?;
        let ids: Vec<String> = ports.iter().map(|p| p.id.clone()).collect();
        let opener: PortOpener = Box::new(move |index| {
            let id = ids.get(index as usize).ok_or_else(|| MidiError::PortNotFound(format!("#{index}")))?;
            Ok(Box::new(crate::open_output(id)?) as Box<dyn MidiSink>)
        });
        Self::start_named(capacity, opener, ports)
    }

    fn start_named(
        capacity: usize,
        opener: PortOpener,
        ports: Vec<MidiPortInfo>,
    ) -> Result<(MessageSender, OutputScheduler), MidiError> {
        now_ns();
        let (tx, rx) = queue(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("ponzik-midi-out".into())
            .spawn(move || run(rx, opener, &flag))
            .map_err(|e| MidiError::System(format!("MIDI output thread: {e}")))?;
        Ok((tx, OutputScheduler { stop, thread: Some(thread), ports }))
    }

    /// Ports de sortie relevés au démarrage (vide pour [`OutputScheduler::start`]).
    pub fn ports(&self) -> &[MidiPortInfo] {
        &self.ports
    }
}

impl Drop for OutputScheduler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Insère `m` après les messages de même échéance (ordre d'arrivée conservé).
fn insert_sorted(pending: &mut Vec<TimedMessage>, m: TimedMessage) {
    let at = pending.partition_point(|p| p.at_ns <= m.at_ns);
    pending.insert(at, m);
}

fn run(mut rx: MessageReceiver, mut opener: PortOpener, stop: &AtomicBool) {
    let mut slots: Vec<Slot> = (0..MAX_PORTS).map(|_| Slot::default()).collect();
    let mut pending: Vec<TimedMessage> = Vec::with_capacity(DEFAULT_QUEUE);
    while !stop.load(Ordering::Acquire) {
        while let Some(m) = rx.pop() {
            insert_sorted(&mut pending, m);
        }
        let now = now_ns();
        let due = pending.partition_point(|p| p.at_ns <= now);
        for m in pending.drain(..due) {
            deliver(&mut slots, &mut opener, m, now);
        }
        match pending.first().map(|p| p.at_ns.saturating_sub(now_ns())) {
            Some(wait) if wait <= SPIN_NS => std::hint::spin_loop(),
            Some(wait) => thread::park_timeout(Duration::from_nanos(wait - SPIN_NS).min(POLL)),
            None => thread::park_timeout(POLL),
        }
    }
    // Fin : les messages en attente partent tout de suite, puis Stop et
    // All Notes Off pour ne rien laisser tourner ni sonner.
    while let Some(m) = rx.pop() {
        insert_sorted(&mut pending, m);
    }
    let now = now_ns();
    for m in pending.drain(..) {
        deliver(&mut slots, &mut opener, m, now);
    }
    for slot in &mut slots {
        let Some(sink) = slot.sink.as_mut() else { continue };
        if slot.clock_running {
            let _ = sink.send(ShortMessage::realtime(STOP).bytes());
        }
        for ch in (0..16u8).filter(|c| slot.channels & (1 << c) != 0) {
            let _ = sink.send(ShortMessage::all_notes_off(ch).bytes());
        }
    }
}

fn deliver(slots: &mut [Slot], opener: &mut PortOpener, m: TimedMessage, now: u64) {
    let Some(slot) = slots.get_mut(m.port as usize) else { return };
    if slot.sink.is_none() {
        if now < slot.retry_at {
            return;
        }
        match opener(m.port) {
            Ok(sink) => slot.sink = Some(sink),
            Err(e) => {
                eprintln!("ponzik-midi: output {}: {e}", m.port);
                slot.retry_at = now + RETRY_NS;
                return;
            }
        }
    }
    let Some(sink) = slot.sink.as_mut() else { return };
    if let Err(e) = sink.send(m.message.bytes()) {
        eprintln!("ponzik-midi: output {}: {e}", m.port);
        slot.sink = None;
        slot.retry_at = now + RETRY_NS;
        return;
    }
    if let Some(ch) = m.message.channel() {
        slot.channels |= 1 << ch;
    }
    match m.message.status() {
        START | CONTINUE => slot.clock_running = true,
        STOP => slot.clock_running = false,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::CLOCK;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<(u8, u64, Vec<u8>)>>>;

    /// Port factice : journalise (port, heure d'envoi, octets).
    struct FakePort {
        port: u8,
        log: Log,
    }

    impl MidiSink for FakePort {
        fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
            self.log.lock().unwrap().push((self.port, now_ns(), bytes.to_vec()));
            Ok(())
        }
    }

    fn fake_scheduler() -> (MessageSender, OutputScheduler, Log, Arc<AtomicUsize>) {
        let log: Log = Arc::default();
        let opened = Arc::new(AtomicUsize::new(0));
        let (l, o) = (log.clone(), opened.clone());
        let opener: PortOpener = Box::new(move |port| {
            o.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakePort { port, log: l.clone() }) as Box<dyn MidiSink>)
        });
        let (tx, sched) = OutputScheduler::start(64, opener).unwrap();
        (tx, sched, log, opened)
    }

    fn msg(port: u8, at_ns: u64, message: ShortMessage) -> TimedMessage {
        TimedMessage { port, at_ns, frame: 0, message }
    }

    #[test]
    fn queue_is_fifo_and_counts_drops() {
        let (mut tx, mut rx) = queue(4);
        for i in 0..4 {
            assert!(tx.push(msg(0, i, ShortMessage::realtime(CLOCK))));
        }
        assert!(!tx.push(msg(0, 9, ShortMessage::realtime(CLOCK))));
        assert_eq!(tx.dropped(), 1);
        assert_eq!((0..4).map(|_| rx.pop().unwrap().at_ns).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn sorted_insert_keeps_arrival_order_on_ties() {
        let mut pending = Vec::new();
        for (at, key) in [(20, 1), (10, 2), (20, 3), (10, 4), (5, 5)] {
            insert_sorted(&mut pending, msg(0, at, ShortMessage::note_on(0, key, 100)));
        }
        let keys: Vec<u8> = pending.iter().map(|m| m.message.bytes()[1]).collect();
        assert_eq!(keys, [5, 2, 4, 1, 3]);
    }

    #[test]
    fn messages_go_out_in_deadline_order_near_their_time() {
        let (mut tx, sched, log, opened) = fake_scheduler();
        let t0 = now_ns() + 20_000_000;
        let ms = 1_000_000;
        // Poussés dans le désordre, sur deux ports.
        tx.push(msg(1, t0 + 10 * ms, ShortMessage::note_off(0, 60, 0)));
        tx.push(msg(0, t0, ShortMessage::realtime(START)));
        tx.push(msg(1, t0 + 2 * ms, ShortMessage::note_on(0, 60, 100)));
        tx.push(msg(0, t0 + 5 * ms, ShortMessage::realtime(CLOCK)));
        while log.lock().unwrap().len() < 4 {
            thread::sleep(Duration::from_millis(2));
        }
        let entries = log.lock().unwrap().clone();
        let order: Vec<(u8, u8)> = entries.iter().map(|(p, _, b)| (*p, b[0])).collect();
        assert_eq!(order, [(0, 0xFA), (1, 0x90), (0, 0xF8), (1, 0x80)]);
        for ((_, sent, _), due) in entries.iter().zip([t0, t0 + 2 * ms, t0 + 5 * ms, t0 + 10 * ms]) {
            assert!(*sent >= due, "sent {} ns early", due - sent);
            // Large marge : une machine de test chargée peut retarder le thread.
            assert!(*sent - due < 15 * ms, "sent {} µs late", (sent - due) / 1000);
        }
        assert_eq!(opened.load(Ordering::SeqCst), 2, "each port is opened once");
        drop(sched);
    }

    #[test]
    fn shutdown_stops_the_clock_and_releases_used_channels() {
        let (mut tx, sched, log, _) = fake_scheduler();
        let now = now_ns();
        tx.push(msg(0, now, ShortMessage::realtime(START)));
        tx.push(msg(0, now, ShortMessage::note_on(3, 60, 100)));
        tx.push(msg(2, now, ShortMessage::note_on(9, 36, 100)));
        // Une échéance lointaine part quand même à l'arrêt.
        tx.push(msg(2, now + 60_000_000_000, ShortMessage::note_off(9, 36, 0)));
        drop(sched);
        let sent: Vec<(u8, Vec<u8>)> = log.lock().unwrap().iter().map(|(p, _, b)| (*p, b.clone())).collect();
        let tail = &sent[sent.len() - 3..];
        assert!(sent.contains(&(2, vec![0x89, 36, 0])));
        assert!(tail.contains(&(0, vec![0xFC])));
        assert!(tail.contains(&(0, vec![0xB3, 123, 0])));
        assert!(tail.contains(&(2, vec![0xB9, 123, 0])));
    }

    #[test]
    fn failing_port_is_retried_later_not_every_message() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = attempts.clone();
        let opener: PortOpener = Box::new(move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Err(MidiError::PortNotFound("gone".into()))
        });
        let (mut tx, sched) = OutputScheduler::start(64, opener).unwrap();
        let now = now_ns();
        for _ in 0..10 {
            tx.push(msg(0, now, ShortMessage::realtime(CLOCK)));
        }
        thread::sleep(Duration::from_millis(20));
        drop(sched);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
