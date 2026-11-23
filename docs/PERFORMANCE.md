# Performance

How Ponzik is measured, what was optimized and what to look at next.

## Measuring

```sh
cargo build --release
./target/release/ponzik bench zalgo 12        # or house, breakcore, empty, a project file
PONZIK_AUDIO_THREADS=0 ./target/release/ponzik bench zalgo 12   # single core, steadier numbers
PONZIK_SMART_DISABLE=0 ./target/release/ponzik bench zalgo 12   # every effect runs, as before
PONZIK_PROFILE=1 ./target/release/ponzik zalgo  # startup time, then per-frame timings every 2 s
PONZIK_BENCH=1 ./target/release/ponzik screenshot /tmp/x.png zg-mixer
```

`ponzik bench` renders the project offline in blocks of 240 frames (5 ms at
48 kHz, the default ALSA period), first stopped then playing, and prints for
each phase the mean, median, p99 and worst block time against the 5 ms
budget, the mean once tails have died (after 3 s), the heaviest instruments,
tracks and inserts, the realtime factor, the time of one interface image per
view (full, tracked, idle) with its allocations, and the process memory.

Process CPU and memory come from `/proc/<pid>/stat` and `/proc/<pid>/status`
(VmRSS) of the running interface, sampled over 10 s.

## Before and after

Linux, i5-11400H (6 cores, 12 threads), PipeWire, 1600x960 window. The
machine was shared with other builds, so process CPU figures vary by a few
points between runs; `bench` medians are the steadier numbers.

| Measure | Before | After |
|---|---|---|
| Startup to first image, Zalgo demo | 710 to 930 ms | 105 to 125 ms |
| Startup to first image, empty project (`ponzik new`) | 29 ms | 28 ms |
| Sample loading, Zalgo demo | 640 to 700 ms | 6 to 55 ms |
| RSS, empty project, idle | 33 MiB | 31 to 33 MiB |
| RSS, Zalgo demo, idle | 84 MiB | 73 MiB |
| Process CPU, empty project, idle | 3.3 % | 1.1 % |
| Process CPU, Zalgo stopped, window open | 10 to 17 % | 2.3 % |
| Process CPU, Zalgo playing (playlist) | 18 to 23 % | 12 % |
| Audio block, Zalgo stopped (single core) | 0.477 ms (9.5 % of budget) | 0.046 ms (0.9 %) |
| Audio block, Zalgo playing (single core) | 0.556 ms (11.1 %) | 0.319 ms (6.4 %) |
| Offline render speed, Zalgo | 9.0x realtime | 15.7x realtime |
| Audio block, empty project | | 0.014 ms (0.3 %) |
| UI tracked frame, playlist / piano / mixer / rack (Zalgo) | 0.82 / 0.32 / 1.51 / 0.74 ms | unchanged |
| UI full redraw, playlist / piano / mixer / rack (Zalgo) | 3.9 / 2.7 / 5.2 / 3.3 ms | unchanged |
| Release binary | 10.11 MB | 10.16 MB |

The interface was already cheap: it sleeps until an event when nothing moves,
draws 30 images per second while playing (60 while something animates), and
rasterizes only the regions that changed. A test now guards that an untouched,
stopped interface stops asking for images in every view.

## What changed

- **Smart disable** (engine, on by default in the interface). An insert fed
  silence (under -94 dBFS) that has given silence back for 0.5 s plus its own
  tail (delay time, pre-delay, 2 s for CLAP and VST 3 plugins) is skipped until
  sound comes in again. Effects that make sound out of silence never sleep
  (their output is checked), keyed inserts never sleep, a replaced insert or a
  changed parameter wakes. This is most of the idle saving: the Zalgo demo had
  every reverb, saturator and limiter processing zeros.
- **Exact exports**: offline renders keep every effect running by default;
  Settings can let them sleep as live for faster exports.
- **Mixer**: peaks are computed with eight running maxima the compiler turns
  into SIMD; a track whose block is all zeros is not added to its
  destinations.
- **Startup**: project samples load on several threads, and library sounds
  named by relative path (the demos) are decoded from the library on disk
  instead of being synthesized again at each launch.
- **Eco mode** (Settings, off by default): after 3 s without input the
  interface draws playback at 15 images per second and ambient animations get
  no images of their own.
- **Settings > Performance**: live load and its last 24 seconds, the heaviest
  instruments and effects (sleeping ones marked with a moon), the audio buffer
  and the three switches. The top bar's load gauge opens it.

Already in place before this pass: denormals flushed on the audio thread
(`FlushDenormals`), adaptive worker threads, the dropout counter with
automatic buffer raise, damage-tracked rendering.

## What remains

- UI allocations: 200 to 730 per tracked frame (labels formatted each frame).
  They cost little next to drawing, but a string cache for labels that do not
  change would remove most.
- Heaviest DSP while playing the Zalgo demo: the wavetable pad, the vintage
  reverb, the character saturation (three 2x oversamplers in multiband mode)
  and the mastering limiter, about 1 % of a core each. Block-wise processing
  instead of per-sample parameter smoothing would help them.
- Instrument voice limits and stealing are per instrument; a global cap is
  not enforced.
- Idle engine cost (about 14 us per block for an empty project) is mostly the
  33 default tracks; tracks with no input and no inserts could be skipped
  entirely.
