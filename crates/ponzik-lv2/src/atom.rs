//! Atom sequence buffers of the atom ports, allocated once: the host writes
//! events (MIDI, interface messages) into an input sequence before `run`,
//! and reads what the plugin wrote into an output one after.

use std::ffi::c_void;

use crate::abi::{LV2_Atom, LV2_Atom_Event, LV2_Atom_Sequence, LV2_Atom_Sequence_Body, atom_pad};

const SEQ: usize = size_of::<LV2_Atom_Sequence>();
const EV: usize = size_of::<LV2_Atom_Event>();

/// A sequence buffer, 8-byte aligned.
pub(crate) struct AtomBuf {
    data: Box<[u64]>,
}

impl AtomBuf {
    /// A buffer of at least `bytes` (header included).
    pub(crate) fn new(bytes: usize) -> Self {
        let words = bytes.max(SEQ + 64).div_ceil(8);
        AtomBuf { data: vec![0u64; words].into_boxed_slice() }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.data.len() * 8
    }

    pub(crate) fn as_ptr(&mut self) -> *mut c_void {
        self.data.as_mut_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the words are plain bytes too.
        unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast::<u8>(), self.capacity()) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        let n = self.capacity();
        // SAFETY: idem, exclusive borrow.
        unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr().cast::<u8>(), n) }
    }

    fn header(&mut self) -> &mut LV2_Atom_Sequence {
        // SAFETY: aligned, at least `SEQ` bytes.
        unsafe { &mut *self.data.as_mut_ptr().cast::<LV2_Atom_Sequence>() }
    }

    /// An empty input sequence (frame time).
    pub(crate) fn clear_input(&mut self, sequence: u32) {
        let h = self.header();
        h.atom = LV2_Atom { size: size_of::<LV2_Atom_Sequence_Body>() as u32, type_: sequence };
        h.body = LV2_Atom_Sequence_Body { unit: 0, pad: 0 };
    }

    /// Room for the plugin to write its output sequence.
    pub(crate) fn prepare_output(&mut self, chunk: u32) {
        let cap = self.capacity();
        let h = self.header();
        h.atom = LV2_Atom { size: (cap - size_of::<LV2_Atom>()) as u32, type_: chunk };
        h.body = LV2_Atom_Sequence_Body { unit: 0, pad: 0 };
    }

    fn used(&self) -> usize {
        // SAFETY: aligned header.
        let size = unsafe { (*self.data.as_ptr().cast::<LV2_Atom_Sequence>()).atom.size } as usize;
        (size_of::<LV2_Atom>() + size).min(self.capacity())
    }

    /// Bytes holding events, if the buffer holds a sequence (a plugin that
    /// wrote nothing leaves the chunk: no events).
    fn events_end(&self, sequence: u32) -> usize {
        // SAFETY: aligned header.
        let ty = unsafe { (*self.data.as_ptr().cast::<LV2_Atom_Sequence>()).atom.type_ };
        if ty == sequence { self.used() } else { 0 }
    }

    /// Appends an event at `frames` (events must come in time order); false
    /// when the buffer is full.
    pub(crate) fn push(&mut self, frames: i64, type_: u32, body: &[u8]) -> bool {
        let at = self.used();
        let total = EV + atom_pad(body.len() as u32) as usize;
        if at + total > self.capacity() || body.len() > u32::MAX as usize {
            return false;
        }
        let ev = LV2_Atom_Event { frames, body: LV2_Atom { size: body.len() as u32, type_ } };
        let bytes = self.bytes_mut();
        // SAFETY: `LV2_Atom_Event` is plain data of `EV` bytes.
        let raw = unsafe { std::slice::from_raw_parts((&ev as *const LV2_Atom_Event).cast::<u8>(), EV) };
        bytes[at..at + EV].copy_from_slice(raw);
        bytes[at + EV..at + EV + body.len()].copy_from_slice(body);
        bytes[at + EV + body.len()..at + total].fill(0);
        self.header().atom.size += total as u32;
        true
    }

    /// Events of the sequence the plugin wrote: `(frames, type, body)`.
    /// The sizes are checked, a broken sequence ends early.
    pub(crate) fn for_each(&self, sequence: u32, mut f: impl FnMut(i64, u32, &[u8])) {
        let used = self.events_end(sequence);
        let bytes = self.bytes();
        let mut at = SEQ;
        while at + EV <= used {
            // SAFETY: 8-byte aligned (events are padded), inside the buffer.
            let ev = unsafe { *bytes.as_ptr().add(at).cast::<LV2_Atom_Event>() };
            let size = ev.body.size as usize;
            if at + EV + size > used {
                break;
            }
            f(ev.frames, ev.body.type_, &bytes[at + EV..at + EV + size]);
            at += EV + atom_pad(ev.body.size) as usize;
        }
    }

    /// Same, with the atom (header and body) of each event, as an interface
    /// gets it through `port_event`.
    pub(crate) fn for_each_atom(&self, sequence: u32, mut f: impl FnMut(&[u8])) {
        let used = self.events_end(sequence);
        let bytes = self.bytes();
        let mut at = SEQ;
        while at + EV <= used {
            // SAFETY: as in `for_each`.
            let ev = unsafe { *bytes.as_ptr().add(at).cast::<LV2_Atom_Event>() };
            let size = ev.body.size as usize;
            if at + EV + size > used {
                break;
            }
            f(&bytes[at + 8..at + EV + size]);
            at += EV + atom_pad(ev.body.size) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_go_in_and_come_out() {
        let mut b = AtomBuf::new(128);
        b.clear_input(7);
        assert!(b.push(0, 3, &[0x90, 60, 100]));
        assert!(b.push(12, 3, &[0x80, 60, 0]));
        let mut got = Vec::new();
        b.for_each(7, |t, ty, body| got.push((t, ty, body.to_vec())));
        assert_eq!(got, [(0, 3, vec![0x90, 60, 100]), (12, 3, vec![0x80, 60, 0])]);
        let mut atoms = Vec::new();
        b.for_each_atom(7, |a| atoms.push(a.to_vec()));
        assert_eq!(atoms[0], [3, 0, 0, 0, 3, 0, 0, 0, 0x90, 60, 100]);
        // Full: refused without damage.
        let big = [0u8; 200];
        assert!(!b.push(20, 3, &big));
        let mut n = 0;
        b.for_each(7, |_, _, _| n += 1);
        assert_eq!(n, 2);
        b.clear_input(7);
        b.for_each(7, |_, _, _| panic!("empty"));
        // An output buffer offers all its room.
        b.prepare_output(9);
        // SAFETY: aligned header.
        let size = unsafe { (*b.as_ptr().cast::<LV2_Atom_Sequence>()).atom.size };
        assert_eq!(size as usize, b.capacity() - 8);
        b.for_each(7, |_, _, _| panic!("a chunk holds no events"));
    }
}
