# Architecture

| Crate | Role |
|---|---|
| `crates/ponzik-dsp` | Allocation-free DSP: oscillators, filters, delays, dynamics, oversampling, FFT, spectrum, loudness, pitch tracking, time-stretching, onset and tempo detection |
| `crates/ponzik-engine` | Real-time engine: sequencing, mixer with PDC and sidechain, automation, clips, Session, multi-core graph, instruments, effects, note effects, codecs (WAV, FLAC, MP3, Ogg Vorbis) |
| `crates/ponzik-project` | Project model and files, sync to the engine, offline rendering, undo, scales and harmony, generators, FL Studio import, sound library, presets |
| `crates/ponzik-audio` | Audio drivers: ALSA, WASAPI, CoreAudio, full-duplex |
| `crates/ponzik-midi` | MIDI drivers: ALSA rawmidi, WinMM, CoreMIDI, input and output |
| `crates/ponzik-window` | Windowing: X11 protocol with MIT-SHM, Win32, Cocoa; plugin windows, file drops, cursors |
| `crates/ponzik-ui` | 2D rendering: anti-aliased rasterizer, TrueType reader, animations, PNG |
| `crates/ponzik-i18n` | Translations (gettext-style catalogs embedded at build time) |
| `crates/ponzik-clap`, `crates/ponzik-vst3` | Plugin hosts, with test plugins in `*-test-plugin` |
| `apps/ponzik` | The application and its immediate-mode interface (`src/gui/`) |

The golden rule of the audio thread: **no allocations, no locks, no blocking**. The
interface edits the project; the difference is sent to the engine as commands, and
replaced objects travel back to the interface thread to be freed there.

## Platform support

| Platform | Window | Audio | MIDI |
|---|---|---|---|
| Linux | X11 protocol spoken directly, MIT-SHM, HiDPI (Wayland through XWayland) | ALSA (works with PipeWire and PulseAudio) | ALSA rawmidi, in and out |
| Windows | Win32, per-monitor DPI | WASAPI (low-latency shared mode) | WinMM, in and out |
| macOS (experimental) | Cocoa through the Objective-C runtime | CoreAudio | CoreMIDI |

The interface is in English and French; adding a language is one translation file
(`crates/ponzik-i18n/lang/`).
