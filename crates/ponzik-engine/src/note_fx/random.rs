use super::{Countdown, NoteBuffer, NoteEvent, NoteFx, Rng};
use crate::effects::{DEFAULT_SR, Values};
use crate::param::{ParamInfo, Params, Unit};

const P_PROBABILITY: u32 = 0;
const P_VELOCITY: u32 = 1;
const P_TIMING: u32 = 2;

/// Notes retardées en attente ; au-delà, elles partent sans retard.
const MAX_PENDING: usize = 64;

static PARAMS: [ParamInfo; 3] = [
    ParamInfo::new(P_PROBABILITY, "Probability", 0.0, 1.0, 1.0, Unit::Percent).group("Chance"),
    ParamInfo::new(P_VELOCITY, "Velocity", 0.0, 1.0, 0.0, Unit::Percent).group("Humanize"),
    ParamInfo::new(P_TIMING, "Timing", 0.0, 0.1, 0.0, Unit::Seconds).group("Humanize"),
];

/// Hasard et humanisation : chaque note a une probabilité de jouer, sa vélocité
/// varie (± la part choisie) et elle part avec un retard aléatoire jusqu'au
/// maximum choisi.
///
/// Une note retardée garde sa durée : son arrêt est retardé d'autant. Une note
/// écartée n'émet rien, ni début ni fin.
pub struct Humanizer {
    values: Values<3>,
    sample_rate: f32,
    rng: Rng,
    /// Notes jouées écartées par la probabilité.
    dropped: [bool; 128],
    /// Retard appliqué au début de chaque note jouée, en échantillons.
    delay: [f64; 128],
    pending: [(Countdown, NoteEvent); MAX_PENDING],
    pending_len: usize,
}

impl Default for Humanizer {
    fn default() -> Self {
        Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            rng: Rng::new(0x0BAD_5EED),
            dropped: [false; 128],
            delay: [0.0; 128],
            pending: [(Countdown::default(), NoteEvent::off(0)); MAX_PENDING],
            pending_len: 0,
        }
    }
}

impl Humanizer {
    fn send(&mut self, delay: f64, ev: NoteEvent, out: &mut NoteBuffer) {
        let at = Countdown(delay);
        if at.due() || self.pending_len == MAX_PENDING {
            out.push(ev);
        } else {
            self.pending[self.pending_len] = (at, ev);
            self.pending_len += 1;
        }
    }
}

impl Params for Humanizer {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        self.values.set(&PARAMS, id, value);
    }
}

impl NoteFx for Humanizer {
    fn kind(&self) -> &'static str {
        "random"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    fn reset(&mut self) {
        self.dropped = [false; 128];
        self.delay = [0.0; 128];
        self.pending_len = 0;
    }

    fn note_on(&mut self, key: u8, vel: f32, out: &mut NoteBuffer) {
        let key = key.min(127);
        let i = key as usize;
        // Tirage toujours fait : une probabilité de 0 écarte tout, de 1 garde tout.
        self.dropped[i] = self.rng.next_f32() >= self.values.get(P_PROBABILITY);
        if self.dropped[i] {
            return;
        }
        let spread = self.values.get(P_VELOCITY) * (2.0 * self.rng.next_f32() - 1.0);
        let vel = (vel * (1.0 + spread)).clamp(0.01, 1.0);
        let delay = (self.rng.next_f32() * self.values.get(P_TIMING) * self.sample_rate) as f64;
        self.delay[i] = delay;
        self.send(delay, NoteEvent::on(key, vel), out);
    }

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer) {
        let i = key.min(127) as usize;
        if std::mem::replace(&mut self.dropped[i], false) {
            return;
        }
        self.send(self.delay[i], NoteEvent::off(key.min(127)), out);
    }

    fn advance(&mut self, frames: u32) {
        for (at, _) in &mut self.pending[..self.pending_len] {
            at.advance(frames);
        }
    }

    fn tick(&mut self, out: &mut NoteBuffer) {
        // Dans l'ordre d'arrivée : un début et sa fin dus ensemble restent dans l'ordre.
        let mut i = 0;
        while i < self.pending_len {
            let (at, ev) = self.pending[i];
            if at.due() {
                out.push(ev);
                self.pending.copy_within(i + 1..self.pending_len, i);
                self.pending_len -= 1;
            } else {
                i += 1;
            }
        }
    }

    fn next_event(&self) -> Option<u32> {
        self.pending[..self.pending_len].iter().map(|(at, _)| at.wait()).min()
    }
}

#[cfg(test)]
mod tests {
    use super::super::NoteEvent;
    use super::super::testing::{make, offs, ons, play};

    fn notes(count: u64) -> Vec<(u64, NoteEvent)> {
        let mut v = Vec::new();
        for n in 0..count {
            v.push((n * 1_000, NoteEvent::on(60 + (n % 12) as u8, 0.8)));
            v.push((n * 1_000 + 500, NoteEvent::off(60 + (n % 12) as u8)));
        }
        v
    }

    #[test]
    fn probability_zero_drops_everything_one_keeps_everything() {
        let mut fx = make("random");
        fx.set_param(0, 0.0);
        assert!(play(&mut *fx, &notes(200), 200_000).is_empty());
        let mut fx = make("random");
        let log = play(&mut *fx, &notes(200), 200_000);
        assert_eq!(ons(&log).len(), 200);
        assert_eq!(offs(&log).len(), 200);
    }

    #[test]
    fn half_probability_keeps_about_half_and_never_orphans_an_off() {
        let mut fx = make("random");
        fx.set_param(0, 0.5);
        let log = play(&mut *fx, &notes(1_000), 1_000_000);
        let (on, off) = (ons(&log), offs(&log));
        assert!((400..600).contains(&on.len()), "{}", on.len());
        assert_eq!(on.iter().map(|n| n.1).collect::<Vec<_>>(), off.iter().map(|n| n.1).collect::<Vec<_>>());
    }

    #[test]
    fn timing_delays_note_and_keeps_its_length() {
        let mut fx = make("random");
        fx.set_param(2, 0.01); // Jusqu'à 480 échantillons.
        let log = play(&mut *fx, &notes(100), 100_000);
        let (on, off) = (ons(&log), offs(&log));
        assert_eq!(on.len(), 100);
        assert!(on.iter().any(|&(t, _)| t % 1_000 != 0), "au moins une note retardée");
        for (n, (&(t_on, _), &(t_off, _))) in on.iter().zip(&off).enumerate() {
            assert!((n as u64 * 1_000..=n as u64 * 1_000 + 480).contains(&t_on));
            assert_eq!(t_off - t_on, 500);
        }
    }

    #[test]
    fn velocity_spread_stays_in_range() {
        let mut fx = make("random");
        fx.set_param(1, 0.5);
        let log = play(&mut *fx, &notes(200), 200_000);
        let vels: Vec<f32> = log.iter().filter(|(_, e)| e.on).map(|(_, e)| e.vel).collect();
        assert!(vels.iter().all(|v| (0.39..=1.0).contains(v)));
        assert!(vels.iter().any(|&v| (v - 0.8).abs() > 0.05));
    }
}
