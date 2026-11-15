//! Liste les entrées MIDI puis affiche les événements reçus pendant N secondes.
//! `cargo run -p ponzik-midi --example midi_monitor -- 10`

use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    match ponzik_midi::list_inputs() {
        Ok(ports) => {
            println!("{} entrée(s) MIDI", ports.len());
            for p in &ports {
                println!("  {} : {}", p.id, p.name);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    }
    let inputs = ponzik_midi::open_all_inputs();
    for i in &inputs {
        println!("ouvert : {}", i.name());
    }
    let end = Instant::now() + Duration::from_secs(secs);
    let mut events = Vec::new();
    while Instant::now() < end {
        for i in &inputs {
            i.poll(&mut events);
            for e in events.drain(..) {
                println!("{} : {e:?}", i.name());
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
