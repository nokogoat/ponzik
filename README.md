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
- **Third-party plugins**: CLAP and VST3 instruments and effects, with their own
  interface in a window of its own (edits there are undoable and automatable),
  their parameters in the device panel and their latency compensated. Plugins are
  scanned in separate processes, so a broken one can't take Ponzik down.
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
macOS (experimental): Xcode command line tools only; CoreAudio, CoreMIDI and Cocoa
backends, not yet tested on real hardware.

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
| `VST3_PATH` | Extra folders to scan for VST3 plugins |

## Plugins

Ponzik hosts **CLAP** and **VST3** plugins (not VST2). It looks for them in the
standard folders, subfolders included:

| Platform | CLAP | VST3 |
|---|---|---|
| Linux | `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |

Each plugin file is loaded once by a child process (`ponzik scan-plugin <path>`)
with a timeout; the results are cached in `~/.config/ponzik/plugins.cache`, so
only new or updated plugins are loaded again. Plugins that crash or hang while
loading are listed at the bottom of the browser's plugin tab, with the reason
in the tooltip. After installing or fixing a plugin, run **Rescan plugins**
(command palette, Ctrl+K, or the button under the plugin list).

Click the window button next to a plugin's name in the device panel (or
double-click the name) to open its own interface. Native Linux plugins such as
[Surge XT](https://surge-synthesizer.github.io/) (`pacman -S surge-xt`,
`apt install surge-xt`) or [Vital](https://vital.audio/) work out of the box.

### Windows plugins on Linux (Serum, ZENOLOGY…) with Wine and yabridge

Plugins that only exist for Windows can run on Linux through
[Wine](https://www.winehq.org/) and [yabridge](https://github.com/robbert-vdh/yabridge),
which makes them look like native Linux plugins:

1. Install Wine (yabridge recommends Wine Staging) and yabridge, for example
   `sudo pacman -S wine-staging yabridge yabridgectl` on CachyOS, or the
   packages listed in the yabridge README for other distributions.
2. Install the plugin with its Windows installer through Wine:
   `wine <installer>.exe`. Pick the **VST3** (or CLAP) format; VST3 plugins
   usually land in `~/.wine/drive_c/Program Files/Common Files/VST3`.
   Plugins that need an online account (Roland Cloud for ZENOLOGY, for instance)
   also need their manager app installed and logged in through Wine.
3. Tell yabridge where they are and create the Linux bridges:
   ```sh
   yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/VST3"
   yabridgectl sync
   ```
   yabridge writes the bridged plugins to `~/.vst3/yabridge` and
   `~/.clap/yabridge`, which Ponzik scans already.
4. Start Ponzik (or run **Rescan plugins**): the plugins appear in the browser
   with a VST3 or CLAP badge. The first load of a bridged plugin starts Wine and
   can take a few seconds.

Run `yabridgectl sync` again after installing, updating or removing a Windows
plugin, then rescan. `yabridgectl status` shows what is bridged; if a plugin
fails to load, its reason shows in the browser and in yabridge's own logs
(`YABRIDGE_DEBUG_LEVEL=1`).

## Platform support

| Platform | Window | Audio | MIDI |
|---|---|---|---|
| Linux | X11 protocol spoken directly, with MIT-SHM (Wayland through XWayland) | ALSA (works with PipeWire and PulseAudio) | ALSA rawmidi |
| Windows | Win32 | WASAPI | WinMM |
| macOS (experimental) | Cocoa through the Objective-C runtime | CoreAudio (AUHAL) | CoreMIDI |

## Architecture

| Crate | Role |
|---|---|
| `crates/ponzik-dsp` | Allocation-free DSP building blocks: oscillators, envelopes, filters, biquads, delays, dynamics, oversampling, FFT, spectrum, BS.1770 loudness, YIN tuner |
| `crates/ponzik-engine` | Real-time engine: sequencing, mixer with PDC and sidechain, automation, audio clips, Session view, native instruments, effects and note effects, registry |
| `crates/ponzik-project` | Project model, JSON files, offline rendering, undo history, scales and chords, templates, autosave, MIDI files, demos |
| `crates/ponzik-audio` | In-house audio drivers: ALSA (Linux), WASAPI (Windows), playback and capture |
| `crates/ponzik-midi` | In-house MIDI input: ALSA rawmidi (Linux), WinMM (Windows) |
| `crates/ponzik-window` | In-house windowing: direct X11 protocol with MIT-SHM (Linux), Win32 (Windows), Cocoa (macOS); plugin host windows |
| `crates/ponzik-ui` | In-house 2D rendering: anti-aliased rasterizer, TrueType reader (glyf, cmap, GPOS), animations, PNG |
| `crates/ponzik-clap` | CLAP plugin host (audio, parameters, notes, latency, `clap.gui` with timer and fd support) |
| `crates/ponzik-vst3` | VST3 plugin host (hand-written COM ABI, bundle loading, instruments and effects, latency, `IPlugView` with the Linux run loop) |
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
