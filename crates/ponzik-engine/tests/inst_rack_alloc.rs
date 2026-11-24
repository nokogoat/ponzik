//! The Instrument Rack on the audio thread: dispatching notes to its chains
//! (zones, transpose, note effects), rendering them with their effects,
//! moving macros, changing zones and mappings, and swapping devices never
//! allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ponzik_engine::instruments::InstrumentRack;
use ponzik_engine::instruments::inst_rack::{
    ChainZone, LayerDevice, P_MACRO, P_SELECTOR, SELF_UNIT, Slot, Zone, unit_of, unit_param,
};
use ponzik_engine::{Channel, Command, Engine, Instrument, MacroMap, Params, Sequence, registry};

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

fn instrument(kind: &str) -> Box<dyn Instrument> {
    let mut i = registry::create_instrument(kind).expect(kind);
    i.set_sample_rate(SR);
    i
}

#[test]
fn instrument_racks_never_allocate() {
    // Piano below C4, strings from C4 an octave up behind an arpeggiator and a
    // reverb, a Pulse layer driven by the chain selector.
    let mut rack = InstrumentRack::default();
    rack.set_sample_rate(SR);
    rack.set_device(unit_of(0, Slot::Instrument), LayerDevice::Instrument(instrument("piano")));
    rack.set_zone(0, ChainZone { key: Zone::new(0, 59).with_fades(0, 4), ..ChainZone::active() });
    rack.set_device(unit_of(1, Slot::Instrument), LayerDevice::Instrument(instrument("strings")));
    let mut arp = registry::create_note_fx("arpeggiator").expect("arp");
    arp.set_sample_rate(SR);
    rack.set_device(unit_of(1, Slot::NoteFx(0)), LayerDevice::NoteFx(arp));
    let mut reverb = registry::create_effect("reverb").expect("reverb");
    reverb.set_sample_rate(SR);
    rack.set_device(unit_of(1, Slot::Effect(0)), LayerDevice::Effect(reverb));
    rack.set_zone(1, ChainZone { key: Zone::new(60, 127).with_fades(4, 0), transpose: 12, ..ChainZone::active() });
    rack.set_device(unit_of(2, Slot::Instrument), LayerDevice::Instrument(instrument("pulse")));
    rack.set_zone(2, ChainZone { vel: Zone::new(64, 127).with_fades(20, 0), sel: Zone::new(64, 127).with_fades(30, 0), ..ChainZone::active() });
    let cutoff = rack.instrument(2).unwrap().params().iter().find(|p| p.name == "Cutoff").map(|p| p.id).expect("cutoff");
    assert!(rack.param_info(unit_param(unit_of(2, Slot::Instrument), cutoff)).is_some());

    let channel = Channel::new("layers", Box::new(rack)).with_track(1);
    let (mut engine, mut handle) = Engine::new(SR, vec![channel], Sequence::default());
    let pulse = unit_of(2, Slot::Instrument);
    handle.send(Command::SetLayerMapping { channel: 0, index: 0, map: Some(MacroMap { macro_index: 0, unit: SELF_UNIT, param: P_SELECTOR, min: 0.0, max: 1.0 }) }).unwrap();
    handle.send(Command::SetLayerMapping { channel: 0, index: 1, map: Some(MacroMap { macro_index: 1, unit: pulse, param: cutoff, min: 0.2, max: 0.9 }) }).unwrap();
    // Devices built ahead: swapping them in must not allocate either.
    let mut spare_inst = Some(instrument("pluck"));
    let mut spare_fx = Some({
        let mut fx = registry::create_effect("chorus").expect("chorus");
        fx.set_sample_rate(SR);
        fx
    });

    let mut out = vec![0.0f32; 256 * 2];
    engine.process(&mut out, 2);
    handle.send(Command::Play).unwrap();
    engine.process(&mut out, 2);

    let probe = allocs();
    std::hint::black_box(vec![1u8; 16]);
    assert!(allocs() > probe);

    let mut peak = 0.0f32;
    let before = allocs();
    for block in 0..600u32 {
        if block % 9 == 0 {
            let note = 36 + (block / 9 % 48) as u8;
            handle.send(Command::NoteOn { channel: 0, note, velocity: 0.3 + (block % 7) as f32 / 10.0 }).unwrap();
            handle.send(Command::NoteOn { channel: 0, note: note + 7, velocity: 0.9 }).unwrap();
        }
        if block % 9 == 5 {
            let note = 36 + (block / 9 % 48) as u8;
            handle.send(Command::NoteOff { channel: 0, note }).unwrap();
            handle.send(Command::NoteOff { channel: 0, note: note + 7 }).unwrap();
        }
        if block % 5 == 0 {
            handle.send(Command::SetInstrumentParam { channel: 0, id: P_MACRO, value: (block % 11) as f32 / 10.0 }).unwrap();
            handle.send(Command::SetInstrumentParam { channel: 0, id: P_MACRO + 1, value: (block % 13) as f32 / 12.0 }).unwrap();
        }
        if block == 200 {
            handle.send(Command::SetLayerDevice { channel: 0, unit: unit_of(3, Slot::Instrument), device: LayerDevice::Instrument(spare_inst.take().unwrap()) }).unwrap();
            handle.send(Command::SetLayerZone { channel: 0, chain: 3, zone: ChainZone { transpose: -12, ..ChainZone::active() } }).unwrap();
            handle.send(Command::SetLayerDevice { channel: 0, unit: unit_of(0, Slot::Effect(2)), device: LayerDevice::Effect(spare_fx.take().unwrap()) }).unwrap();
        }
        if block == 400 {
            handle.send(Command::SetLayerZone { channel: 0, chain: 1, zone: ChainZone { solo: true, ..ChainZone::active() } }).unwrap();
            handle.send(Command::SetLayerMapping { channel: 0, index: 1, map: None }).unwrap();
        }
        engine.process(&mut out, 2);
        peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
    }
    assert_eq!(allocs() - before, 0, "the audio path allocated");
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak > 0.01, "the layers play: {peak}");
}
