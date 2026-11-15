//! Différence entre deux états du projet → commandes moteur.
//!
//! L'interface ne parle jamais directement au moteur pour éditer : elle modifie
//! le projet, puis cette fonction envoie exactement ce qui a changé. L'annulation
//! et l'ouverture de projet passent par le même chemin.

use std::sync::Arc;

use ponzik_engine::mixer::{INSERT_SLOTS, SEND_SLOTS, TRACKS};
use ponzik_engine::{Command, Sample, registry};
use ponzik_project::{InstrumentDef, Project};

fn default_param(kind: &str, id: u32) -> f32 {
    registry::create_instrument(kind)
        .and_then(|i| i.param_info(id).map(|p| p.default))
        .or_else(|| registry::create_effect(kind).and_then(|e| e.param_info(id).map(|p| p.default)))
        .unwrap_or(0.0)
}

/// Paramètres modifiés entre deux listes (identifiant, valeur).
fn param_changes(kind: &str, old: &[(u32, f32)], new: &[(u32, f32)]) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, f32)> = new
        .iter()
        .filter(|(id, v)| old.iter().find(|(i, _)| i == id).is_none_or(|(_, o)| o != v))
        .copied()
        .collect();
    for (id, _) in old {
        if !new.iter().any(|(i, _)| i == id) {
            out.push((*id, default_param(kind, *id)));
        }
    }
    out
}

fn same_instrument(a: &InstrumentDef, b: &InstrumentDef) -> bool {
    a.kind == b.kind && a.sample == b.sample
}

pub fn diff(old: &Project, new: &Project, sample_rate: f32, loader: &dyn Fn(&str) -> Option<Arc<Sample>>) -> Vec<Command> {
    let mut cmds = Vec::new();
    if old.bpm != new.bpm {
        cmds.push(Command::SetBpm(new.bpm));
    }

    // Canaux : par position ; un retrait au milieu remplace les suivants.
    let (n_old, n_new) = (old.channels.len(), new.channels.len());
    for i in (n_new..n_old).rev() {
        cmds.push(Command::RemoveChannel(i));
    }
    for i in 0..n_new.min(n_old) {
        let (a, b) = (&old.channels[i], &new.channels[i]);
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
    }
    for i in n_old..n_new {
        cmds.push(Command::AddChannel(Box::new(new.make_channel(i, sample_rate, loader))));
    }

    if old.patterns != new.patterns
        || old.clips != new.clips
        || old.mode != new.mode
        || old.swing != new.swing
        || old.automation != new.automation
        || old.audio_clips != new.audio_clips
        || n_old != n_new
    {
        cmds.push(Command::SetSequence(Box::new(new.sequence_with(loader))));
    }

    // Mixer.
    let mut routing = false;
    for t in 0..TRACKS.min(old.tracks.len()).min(new.tracks.len()) {
        let (a, b) = (&old.tracks[t], &new.tracks[t]);
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
        for s in 0..SEND_SLOTS {
            if a.sends[s] != b.sends[s] {
                let (target, amount) = b.sends[s];
                cmds.push(Command::SetTrackSend { track: t, slot: s, target, amount });
                routing = true;
            }
        }
        for slot in 0..INSERT_SLOTS {
            match (&a.inserts[slot], &b.inserts[slot]) {
                (Some(x), Some(y)) if x.kind == y.kind => {
                    for (id, value) in param_changes(&y.kind, &x.params, &y.params) {
                        cmds.push(Command::SetEffectParam { track: t, slot, id, value });
                    }
                    if x.bypass != y.bypass {
                        cmds.push(Command::SetInsertBypass { track: t, slot, bypass: y.bypass });
                    }
                }
                (None, None) => {}
                (_, y) => {
                    let effect = y.as_ref().and_then(|d| Project::make_effect(d, sample_rate, new.bpm));
                    cmds.push(Command::SetInsert { track: t, slot, effect });
                    cmds.push(Command::SetInsertBypass { track: t, slot, bypass: y.as_ref().is_some_and(|d| d.bypass) });
                }
            }
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
    use ponzik_project::demo_project;

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
}
