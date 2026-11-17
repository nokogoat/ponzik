//! Warped audio clips: the stretch of each clip is rendered off the audio
//! thread, once per set of parameters, and kept in a process-wide cache.
//!
//! Why render ahead instead of stretching on the audio thread: Beats needs
//! the transients of the whole clip and Tones long FFT frames; ahead of time
//! both run at full quality with no lookahead, no latency and no risk of a
//! dropout, and the audio thread just reads a buffer. A render depends only
//! on the file, the warp settings and the tempo map *under the clip, counted
//! from its start*: moving a clip in a steady tempo never renders again.
//! While a render is in progress the engine plays the file resampled along
//! the tempo map, so the clip stays in time (its pitch follows the tempo for
//! a moment) instead of drifting off the grid.
//!
//! The interface renders in a background thread and looks the cache up
//! ([`cached`]); exports render what is missing synchronously
//! ([`render_cached`]): both get the very same buffer, so an export sounds
//! exactly like playback.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ponzik_dsp::onset::{OnsetAnalysis, mono};
use ponzik_dsp::stretch::resample;
use ponzik_dsp::tempo::{estimate_bpm, snap_to_loop};
use ponzik_dsp::warp::{WarpAlgo, warp_stereo};
use ponzik_engine::{PPQ, Sample, TempoMap, TempoPoint, WarpMap};

use crate::AudioClipDef;

/// Everything a warped clip's render depends on.
#[derive(Clone, Debug)]
pub struct WarpJob {
    pub key: u64,
    pub sample: Arc<Sample>,
    pub sample_rate: f32,
    pub algo: WarpAlgo,
    pub pitch: f64,
    pub map: WarpMap,
    pub offset: f64,
    pub len: u32,
    /// The song tempo from the clip start (tick 0 = clip start).
    pub tempo: TempoMap,
    pub looped: bool,
    pub reverse: bool,
}

/// The tempo map under `[start, start + len]`, shifted to start at 0. Ramps
/// that cross the edges keep their slope.
pub fn local_tempo(map: &TempoMap, start: u32, len: u32) -> TempoMap {
    let view = map.view();
    let end = start.saturating_add(len);
    let mut points = vec![TempoPoint {
        tick: 0,
        bpm: view.bpm_at(start as f64) as f32,
        ramp: false,
    }];
    for p in map.tempos().iter().filter(|p| p.tick > start) {
        points.push(TempoPoint {
            tick: p.tick - start,
            ..*p
        });
        if p.tick >= end {
            break;
        }
    }
    TempoMap::new(points, Vec::new())
}

struct Hasher(u64);

impl Hasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
}

/// Cheap identity of a sample's content: its size, rate and a few samples.
fn fingerprint(h: &mut Hasher, s: &Sample) {
    h.u64(s.len() as u64);
    h.f64(s.sample_rate as f64);
    let len = s.len();
    for k in 0..64 {
        let i = k * len / 64;
        if let (Some(l), Some(r)) = (s.left.get(i), s.right.get(i)) {
            h.u64(((l.to_bits() as u64) << 32) | r.to_bits() as u64);
        }
    }
}

/// True when the warped clip sounds just like its file at its own speed: no
/// render needed.
fn plays_as_is(clip: &AudioClipDef, tempo: &TempoMap) -> bool {
    let steady = tempo.tempos().len() == 1 && (tempo.first_bpm() - clip.warp_bpm()).abs() < 1e-6;
    steady && clip.warp_markers.is_empty() && (clip.pitch_ratio() - 1.0).abs() < 1e-9
}

impl WarpJob {
    /// Render needed by a clip in a song with this tempo map, if any (warp on,
    /// Beats or Tones, and not already at its own tempo and pitch).
    pub fn for_clip(
        clip: &AudioClipDef,
        sample: &Arc<Sample>,
        map: &TempoMap,
        sample_rate: f32,
    ) -> Option<WarpJob> {
        if !clip.warp
            || clip.warp_mode == WarpAlgo::RePitch
            || sample.is_empty()
            || sample_rate <= 0.0
        {
            return None;
        }
        let tempo = local_tempo(map, clip.start, clip.len);
        if plays_as_is(clip, &tempo) {
            return None;
        }
        let warp = clip.warp_map();
        let mut h = Hasher::new();
        fingerprint(&mut h, sample);
        h.f64(sample_rate as f64);
        h.u64(clip.warp_mode as u64);
        h.f64(clip.pitch_ratio());
        h.f64(warp.bpm);
        for m in warp.markers() {
            h.f64(m.secs);
            h.f64(m.beat);
        }
        h.f64(clip.offset);
        h.u64(clip.len as u64);
        for p in tempo.tempos() {
            h.u64(p.tick as u64);
            h.f64(p.bpm as f64);
            h.u64(p.ramp as u64);
        }
        h.u64(clip.looped as u64 | (clip.reverse as u64) << 1);
        Some(WarpJob {
            key: h.0,
            sample: sample.clone(),
            sample_rate,
            algo: clip.warp_mode,
            pitch: clip.pitch_ratio(),
            map: warp,
            offset: clip.offset,
            len: clip.len,
            tempo,
            looped: clip.looped,
            reverse: clip.reverse,
        })
    }

    /// Samples of the render: the clip's duration at the output rate.
    pub fn out_len(&self) -> usize {
        (self.tempo.secs_at(self.len as f64) * self.sample_rate as f64)
            .ceil()
            .max(0.0) as usize
    }

    /// The stretched clip, at `sample_rate`, from the clip start.
    pub fn render(&self) -> Sample {
        let s = &*self.sample;
        let right = if s.right.len() == s.len() {
            &s.right
        } else {
            &s.left
        };
        let (mut left, mut right) = (s.left.clone(), right.clone());
        if self.reverse {
            left.reverse();
            right.reverse();
        }
        let sr = self.sample_rate as f64;
        let factor = sr / s.sample_rate.max(1.0) as f64;
        if (factor - 1.0).abs() > 1e-9 {
            (left, right) = (resample(&left, factor), resample(&right, factor));
        }
        let view = self.tempo.view();
        let origin = self.map.beat_at(self.offset);
        let map = |k: f64| {
            let beat = origin + view.tick_at_secs(k / sr) / PPQ as f64;
            self.map.secs_at(beat) * sr
        };
        let (l, r) = warp_stereo(
            &left,
            &right,
            self.sample_rate,
            self.out_len(),
            &map,
            self.looped,
            self.algo,
            self.pitch,
        );
        Sample {
            name: s.name.clone(),
            sample_rate: self.sample_rate,
            left: l,
            right: r,
            markers: Vec::new(),
        }
    }
}

struct Entry {
    sample: Arc<Sample>,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<u64, Entry>,
    clock: u64,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Cache::default))
}

/// The render of a job, if it is done.
pub fn cached(key: u64) -> Option<Arc<Sample>> {
    with_cache(|c| {
        c.clock += 1;
        let clock = c.clock;
        c.entries.get_mut(&key).map(|e| {
            e.used = clock;
            e.sample.clone()
        })
    })
}

/// The render of a job, computed now if needed (outside the lock).
pub fn render_cached(job: &WarpJob) -> Arc<Sample> {
    if let Some(s) = cached(job.key) {
        return s;
    }
    let sample = Arc::new(job.render());
    with_cache(|c| {
        c.clock += 1;
        let used = c.clock;
        c.entries
            .entry(job.key)
            .or_insert(Entry { sample, used })
            .sample
            .clone()
    })
}

/// Forgets the renders nobody needs: keeps `wanted` and the `spare` most
/// recently used others (an undo finds them again).
pub fn prune(wanted: &[u64], spare: usize) {
    with_cache(|c| {
        let mut others: Vec<(u64, u64)> = c
            .entries
            .iter()
            .filter(|(k, _)| !wanted.contains(k))
            .map(|(k, e)| (e.used, *k))
            .collect();
        others.sort_unstable_by(|a, b| b.cmp(a));
        for (_, k) in others.into_iter().skip(spare) {
            c.entries.remove(&k);
        }
    })
}

/// Original tempo of a file, for warping: the tempo of its rhythm (first
/// minute), exact when the file is a whole number of bars.
pub fn detect_tempo(sample: &Sample) -> Option<f32> {
    let sr = sample.sample_rate.max(1.0);
    let n = sample.len().min((60.0 * sr) as usize);
    let right = if sample.right.len() == sample.len() {
        &sample.right
    } else {
        &sample.left
    };
    let m = mono(&sample.left[..n], &right[..n]);
    let analysis = OnsetAnalysis::new(&m, sr);
    let bpm = estimate_bpm(&analysis, n as f32 / sr)?;
    Some(snap_to_loop(bpm, sample.duration()))
}

/// True when the file lasts a whole number of bars at `bpm` (a loop: worth
/// warping right away).
pub fn is_loop(sample: &Sample, bpm: f32) -> bool {
    let bars = sample.duration() * bpm / 240.0;
    bars >= 0.99 && (bars - bars.round()).abs() < 0.01
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_dsp::tuner::Tuner;

    const SR: f32 = 48_000.0;

    fn tone(hz: f32, secs: f32) -> Arc<Sample> {
        let left: Vec<f32> = (0..(secs * SR) as usize)
            .map(|i| 0.5 * (i as f32 * hz * std::f32::consts::TAU / SR).sin())
            .collect();
        Arc::new(Sample {
            name: "tone".into(),
            sample_rate: SR,
            right: left.clone(),
            left,
            markers: Vec::new(),
        })
    }

    fn clip(bpm: f32, len: u32) -> AudioClipDef {
        AudioClipDef {
            path: "tone.wav".into(),
            len,
            warp: true,
            bpm,
            warp_mode: WarpAlgo::Tones,
            ..Default::default()
        }
    }

    fn pitch_of(x: &[f32]) -> f32 {
        let mut tuner = Tuner::new(SR, 2048);
        let mut found = Vec::new();
        for chunk in x[x.len() / 5..x.len() * 4 / 5].chunks(1024) {
            tuner.push(chunk);
            found.extend(tuner.detect().map(|p| p.hz));
        }
        found.sort_by(f32::total_cmp);
        found[found.len() / 2]
    }

    #[test]
    fn render_length_follows_the_tempo_ratio() {
        let s = tone(440.0, 4.0);
        // 8 beats of a 120 BPM file in a 150 BPM song: 3.2 s instead of 4, same pitch.
        for mode in [WarpAlgo::Tones, WarpAlgo::Beats] {
            let c = AudioClipDef { warp_mode: mode, ..clip(120.0, 8 * PPQ) };
            let job = WarpJob::for_clip(&c, &s, &TempoMap::constant(150.0), SR).expect("job");
            let out = job.render();
            assert_eq!(out.len(), (3.2 * SR) as usize);
            let hz = pitch_of(&out.left);
            assert!((hz / 440.0 - 1.0).abs() < 0.01, "{mode:?}: {hz}");
        }
    }

    #[test]
    fn transpose_shifts_the_pitch_only() {
        let s = tone(440.0, 4.0);
        let c = AudioClipDef {
            transpose: 7.0,
            fine: -10.0,
            ..clip(120.0, 8 * PPQ)
        };
        let job = WarpJob::for_clip(&c, &s, &TempoMap::constant(100.0), SR).expect("job");
        let out = job.render();
        assert_eq!(out.len(), (4.8 * SR) as usize);
        let want = 440.0 * 2f32.powf(6.9 / 12.0);
        let hz = pitch_of(&out.left);
        assert!((hz / want - 1.0).abs() < 0.01, "{hz} instead of {want}");
    }

    #[test]
    fn repitch_plays_faster_and_higher() {
        use crate::{Project, RenderMode, render_song};
        // A 100 BPM file in a 125 BPM song: 1.25 times faster, a major third up.
        let mut p = Project::empty();
        p.bpm = 125.0;
        p.mode = ponzik_engine::PlayMode::Song;
        let s = tone(440.0, 4.0);
        p.audio_clips.push(AudioClipDef { warp_mode: WarpAlgo::RePitch, track: 2, ..clip(100.0, 4 * PPQ) });
        let loader = |_: &str| Some(s.clone());
        let out = render_song(&p, SR as u32, &loader, RenderMode::Song);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        // 4 beats at 125 BPM: 1.92 s.
        let heard = left.iter().rposition(|v| v.abs() > 1e-3).unwrap_or(0);
        assert!((heard as f32 / SR - 1.92).abs() < 0.01, "{}", heard as f32 / SR);
        let hz = pitch_of(&left[..heard]);
        assert!((hz / 550.0 - 1.0).abs() < 0.01, "{hz}");
    }

    #[test]
    fn keys_ignore_where_a_steady_clip_sits() {
        let s = tone(440.0, 2.0);
        let map = TempoMap::constant(140.0);
        let a = WarpJob::for_clip(&clip(120.0, 384), &s, &map, SR).unwrap();
        let b = WarpJob::for_clip(
            &AudioClipDef {
                start: 960,
                ..clip(120.0, 384)
            },
            &s,
            &map,
            SR,
        )
        .unwrap();
        assert_eq!(a.key, b.key);
        // A different tempo, length or mode is another render.
        for other in [
            WarpJob::for_clip(&clip(120.0, 384), &s, &TempoMap::constant(141.0), SR),
            WarpJob::for_clip(&clip(120.0, 480), &s, &map, SR),
            WarpJob::for_clip(
                &AudioClipDef {
                    warp_mode: WarpAlgo::Beats,
                    ..clip(120.0, 384)
                },
                &s,
                &map,
                SR,
            ),
        ] {
            assert_ne!(other.unwrap().key, a.key);
        }
        // At its own tempo, or in Re-Pitch, nothing to render.
        assert!(WarpJob::for_clip(&clip(140.0, 384), &s, &map, SR).is_none());
        assert!(
            WarpJob::for_clip(
                &AudioClipDef {
                    warp_mode: WarpAlgo::RePitch,
                    ..clip(120.0, 384)
                },
                &s,
                &map,
                SR
            )
            .is_none()
        );
    }

    #[test]
    fn local_tempo_keeps_ramps_across_the_edges() {
        let map = TempoMap::new(
            vec![
                TempoPoint {
                    tick: 0,
                    bpm: 100.0,
                    ramp: false,
                },
                TempoPoint {
                    tick: 1_000,
                    bpm: 200.0,
                    ramp: true,
                },
            ],
            Vec::new(),
        );
        let local = local_tempo(&map, 400, 200);
        let view = map.view();
        for t in [0.0, 100.0, 200.0] {
            let want = view.secs_at(400.0 + t) - view.secs_at(400.0);
            assert!((local.secs_at(t) - want).abs() < 1e-9);
        }
    }

    #[test]
    fn cache_returns_the_same_render() {
        let s = tone(220.0, 1.0);
        let job = WarpJob::for_clip(&clip(120.0, 192), &s, &TempoMap::constant(90.0), SR).unwrap();
        let a = render_cached(&job);
        assert!(Arc::ptr_eq(&a, &cached(job.key).unwrap()));
        assert!(Arc::ptr_eq(&a, &render_cached(&job)));
    }

    #[test]
    fn detects_loops() {
        let s = tone(220.0, 2.0);
        assert!(is_loop(&s, 120.0));
        assert!(!is_loop(&s, 100.0));
    }
}
