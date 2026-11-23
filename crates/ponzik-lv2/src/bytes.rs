//! A lock-free ring of variable-size messages between two threads (one
//! writer, one reader): worker requests and responses, atoms between a
//! plugin and its interface. Neither side allocates or waits.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes in front of each message: its length.
const HEADER: usize = 4;

pub(crate) struct ByteRing {
    buf: Box<[UnsafeCell<u8>]>,
    mask: usize,
    /// Total bytes written (writer side).
    head: AtomicUsize,
    /// Total bytes read (reader side).
    tail: AtomicUsize,
}

// SAFETY: one writer and one reader at a time, synchronized by `head` and `tail`.
unsafe impl Sync for ByteRing {}
unsafe impl Send for ByteRing {}

impl ByteRing {
    /// A ring of at least `capacity` bytes (rounded up to a power of two).
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(64).next_power_of_two();
        ByteRing {
            buf: (0..cap).map(|_| UnsafeCell::new(0)).collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn cap(&self) -> usize {
        self.mask + 1
    }

    /// Copies `data` in at the absolute position `at` (writer only).
    fn put(&self, at: usize, data: &[u8]) {
        for (k, &b) in data.iter().enumerate() {
            // SAFETY: the reader never touches the bytes between `tail` and the
            // free space the writer checked.
            unsafe { *self.buf[(at + k) & self.mask].get() = b };
        }
    }

    fn get(&self, at: usize, out: &mut [u8]) {
        for (k, b) in out.iter_mut().enumerate() {
            // SAFETY: bytes published by the writer (before `head`).
            *b = unsafe { *self.buf[(at + k) & self.mask].get() };
        }
    }

    /// Writes one message made of `parts` back to back; false (nothing
    /// written) when it does not fit. Writer side only.
    pub(crate) fn push(&self, parts: &[&[u8]]) -> bool {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len > u32::MAX as usize {
            return false;
        }
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if self.cap() - (head - tail) < HEADER + len {
            return false;
        }
        self.put(head, &(len as u32).to_le_bytes());
        let mut at = head + HEADER;
        for p in parts {
            self.put(at, p);
            at += p.len();
        }
        self.head.store(at, Ordering::Release);
        true
    }

    /// Reads the next message into `out`: its length, or `None` when the
    /// ring is empty. A message longer than `out` is dropped (`Some(0)`).
    /// Reader side only.
    pub(crate) fn pop(&self, out: &mut [u8]) -> Option<usize> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let mut len = [0u8; HEADER];
        self.get(tail, &mut len);
        let len = u32::from_le_bytes(len) as usize;
        let n = if len <= out.len() {
            self.get(tail + HEADER, &mut out[..len]);
            len
        } else {
            0
        };
        self.tail.store(tail + HEADER + len, Ordering::Release);
        Some(n)
    }

    /// Drops everything waiting (reader side).
    pub(crate) fn clear(&self) {
        self.tail.store(self.head.load(Ordering::Acquire), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn messages_come_out_whole_and_in_order() {
        let r = ByteRing::new(64);
        assert!(r.push(&[b"ab", b"cde"]));
        assert!(r.push(&[b""]));
        let mut out = [0u8; 16];
        assert_eq!(r.pop(&mut out), Some(5));
        assert_eq!(&out[..5], b"abcde");
        assert_eq!(r.pop(&mut out), Some(0));
        assert_eq!(r.pop(&mut out), None);
        // Full: refused, then room again after a read (and wrapping around).
        for round in 0..20u8 {
            assert!(r.push(&[&[round; 20]]));
            assert!(r.push(&[&[round + 1; 20]]));
            assert!(!r.push(&[&[0; 30]]), "no room left");
            assert_eq!(r.pop(&mut out), Some(0), "too long for `out`: dropped");
            let mut big = [0u8; 32];
            assert_eq!(r.pop(&mut big), Some(20));
            assert_eq!(big[..20], [round + 1; 20]);
        }
        r.push(&[b"x"]);
        r.clear();
        assert_eq!(r.pop(&mut out), None);
    }

    #[test]
    fn two_threads_exchange_without_loss() {
        let r = Arc::new(ByteRing::new(256));
        let w = r.clone();
        let t = std::thread::spawn(move || {
            for i in 0..20_000u32 {
                while !w.push(&[&i.to_le_bytes(), &[i as u8; 3]]) {
                    std::hint::spin_loop();
                }
            }
        });
        let mut out = [0u8; 16];
        let mut next = 0u32;
        while next < 20_000 {
            if let Some(n) = r.pop(&mut out) {
                assert_eq!(n, 7);
                assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), next);
                assert_eq!(out[4..7], [next as u8; 3]);
                next += 1;
            }
        }
        t.join().unwrap();
    }
}
