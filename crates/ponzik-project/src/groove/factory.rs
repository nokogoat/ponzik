//! Built-in grooves: the feels beatmakers reach for. Values are in ticks
//! (a sixteenth is 24, so at 90 BPM a tick lasts about 7 ms) and velocity and
//! length ratios around 1.

use ponzik_i18n::tr;

use super::Groove;

/// Swing amounts of the MPC grooves, in percent.
pub const SWINGS: [u8; 7] = [50, 54, 58, 62, 66, 71, 75];

/// A groove of `bars` bars of `steps` steps from repeating step lists
/// (shorter lists repeat, so a one-beat cell fills the bar).
fn cell(name: &str, steps: u32, bars: u32, timing: &[f32], velocity: &[f32], duration: &[f32]) -> Groove {
    let mut g = Groove::straight(name, steps, bars);
    let n = g.timing.len();
    for i in 0..n {
        g.timing[i] = timing[i % timing.len()];
        g.velocity[i] = velocity[i % velocity.len()];
        g.duration[i] = duration[i % duration.len()];
    }
    g
}

/// Every built-in groove, MPC swings first. Names are in the interface's
/// language (a groove keeps its name once in a project).
pub fn library() -> Vec<Groove> {
    let mut v: Vec<Groove> = SWINGS.iter().map(|&p| Groove::swing(p as f32)).collect();

    // J Dilla: drunk, uneven sixteenths, the snares dragged behind the beat,
    // a kick that pushes ahead on beat 3.
    v.push(cell(
        tr("Dilla late snares"),
        16,
        1,
        &[0.0, 8.0, 1.0, 10.0, 5.0, 9.0, 2.0, 11.0, -1.5, 7.0, 1.0, 10.0, 5.5, 10.0, 3.0, 12.0],
        &[1.15, 0.7, 0.9, 0.62, 1.1, 0.75, 0.95, 0.6, 1.12, 0.7, 0.88, 0.65, 1.15, 0.75, 0.9, 0.68],
        &[1.0, 0.85],
    ));
    // Blues/rock shuffle: off eighths late, the sixteenths between in step.
    v.push(cell(tr("Shuffle"), 16, 1, &[0.0, 6.0, 12.0, 6.0], &[1.05, 0.6, 0.9, 0.6], &[1.0, 0.8, 0.9, 0.8]));
    // Every eighth split as a triplet: the off eighth on the third triplet.
    v.push(cell(tr("Triplet feel"), 16, 1, &[0.0, 8.0, 16.0, 8.0], &[1.1, 0.8, 0.95, 0.8], &[1.0]));
    // Dembow: 3 + 3 + 2, the "and" of each figure pushed and accented.
    v.push(cell(
        tr("Reggaeton dembow"),
        16,
        1,
        &[0.0, 0.0, 0.0, 3.0, 0.0, 0.0, -2.0, 1.0],
        &[1.15, 0.7, 0.8, 1.08, 0.95, 0.7, 1.05, 0.72],
        &[1.0, 1.0, 1.0, 0.8, 1.0, 1.0, 0.75, 1.0],
    ));
    // UK garage 2-step: a heavy sixteenth swing, snares a hair late, the
    // second bar's skipping kick later still.
    let mut garage = cell(
        tr("UK garage 2-step"),
        16,
        2,
        &[0.0, 7.0, 0.0, 7.0, 1.5, 7.0, 0.0, 8.0],
        &[1.1, 0.7, 0.9, 0.62, 1.05, 0.72, 0.85, 0.58],
        &[1.0, 0.75],
    );
    garage.timing[16 + 10] = 4.0;
    garage.velocity[16 + 10] = 1.0;
    garage.timing[16 + 15] = 9.0;
    v.push(garage);
    // Trap hi-hats on thirty-seconds: the sixteenths and thirty-seconds a
    // touch late, eighths loud, rolls rising into the next beat.
    v.push(cell(
        tr("Trap hi-hat swing"),
        32,
        1,
        &[0.0, 2.0, 3.0, 2.5, 0.0, 2.0, 3.5, 3.0],
        &[1.2, 0.55, 0.85, 0.6, 1.05, 0.65, 0.75, 0.85],
        &[1.0, 0.7, 0.9, 0.7],
    ));
    // Drum and bass two-step: a light swing, the snares pushed ahead,
    // ghost notes soft.
    let mut dnb = cell(tr("Drum and bass"), 16, 2, &[0.0, 2.0, 0.0, 2.5], &[1.0, 0.6, 0.8, 0.6], &[1.0, 0.8]);
    for bar in 0..2 {
        for (s, t, vel) in [(4, -1.5, 1.2), (12, -1.0, 1.2), (10, 0.5, 1.05), (0, 0.0, 1.15)] {
            dnb.timing[bar * 16 + s] = t;
            dnb.velocity[bar * 16 + s] = vel;
        }
    }
    v.push(dnb);
    // A drummer: beats a little ahead, backbeats behind, everything a bit
    // uneven over two bars (fixed values, so the groove never changes).
    let mut human = Groove::straight(tr("Human drummer"), 16, 2);
    let wobble = [0.0, 2.5, -1.0, 3.0, 2.0, 1.5, -0.5, 3.5, -1.0, 2.0, 0.5, 2.5, 2.5, 1.0, -1.5, 3.0];
    let accents = [1.12, 0.78, 0.92, 0.7, 1.08, 0.82, 0.88, 0.74, 1.05, 0.76, 0.95, 0.72, 1.1, 0.8, 0.86, 0.7];
    for i in 0..32 {
        let k = (i * 7 + i / 16 * 3) % 16;
        human.timing[i] = wobble[i % 16] * 0.8 + (wobble[k] - 1.0) * 0.4;
        human.velocity[i] = accents[i % 16] * (0.96 + 0.08 * (k as f32 / 15.0));
        human.duration[i] = 0.92 + 0.16 * ((k * 5) % 16) as f32 / 15.0;
    }
    v.push(human);
    for g in &mut v {
        g.normalize();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_complete_and_sane() {
        let lib = library();
        assert!(lib.len() >= 15);
        for g in &lib {
            assert_eq!(g.timing.len(), (g.steps * g.bars()) as usize, "{}", g.name);
            assert_eq!(g.velocity.len(), g.timing.len());
            assert!(g.depth() <= 24.0, "{}", g.name);
            assert!(g.velocity.iter().all(|v| (0.3..=1.5).contains(v)), "{}", g.name);
        }
        assert!(lib.iter().any(|g| g.steps == 32));
        assert!(lib.iter().any(|g| g.bars() == 2));
        // Every name is distinct.
        for (i, a) in lib.iter().enumerate() {
            assert!(lib[i + 1..].iter().all(|b| b.name != a.name), "{}", a.name);
        }
    }
}
