//! File SPSC (un producteur, un consommateur) sans verrou ni allocation après création.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Shared<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    /// Prochain indice à lire ; écrit uniquement par le consommateur.
    head: AtomicUsize,
    /// Prochain indice à écrire ; écrit uniquement par le producteur.
    tail: AtomicUsize,
}

// SAFETY: chaque case est accédée soit par le producteur (avant publication de
// `tail`), soit par le consommateur (avant publication de `head`), jamais les deux.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let mut i = *self.head.get_mut();
        let tail = *self.tail.get_mut();
        while i != tail {
            // SAFETY: les cases entre head et tail sont initialisées et jamais lues.
            unsafe { self.slots[i & self.mask].get_mut().assume_init_drop() };
            i = i.wrapping_add(1);
        }
    }
}

pub struct Producer<T> {
    shared: Arc<Shared<T>>,
}

pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
}

/// Crée une file d'au moins `capacity` éléments (arrondi à la puissance de 2 supérieure).
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let capacity = capacity.max(2).next_power_of_two();
    let slots = (0..capacity).map(|_| UnsafeCell::new(MaybeUninit::uninit())).collect();
    let shared = Arc::new(Shared {
        slots,
        mask: capacity - 1,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (Producer { shared: shared.clone() }, Consumer { shared })
}

impl<T> Producer<T> {
    /// Rend la valeur si la file est pleine.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let s = &*self.shared;
        let tail = s.tail.load(Ordering::Relaxed);
        let head = s.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == s.slots.len() {
            return Err(value);
        }
        // SAFETY: case libre (hors de [head, tail)), seul le producteur y écrit.
        unsafe { (*s.slots[tail & s.mask].get()).write(value) };
        s.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

impl<T> Consumer<T> {
    pub fn pop(&mut self) -> Option<T> {
        let s = &*self.shared;
        let head = s.head.load(Ordering::Relaxed);
        let tail = s.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: case publiée par le producteur via `tail` (Release/Acquire).
        let value = unsafe { (*s.slots[head & s.mask].get()).assume_init_read() };
        s.head.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn fifo_order_and_capacity() {
        let (mut tx, mut rx) = channel(3);
        for i in 0..4 {
            tx.push(i).unwrap();
        }
        assert_eq!(tx.push(99), Err(99));
        assert_eq!(rx.pop(), Some(0));
        tx.push(4).unwrap();
        assert_eq!((1..=4).map(|_| rx.pop().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn drops_unread_items() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct D;
        impl Drop for D {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (mut tx, mut rx) = channel(8);
        for _ in 0..5 {
            assert!(tx.push(D).is_ok());
        }
        drop(rx.pop());
        drop((tx, rx));
        assert_eq!(DROPS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn cross_thread() {
        let (mut tx, mut rx) = channel::<u64>(64);
        const N: u64 = 200_000;
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                while tx.push(i).is_err() {
                    std::hint::spin_loop();
                }
            }
        });
        let mut expected = 0;
        while expected < N {
            if let Some(v) = rx.pop() {
                assert_eq!(v, expected);
                expected += 1;
            }
        }
        producer.join().unwrap();
    }
}
