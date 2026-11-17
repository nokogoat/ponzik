//! Différence entre deux états du projet → commandes moteur.
//!
//! L'interface ne parle jamais directement au moteur pour éditer : elle modifie
//! le projet, puis cette fonction envoie exactement ce qui a changé. L'annulation
//! et l'ouverture de projet passent par le même chemin.

use std::sync::Arc;

use ponzik_engine::mixer::{INSERT_SLOTS, SEND_SLOTS, TRACKS};
use ponzik_engine::{Command, Frozen, NOTE_FX_SLOTS, Sample, registry};
use ponzik_project::{InstrumentDef, Project};

type Loader<'a> = &'a dyn Fn(&str) -> Option<Arc<Sample>>;

/// What the engine plays for a channel: its devices (`None`), or the freeze
/// files that replace them (its own, its track's).
fn channel_freeze(p: &Project, i: usize, loader: Loader) -> Option<(Option<String>, Option<String>)> {
    let c = &p.channels[i];
    let track = p.tracks.get(c.track).and_then(|t| t.frozen.clone());
    (!p.channel_live(i, loader)).then(|| (c.frozen.clone(), track))
}

/// The freeze file a track plays, when it loads.
fn track_freeze(p: &Project, t: usize, loader: Loader) -> Option<String> {
    p.frozen_track(t, loader).and(p.tracks[t].frozen.clone())
}

fn default_param(kind: &str, id: u32) -> f32 {
    registry::create_instrument(kind)
        .and_then(|i| i.param_info(id).map(|p| p.default))
        .or_else(|| registry::create_effect(kind).and_then(|e| e.param_info(id).map(|p| p.default)))
        .unwrap_or(0.0)
}

fn note_fx_default(kind: &str, id: u32) -> f32 {
    registry::create_note_fx(kind).and_then(|f| f.param_info(id).map(|p| p.default)).unwrap_or(0.0)
}

/// Paramètres modifiés entre deux listes (identifiant, valeur).
fn param_changes(kind: &str, old: &[(u32, f32)], new: &[(u32, f32)]) -> Vec<(u32, f32)> {
    changes_with(kind, old, new, default_param)
}

fn changes_with(kind: &str, old: &[(u32, f32)], new: &[(u32, f32)], default: fn(&str, u32) -> f32) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, f32)> = new
        .iter()
        .filter(|(id, v)| old.iter().find(|(i, _)| i == id).is_none_or(|(_, o)| o != v))
        .copied()
        .collect();
    for (id, _) in old {
        if !new.iter().any(|(i, _)| i == id) {
            out.push((*id, default(kind, *id)));
        }
    }
    out
}

fn same_instrument(a: &InstrumentDef, b: &InstrumentDef) -> bool {
    a.kind == b.kind && a.sample == b.sample && a.pads == b.pads
}

pub fn diff(old: &Project, new: &Project, sample_rate: f32, loader: Loader) -> Vec<Command> {
    let mut cmds = Vec::new();

    // Canaux : par position ; un retrait au milieu remplace les suivants.
    let (n_old, n_new) = (old.channels.len(), new.channels.len());
    for i in (n_new..n_old).rev() {
        cmds.push(Command::RemoveChannel(i));
    }
    for i in 0..n_new.min(n_old) {
        let (a, b) = (&old.channels[i], &new.channels[i]);
        let frozen = channel_freeze(new, i, loader);
        if channel_freeze(old, i, loader) != frozen {
            // Frozen or thawed: the whole channel changes (devices unloaded or back).
            cmds.push(Command::ReplaceChannel { channel: i, ch: Box::new(new.make_channel(i, sample_rate, loader)) });
            continue;
        }
        if frozen.is_some() {
            // Its devices are not in the engine: only what stays live.
            if a.gain != b.gain {
                cmds.push(Command::SetChannelGain { channel: i, gain: b.gain });
            }
            if a.pan != b.pan {
                cmds.push(Command::SetChannelPan { channel: i, pan: b.pan });
            }
            if a.muted != b.muted {
                cmds.push(Command::SetChannelMute { channel: i, muted: b.muted });
            }
            continue;
        }
        if !same_instrument(&a.instrument, &b.instrument) {
            cmds.push(Command::ReplaceInstrument {
                channel: i,
                instrument: Project::make_instrument(&b.instrument, sample_rate, loader),
            });
        } else {
            for (id, value) in param_changes(&b.instrument.kind, &a.instrument.params, &b.instrument.params) {
                cmds.push(Command::SetInstrumentParam { channel: i, id, value });
            }
        }
        for slot in 0..NOTE_FX_SLOTS {
            let (x, y) = (a.note_fx.get(slot).and_then(Option::as_ref), b.note_fx.get(slot).and_then(Option::as_ref));
            match (x, y) {
                (Some(x), Some(y)) if x.kind == y.kind => {
                    for (id, value) in changes_with(&y.kind, &x.params, &y.params, note_fx_default) {
                        cmds.push(Command::SetNoteFxParam { channel: i, slot, id, value });
                    }
                }
                (None, None) => {}
                (_, y) => {
                    let fx = y.and_then(|d| Project::make_note_fx(d, sample_rate, new.bpm));
                    cmds.push(Command::SetNoteFx { channel: i, slot, fx });
                }
            }
        }
        if a.gain != b.gain {
            cmds.push(Command::SetChannelGain { channel: i, gain: b.gain });
        }
        if a.pan != b.pan {
            cmds.push(Command::SetChannelPan { channel: i, pan: b.pan });
        }
        if a.muted != b.muted {
            cmds.push(Command::SetChannelMute { channel: i, muted: b.muted });
        }
        if a.track != b.track {
            cmds.push(Command::SetChannelTrack { channel: i, track: b.track });
        }
        // After the parameters: the set reads its base values from `new`.
        if a.modulators != b.modulators || (!b.modulators.is_empty() && !same_instrument(&a.instrument, &b.instrument)) {
            cmds.push(Command::SetChannelModulators { channel: i, mods: new.make_channel_mods(i) });
        }
    }
    for i in n_old..n_new {
        cmds.push(Command::AddChannel(Box::new(new.make_channel(i, sample_rate, loader))));
    }

    if old.patterns != new.patterns
        || old.bpm != new.bpm
        || old.tempo_changes != new.tempo_changes
        || old.signatures != new.signatures
        || old.clips != new.clips
        || old.mode != new.mode
        || old.swing != new.swing
        || old.automation != new.automation
        || old.audio_clips != new.audio_clips
        || old.take_groups != new.take_groups
        || n_old != n_new
        || (0..TRACKS.min(old.tracks.len()).min(new.tracks.len()))
            .any(|t| track_freeze(old, t, loader).is_some() != track_freeze(new, t, loader).is_some())
    {
        cmds.push(Command::SetSequence(Box::new(new.sequence_for(sample_rate, loader, &|job| ponzik_project::warp::cached(job.key)))));
    }

    // Mixer.
    let mut routing = false;
    // Tracks frozen or thawed: their inserts and modulators are unloaded or come back.
    let mut refrozen = Vec::new();
    for t in 0..TRACKS.min(old.tracks.len()).min(new.tracks.len()) {
        let frozen = track_freeze(new, t, loader);
        if track_freeze(old, t, loader) != frozen {
            refrozen.push(t);
            routing = true;
            let def = &new.tracks[t];
            for slot in 0..INSERT_SLOTS {
                let fx = def.inserts[slot].as_ref().filter(|_| frozen.is_none());
                cmds.push(Command::SetInsert { track: t, slot, effect: fx.and_then(|d| Project::make_effect(d, sample_rate, new.bpm)) });
                cmds.push(Command::SetInsertBypass { track: t, slot, bypass: fx.is_some_and(|d| d.bypass) });
                cmds.push(Command::SetInsertSidechain { track: t, slot, key: fx.and_then(|d| d.sidechain) });
            }
            let mods = if frozen.is_some() { None } else { new.make_track_mods(t) };
            cmds.push(Command::SetTrackModulators { track: t, mods });
            let player = new.frozen_track(t, loader).map(|s| Box::new(Frozen::new(s)));
            cmds.push(Command::SetTrackFrozen { track: t, frozen: player });
        }
    }
    for t in 0..TRACKS.min(old.tracks.len()).min(new.tracks.len()) {
        let (a, b) = (&old.tracks[t], &new.tracks[t]);
        let unloaded = refrozen.contains(&t) || track_freeze(new, t, loader).is_some();
        if a.gain != b.gain {
            cmds.push(Command::SetTrackGain { track: t, gain: b.gain });
        }
        if a.pan != b.pan {
            cmds.push(Command::SetTrackPan { track: t, pan: b.pan });
        }
        if a.muted != b.muted {
            cmds.push(Command::SetTrackMute { track: t, muted: b.muted });
        }
        if a.output != b.output {
            cmds.push(Command::SetTrackOutput { track: t, output: b.output });
            routing = true;
        }
        if a.vca != b.vca {
            cmds.push(Command::SetTrackVca { track: t, vca: b.vca });
        }
        for s in 0..SEND_SLOTS {
            if a.sends[s] != b.sends[s] {
                let (target, amount) = b.sends[s];
                cmds.push(Command::SetTrackSend { track: t, slot: s, target, amount });
                routing = true;
            }
        }
        for slot in (0..INSERT_SLOTS).filter(|_| !unloaded) {
            match (&a.inserts[slot], &b.inserts[slot]) {
                (Some(x), Some(y)) if x.kind == y.kind => {
                    if y.kind == ponzik_project::rack::RACK {
                        cmds.extend(ponzik_project::rack::rack_commands(t, slot, x, y, sample_rate, new.bpm));
                    } else {
                        for (id, value) in param_changes(&y.kind, &x.params, &y.params) {
                            cmds.push(Command::SetEffectParam { track: t, slot, id, value });
                        }
                    }
                    if x.bypass != y.bypass {
                        cmds.push(Command::SetInsertBypass { track: t, slot, bypass: y.bypass });
                    }
                    if x.sidechain != y.sidechain {
                        cmds.push(Command::SetInsertSidechain { track: t, slot, key: y.sidechain });
                        routing = true;
                    }
                }
                (None, None) => {}
                (_, y) => {
                    let effect = y.as_ref().and_then(|d| Project::make_effect(d, sample_rate, new.bpm));
                    cmds.push(Command::SetInsert { track: t, slot, effect });
                    cmds.push(Command::SetInsertBypass { track: t, slot, bypass: y.as_ref().is_some_and(|d| d.bypass) });
                    // Le nouvel effet ne garde pas la clé de l'ancien.
                    let key = y.as_ref().and_then(|d| d.sidechain);
                    let old_key = a.inserts[slot].as_ref().and_then(|d| d.sidechain);
                    if key.is_some() || old_key.is_some() {
                        cmds.push(Command::SetInsertSidechain { track: t, slot, key });
                        routing |= key != old_key;
                    }
                }
            }
        }
    }
    // Modulators after the inserts they drive; a replaced insert takes them again.
    for t in 0..TRACKS.min(old.tracks.len()).min(new.tracks.len()) {
        if refrozen.contains(&t) || track_freeze(new, t, loader).is_some() {
            continue;
        }
        let (a, b) = (&old.tracks[t], &new.tracks[t]);
        let replaced = (0..INSERT_SLOTS).any(|s| a.inserts[s].as_ref().map(|f| &f.kind) != b.inserts[s].as_ref().map(|f| &f.kind));
        if a.modulators != b.modulators || (replaced && !b.modulators.is_empty()) {
            cmds.push(Command::SetTrackModulators { track: t, mods: new.make_track_mods(t) });
        }
    }
    if routing && let Some(order) = new.routing_order() {
        cmds.push(Command::SetRoutingOrder(order));
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_project::{NoteFxDef, demo_project};

    fn none(_: &str) -> Option<Arc<Sample>> {
        None
    }

    fn names(cmds: &[Command]) -> Vec<String> {
        cmds.iter().map(|c| format!("{c:?}")).collect()
    }

    #[test]
    fn identical_projects_produce_nothing() {
        let p = demo_project();
        assert!(diff(&p, &p, 48_000.0, &none).is_empty());
    }

    #[test]
    fn edits_map_to_minimal_commands() {
        let a = demo_project();
        let mut b = a.clone();
        b.channels[0].gain = 0.3;
        b.channels[4].instrument.set(2, 900.0);
        b.set_step(0, 0, 3, true);
        let cmds = names(&diff(&a, &b, 48_000.0, &none));
        assert_eq!(cmds, ["SetChannelGain", "SetInstrumentParam", "SetSequence"]);

        let mut c = a.clone();
        c.remove_channel(1);
        let cmds = names(&diff(&a, &c, 48_000.0, &none));
        assert!(cmds.contains(&"RemoveChannel".to_string()));
        assert!(cmds.contains(&"SetSequence".to_string()));
    }

    #[test]
    fn sidechain_key_is_sent_and_reorders() {
        let mut a = demo_project();
        a.tracks[3].inserts[5] = Some(ponzik_project::EffectDef::new("compressor"));
        let mut b = a.clone();
        b.tracks[3].inserts[5].as_mut().unwrap().sidechain = Some(2);
        let cmds = diff(&a, &b, 48_000.0, &none);
        assert!(cmds.iter().any(|c| matches!(c, Command::SetInsertSidechain { track: 3, slot: 5, key: Some(2) })));
        let order = cmds.iter().find_map(|c| match c {
            Command::SetRoutingOrder(o) => Some(o.clone()),
            _ => None,
        });
        let order = order.expect("nouvel ordre de routage");
        let pos = |t| order.iter().position(|&x| x == t).unwrap();
        assert!(pos(2) < pos(3));
        // Retirer l'effet libère la clé.
        let mut c = b.clone();
        c.tracks[3].inserts[5] = None;
        let cmds = diff(&b, &c, 48_000.0, &none);
        assert!(cmds.iter().any(|c| matches!(c, Command::SetInsertSidechain { track: 3, slot: 5, key: None })));
    }

    #[test]
    fn grouping_into_a_bus_reroutes_and_reorders() {
        let a = demo_project();
        let mut b = a.clone();
        let bus = b.add_bus(&[1, 2]).unwrap();
        let cmds = diff(&a, &b, 48_000.0, &none);
        let outputs: Vec<(usize, usize)> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::SetTrackOutput { track, output } => Some((*track, *output)),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, [(1, bus), (2, bus)]);
        assert!(matches!(cmds.last(), Some(Command::SetRoutingOrder(_))));
        // Replier le bus ne change rien au son.
        let mut c = b.clone();
        c.tracks[bus].folded = true;
        assert!(diff(&b, &c, 48_000.0, &none).is_empty());
        // Un VCA : une commande par piste pilotée, sans réordonner.
        let mut d = c.clone();
        let vca = d.add_vca(&[3, 4]).unwrap();
        let cmds = diff(&c, &d, 48_000.0, &none);
        assert_eq!(names(&cmds), ["SetTrackVca", "SetTrackVca"]);
        assert!(matches!(cmds[0], Command::SetTrackVca { track: 3, vca: Some(v) } if v == vca));
    }

    #[test]
    fn rack_and_modulator_edits_map_to_commands() {
        use ponzik_engine::ModKind;
        use ponzik_project::{EffectDef, ModulatorDef};
        let mut a = demo_project();
        let mut rack = EffectDef::new_rack();
        rack.rack_insert(0, None, "eq");
        a.tracks[3].inserts[6] = Some(rack);
        let mut b = a.clone();
        b.tracks[3].inserts[6].as_mut().unwrap().rack_insert(0, None, "delay");
        assert_eq!(names(&diff(&a, &b, 48_000.0, &none)), ["SetRackUnit", "SetRackLayout"]);
        let mut c = b.clone();
        let mut lfo = ModulatorDef::new(ModKind::Lfo);
        lfo.connect(6, 0, 0.5);
        c.tracks[3].modulators.push(lfo);
        let cmds = diff(&b, &c, 48_000.0, &none);
        assert!(matches!(&cmds[..], [Command::SetTrackModulators { track: 3, mods: Some(_) }]));
        // Moving the knob of a modulated parameter: just the parameter.
        let mut d = c.clone();
        d.tracks[3].inserts[6].as_mut().unwrap().set(0, 0.4);
        assert_eq!(names(&diff(&c, &d, 48_000.0, &none)), ["SetEffectParam"]);
        // Replacing the rack: the set follows.
        let mut e = d.clone();
        e.tracks[3].inserts[6] = Some(EffectDef::new("reverb"));
        let n = names(&diff(&d, &e, 48_000.0, &none));
        assert_eq!(n.last().map(String::as_str), Some("SetTrackModulators"));
    }

    #[test]
    fn note_fx_edits_map_to_slot_commands() {
        let a = demo_project();
        let mut b = a.clone();
        b.channels[1].note_fx = vec![None, Some(NoteFxDef::new("chord"))];
        assert_eq!(names(&diff(&a, &b, 48_000.0, &none)), ["SetNoteFx"]);

        let mut c = b.clone();
        c.channels[1].note_fx[1].as_mut().unwrap().set(0, 2.0);
        assert_eq!(names(&diff(&b, &c, 48_000.0, &none)), ["SetNoteFxParam"]);

        // Retour au défaut : le paramètre est renvoyé avec sa valeur par défaut.
        let cmds = diff(&c, &b, 48_000.0, &none);
        assert!(matches!(cmds[..], [Command::SetNoteFxParam { channel: 1, slot: 1, id: 0, value }] if value == 1.0));

        let mut d = c.clone();
        d.channels[1].note_fx[1] = None;
        assert!(matches!(diff(&c, &d, 48_000.0, &none)[..], [Command::SetNoteFx { channel: 1, slot: 1, fx: None }]));
    }

    #[test]
    fn freezing_unloads_and_thawing_reloads() {
        use ponzik_project::FreezeTarget;
        let file = Arc::new(Sample { sample_rate: 48_000.0, left: vec![0.1; 64], right: vec![0.1; 64], ..Default::default() });
        let loader = |p: &str| (p == "f.wav").then(|| file.clone());
        let mut a = demo_project();
        a.tracks[3].inserts[0] = Some(ponzik_project::EffectDef::new("compressor"));
        let mut b = a.clone();
        b.set_frozen(FreezeTarget::Channel(1), Some("f.wav".into()));
        let cmds = diff(&a, &b, 48_000.0, &loader);
        assert!(matches!(&cmds[..], [Command::ReplaceChannel { channel: 1, ch }] if ch.frozen.is_some() && ch.instrument.kind() == "silence"));
        // Frozen: its device edits do not reach the engine, its fader does.
        let mut c = b.clone();
        c.channels[1].instrument.set(2, 0.5);
        c.channels[1].gain = 0.1;
        assert_eq!(names(&diff(&b, &c, 48_000.0, &loader)), ["SetChannelGain"]);
        // Thawed: the devices come back.
        let cmds = diff(&c, &a, 48_000.0, &loader);
        assert!(matches!(&cmds[..], [Command::ReplaceChannel { channel: 1, ch }] if ch.frozen.is_none() && ch.instrument.kind() != "silence"));
        // A missing file changes nothing: the live devices play.
        let mut lost = a.clone();
        lost.set_frozen(FreezeTarget::Channel(1), Some("gone.wav".into()));
        assert!(diff(&a, &lost, 48_000.0, &loader).is_empty());

        // A track: its inserts unloaded, its channels too, the sequence resent.
        let mut d = a.clone();
        d.set_frozen(FreezeTarget::Track(3), Some("f.wav".into()));
        let cmds = diff(&a, &d, 48_000.0, &loader);
        let n = names(&cmds);
        assert!(n.contains(&"SetTrackFrozen".to_string()) && n.contains(&"SetSequence".to_string()));
        assert!(cmds.iter().any(|c| matches!(c, Command::SetInsert { track: 3, slot: 0, effect: None })));
        let on_track: Vec<usize> = (0..a.channels.len()).filter(|&i| a.channels[i].track == 3).collect();
        for i in &on_track {
            assert!(cmds.iter().any(|c| matches!(c, Command::ReplaceChannel { channel, .. } if channel == i)));
        }
        let mut e = d.clone();
        e.tracks[3].inserts[0].as_mut().unwrap().set(0, -30.0);
        assert!(diff(&d, &e, 48_000.0, &loader).is_empty(), "a frozen insert is not in the engine");
        let cmds = diff(&e, &a, 48_000.0, &loader);
        assert!(cmds.iter().any(|c| matches!(c, Command::SetInsert { track: 3, slot: 0, effect: Some(_) })));
        assert!(cmds.iter().any(|c| matches!(c, Command::SetTrackFrozen { track: 3, frozen: None })));
    }
}
