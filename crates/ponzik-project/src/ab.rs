//! A/B comparison of a device: two full states (parameters, plugin state,
//! rack), switched instantly. The device's definition always holds the
//! active state; [`DeviceAb`] keeps the other one and which letter is on.

use crate::json::Json;
use crate::rack::RackDef;
use crate::{EffectDef, InstrumentDef, PluginState};

/// Everything a device's sound depends on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceState {
    /// Values off their defaults.
    pub params: Vec<(u32, f32)>,
    /// A plugin's opaque state.
    pub state: Option<PluginState>,
    /// A rack's chains and mappings.
    pub rack: Option<Box<RackDef>>,
}

/// The A/B of a device: the letter in use and the state of the other one.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceAb {
    /// 0: A, 1: B.
    pub active: u8,
    pub other: DeviceState,
}

/// A device with A/B states.
pub trait AbDevice {
    fn snapshot(&self) -> DeviceState;
    fn restore(&mut self, s: DeviceState);
    fn ab_slot(&mut self) -> &mut Option<Box<DeviceAb>>;
    fn ab_ref(&self) -> Option<&DeviceAb>;

    /// Letter in use: 0 (A) or 1 (B).
    fn ab_active(&self) -> u8 {
        self.ab_ref().map_or(0, |a| a.active)
    }

    /// Switches to the other letter. The first time, B starts as a copy of A.
    fn ab_switch(&mut self) {
        let current = self.snapshot();
        let (active, other) = match self.ab_slot().take() {
            Some(ab) => (ab.active, ab.other),
            None => (0, current.clone()),
        };
        self.restore(other);
        *self.ab_slot() = Some(Box::new(DeviceAb { active: 1 - active.min(1), other: current }));
    }

    /// Copies the letter in use onto the other one (A → B, or B → A).
    fn ab_copy(&mut self) {
        let current = self.snapshot();
        let active = self.ab_active();
        *self.ab_slot() = Some(Box::new(DeviceAb { active, other: current }));
    }

    /// True when both letters sound the same (or B was never used).
    fn ab_same(&self) -> bool {
        self.ab_ref().is_none_or(|ab| ab.other == self.snapshot())
    }
}

impl AbDevice for InstrumentDef {
    fn snapshot(&self) -> DeviceState {
        DeviceState { params: self.params.clone(), state: self.state.clone(), rack: None }
    }

    fn restore(&mut self, s: DeviceState) {
        self.params = s.params;
        // A plugin without a captured state keeps the one it has.
        if s.state.is_some() {
            self.state = s.state;
        }
    }

    fn ab_slot(&mut self) -> &mut Option<Box<DeviceAb>> {
        &mut self.ab
    }

    fn ab_ref(&self) -> Option<&DeviceAb> {
        self.ab.as_deref()
    }
}

impl AbDevice for EffectDef {
    fn snapshot(&self) -> DeviceState {
        DeviceState { params: self.params.clone(), state: self.state.clone(), rack: self.rack.clone() }
    }

    fn restore(&mut self, s: DeviceState) {
        self.params = s.params;
        if s.state.is_some() {
            self.state = s.state;
        }
        if s.rack.is_some() {
            self.rack = s.rack;
        }
    }

    fn ab_slot(&mut self) -> &mut Option<Box<DeviceAb>> {
        &mut self.ab
    }

    fn ab_ref(&self) -> Option<&DeviceAb> {
        self.ab.as_deref()
    }
}

// --- JSON ---

pub(crate) fn ab_json(ab: &DeviceAb, params_json: fn(&[(u32, f32)]) -> Json) -> Json {
    let mut pairs = vec![("active".to_string(), ab.active.into()), ("params".to_string(), params_json(&ab.other.params))];
    if let Some(state) = &ab.other.state {
        pairs.push(("state".to_string(), crate::io::state_json(state)));
    }
    if let Some(rack) = ab.other.rack.as_deref() {
        pairs.push(("rack".to_string(), crate::io::rack_json(rack)));
    }
    Json::Obj(pairs)
}

/// The A/B of a device read back; `rack`: the device is a rack.
pub(crate) fn load_ab(v: Option<&Json>, load_params: fn(Option<&Json>) -> Vec<(u32, f32)>, rack: bool) -> Option<Box<DeviceAb>> {
    let v = v.filter(|v| v.as_obj().is_some())?;
    let active = (v.get("active").and_then(Json::as_f64) == Some(1.0)) as u8;
    let other = DeviceState {
        params: load_params(v.get("params")),
        state: crate::io::load_state(v.get("state")),
        rack: (rack && v.get("rack").is_some()).then(|| Box::new(crate::io::load_rack(v.get("rack")))),
    };
    Some(Box::new(DeviceAb { active, other }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_keeps_both_states_and_copies() {
        let mut fx = EffectDef::new("clap:/p.clap#x");
        fx.params = vec![(0, 0.5)];
        fx.state = Some(vec![1, 2, 3].into());
        assert_eq!(fx.ab_active(), 0);
        assert!(fx.ab_same());
        // B starts as A, then moves on its own.
        fx.ab_switch();
        assert_eq!(fx.ab_active(), 1);
        assert_eq!(fx.params, [(0, 0.5)]);
        fx.params = vec![(0, 0.9), (2, 4.0)];
        fx.state = Some(vec![9].into());
        assert!(!fx.ab_same());
        // Back to A: its values and its state.
        fx.ab_switch();
        assert_eq!(fx.ab_active(), 0);
        assert_eq!(fx.params, [(0, 0.5)]);
        assert_eq!(fx.state.as_deref(), Some(&[1u8, 2, 3][..]));
        fx.ab_switch();
        assert_eq!(fx.params, [(0, 0.9), (2, 4.0)]);
        assert_eq!(fx.state.as_deref(), Some(&[9u8][..]));
        // Copy B → A.
        fx.ab_copy();
        assert!(fx.ab_same());
        fx.ab_switch();
        assert_eq!((fx.ab_active(), fx.params.clone()), (0, vec![(0, 0.9), (2, 4.0)]));

        // A native instrument: just its values.
        let mut inst = InstrumentDef::new("pulse").with(1, 0.2);
        inst.ab_switch();
        inst.set(1, 0.7);
        inst.ab_switch();
        assert_eq!(inst.get(1), Some(0.2));
        assert_eq!(inst.state, None);
    }

    #[test]
    fn ab_states_roundtrip_through_the_project_file() {
        let mut p = crate::demo_project();
        p.channels[0].instrument.set(3, 0.25);
        p.channels[0].instrument.ab_switch();
        p.channels[0].instrument.set(3, 0.75);
        let mut fx = EffectDef::new_rack();
        fx.rack_insert(0, None, "eq");
        fx.state = Some(vec![4, 5].into());
        fx.ab_switch();
        fx.rack_insert(0, None, "delay");
        fx.set(1, 0.3);
        p.tracks[2].inserts[1] = Some(fx);
        let back = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(back.channels[0].instrument.ab, p.channels[0].instrument.ab);
        assert_eq!(back.channels[0].instrument.ab_active(), 1);
        assert_eq!(back.tracks[2].inserts[1], p.tracks[2].inserts[1]);
        // Junk: no A/B.
        let junk = r#"{"format": "ponzik-project", "channels": [{"instrument": {"kind": "pulse", "ab": 7}}]}"#;
        assert_eq!(crate::io::load(junk).unwrap().channels[0].instrument.ab, None);
    }
}
