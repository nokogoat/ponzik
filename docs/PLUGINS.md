# Plugins

Ponzik hosts **CLAP**, **VST3**, **LV2** and **LADSPA** plugins (not VST2 or AU),
with their own interface in a window where the format has one, their full state
saved in your project, and their latency compensated. It looks for them in the
standard folders:

| Platform | CLAP | VST3 |
|---|---|---|
| Linux | `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |

| Platform | LV2 (`$LV2_PATH` first if set) | LADSPA (`$LADSPA_PATH` first if set) |
|---|---|---|
| Linux | `~/.lv2`, `/usr/local/lib/lv2`, `/usr/lib/lv2`, `/usr/lib64/lv2` | `~/.ladspa`, `/usr/local/lib/ladspa`, `/usr/lib/ladspa`, `/usr/lib64/ladspa` |
| Windows | `%APPDATA%\LV2`, `%COMMONPROGRAMFILES%\LV2` | `%COMMONPROGRAMFILES%\LADSPA` |
| macOS | `~/Library/Audio/Plug-Ins/LV2`, `~/.lv2`, `/Library/Audio/Plug-Ins/LV2` | `~/Library/Audio/Plug-Ins/LADSPA`, `/Library/Audio/Plug-Ins/LADSPA` |

Each plugin file or bundle is loaded once in a separate process (with a time
limit), so a broken plugin can't take Ponzik down; the results are cached in
`~/.config/ponzik/plugins.cache` and only new or changed files are loaded again.
Failures, and the plugins of a bundle Ponzik cannot host, are listed at the bottom
of the plugin browser with the reason in their tooltip. Run **Rescan plugins**
(Ctrl+K) after installing new ones. Native Linux plugins such as
[Surge XT](https://surge-synthesizer.github.io/), [Vital](https://vital.audio/),
[Calf](https://calf-studio-gear.org/) and the [x42 plugins](https://x42-plugins.com/)
work out of the box.

In the browser's plugin tab each plugin carries its format badge (CLAP, VST3, LV2,
LADSPA) and sits in a folder per format under Instruments and Effects; typing the
format in the search box (`lv2`, `ladspa`) lists only those.

## LV2

Ponzik reads the bundles' Turtle files itself and loads the plugin libraries
directly (no lilv, no external library).

- **Ports**: audio, control (bounds, default, toggled, integer, enumeration with
  scale points, logarithmic, sample-rate bounds, units dB, Hz, kHz, s, ms,
  semitones, cents), atom sequences carrying MIDI, CV (connected to silence). A
  latency output port feeds the delay compensation; a `time:beatsPerMinute` port
  follows the project tempo; a sidechain input (`lv2:isSideChain`) gets the
  insert's sidechain key.
- **Features**: `urid:map` and `urid:unmap`, `options:options` (sample rate, block
  lengths, sequence size), `buf-size` (bounded block length; fixed and power of
  two block lengths run on 256-sample blocks through a FIFO, which adds 256
  samples of latency), `log:log`, `worker:schedule` (the plugin's work runs on a
  thread of its own, requests and answers go through lock-free queues), and
  `state:interface` with `state:loadDefaultState`.
- **Kinds**: instruments (`lv2:InstrumentPlugin`, or MIDI in and audio out without
  audio in), effects, and MIDI effects (MIDI in and out, no audio), which appear in
  a channel's note effects menu.
- **State**: control values by port symbol and the properties stored through
  `state:interface` are saved in the project.
- **Presets**: the plugin's `pset:Preset` presets from every installed bundle are
  read with their port values.
- **Interfaces**: `ui:X11UI` on Linux, `ui:WindowsUI` on Windows and `ui:CocoaUI`
  on macOS are embedded in a plugin window through `ui:parent`, driven by
  `ui:idleInterface`, sized with `ui:resize`, with `ui:touch`, `ui:portMap`,
  `instance-access` and `data-access`. Control values and atom messages travel
  both ways (the x42 meters and equalizer displays work). Every LV2 plugin also
  gets Ponzik's own knobs on its device card.
- **Not supported**: GTK and Qt interfaces (Calf's, for example: the knobs remain),
  external-window interfaces (`kx:Widget`), the obsolete event and URI map
  extensions, `state:mapPath` / `state:makePath` (files referenced by a state are
  kept as absolute paths), `lv2_lib_descriptor` libraries, time position atoms,
  and CV signals. A plugin requiring a feature Ponzik lacks, or with no audio
  output and no MIDI output, is listed as skipped with the feature named.

An LV2 plugin is saved in a project by its URI (`lv2:<uri>`), so a project finds
it again wherever the bundle is installed.

## LADSPA

LADSPA libraries hold effects only; each plugin is shown with Ponzik's knobs, built
from its port hints (bounded, toggled, integer, logarithmic, sample rate and the
default hints). A control output named `latency` feeds the delay compensation. The
state of a LADSPA plugin is its parameter values, saved with the project. Plugins
without an audio output are skipped.

## Real-time safety

`run`, port connections and parameter changes never allocate or lock on the audio
thread: port buffers, atom sequences and queues are made when the plugin is
created, parameters are written into the control port values, and MIDI goes into a
preallocated atom sequence (see the `*_alloc` tests of `ponzik-lv2` and the
allocation test of `ponzik-ladspa`).

### Windows plugins on Linux (Serum, ZENOLOGY…)

Windows-only plugins run on Linux through [Wine](https://www.winehq.org/) and
[yabridge](https://github.com/robbert-vdh/yabridge):

1. Install Wine (Wine Staging recommended) and yabridge, e.g.
   `sudo pacman -S wine-staging yabridge yabridgectl`.
2. Install the plugin with its Windows installer: `wine <installer>.exe`, choosing
   the VST3 (or CLAP) format.
3. Bridge it:
   ```sh
   yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/VST3"
   yabridgectl sync
   ```
4. Start Ponzik or run **Rescan plugins**: the plugin appears with a VST3 or CLAP badge.

Run `yabridgectl sync` again after installing or updating a Windows plugin.
