//! Instrument Rack (Ableton's Instrument Rack, FL Studio's Layer): up to
//! [`CHAINS`] chains played at once from one channel, each an instrument
//! (native or plugin) behind its own note effects and followed by its own
//! effects, with a level, a pan, mute and solo, and three zones that decide
//! which notes it plays:
//!
//! - the **key zone** (with fades: the velocity of the notes in a fade drops
//!   to nothing at its outer edge, a key split or a crossfade over the
//!   keyboard),
//! - the **velocity zone** (same, velocity layers),
//! - the **chain selector zone**: one value (0 to 127) for the whole rack,
//!   [`P_SELECTOR`], and a range per chain; its fades scale the chain's level
//!   continuously, so a macro on the selector morphs from one layer to the
//!   next.
//!
//! A chain plays a note shifted by its transpose. Sixteen macros drive any
//! parameter of any device of any chain, and the rack's own levels and
//! selector, through [`MacroMap`]s (a map whose `unit` is [`SELF_UNIT`]
//! targets the rack itself).
//!
//! Every device has a stable *unit* number, `chain * SLOTS + slot` (slot 0:
//! the instrument, then the note effects, then the effects): its parameters
//! are addressed through the rack with [`unit_param`], like in the Effect
//! Rack, so automation, modulators and presets see one flat list.
//!
//! Real time: everything is allocated on construction; dispatching notes,
//! moving macros and rendering allocate nothing. Effects inside a chain add
//! no latency compensation (an instrument has none).

use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use crate::effects::fx_rack::MacroMap;
use crate::midi_out::MidiOutEvent;
use crate::note_fx::{NoteBuffer, NoteEvent, NoteFx};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Effect, Instrument, NoteExpression};

/// Kind of the rack in the registry.
pub const KIND: &str = "instrument_rack";
/// Chains in a rack.
pub const CHAINS: usize = 16;
/// Note effects before a chain's instrument.
pub const CHAIN_NOTE_FX: usize = 2;
/// Effects after a chain's instrument.
pub const CHAIN_FX: usize = 5;
/// Devices of one chain: its instrument, its note effects, its effects.
pub const SLOTS: usize = 1 + CHAIN_NOTE_FX + CHAIN_FX;
/// Device numbers in a rack.
pub const UNITS: usize = CHAINS * SLOTS;
/// Unit of a macro mapping that drives one of the rack's own parameters
/// (chain selector, chain level or pan).
pub const SELF_UNIT: usize = UNITS;
/// Macro knobs.
pub const MACROS: usize = 16;
/// Macro mappings in a rack.
pub const MAX_MAPPINGS: usize = 128;

/// First macro (ids `P_MACRO..P_MACRO + 16`).
pub const P_MACRO: u32 = 0;
/// Chain selector, 0 to 127.
pub const P_SELECTOR: u32 = 16;
/// Level of each chain in dB (ids `P_VOLUME..P_VOLUME + 16`).
pub const P_VOLUME: u32 = 17;
/// Pan of each chain (ids `P_PAN..P_PAN + 16`).
pub const P_PAN: u32 = 33;
/// Parameters of the rack itself; above, those of its devices.
pub const RACK_PARAMS: u32 = 49;

const UNIT_SHIFT: u32 = 24;
const LOCAL_MASK: u32 = (1 << UNIT_SHIFT) - 1;

/// Rack parameter id of parameter `id` of the device `unit`. Plugin
/// parameters above 2²⁴ cannot be addressed through a rack.
pub const fn unit_param(unit: usize, id: u32) -> u32 {
    ((unit as u32 + 1) << UNIT_SHIFT) | (id & LOCAL_MASK)
}

/// `(unit, id)` of a device parameter id; `None` for the rack's own parameters.
pub const fn split_param(id: u32) -> Option<(usize, u32)> {
    let unit = id >> UNIT_SHIFT;
    if unit >= 1 && unit as usize <= UNITS { Some((unit as usize - 1, id & LOCAL_MASK)) } else { None }
}

/// What a slot of a chain holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Instrument,
    NoteFx(usize),
    Effect(usize),
}

impl Slot {
    pub const fn index(self) -> usize {
        match self {
            Slot::Instrument => 0,
            Slot::NoteFx(k) => 1 + k,
            Slot::Effect(k) => 1 + CHAIN_NOTE_FX + k,
        }
    }

    pub const fn from_index(i: usize) -> Self {
        if i == 0 {
            Slot::Instrument
        } else if i <= CHAIN_NOTE_FX {
            Slot::NoteFx(i - 1)
        } else {
            Slot::Effect(i - 1 - CHAIN_NOTE_FX)
        }
    }
}

/// Unit of slot `slot` of chain `chain`.
pub const fn unit_of(chain: usize, slot: Slot) -> usize {
    chain * SLOTS + slot.index()
}

/// (chain, slot) of a unit.
pub const fn slot_of(unit: usize) -> (usize, Slot) {
    (unit / SLOTS, Slot::from_index(unit % SLOTS))
}

/// Frames rendered at once into a chain's buffer.
const SCRATCH: usize = 256;

macro_rules! chain_params {
    ($base:expr, $group:expr, $min:expr, $max:expr, $def:expr, $unit:expr, [$($n:literal),*]) => {
        [$(ParamInfo::new($base + $n - 1, concat!("Chain ", $n, " ", $group), $min, $max, $def, $unit).group("Chains")),*]
    };
}

const MACRO_PARAMS: [ParamInfo; MACROS] = [
    ParamInfo::new(0, "Macro 1", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(1, "Macro 2", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(2, "Macro 3", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(3, "Macro 4", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(4, "Macro 5", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(5, "Macro 6", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(6, "Macro 7", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(7, "Macro 8", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(8, "Macro 9", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(9, "Macro 10", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(10, "Macro 11", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(11, "Macro 12", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(12, "Macro 13", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(13, "Macro 14", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(14, "Macro 15", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(15, "Macro 16", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
];
const VOLUME_PARAMS: [ParamInfo; CHAINS] =
    chain_params!(P_VOLUME, "Volume", -60.0, 6.0, 0.0, Unit::Db, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
const PAN_PARAMS: [ParamInfo; CHAINS] =
    chain_params!(P_PAN, "Pan", -1.0, 1.0, 0.0, Unit::Pan, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);

const fn build_params() -> [ParamInfo; RACK_PARAMS as usize] {
    let mut out = [ParamInfo::new(0, "", 0.0, 1.0, 0.0, Unit::None); RACK_PARAMS as usize];
    let mut i = 0;
    while i < MACROS {
        out[i] = MACRO_PARAMS[i];
        i += 1;
    }
    out[P_SELECTOR as usize] = ParamInfo::new(P_SELECTOR, "Chain Selector", 0.0, 127.0, 0.0, Unit::None).group("Chains");
    let mut c = 0;
    while c < CHAINS {
        out[P_VOLUME as usize + c] = VOLUME_PARAMS[c];
        out[P_PAN as usize + c] = PAN_PARAMS[c];
        c += 1;
    }
    out
}

static PARAMS: [ParamInfo; RACK_PARAMS as usize] = build_params();

/// The rack's own parameter sheets (macros, selector, chain levels and pans).
pub fn rack_params() -> &'static [ParamInfo] {
    &PARAMS
}

/// A range of keys, velocities or selector values (0 to 127), whole from `lo`
/// to `hi`, fading out over `fade_lo` values below and `fade_hi` above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub lo: u8,
    pub hi: u8,
    pub fade_lo: u8,
    pub fade_hi: u8,
}

impl Zone {
    /// Every value, no fade.
    pub const FULL: Zone = Zone { lo: 0, hi: 127, fade_lo: 0, fade_hi: 0 };

    pub const fn new(lo: u8, hi: u8) -> Self {
        Self { lo, hi, fade_lo: 0, fade_hi: 0 }
    }

    pub const fn with_fades(mut self, fade_lo: u8, fade_hi: u8) -> Self {
        self.fade_lo = fade_lo;
        self.fade_hi = fade_hi;
        self
    }

    /// Bounded, ordered, fades within 0..127.
    pub fn sanitized(self) -> Self {
        let (lo, hi) = (self.lo.min(127), self.hi.min(127));
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        Self { lo, hi, fade_lo: self.fade_lo.min(lo), fade_hi: self.fade_hi.min(127 - hi) }
    }

    /// Lowest and highest values with some level, fades included.
    pub fn outer(&self) -> (u8, u8) {
        (self.lo.saturating_sub(self.fade_lo), self.hi.saturating_add(self.fade_hi).min(127))
    }

    /// Level at `x` in [0, 1]: 1 inside, a straight line down through the
    /// fades (the first value of a fade of `n` gets `n / (n + 1)`), 0 beyond.
    pub fn gain(&self, x: f32) -> f32 {
        let (lo, hi) = (self.lo as f32, self.hi as f32);
        if x >= lo && x <= hi {
            1.0
        } else if x < lo {
            let n = self.fade_lo as f32 + 1.0;
            ((x - (lo - n)) / n).clamp(0.0, 1.0)
        } else {
            let n = self.fade_hi as f32 + 1.0;
            (((hi + n) - x) / n).clamp(0.0, 1.0)
        }
    }

    pub fn is_full(&self) -> bool {
        self.lo == 0 && self.hi == 127
    }
}

impl Default for Zone {
    fn default() -> Self {
        Self::FULL
    }
}

/// Everything about a chain but its devices and levels: copyable, the
/// interface sends it whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainZone {
    /// The chain exists.
    pub on: bool,
    pub mute: bool,
    pub solo: bool,
    pub key: Zone,
    pub vel: Zone,
    pub sel: Zone,
    /// Semitones added to the notes it plays.
    pub transpose: i8,
    /// One bit per bypassed device slot.
    pub bypass: u16,
}

impl Default for ChainZone {
    fn default() -> Self {
        Self { on: false, mute: false, solo: false, key: Zone::FULL, vel: Zone::FULL, sel: Zone::FULL, transpose: 0, bypass: 0 }
    }
}

impl ChainZone {
    /// A chain in use, every note, no transpose.
    pub fn active() -> Self {
        Self { on: true, ..Self::default() }
    }

    pub fn bypassed(&self, slot: Slot) -> bool {
        self.bypass & (1 << slot.index()) != 0
    }

    pub fn set_bypass(&mut self, slot: Slot, on: bool) {
        let bit = 1 << slot.index();
        if on { self.bypass |= bit } else { self.bypass &= !bit }
    }

    /// Level a note `key` at velocity `vel` (0..1) gets from the key and
    /// velocity zones (0: the chain does not play it).
    pub fn note_gain(&self, key: u8, vel: f32) -> f32 {
        let v = (vel * 127.0).round().clamp(1.0, 127.0);
        self.key.gain(key as f32) * self.vel.gain(v)
    }

    /// Key the chain plays for `key`, `None` when the transpose leaves the range.
    pub fn out_key(&self, key: u8) -> Option<u8> {
        u8::try_from(key as i32 + self.transpose as i32).ok().filter(|&k| k <= 127)
    }
}

/// A device for a slot of a chain, sent by the interface (already at the
/// sample rate) and coming back when replaced.
pub enum LayerDevice {
    None,
    Instrument(Box<dyn Instrument>),
    NoteFx(Box<dyn NoteFx>),
    Effect(Box<dyn Effect>),
}

impl LayerDevice {
    pub fn is_none(&self) -> bool {
        matches!(self, LayerDevice::None)
    }
}

struct Chain {
    zone: ChainZone,
    instrument: Option<Box<dyn Instrument>>,
    note_fx: [Option<Box<dyn NoteFx>>; CHAIN_NOTE_FX],
    effects: [Option<Box<dyn Effect>>; CHAIN_FX],
    /// Left and right gains: level × selector fade × audible, with the pan.
    gl: Smoother,
    gr: Smoother,
    /// Keys of the rack's input this chain plays, and what it plays for each.
    held: u128,
    out_key: [u8; 128],
    /// Notes waiting for the note effects, and the buffers between them.
    inputs: NoteBuffer,
    stages: [NoteBuffer; 2],
    /// Frames played since the note effects last advanced.
    elapsed: u32,
}

impl Chain {
    fn new() -> Self {
        Self {
            zone: ChainZone::default(),
            instrument: None,
            note_fx: Default::default(),
            effects: Default::default(),
            gl: Smoother::new(0.0),
            gr: Smoother::new(0.0),
            held: 0,
            out_key: [0; 128],
            inputs: NoteBuffer::default(),
            stages: Default::default(),
            elapsed: 0,
        }
    }

    fn has_note_fx(&self) -> bool {
        self.note_fx.iter().enumerate().any(|(k, f)| f.is_some() && !self.zone.bypassed(Slot::NoteFx(k)))
    }

    fn note_in(&mut self, ev: NoteEvent) {
        if !self.has_note_fx() {
            if let Some(inst) = self.instrument.as_mut() {
                if ev.on { inst.note_on(ev.key, ev.vel) } else { inst.note_off(ev.key) }
            }
            return;
        }
        if !self.inputs.push(ev) {
            // Full: play what waits, then queue this one.
            self.flush();
            self.inputs.push(ev);
        }
    }

    /// The note effects advance to now, take the waiting notes and play
    /// their output on the instrument.
    fn flush(&mut self) {
        let elapsed = std::mem::take(&mut self.elapsed);
        let Self { zone, instrument, note_fx, inputs, stages, .. } = self;
        let [a, b] = stages;
        let (mut src, mut dst) = (a, b);
        src.clear();
        for &ev in inputs.as_slice() {
            src.push(ev);
        }
        inputs.clear();
        for (k, fx) in note_fx.iter_mut().enumerate() {
            let Some(fx) = fx.as_mut() else { continue };
            if zone.bypassed(Slot::NoteFx(k)) {
                continue;
            }
            dst.clear();
            fx.advance(elapsed);
            for ev in src.as_slice() {
                if ev.on { fx.note_on(ev.key, ev.vel, dst) } else { fx.note_off(ev.key, dst) }
            }
            fx.tick(dst);
            std::mem::swap(&mut src, &mut dst);
        }
        if let Some(inst) = instrument.as_mut() {
            for ev in src.as_slice() {
                if ev.on { inst.note_on(ev.key, ev.vel) } else { inst.note_off(ev.key) }
            }
        }
        src.clear();
    }

    /// Frames before the next spontaneous note of the note effects.
    fn next_fx_event(&self) -> Option<u32> {
        let mut wait: Option<u32> = None;
        for (k, fx) in self.note_fx.iter().enumerate() {
            if let Some(fx) = fx.as_ref()
                && !self.zone.bypassed(Slot::NoteFx(k))
                && let Some(w) = fx.next_event()
            {
                wait = Some(wait.map_or(w, |x| x.min(w)));
            }
        }
        wait.map(|w| w.saturating_sub(self.elapsed))
    }

    /// Renders the instrument into `l`, `r` (cleared), the note effects
    /// playing on time, then the effects.
    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        l.fill(0.0);
        r.fill(0.0);
        if self.has_note_fx() {
            let mut pos = 0;
            while pos < n {
                self.flush();
                let seg = self.next_fx_event().map_or(n - pos, |w| (w.max(1) as usize).min(n - pos));
                if let Some(inst) = self.instrument.as_mut() {
                    inst.render(&mut l[pos..pos + seg], &mut r[pos..pos + seg]);
                }
                self.elapsed = self.elapsed.saturating_add(seg as u32);
                pos += seg;
            }
        } else if let Some(inst) = self.instrument.as_mut() {
            inst.render(l, r);
        }
        for (k, fx) in self.effects.iter_mut().enumerate() {
            if let Some(fx) = fx.as_mut()
                && !self.zone.bypassed(Slot::Effect(k))
            {
                fx.process(l, r);
            }
        }
    }

    fn silence(&mut self) {
        self.held = 0;
        self.inputs.clear();
        self.elapsed = 0;
        for fx in self.note_fx.iter_mut().flatten() {
            fx.reset();
        }
        if let Some(inst) = self.instrument.as_mut() {
            inst.all_notes_off();
        }
    }
}

pub struct InstrumentRack {
    values: [f32; RACK_PARAMS as usize],
    chains: Vec<Chain>,
    maps: [Option<MacroMap>; MAX_MAPPINGS],
    sample_rate: f32,
    bpm: f32,
    buf: [Vec<f32>; 2],
}

impl Default for InstrumentRack {
    fn default() -> Self {
        let mut rack = Self {
            values: std::array::from_fn(|i| PARAMS[i].default),
            chains: (0..CHAINS).map(|_| Chain::new()).collect(),
            maps: [None; MAX_MAPPINGS],
            sample_rate: 0.0,
            bpm: 120.0,
            buf: std::array::from_fn(|_| vec![0.0; SCRATCH]),
        };
        rack.set_sample_rate(48_000.0);
        rack
    }
}

impl InstrumentRack {
    pub fn zone(&self, chain: usize) -> ChainZone {
        self.chains.get(chain).map_or_else(ChainZone::default, |c| c.zone)
    }

    /// Zones, switches and bypasses of chain `chain`. Notes it holds that its
    /// new zones no longer reach keep playing until released.
    pub fn set_zone(&mut self, chain: usize, zone: ChainZone) {
        let Some(c) = self.chains.get_mut(chain) else { return };
        let zone = ChainZone { key: zone.key.sanitized(), vel: zone.vel.sanitized(), sel: zone.sel.sanitized(), ..zone };
        let was = c.zone;
        c.zone = zone;
        if !zone.on && was.on {
            c.silence();
        }
        // Note effects switched on or off mid-note: start clean.
        if zone.bypass != was.bypass && c.note_fx.iter().any(Option::is_some) {
            c.silence();
        }
        self.update_gains();
    }

    /// The device of a unit, as an instrument, note effect or effect.
    pub fn instrument(&self, chain: usize) -> Option<&dyn Instrument> {
        self.chains.get(chain)?.instrument.as_deref()
    }

    /// Places a device (already at the right sample rate) on unit `unit`;
    /// returns the one it replaces, to be freed off the audio thread. A wrong
    /// kind for the slot comes straight back.
    pub fn set_device(&mut self, unit: usize, device: LayerDevice) -> LayerDevice {
        if unit >= UNITS {
            return device;
        }
        let (c, slot) = slot_of(unit);
        let bpm = self.bpm;
        let chain = &mut self.chains[c];
        let old = match (slot, device) {
            (Slot::Instrument, LayerDevice::Instrument(mut i)) => {
                i.set_tempo(bpm);
                chain.held = 0;
                let old = chain.instrument.replace(i);
                old.map_or(LayerDevice::None, LayerDevice::Instrument)
            }
            (Slot::Instrument, LayerDevice::None) => {
                chain.held = 0;
                chain.instrument.take().map_or(LayerDevice::None, LayerDevice::Instrument)
            }
            (Slot::NoteFx(k), LayerDevice::NoteFx(mut f)) => {
                f.set_tempo(bpm);
                chain.silence();
                chain.note_fx[k].replace(f).map_or(LayerDevice::None, LayerDevice::NoteFx)
            }
            (Slot::NoteFx(k), LayerDevice::None) => {
                chain.silence();
                chain.note_fx[k].take().map_or(LayerDevice::None, LayerDevice::NoteFx)
            }
            (Slot::Effect(k), LayerDevice::Effect(mut f)) => {
                f.set_tempo(bpm);
                chain.effects[k].replace(f).map_or(LayerDevice::None, LayerDevice::Effect)
            }
            (Slot::Effect(k), LayerDevice::None) => chain.effects[k].take().map_or(LayerDevice::None, LayerDevice::Effect),
            (_, wrong) => return wrong,
        };
        for k in 0..MAX_MAPPINGS {
            if let Some(map) = self.maps[k].filter(|m| m.unit == unit) {
                self.apply_map(&map);
            }
        }
        old
    }

    pub fn mapping(&self, index: usize) -> Option<MacroMap> {
        self.maps.get(index).copied().flatten()
    }

    /// Sets mapping `index` and applies it at once. A mapping onto a macro
    /// is refused (no loops).
    pub fn set_mapping(&mut self, index: usize, map: Option<MacroMap>) {
        let Some(place) = self.maps.get_mut(index) else { return };
        *place = map.filter(|m| {
            m.macro_index < MACROS && (m.unit < UNITS || (m.unit == SELF_UNIT && m.param >= P_SELECTOR && m.param < RACK_PARAMS))
        });
        if let Some(map) = *place {
            self.apply_map(&map);
        }
    }

    fn apply_map(&mut self, map: &MacroMap) {
        let m = self.values[(P_MACRO as usize) + map.macro_index];
        if map.unit == SELF_UNIT {
            if let Some(info) = PARAMS.get(map.param as usize) {
                self.set_own(map.param, map.value(info, m));
            }
            return;
        }
        if let Some(info) = self.unit_info(map.unit, map.param) {
            let v = map.value(info, m);
            self.set_unit_param(map.unit, map.param, v);
        }
    }

    fn apply_macro(&mut self, index: usize) {
        for k in 0..MAX_MAPPINGS {
            if let Some(map) = self.maps[k].filter(|m| m.macro_index == index) {
                self.apply_map(&map);
            }
        }
    }

    fn unit_params(&self, unit: usize) -> Option<&dyn Params> {
        let (c, slot) = slot_of(unit);
        let chain = self.chains.get(c)?;
        match slot {
            Slot::Instrument => chain.instrument.as_deref().map(|i| i as &dyn Params),
            Slot::NoteFx(k) => chain.note_fx[k].as_deref().map(|i| i as &dyn Params),
            Slot::Effect(k) => chain.effects[k].as_deref().map(|i| i as &dyn Params),
        }
    }

    fn unit_info(&self, unit: usize, id: u32) -> Option<&'static ParamInfo> {
        self.unit_params(unit)?.param_info(id)
    }

    fn set_unit_param(&mut self, unit: usize, id: u32, value: f32) {
        let (c, slot) = slot_of(unit);
        let Some(chain) = self.chains.get_mut(c) else { return };
        match slot {
            Slot::Instrument => {
                if let Some(i) = chain.instrument.as_mut() {
                    i.set_param(id, value)
                }
            }
            Slot::NoteFx(k) => {
                if let Some(f) = chain.note_fx[k].as_mut() {
                    f.set_param(id, value)
                }
            }
            Slot::Effect(k) => {
                if let Some(f) = chain.effects[k].as_mut() {
                    f.set_param(id, value)
                }
            }
        }
    }

    /// One of the rack's own values, without the macro path.
    fn set_own(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        self.values[id as usize] = info.clamp(value);
        if id >= P_SELECTOR {
            self.update_gains();
        }
    }

    /// Level of chain `c` from the selector (0..1).
    pub fn selector_gain(&self, c: usize) -> f32 {
        self.chains.get(c).map_or(0.0, |ch| ch.zone.sel.gain(self.values[P_SELECTOR as usize]))
    }

    /// True when chain `c` sounds and takes new notes: in use, not muted,
    /// soloed if any chain is.
    pub fn audible(&self, c: usize) -> bool {
        let solo = self.chains.iter().any(|ch| ch.zone.on && ch.zone.solo);
        self.chains.get(c).is_some_and(|ch| ch.zone.on && !ch.zone.mute && (!solo || ch.zone.solo))
    }

    fn update_gains(&mut self) {
        for c in 0..CHAINS {
            let db = self.values[P_VOLUME as usize + c];
            let pan = self.values[P_PAN as usize + c];
            let level = if db <= -59.9 { 0.0 } else { db_to_gain(db) };
            let g = if self.audible(c) { level * self.selector_gain(c) } else { 0.0 };
            let (pl, pr) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
            let ch = &mut self.chains[c];
            ch.gl.set_target(g * pl);
            ch.gr.set_target(g * pr);
        }
    }

    /// Chains that play a note `key` at `vel` now, with the level the zones give it.
    pub fn routes(&self, key: u8, vel: f32) -> impl Iterator<Item = (usize, f32)> + '_ {
        (0..CHAINS).filter_map(move |c| {
            let ch = &self.chains[c];
            if !self.audible(c) || ch.instrument.is_none() || self.selector_gain(c) <= 0.0 {
                return None;
            }
            let g = ch.zone.note_gain(key, vel);
            (g > 0.0).then_some((c, g))
        })
    }
}

impl Params for InstrumentRack {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match split_param(id) {
            Some((unit, local)) => self.unit_params(unit).map_or(0.0, |p| p.get_param(local)),
            None => self.values.get(id as usize).copied().unwrap_or(0.0),
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if let Some((unit, local)) = split_param(id) {
            return self.set_unit_param(unit, local, value);
        }
        if id >= RACK_PARAMS {
            return;
        }
        if id < P_SELECTOR {
            self.values[id as usize] = PARAMS[id as usize].clamp(value);
            self.apply_macro(id as usize);
        } else {
            self.set_own(id, value);
        }
    }

    fn param_info(&self, id: u32) -> Option<&'static ParamInfo> {
        match split_param(id) {
            Some((unit, local)) => self.unit_info(unit, local),
            None => PARAMS.get(id as usize),
        }
    }
}

impl Instrument for InstrumentRack {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for ch in &mut self.chains {
            if let Some(i) = ch.instrument.as_mut() {
                i.set_sample_rate(sample_rate);
            }
            for f in ch.note_fx.iter_mut().flatten() {
                f.set_sample_rate(sample_rate);
            }
            for f in ch.effects.iter_mut().flatten() {
                f.set_sample_rate(sample_rate);
            }
            ch.gl.set_time(0.015, sample_rate);
            ch.gr.set_time(0.015, sample_rate);
        }
        self.update_gains();
        for ch in &mut self.chains {
            ch.gl.snap();
            ch.gr.snap();
        }
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm;
        for ch in &mut self.chains {
            if let Some(i) = ch.instrument.as_mut() {
                i.set_tempo(bpm);
            }
            for f in ch.note_fx.iter_mut().flatten() {
                f.set_tempo(bpm);
            }
            for f in ch.effects.iter_mut().flatten() {
                f.set_tempo(bpm);
            }
        }
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let key = note & 127;
        let bit = 1u128 << key;
        // A key played again: its earlier note ends first, on every chain.
        if self.chains.iter().any(|c| c.held & bit != 0) {
            self.note_off(key);
        }
        for c in 0..CHAINS {
            let ch = &self.chains[c];
            if !self.audible(c) || ch.instrument.is_none() || self.selector_gain(c) <= 0.0 {
                continue;
            }
            let g = ch.zone.note_gain(key, velocity);
            let Some(out) = ch.zone.out_key(key) else { continue };
            if g <= 0.0 {
                continue;
            }
            let ch = &mut self.chains[c];
            ch.held |= bit;
            ch.out_key[key as usize] = out;
            ch.note_in(NoteEvent::on(out, (velocity * g).clamp(0.0, 1.0)));
        }
    }

    fn note_off(&mut self, note: u8) {
        let key = note & 127;
        let bit = 1u128 << key;
        for ch in &mut self.chains {
            if ch.held & bit != 0 {
                ch.held &= !bit;
                let out = ch.out_key[key as usize];
                ch.note_in(NoteEvent::off(out));
            }
        }
    }

    fn supports_expression(&self) -> bool {
        self.chains.iter().any(|c| c.zone.on && c.instrument.as_ref().is_some_and(|i| i.supports_expression()))
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let bit = 1u128 << (key & 127);
        for ch in &mut self.chains {
            if ch.held & bit != 0 && !ch.has_note_fx() {
                let out = ch.out_key[(key & 127) as usize];
                if let Some(i) = ch.instrument.as_mut() {
                    i.note_expression(out, expr);
                }
            }
        }
    }

    fn controller(&mut self, ctl: Controller) {
        for i in self.chains.iter_mut().filter_map(|c| c.instrument.as_mut()) {
            i.controller(ctl);
        }
    }

    fn all_notes_off(&mut self) {
        for ch in &mut self.chains {
            ch.silence();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let [bl, br] = &mut self.buf;
        let mut start = 0;
        while start < n {
            let m = (n - start).min(SCRATCH);
            for ch in self.chains.iter_mut() {
                if ch.instrument.is_none() || !ch.zone.on {
                    continue;
                }
                let (l, r) = (&mut bl[..m], &mut br[..m]);
                ch.render(l, r);
                let (outl, outr) = (&mut left[start..start + m], &mut right[start..start + m]);
                for i in 0..m {
                    outl[i] += ch.gl.next_value() * l[i];
                    outr[i] += ch.gr.next_value() * r[i];
                }
            }
            start += m;
        }
    }

    fn drain_midi(&mut self, out: &mut dyn FnMut(MidiOutEvent)) {
        for i in self.chains.iter_mut().filter_map(|c| c.instrument.as_mut()) {
            i.drain_midi(out);
        }
    }

    fn as_inst_rack(&mut self) -> Option<&mut InstrumentRack> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    const SR: f32 = 48_000.0;

    /// A steady level equal to the velocity of the notes it holds, on both
    /// sides: level tests read the velocities a chain got.
    struct Dc {
        held: [f32; 128],
    }

    impl Default for Dc {
        fn default() -> Self {
            Self { held: [0.0; 128] }
        }
    }

    thread_local! {
        static NOTE_ONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    impl Params for Dc {
        fn params(&self) -> &'static [ParamInfo] {
            static P: [ParamInfo; 1] = [ParamInfo::new(0, "Level", 0.0, 1.0, 1.0, Unit::Percent)];
            &P
        }
        fn get_param(&self, _: u32) -> f32 {
            0.0
        }
        fn set_param(&mut self, _: u32, _: f32) {}
    }

    impl Instrument for Dc {
        fn kind(&self) -> &'static str {
            "dc"
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn note_on(&mut self, note: u8, velocity: f32) {
            self.held[note as usize] = velocity;
            NOTE_ONS.with(|n| n.set(n.get() + 1));
        }
        fn note_off(&mut self, note: u8) {
            self.held[note as usize] = 0.0;
        }
        fn all_notes_off(&mut self) {
            self.held = [0.0; 128];
        }
        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            let v: f32 = self.held.iter().sum();
            left.iter_mut().for_each(|s| *s += v);
            right.iter_mut().for_each(|s| *s += v);
        }
    }

    fn rack_of(zones: &[ChainZone]) -> InstrumentRack {
        let mut rack = InstrumentRack::default();
        rack.set_sample_rate(SR);
        for (c, z) in zones.iter().enumerate() {
            rack.set_device(unit_of(c, Slot::Instrument), LayerDevice::Instrument(Box::new(Dc::default())));
            rack.set_zone(c, *z);
        }
        for ch in &mut rack.chains {
            ch.gl.snap();
            ch.gr.snap();
        }
        rack
    }

    fn level(rack: &mut InstrumentRack) -> f32 {
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        rack.render(&mut l, &mut r);
        l[63]
    }

    /// Keys chain `c` holds.
    fn held(rack: &InstrumentRack, c: usize) -> f32 {
        rack.chains[c].held.count_ones() as f32
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(split_param(unit_param(5, 12)), Some((5, 12)));
        assert_eq!(split_param(unit_param(UNITS - 1, 0xFF_FFFF)), Some((UNITS - 1, 0xFF_FFFF)));
        assert_eq!(split_param(P_SELECTOR), None);
        assert_eq!(slot_of(unit_of(3, Slot::Effect(2))), (3, Slot::Effect(2)));
        assert_eq!(slot_of(unit_of(15, Slot::NoteFx(1))), (15, Slot::NoteFx(1)));
        assert!(PARAMS.iter().enumerate().all(|(i, p)| p.id as usize == i && !p.name.is_empty()));
    }

    #[test]
    fn zone_fades_are_straight_lines() {
        let z = Zone::new(60, 72).with_fades(3, 1);
        assert_eq!(z.gain(60.0), 1.0);
        assert_eq!(z.gain(72.0), 1.0);
        assert_eq!(z.gain(59.0), 0.75);
        assert_eq!(z.gain(57.0), 0.25);
        assert_eq!(z.gain(56.0), 0.0);
        assert_eq!(z.gain(73.0), 0.5);
        assert_eq!(z.gain(74.0), 0.0);
        assert_eq!(z.outer(), (57, 73));
        assert_eq!(Zone { lo: 90, hi: 10, fade_lo: 50, fade_hi: 200 }.sanitized(), Zone { lo: 10, hi: 90, fade_lo: 10, fade_hi: 37 });
    }

    #[test]
    fn key_split_plays_the_right_chains() {
        // Chain 1 below middle C, chain 2 from it, chain 3 everywhere an octave up.
        let low = ChainZone { key: Zone::new(0, 59), ..ChainZone::active() };
        let high = ChainZone { key: Zone::new(60, 127), ..ChainZone::active() };
        let all = ChainZone { transpose: 12, ..ChainZone::active() };
        let mut rack = rack_of(&[low, high, all]);
        rack.note_on(48, 0.5);
        assert_eq!((held(&rack, 0), held(&rack, 1), held(&rack, 2)), (1.0, 0.0, 1.0));
        assert_eq!(rack.chains[2].out_key[48], 60);
        rack.note_on(64, 0.5);
        assert_eq!((held(&rack, 0), held(&rack, 1), held(&rack, 2)), (1.0, 1.0, 2.0));
        assert!((level(&mut rack) - 2.0).abs() < 1e-5);
        // Releases reach the chains that played, at the keys they played.
        rack.note_off(48);
        rack.note_off(64);
        assert!(rack.chains.iter().all(|c| c.held == 0));
        assert!(level(&mut rack).abs() < 1e-6);
        // A transpose out of range plays nothing on that chain.
        rack.note_on(120, 0.5);
        assert_eq!(held(&rack, 2), 0.0);
        // Muted, or another chain soloed: no new notes.
        let mut z = rack.zone(1);
        z.mute = true;
        rack.set_zone(1, z);
        rack.note_on(100, 0.5);
        assert_eq!(rack.chains[1].held & (1 << 100), 0);
        let mut z = rack.zone(0);
        z.solo = true;
        rack.set_zone(0, z);
        rack.note_on(30, 0.5);
        assert_eq!(rack.chains[2].held & (1 << 30), 0);
        assert_ne!(rack.chains[0].held & (1 << 30), 0);
    }

    #[test]
    fn velocity_crossfade_levels() {
        // Soft layer up to 63 fading out over 32, loud layer from 64 fading in over 32.
        let soft = ChainZone { vel: Zone::new(1, 63).with_fades(0, 31), ..ChainZone::active() };
        let loud = ChainZone { vel: Zone::new(64, 127).with_fades(31, 0), ..ChainZone::active() };
        let mut rack = rack_of(&[soft, loud]);
        let per_chain = |rack: &mut InstrumentRack, vel: f32| {
            rack.all_notes_off();
            rack.note_on(60, vel);
            let mut out = [0.0; 2];
            for (c, o) in out.iter_mut().enumerate() {
                let (mut l, mut r) = (vec![0.0; 8], vec![0.0; 8]);
                if let Some(i) = rack.chains[c].instrument.as_mut() {
                    i.render(&mut l, &mut r);
                }
                *o = l[0];
            }
            out
        };
        // Quiet: only the soft layer, at the note's velocity.
        let v = 20.0 / 127.0;
        let [s, l] = per_chain(&mut rack, v);
        assert!((s - v).abs() < 1e-5 && l == 0.0, "{s} {l}");
        // In the crossfades both play, each at the level its fade gives.
        let v = 64.0 / 127.0;
        let [s, l] = per_chain(&mut rack, v);
        let (gs, gl) = (s / v, l / v);
        assert!((gs - 31.0 / 32.0).abs() < 1e-4, "{gs}");
        assert!((gl - 1.0).abs() < 1e-4, "{gl}");
        let v = 48.0 / 127.0;
        let [s, l] = per_chain(&mut rack, v);
        assert!((s / v - 1.0).abs() < 1e-4 && (l / v - 16.0 / 32.0).abs() < 1e-4, "{} {}", s / v, l / v);
        // Loud: only the loud layer.
        let [s, l] = per_chain(&mut rack, 1.0);
        assert!(s == 0.0 && (l - 1.0).abs() < 1e-5, "{s} {l}");
    }

    #[test]
    fn chain_selector_morphs_between_layers() {
        // Chain 1 at 0..=40 fading out to 80, chain 2 at 80..=127 fading in from 40.
        let a = ChainZone { sel: Zone::new(0, 40).with_fades(0, 39), ..ChainZone::active() };
        let b = ChainZone { sel: Zone::new(80, 127).with_fades(39, 0), ..ChainZone::active() };
        let mut rack = rack_of(&[a, b]);
        // A macro drives the selector over its whole range.
        rack.set_mapping(0, Some(MacroMap { macro_index: 0, unit: SELF_UNIT, param: P_SELECTOR, min: 0.0, max: 1.0 }));
        rack.set_param(P_MACRO, 0.0);
        assert_eq!(rack.get_param(P_SELECTOR), 0.0);
        assert_eq!((rack.selector_gain(0), rack.selector_gain(1)), (1.0, 0.0));
        rack.note_on(60, 1.0);
        assert_eq!(rack.chains[1].held, 0, "outside its zone: no note");
        rack.set_param(P_MACRO, 0.5);
        assert!((rack.get_param(P_SELECTOR) - 63.5).abs() < 1e-4);
        let (g0, g1) = (rack.selector_gain(0), rack.selector_gain(1));
        assert!((g0 + g1 - 1.0).abs() < 0.03 && g0 > 0.3 && g1 > 0.3, "{g0} {g1}");
        // The output follows the selector on held notes, smoothly.
        for _ in 0..200 {
            level(&mut rack);
        }
        assert!((level(&mut rack) - g0).abs() < 1e-3);
        rack.set_param(P_MACRO, 1.0);
        assert_eq!((rack.selector_gain(0), rack.selector_gain(1)), (0.0, 1.0));
        for _ in 0..200 {
            level(&mut rack);
        }
        assert!(level(&mut rack).abs() < 1e-3);
        rack.note_off(60);
        rack.note_on(62, 1.0);
        assert_eq!(rack.chains[0].held, 0);
        assert_ne!(rack.chains[1].held, 0);
    }

    #[test]
    fn macros_drive_device_parameters_with_ranges() {
        let mut rack = InstrumentRack::default();
        let mut pulse = registry::create_instrument("pulse").expect("pulse");
        pulse.set_sample_rate(SR);
        rack.set_device(unit_of(2, Slot::Instrument), LayerDevice::Instrument(pulse));
        let mut eq = registry::create_effect("eq").expect("eq");
        eq.set_sample_rate(SR);
        rack.set_device(unit_of(2, Slot::Effect(1)), LayerDevice::Effect(eq));
        let inst = unit_of(2, Slot::Instrument);
        let fx = unit_of(2, Slot::Effect(1));
        let pid = rack.chains[2].instrument.as_ref().unwrap().params().iter().find(|p| p.unit != Unit::Choice).unwrap().id;
        let info = *rack.param_info(unit_param(inst, pid)).expect("pulse param");
        let eq_info = *rack.param_info(unit_param(fx, 1)).expect("eq param");
        rack.set_mapping(0, Some(MacroMap { macro_index: 3, unit: inst, param: pid, min: 0.25, max: 0.75 }));
        rack.set_mapping(1, Some(MacroMap { macro_index: 3, unit: fx, param: 1, min: 1.0, max: 0.0 }));
        rack.set_mapping(2, Some(MacroMap { macro_index: 3, unit: SELF_UNIT, param: P_VOLUME + 2, min: 0.0, max: 1.0 }));
        // A mapping onto a macro is refused.
        rack.set_mapping(3, Some(MacroMap { macro_index: 3, unit: SELF_UNIT, param: P_MACRO + 1, min: 0.0, max: 1.0 }));
        assert_eq!(rack.mapping(3), None);
        rack.set_param(P_MACRO + 3, 0.0);
        assert!((info.to_norm(rack.get_param(unit_param(inst, pid))) - 0.25).abs() < 1e-4);
        assert!((eq_info.to_norm(rack.get_param(unit_param(fx, 1))) - 1.0).abs() < 1e-4);
        assert_eq!(rack.get_param(P_VOLUME + 2), -60.0);
        rack.set_param(P_MACRO + 3, 1.0);
        assert!((info.to_norm(rack.get_param(unit_param(inst, pid))) - 0.75).abs() < 1e-4);
        assert!(eq_info.to_norm(rack.get_param(unit_param(fx, 1))).abs() < 1e-4);
        assert_eq!(rack.get_param(P_VOLUME + 2), 6.0);
        rack.set_param(P_MACRO + 3, 0.5);
        assert!((info.to_norm(rack.get_param(unit_param(inst, pid))) - 0.5).abs() < 1e-4);
        // Another macro leaves them alone.
        rack.set_param(P_MACRO, 1.0);
        assert!((info.to_norm(rack.get_param(unit_param(inst, pid))) - 0.5).abs() < 1e-4);
        // A device placed later on a mapped unit takes the macro's value.
        let mut eq = registry::create_effect("eq").expect("eq");
        eq.set_sample_rate(SR);
        rack.set_device(fx, LayerDevice::Effect(eq));
        assert!((eq_info.to_norm(rack.get_param(unit_param(fx, 1))) - 0.5).abs() < 1e-4);
        // A wrong kind for a slot comes back.
        let eq = registry::create_effect("eq").expect("eq");
        assert!(matches!(rack.set_device(inst, LayerDevice::Effect(eq)), LayerDevice::Effect(_)));
    }

    #[test]
    fn note_effects_play_on_time_inside_a_chain() {
        // An arpeggiator before chain 1: the chain plays several notes over a held chord.
        let mut rack = rack_of(&[ChainZone::active()]);
        let mut arp = registry::create_note_fx("arpeggiator").expect("arp");
        arp.set_sample_rate(SR);
        rack.set_device(unit_of(0, Slot::NoteFx(0)), LayerDevice::NoteFx(arp));
        let before = NOTE_ONS.with(|n| n.get());
        rack.note_on(60, 0.8);
        rack.note_on(64, 0.8);
        let mut sound = 0.0f32;
        for _ in 0..200 {
            let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
            rack.render(&mut l, &mut r);
            sound = sound.max(l.iter().fold(0.0f32, |m, s| m.max(s.abs())));
        }
        let ons = NOTE_ONS.with(|n| n.get()) - before;
        assert!(ons >= 4, "{ons}");
        assert!(sound > 0.1);
        rack.all_notes_off();
        assert!(rack.chains.iter().all(|c| c.held == 0));
    }

    #[test]
    fn pan_and_volume_shape_each_chain() {
        let mut rack = rack_of(&[ChainZone::active(), ChainZone::active()]);
        rack.set_param(P_PAN, -1.0);
        rack.set_param(P_VOLUME + 1, -6.0);
        for ch in &mut rack.chains {
            ch.gl.snap();
            ch.gr.snap();
        }
        rack.note_on(60, 1.0);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        rack.render(&mut l, &mut r);
        let g = db_to_gain(-6.0);
        assert!((l[10] - (1.0 + g)).abs() < 1e-4, "{}", l[10]);
        assert!((r[10] - g).abs() < 1e-4, "{}", r[10]);
    }
}
