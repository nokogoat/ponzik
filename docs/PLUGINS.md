# Plugins

Ponzik hosts **CLAP** and **VST3** plugins (not VST2), with their own interface in a
window, their full state saved in your project, and their latency compensated. It
looks for them in the standard folders:

| Platform | CLAP | VST3 |
|---|---|---|
| Linux | `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |

Each plugin is loaded once in a separate process, so a broken plugin can't take
Ponzik down; failures are listed at the bottom of the plugin browser. Run
**Rescan plugins** (Ctrl+K) after installing new ones. Native Linux plugins such as
[Surge XT](https://surge-synthesizer.github.io/) and [Vital](https://vital.audio/)
work out of the box.

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
