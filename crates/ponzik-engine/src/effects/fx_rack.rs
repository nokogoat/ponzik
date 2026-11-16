//! Effect rack: up to [`CHAINS`] parallel chains of up to [`CHAIN_LEN`] effects
//! inside one insert slot, with eight macros mapped onto the parameters of the
//! devices inside (Ableton's Audio Effect Rack).
//!
//! Every device of the rack has a stable *unit* number (0..[`UNITS`]): its
//! parameters are addressed through the rack with [`unit_param`], so moving a
//! device in the chain changes neither its saved values nor its automation.
//! The order of the units in the chains is a small copyable [`RackLayout`].
//!
//! Latency: the sum along a chain; with several chains (or a dry/wet mix) every
//! chain and the dry signal are delayed to line up with the slowest chain.

use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

/// Parallel chains in a rack.
pub const CHAINS: usize = 4;
/// Devices in one chain.
pub const CHAIN_LEN: usize = 8;
/// Device numbers available in a rack.
pub const UNITS: usize = CHAINS * CHAIN_LEN;
/// Macro knobs.
pub const MACROS: usize = 8;
/// Macro mappings in a rack.
pub const MAX_MAPPINGS: usize = 64;

/// First macro (ids `P_MACRO..P_MACRO + 8`).
pub const P_MACRO: u32 = 0;
/// Volume of each chain, in dB (ids `P_VOLUME..P_VOLUME + 4`).
pub const P_VOLUME: u32 = 8;
/// Dry/wet of each chain (ids `P_MIX..P_MIX + 4`).
pub const P_MIX: u32 = 12;
/// Parameters of the rack itself; above, the parameters of its devices.
pub const RACK_PARAMS: u32 = 16;

const UNIT_SHIFT: u32 = 16;
const LOCAL_MASK: u32 = (1 << UNIT_SHIFT) - 1;

/// Rack parameter id of parameter `id` of the device `unit`.
pub const fn unit_param(unit: usize, id: u32) -> u32 {
    ((unit as u32 + 1) << UNIT_SHIFT) | (id & LOCAL_MASK)
}

/// `(unit, id)` of a device parameter id; `None` for the rack's own parameters.
pub const fn split_param(id: u32) -> Option<(usize, u32)> {
    let unit = id >> UNIT_SHIFT;
    if unit >= 1 && unit as usize <= UNITS { Some((unit as usize - 1, id & LOCAL_MASK)) } else { None }
}

/// Longest chain latency lined up, in seconds.
const MAX_ALIGN_SECONDS: f32 = 0.5;
/// Frames mixed at once when the chains run in parallel.
const SCRATCH: usize = 512;

static PARAMS: [ParamInfo; RACK_PARAMS as usize] = [
    ParamInfo::new(0, "Macro 1", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(1, "Macro 2", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(2, "Macro 3", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(3, "Macro 4", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(4, "Macro 5", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(5, "Macro 6", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(6, "Macro 7", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(7, "Macro 8", 0.0, 1.0, 0.0, Unit::Percent).group("Macros"),
    ParamInfo::new(8, "Chain 1 Volume", -60.0, 6.0, 0.0, Unit::Db).group("Chains"),
    ParamInfo::new(9, "Chain 2 Volume", -60.0, 6.0, 0.0, Unit::Db).group("Chains"),
    ParamInfo::new(10, "Chain 3 Volume", -60.0, 6.0, 0.0, Unit::Db).group("Chains"),
    ParamInfo::new(11, "Chain 4 Volume", -60.0, 6.0, 0.0, Unit::Db).group("Chains"),
    ParamInfo::new(12, "Chain 1 Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Chains"),
    ParamInfo::new(13, "Chain 2 Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Chains"),
    ParamInfo::new(14, "Chain 3 Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Chains"),
    ParamInfo::new(15, "Chain 4 Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Chains"),
];

/// Empty place in a chain.
const EMPTY: u8 = u8::MAX;

/// Which device plays where: the units of each chain, in order, and the
/// bypassed ones. Copyable, so the interface sends it whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RackLayout {
    /// Chains in use (1 to [`CHAINS`]).
    pub chains: usize,
    order: [[u8; CHAIN_LEN]; CHAINS],
    /// One bit per bypassed unit.
    pub bypass: u32,
}

impl Default for RackLayout {
    fn default() -> Self {
        Self { chains: 1, order: [[EMPTY; CHAIN_LEN]; CHAINS], bypass: 0 }
    }
}

impl RackLayout {
    /// Units of chain `c`, in order.
    pub fn chain(&self, c: usize) -> impl Iterator<Item = usize> + '_ {
        let row: &[u8] = self.order.get(c).map_or(&[], |r| &r[..]);
        row.iter().take_while(|&&u| u != EMPTY).map(|&u| u as usize)
    }

    /// Sets chain `c` (at most [`CHAIN_LEN`] units, each below [`UNITS`]).
    pub fn set_chain(&mut self, c: usize, units: &[usize]) {
        let Some(row) = self.order.get_mut(c) else { return };
        *row = [EMPTY; CHAIN_LEN];
        for (place, &u) in row.iter_mut().zip(units.iter().filter(|&&u| u < UNITS)) {
            *place = u as u8;
        }
    }

    pub fn bypassed(&self, unit: usize) -> bool {
        unit < UNITS && self.bypass & (1 << unit) != 0
    }

    pub fn set_bypass(&mut self, unit: usize, on: bool) {
        if unit < UNITS {
            if on { self.bypass |= 1 << unit } else { self.bypass &= !(1 << unit) }
        }
    }
}

/// A macro drives one device parameter between `min` and `max`, positions in
/// [0, 1] of the parameter's range; `min > max` inverts the direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacroMap {
    pub macro_index: usize,
    pub unit: usize,
    pub param: u32,
    pub min: f32,
    pub max: f32,
}

impl MacroMap {
    /// Position in the parameter range for a macro at `m` (0..1).
    pub fn norm(&self, m: f32) -> f32 {
        (self.min + (self.max - self.min) * m.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    /// Parameter value for a macro at `m`.
    pub fn value(&self, info: &ParamInfo, m: f32) -> f32 {
        info.from_norm(self.norm(m))
    }
}

/// Stereo delay that lines a signal up with a slower one.
struct Align {
    l: Vec<f32>,
    r: Vec<f32>,
    pos: usize,
}

impl Align {
    fn new(len: usize) -> Self {
        Self { l: vec![0.0; len.max(1)], r: vec![0.0; len.max(1)], pos: 0 }
    }

    fn clear(&mut self) {
        self.l.fill(0.0);
        self.r.fill(0.0);
    }

    /// Delays the block by `d` samples, in place.
    fn run(&mut self, l: &mut [f32], r: &mut [f32], d: usize) {
        let len = self.l.len();
        let d = d.min(len - 1);
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            self.l[self.pos] = *a;
            self.r[self.pos] = *b;
            let k = (self.pos + len - d) % len;
            *a = self.l[k];
            *b = self.r[k];
            self.pos += 1;
            if self.pos == len {
                self.pos = 0;
            }
        }
    }
}

pub struct EffectRack {
    values: Values<{ RACK_PARAMS as usize }>,
    units: [Option<Box<dyn Effect>>; UNITS],
    layout: RackLayout,
    maps: [Option<MacroMap>; MAX_MAPPINGS],
    sample_rate: f32,
    bpm: f32,
    gain: [Smoother; CHAINS],
    mix: [Smoother; CHAINS],
    dry_align: Align,
    wet_align: [Align; CHAINS],
    /// Dry input, dry lined up, one chain, sum: left and right each.
    buf: [Vec<f32>; 8],
}

impl Default for EffectRack {
    fn default() -> Self {
        let mut rack = Self {
            values: Values::new(&PARAMS),
            units: std::array::from_fn(|_| None),
            layout: RackLayout::default(),
            maps: [None; MAX_MAPPINGS],
            sample_rate: 0.0,
            bpm: 120.0,
            gain: std::array::from_fn(|_| Smoother::new(1.0)),
            mix: std::array::from_fn(|_| Smoother::new(1.0)),
            dry_align: Align::new(1),
            wet_align: std::array::from_fn(|_| Align::new(1)),
            buf: std::array::from_fn(|_| vec![0.0; SCRATCH]),
        };
        rack.set_sample_rate(DEFAULT_SR);
        rack
    }
}

impl EffectRack {
    pub fn layout(&self) -> RackLayout {
        self.layout
    }

    pub fn set_layout(&mut self, mut layout: RackLayout) {
        layout.chains = layout.chains.clamp(1, CHAINS);
        self.layout = layout;
    }

    pub fn unit(&self, unit: usize) -> Option<&dyn Effect> {
        self.units.get(unit)?.as_deref()
    }

    /// Places a device (already at the right sample rate) on unit `unit`;
    /// returns the one it replaces, to be freed off the audio thread.
    pub fn set_unit(&mut self, unit: usize, mut effect: Option<Box<dyn Effect>>) -> Option<Box<dyn Effect>> {
        if unit >= UNITS {
            return effect;
        }
        if let Some(fx) = effect.as_mut() {
            fx.set_tempo(self.bpm);
        }
        let old = std::mem::replace(&mut self.units[unit], effect);
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

    /// Sets mapping `index` and applies it at once.
    pub fn set_mapping(&mut self, index: usize, map: Option<MacroMap>) {
        let Some(place) = self.maps.get_mut(index) else { return };
        *place = map.filter(|m| m.macro_index < MACROS && m.unit < UNITS);
        if let Some(map) = *place {
            self.apply_map(&map);
        }
    }

    fn apply_map(&mut self, map: &MacroMap) {
        let m = self.values.get(P_MACRO + map.macro_index as u32);
        if let Some(fx) = self.units[map.unit].as_mut()
            && let Some(info) = fx.param_info(map.param)
        {
            let v = map.value(info, m);
            fx.set_param(map.param, v);
        }
    }

    fn apply_macro(&mut self, index: usize) {
        for k in 0..MAX_MAPPINGS {
            if let Some(map) = self.maps[k].filter(|m| m.macro_index == index) {
                self.apply_map(&map);
            }
        }
    }

    /// Latency of chain `c`: its active devices, one after the other.
    pub fn chain_latency(&self, c: usize) -> usize {
        let active = self.layout.chain(c).filter(|&u| !self.layout.bypassed(u));
        active.filter_map(|u| self.units[u].as_ref()).map(|fx| fx.latency()).sum()
    }

    fn run_chain(&mut self, c: usize, l: &mut [f32], r: &mut [f32]) {
        let Self { layout, units, .. } = self;
        for u in layout.chain(c) {
            if layout.bypassed(u) {
                continue;
            }
            if let Some(fx) = units[u].as_mut() {
                fx.process(l, r);
            }
        }
    }

    /// One chain at full level and fully wet: the plain serial chain.
    fn direct(&self) -> bool {
        let unity = |s: &Smoother| s.value() == 1.0 && s.target() == 1.0;
        self.layout.chains == 1 && unity(&self.gain[0]) && unity(&self.mix[0])
    }

    fn update_chain(&mut self, c: usize) {
        let db = self.values.get(P_VOLUME + c as u32);
        self.gain[c].set_target(if db <= -59.9 { 0.0 } else { db_to_gain(db) });
        self.mix[c].set_target(self.values.get(P_MIX + c as u32));
    }
}

impl Params for EffectRack {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match split_param(id) {
            Some((unit, local)) => self.units[unit].as_ref().map_or(0.0, |fx| fx.get_param(local)),
            None => self.values.get(id),
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if let Some((unit, local)) = split_param(id) {
            if let Some(fx) = self.units[unit].as_mut() {
                fx.set_param(local, value);
            }
            return;
        }
        if !self.values.set(&PARAMS, id, value) {
            return;
        }
        match id {
            0..P_VOLUME => self.apply_macro(id as usize),
            P_VOLUME..P_MIX => self.update_chain((id - P_VOLUME) as usize),
            _ => self.update_chain((id - P_MIX) as usize),
        }
    }

    fn param_info(&self, id: u32) -> Option<&'static ParamInfo> {
        match split_param(id) {
            Some((unit, local)) => self.units[unit].as_ref()?.param_info(local),
            None => PARAMS.get(id as usize),
        }
    }
}

impl Effect for EffectRack {
    fn kind(&self) -> &'static str {
        "rack"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let len = (sample_rate.max(1.0) * MAX_ALIGN_SECONDS) as usize + SCRATCH;
        if sample_rate != self.sample_rate {
            self.dry_align = Align::new(len);
            self.wet_align = std::array::from_fn(|_| Align::new(len));
        }
        self.sample_rate = sample_rate;
        for fx in self.units.iter_mut().flatten() {
            fx.set_sample_rate(sample_rate);
        }
        for s in self.gain.iter_mut().chain(self.mix.iter_mut()) {
            s.set_time(0.02, sample_rate);
        }
        for c in 0..CHAINS {
            self.update_chain(c);
        }
        for s in self.gain.iter_mut().chain(self.mix.iter_mut()) {
            s.snap();
        }
    }

    fn reset(&mut self) {
        for fx in self.units.iter_mut().flatten() {
            fx.reset();
        }
        self.dry_align.clear();
        self.wet_align.iter_mut().for_each(Align::clear);
        for s in self.gain.iter_mut().chain(self.mix.iter_mut()) {
            s.snap();
        }
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm;
        for fx in self.units.iter_mut().flatten() {
            fx.set_tempo(bpm);
        }
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        for fx in self.units.iter_mut().flatten() {
            fx.sidechain(left, right);
        }
    }

    fn latency(&self) -> usize {
        (0..self.layout.chains).map(|c| self.chain_latency(c)).max().unwrap_or(0)
    }

    fn as_rack(&mut self) -> Option<&mut EffectRack> {
        Some(self)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.direct() {
            return self.run_chain(0, left, right);
        }
        let chains = self.layout.chains;
        let lat: [usize; CHAINS] = std::array::from_fn(|c| if c < chains { self.chain_latency(c) } else { 0 });
        let most = lat.iter().copied().max().unwrap_or(0);
        let n = left.len().min(right.len());
        let mut start = 0;
        while start < n {
            let m = (n - start).min(SCRATCH);
            let (l, r) = (&mut left[start..start + m], &mut right[start..start + m]);
            let mut buf = std::mem::take(&mut self.buf);
            let [dl, dr, al, ar, cl, cr, sl, sr] = &mut buf;
            dl[..m].copy_from_slice(l);
            dr[..m].copy_from_slice(r);
            al[..m].copy_from_slice(l);
            ar[..m].copy_from_slice(r);
            self.dry_align.run(&mut al[..m], &mut ar[..m], most);
            sl[..m].fill(0.0);
            sr[..m].fill(0.0);
            for (c, &own) in lat.iter().enumerate().take(chains) {
                cl[..m].copy_from_slice(&dl[..m]);
                cr[..m].copy_from_slice(&dr[..m]);
                self.run_chain(c, &mut cl[..m], &mut cr[..m]);
                self.wet_align[c].run(&mut cl[..m], &mut cr[..m], most - own);
                let (gain, mix) = (&mut self.gain[c], &mut self.mix[c]);
                for i in 0..m {
                    let g = gain.next_value();
                    let w = mix.next_value();
                    sl[i] += g * (w * cl[i] + (1.0 - w) * al[i]);
                    sr[i] += g * (w * cr[i] + (1.0 - w) * ar[i]);
                }
            }
            l.copy_from_slice(&sl[..m]);
            r.copy_from_slice(&sr[..m]);
            self.buf = buf;
            start += m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;
    use crate::registry;

    fn unit(kind: &str) -> Box<dyn Effect> {
        let mut fx = registry::create_effect(kind).expect(kind);
        fx.set_sample_rate(SR);
        fx
    }

    fn rack_of(chains: &[&[(usize, &str)]]) -> EffectRack {
        let mut rack = EffectRack::default();
        rack.set_sample_rate(SR);
        let mut layout = RackLayout { chains: chains.len(), ..RackLayout::default() };
        for (c, devices) in chains.iter().enumerate() {
            for &(u, kind) in devices.iter() {
                rack.set_unit(u, Some(unit(kind)));
            }
            layout.set_chain(c, &devices.iter().map(|d| d.0).collect::<Vec<_>>());
        }
        rack.set_layout(layout);
        rack
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(split_param(unit_param(5, 12)), Some((5, 12)));
        assert_eq!(split_param(unit_param(UNITS - 1, 0)), Some((UNITS - 1, 0)));
        assert_eq!(split_param(3), None);
        assert_eq!(split_param(unit_param(UNITS, 0)), None);
    }

    #[test]
    fn one_chain_equals_the_manual_chain_bit_for_bit() {
        let kinds = ["eq", "compressor", "saturator", "delay", "chorus"];
        let mut rack = rack_of(&[&[(3, "eq"), (0, "compressor"), (7, "saturator"), (1, "delay"), (2, "chorus")]]);
        let mut manual: Vec<Box<dyn Effect>> = kinds.iter().map(|k| unit(k)).collect();
        // Same settings on both sides, through the rack ids.
        rack.set_param(unit_param(0, 0), -20.0);
        manual[1].set_param(0, -20.0);
        rack.set_param(unit_param(7, 1), 12.0);
        manual[2].set_param(1, 12.0);
        let input = noise(0.5, 4_800, 7);
        let (mut rl, mut rr) = (input.clone(), input.iter().map(|x| -0.5 * x).collect::<Vec<_>>());
        let (mut ml, mut mr) = (rl.clone(), rr.clone());
        for (a, b) in rl.chunks_mut(256).zip(rr.chunks_mut(256)) {
            rack.process(a, b);
        }
        for (a, b) in ml.chunks_mut(256).zip(mr.chunks_mut(256)) {
            for fx in &mut manual {
                fx.process(a, b);
            }
        }
        assert!(rl.iter().zip(&ml).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(rr.iter().zip(&mr).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert_eq!(rack.latency(), manual.iter().map(|f| f.latency()).sum::<usize>());
    }

    #[test]
    fn latency_sums_along_a_chain_and_takes_the_slowest_chain() {
        let mut rack = rack_of(&[&[(0, "limiter"), (1, "saturator")], &[(2, "eq")]]);
        let (lim, sat) = (unit("limiter").latency(), unit("saturator").latency());
        assert!(lim > 0 && sat > 0);
        assert_eq!(rack.chain_latency(0), lim + sat);
        assert_eq!(rack.chain_latency(1), 0);
        assert_eq!(rack.latency(), lim + sat);
        // A bypassed device adds nothing.
        let mut layout = rack.layout();
        layout.set_bypass(0, true);
        rack.set_layout(layout);
        assert_eq!(rack.latency(), sat);
    }

    #[test]
    fn parallel_chains_line_up() {
        // Chain 1 delays by the saturator's latency, chain 2 is empty: an
        // impulse comes out once, both copies on the same sample.
        let mut rack = rack_of(&[&[(0, "utility")], &[]]);
        rack.set_unit(1, Some(unit("limiter")));
        let mut layout = rack.layout();
        layout.set_chain(0, &[1]);
        rack.set_layout(layout);
        let lat = rack.latency();
        assert!(lat > 0);
        let mut l = impulse(2_048);
        l.iter_mut().for_each(|s| *s *= 0.25);
        let mut r = l.clone();
        rack.process(&mut l, &mut r);
        let peak_at = l.iter().enumerate().fold((0, 0.0f32), |m, (i, &s)| if s.abs() > m.1 { (i, s.abs()) } else { m }).0;
        assert_eq!(peak_at, lat);
        // Both chains add up: twice the impulse.
        assert!((l[lat] - 0.5).abs() < 0.02, "{}", l[lat]);
        assert!(l[..lat].iter().all(|s| s.abs() < 1e-4));
    }

    #[test]
    fn chain_volume_and_mix() {
        let mut rack = rack_of(&[&[(0, "utility")]]);
        rack.set_param(P_VOLUME, -6.0);
        rack.set_param(P_MIX, 0.5);
        rack.reset();
        let mut l = vec![0.5; 512];
        let mut r = l.clone();
        rack.process(&mut l, &mut r);
        let expect = 0.5 * db_to_gain(-6.0);
        assert!((l[511] - expect).abs() < 1e-4, "{} vs {expect}", l[511]);
    }

    #[test]
    fn macros_drive_their_mappings_with_ranges() {
        let mut rack = rack_of(&[&[(0, "eq"), (4, "reverb")]]);
        let eq = unit("eq");
        let info = *eq.param_info(1).expect("eq param");
        // Macro 3 sweeps the parameter over its middle half; macro 3 also
        // drives a reverb parameter the other way round.
        rack.set_mapping(0, Some(MacroMap { macro_index: 2, unit: 0, param: 1, min: 0.25, max: 0.75 }));
        rack.set_mapping(1, Some(MacroMap { macro_index: 2, unit: 4, param: 5, min: 1.0, max: 0.0 }));
        let rev = *rack.param_info(unit_param(4, 5)).expect("reverb mix");
        rack.set_param(P_MACRO + 2, 0.0);
        assert!((info.to_norm(rack.get_param(unit_param(0, 1))) - 0.25).abs() < 1e-4);
        assert!((rev.to_norm(rack.get_param(unit_param(4, 5))) - 1.0).abs() < 1e-4);
        rack.set_param(P_MACRO + 2, 1.0);
        assert!((info.to_norm(rack.get_param(unit_param(0, 1))) - 0.75).abs() < 1e-4);
        assert!(rev.to_norm(rack.get_param(unit_param(4, 5))).abs() < 1e-4);
        rack.set_param(P_MACRO + 2, 0.5);
        assert!((info.to_norm(rack.get_param(unit_param(0, 1))) - 0.5).abs() < 1e-4);
        // Another macro leaves it alone.
        rack.set_param(P_MACRO, 1.0);
        assert!((info.to_norm(rack.get_param(unit_param(0, 1))) - 0.5).abs() < 1e-4);
        // A device placed later on a mapped unit takes the macro's value.
        rack.set_unit(0, Some(unit("eq")));
        assert!((info.to_norm(rack.get_param(unit_param(0, 1))) - 0.5).abs() < 1e-4);
    }
}
