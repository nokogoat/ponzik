# Ponzik

**The free DAW. No more gatekeeping.**

Ponzik is a free, open-source digital audio workstation that pairs the breadth of
FL Studio with the clarity and ergonomics of Ableton Live. It is built for
beginners and professionals alike: you can make a beat in thirty seconds, and the
full power is there when you go looking for it. Everything (audio and MIDI
drivers, windowing, rendering, fonts, engine, DSP, instruments, effects and
plugin hosting) is written from scratch in 100% Rust, with zero external crates.
It runs on Linux and Windows.

> The interface text is currently in French.

## Principles

- **Built in-house, 100% Rust, zero dependencies.** No frameworks, no crates. Ponzik
  talks only to the operating system's own APIs.
- **FL Studio's workflow, Ableton's clarity.** The same gestures and shortcuts as FL,
  with progressive disclosure so the interface never gets in your way.
- **Visuals, not walls of knobs.** An editable EQ curve, transfer curves, drawn
  envelopes and filters, waveforms, effect responses, spectrum and loudness meters.

## Highlights

- **Views**: Channel Rack (step sequencer with swing), Piano roll, Playlist
  (patterns, audio and automation), Ableton-style Session view (8 x 8 clip grid),
  Mixer. Devices open in stacked floating windows; the browser and bottom panel
  are resizable.
- **Composition tools**: project key and scale (out-of-scale rows dimmed, snap to
  scale), chord stamps and a chord grid (shapes, inversions, voicings, keyboard
  preview), humanize, ghost notes, velocity lane, copy/paste/duplicate/quantize.
- **Note effects** placed before the instrument: arpeggiator, chord, scale, random,
  repeat.
- **12 native instruments**: Pulse (subtractive), DrumSynth (808/909), Ponzik FM
  (4 operators), Ponzik Wave (wavetable), Sampler, Acid 303, Kick, Slicer,
  Granular, Plucked Strings, Electric Piano, Drum Machine (eight layered pads,
  hi-hat choke).
- **26 native effects**: EQ, Compressor, Bus Compressor, Multiband (OTT-style upward
  and downward), Limiter, Mastering Limiter (true peak, lookahead), Gate, Transients,
  Pump, Ducking, Reverb, Algorithmic Reverb, Shimmer, Delay, Character Delay,
  Chorus, Phaser, AutoFilter, Saturator, Character Saturation, Bitcrusher, Noise
  and Texture, Slice, Pitch Shifter, Stereo Imager, Utility.
- **Third-party plugins**: CLAP and VST3 instruments and effects, with their
  parameters in the device panel and their latency compensated.
- **Mixer**: master plus 32 tracks, 8 inserts and 4 sends per track, free routing
  with loop rejection, solo, sidechain key per insert, plugin delay compensation
  (PDC) on tracks, sends and keys, master LUFS meter and spectrum analyzer.
- **Recording**: computer keyboard played like a piano, hardware MIDI controllers,
  audio recording on mixer tracks with monitoring, one-bar count-in, tap tempo.
- **Projects**: JSON files, unlimited undo with a clickable history panel,
  templates (Ctrl+N), autosave every two minutes with crash recovery, recent
  projects, device presets, Standard MIDI File import and export.
- **Export**: WAV at 16-bit, 24-bit or 32-bit float, stems, peak or LUFS
  normalization, TPDF dither at 16 bits.
- **Demos**: `ponzik` (house groove) and `ponzik breakcore` ("Amen rave", 180 BPM).

## Build and run

You need a Rust toolchain (1.85 or newer, edition 2024). On Linux, the ALSA
library is also required (`alsa-lib`, or `libasound2-dev` on Debian and Ubuntu).

```sh
cargo run --release                                  # open the UI on the house demo
cargo run --release -- breakcore                     # open the UI on the breakcore demo
cargo run --release -- play [seconds]                # play the demo in the terminal
cargo run --release -- render demo.wav [morceau|pattern]   # render the demo (song or first pattern) to WAV
cargo run --release -- screenshot ui.png mixer       # off-screen UI capture
cargo test
```

Once installed, the same commands are available as `ponzik`, `ponzik breakcore`,
`ponzik render <out.wav>` and `ponzik screenshot <out.png> [view] [width height]`
(views: `rack`, `piano`, `playlist`, `mixer`).

Environment variables:

| Variable | Effect |
|---|---|
| `PONZIK_LATENCY_MS` | Audio output latency in milliseconds (2 to 100, default 10) |
| `CLAP_PATH` | Extra folders to scan for CLAP plugins |

## Platform support

| Platform | Window | Audio | MIDI |
|---|---|---|---|
| Linux | X11 protocol spoken directly, with MIT-SHM (Wayland through XWayland) | ALSA (works with PipeWire and PulseAudio) | ALSA rawmidi |
| Windows | Win32 | WASAPI | WinMM |

## Architecture

| Crate | Role |
|---|---|
| `crates/ponzik-dsp` | Allocation-free DSP building blocks: oscillators, envelopes, filters, biquads, delays, dynamics, oversampling, FFT, spectrum, BS.1770 loudness, YIN tuner |
| `crates/ponzik-engine` | Real-time engine: sequencing, mixer with PDC and sidechain, automation, audio clips, Session view, native instruments, effects and note effects, registry |
| `crates/ponzik-project` | Project model, JSON files, offline rendering, undo history, scales and chords, templates, autosave, MIDI files, demos |
| `crates/ponzik-audio` | In-house audio drivers: ALSA (Linux), WASAPI (Windows), playback and capture |
| `crates/ponzik-midi` | In-house MIDI input: ALSA rawmidi (Linux), WinMM (Windows) |
| `crates/ponzik-window` | In-house windowing: direct X11 protocol with MIT-SHM (Linux), Win32 (Windows) |
| `crates/ponzik-ui` | In-house 2D rendering: anti-aliased rasterizer, TrueType reader (glyf, cmap, GPOS), animations, PNG |
| `crates/ponzik-clap` | CLAP plugin host (audio, parameters, notes, latency) |
| `crates/ponzik-vst3` | VST3 plugin host (hand-written COM ABI, bundle loading, instruments and effects, latency) |
| `crates/ponzik-clap-test-plugin`, `crates/ponzik-vst3-test-plugin` | Minimal plugins used by the host integration tests |
| `apps/ponzik` | The application |

The golden rule of the audio thread: **no allocations, no locks, no blocking**. The
UI edits the project; the difference is sent to the engine as commands, and the
objects it replaces travel back to the UI thread to be freed there.

## Shortcuts (the FL Studio ones)

| Key | Action |
|---|---|
| Ctrl+K | Do-anything command palette: every action in a few keystrokes |
| Space | Play / stop (returns to the start marker) |
| F5 F6 F7 F9 | Playlist, Channel Rack, Piano roll, Mixer |
| Tab | Next view |
| L | Pattern / song mode |
| R | Arm recording |
| T | Tap tempo |
| F4 | New pattern |
| + / - | Next / previous pattern |
| Alt+F8 / Ctrl+F | Browser / search |
| Ctrl+T | Computer keyboard as a piano |
| Ctrl+M | Metronome |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+N | New project from a template |
| Ctrl+S / Ctrl+Shift+S / Ctrl+O | Save / save as / open |
| Ctrl+R | Export to WAV |
| Ctrl+C / Ctrl+V / Ctrl+B | Copy / paste / duplicate (notes, clips, time selection) |
| Ctrl+Q | Quantize |
| Hold right-click | Erase whatever you sweep over (steps, notes, clips) |
| Ruler: click / drag | Start marker / looping time selection |
| Right-click on a knob | Automate, reset to default |

## License

GPL-3.0-or-later. Ponzik is free software and always will be.
IBM Plex Sans and IBM Plex Mono fonts: SIL Open Font License (`crates/ponzik-ui/fonts/OFL.txt`).
