//! Effect racks and modulators on the audio thread: processing, macro moves,
//! parameter changes, layout edits and modulator updates never allocate, and
//! the modulation reaches the parameters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use ponzik_engine::effects::EffectRack;
use ponzik_engine::effects::fx_rack::{P_MACRO, unit_param};
use ponzik_engine::modulation::{LFO_RATE, LFO_SHAPE};
use ponzik_engine::{
    Channel, Command, Effect, Engine, MacroMap, ModKind, ModLink, ModMeters, ModParam, ModSet, Modulator, Params,
    RackLayout, Sequence, registry,
};

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

const SR: f32 = 48_000.0;

fn effect(kind: &str) -> Box<dyn Effect> {
    let mut fx = registry::create_effect(kind).expect(kind);
    fx.set_sample_rate(SR);
    fx
}

fn lfo_set(target_slot: usize, id: u32, info: ponzik_engine::ParamInfo, base: f32, meters: Arc<ModMeters>) -> Box<ModSet> {
    let mut lfo = Modulator::new(ModKind::Lfo);
    lfo.set_param(LFO_RATE, 8.0);
    lfo.set_param(LFO_SHAPE, 1.0);
    let params = vec![ModParam::new(target_slot, id, info, base)];
    let links = vec![ModLink { modulator: 0, param: 0, depth: 0.5 }];
    Box::new(ModSet::new(vec![lfo, Modulator::new(ModKind::Follower)], params, links, meters))
}

#[test]
fn racks_and_modulators_never_allocate() {
    let pulse = registry::create_instrument("pulse").expect("pulse");
    let channel = Channel::new("lead", pulse).with_track(1);
    let (mut engine, mut handle) = Engine::new(SR, vec![channel], Sequence::default());

    // A rack on track 1: chain 1 EQ → saturator, chain 2 reverb; macro 1
    // drives the saturator's drive.
    let mut rack = EffectRack::default();
    rack.set_sample_rate(SR);
    rack.set_unit(0, Some(effect("eq")));
    rack.set_unit(1, Some(effect("saturator")));
    rack.set_unit(2, Some(effect("reverb")));
    let mut layout = RackLayout::default();
    layout.chains = 2;
    layout.set_chain(0, &[0, 1]);
    layout.set_chain(1, &[2]);
    rack.set_layout(layout);
    assert!(rack.param_info(unit_param(1, 1)).is_some());
    handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(Box::new(rack)) }).unwrap();
    handle.send(Command::SetRackMapping { track: 1, slot: 0, index: 0, map: Some(MacroMap { macro_index: 0, unit: 1, param: 1, min: 0.0, max: 1.0 }) }).unwrap();
    // An LFO on macro 1 of the rack, another on the channel's instrument.
    let macro_info = *registry::create_effect("rack").unwrap().param_info(P_MACRO).unwrap();
    let track_meters = Arc::new(ModMeters::default());
    handle.send(Command::SetTrackModulators { track: 1, mods: Some(lfo_set(0, P_MACRO, macro_info, 0.5, track_meters.clone())) }).unwrap();
    let pulse_info = registry::create_instrument("pulse").unwrap().params()[1];
    let ch_meters = Arc::new(ModMeters::default());
    handle.send(Command::SetChannelModulators { channel: 0, mods: Some(lfo_set(0, pulse_info.id, pulse_info, pulse_info.default, ch_meters.clone())) }).unwrap();
    // A replacement set, built ahead: swapping it in must not allocate either.
    let spare = lfo_set(0, P_MACRO, macro_info, 0.2, track_meters.clone());
    let spare_unit = effect("chorus");

    let mut out = vec![0.0f32; 256 * 2];
    engine.process(&mut out, 2);
    handle.send(Command::Play).unwrap();
    handle.send(Command::NoteOn { channel: 0, note: 60, velocity: 0.9 }).unwrap();
    engine.process(&mut out, 2);

    // The counter does see allocations.
    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let mut spare = Some(spare);
    let mut spare_unit = Some(spare_unit);
    let mut seen = Vec::with_capacity(512);
    let before = allocs();
    for block in 0..400u32 {
        if block == 100 {
            handle.send(Command::SetTrackModulators { track: 1, mods: spare.take() }).unwrap();
            handle.send(Command::SetRackUnit { track: 1, slot: 0, unit: 3, effect: spare_unit.take() }).unwrap();
            layout.set_chain(1, &[2, 3]);
            handle.send(Command::SetRackLayout { track: 1, slot: 0, layout }).unwrap();
        }
        if block % 7 == 0 {
            handle.send(Command::SetEffectParam { track: 1, slot: 0, id: P_MACRO, value: (block % 10) as f32 / 10.0 }).unwrap();
            handle.send(Command::SetEffectParam { track: 1, slot: 0, id: unit_param(0, 4), value: 200.0 + block as f32 }).unwrap();
        }
        engine.process(&mut out, 2);
        seen.push(track_meters.get(0));
    }
    assert_eq!(allocs() - before, 0, "the audio path allocated");
    // The LFO moved.
    let (lo, hi) = seen.iter().fold((1.0f32, -1.0f32), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(hi - lo > 1.0, "{lo}..{hi}");
    assert!(ch_meters.get(0).abs() <= 1.0);
    assert!(out.iter().all(|s| s.is_finite()));
}
