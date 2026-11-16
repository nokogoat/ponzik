//! Sortie MIDI : thread d'envoi branché au moteur, horloge MIDI par port.
//!
//! Les ports de sortie sont relevés au démarrage ; leur numéro (« Port 1 »…)
//! est celui que choisit l'instrument External MIDI. L'horloge se règle depuis
//! la palette, port par port, et se retient d'une session à l'autre
//! (`midi-out.cfg`, identifiants de ports).

use ponzik_engine::{Command, MidiOut};
use ponzik_midi::out::{DEFAULT_QUEUE, MAX_PORTS, OutputScheduler};

use super::app::App;
use super::compat;

/// État de la sortie MIDI côté interface.
#[derive(Default)]
pub struct MidiOutState {
    scheduler: Option<OutputScheduler>,
    /// Ports qui reçoivent l'horloge (bit par indice de port).
    pub clock_ports: u32,
}

impl MidiOutState {
    /// Noms des ports de sortie, dans l'ordre de leurs indices.
    pub fn port_names(&self) -> Vec<String> {
        self.scheduler.as_ref().map_or_else(Vec::new, |s| s.ports().iter().map(|p| p.name.clone()).collect())
    }
}

fn config_path() -> std::path::PathBuf {
    compat::config_dir().join("midi-out.cfg")
}

impl App {
    /// Démarre le thread d'envoi MIDI et le branche au moteur.
    pub fn start_midi_out(&mut self) {
        let (sender, scheduler) = match OutputScheduler::start_system(DEFAULT_QUEUE) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("MIDI output unavailable: {e}");
                return;
            }
        };
        for (i, p) in scheduler.ports().iter().enumerate() {
            eprintln!("MIDI out {}: {}", i + 1, p.name);
        }
        let latency = self.rec.output.as_ref().map_or(0.0, |o| o.latency().output as f32 / self.sample_rate.max(1) as f32);
        // Ports d'horloge retenus, retrouvés par identifiant.
        let saved = std::fs::read_to_string(config_path()).unwrap_or_default();
        let clock_ports = scheduler
            .ports()
            .iter()
            .take(MAX_PORTS)
            .enumerate()
            .filter(|(_, p)| saved.lines().any(|l| l.strip_prefix("clock=") == Some(p.id.as_str())))
            .fold(0u32, |mask, (i, _)| mask | 1 << i);
        self.midi_out = MidiOutState { scheduler: Some(scheduler), clock_ports };
        self.send(Command::SetMidiOut(Some(Box::new(MidiOut::new(sender, latency)))));
        self.send(Command::SetMidiClock { ports: clock_ports });
    }

    /// Active ou coupe l'horloge MIDI sur le port `index`.
    pub fn toggle_midi_clock(&mut self, index: usize) {
        if index >= MAX_PORTS {
            return;
        }
        self.midi_out.clock_ports ^= 1 << index;
        let ports = self.midi_out.clock_ports;
        self.send(Command::SetMidiClock { ports });
        let on = ports & (1 << index) != 0;
        let name = self.midi_out.port_names().get(index).cloned().unwrap_or_default();
        self.toast(&ponzik_i18n::tr_fmt!("MIDI clock {} · {}", if on { ponzik_i18n::tr("on") } else { ponzik_i18n::tr("off") }, name));
        if let Some(s) = self.midi_out.scheduler.as_ref() {
            let text: String = s
                .ports()
                .iter()
                .enumerate()
                .filter(|(i, _)| ports & (1 << i) != 0)
                .map(|(_, p)| format!("clock={}\n", p.id))
                .collect();
            let path = config_path();
            let res = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, text));
            if let Err(e) = res {
                eprintln!("MIDI settings not saved ({}): {e}", path.display());
            }
        }
    }

    /// Entrées de palette : horloge MIDI, port par port.
    pub fn midi_out_palette(&self) -> Vec<(String, usize)> {
        self.midi_out
            .port_names()
            .into_iter()
            .take(MAX_PORTS)
            .enumerate()
            .map(|(i, name)| {
                let state = if self.midi_out.clock_ports & (1 << i) != 0 { ponzik_i18n::tr("on") } else { ponzik_i18n::tr("off") };
                (ponzik_i18n::tr_fmt!("MIDI clock out: Port {} · {} ({})", i + 1, name.as_str(), state), i)
            })
            .collect()
    }
}
