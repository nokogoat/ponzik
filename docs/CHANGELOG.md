# Changelog

User-facing changes to Ponzik, newest first. The first section is what the
"What's new" window shows after an update. Every push to `main` is also
published as a nightly build; its release notes list the individual commits.

## 0.1.0 - 2026-11-23

The first public builds: a Linux AppImage and a Windows program, both able to
update themselves.

### Updates and downloads
- Ready-to-run builds on the GitHub Releases page: a Linux AppImage and a Windows `Ponzik.exe` (also as a portable zip).
- Ponzik checks for a newer build at startup, in the background, and offers "Install & restart" when one exists. Downloads are checked against their SHA-256 before anything is replaced.
- Settings › About shows your version and build, and lets you choose the channel (nightly or stable), automatic checks and automatic installs.
- This "What's new" window appears once after each update; tick it off for good, or open it again from the menu.

### New instruments
- A physically modeled Grand Piano with ten factory presets.
- Electric pianos (tine and reed, with pickup character and amp effects), a tonewheel organ with drawbars and rotary speaker, and modal mallets: vibraphone, marimba, glockenspiel and more.
- Modeled Guitar (steel, nylon, electric with amp, bass), bowed Strings from violin to double bass, and Brass and Winds, each with articulations and presets.
- Drum Pads: a 16-pad sample instrument with chokes, velocity layers, a waveform editor and a starter kit.
- External MIDI instrument, to play hardware synths from the piano roll.

### Effects
- Amp & Cab (tube preamp, tone stack, sagging power amp, modeled cabinet) and a Rotary speaker.
- Effect racks with parallel chains and macros; LFO, envelope follower, step and random modulators on tracks and instruments.
- Mix Assistant: it listens to a track and suggests a chain, without imposing anything.

### Recording and voice
- Voice to Notes: sing or hum and the selected instrument plays along; sung notes are recorded into the piano roll.
- Audio recording with full-duplex input, monitoring, latency calibration and stacked takes, with take lanes and comping in the playlist.
- MIDI capture (Ctrl+Shift+C): the phrase you just played becomes a pattern, even with the transport stopped.
- Live automation recording in Touch, Latch and Write modes, and MIDI learn on knobs, faders, sends, macros and transport.

### Composition and piano roll
- Slide notes, per-note pan and fine pitch, channel portamento, pitch bend and CC lanes.
- Per-note chance, pass conditions and ratchets; MIDI tools and seeded generators (melodies, basslines, Euclidean rhythms).
- Key detection and a chord track; a groove pool to apply the feel of one clip to another.
- Chord grid with shapes, inversions and voicings, and scales grouped by family.

### Arrangement, playlist and session
- Section markers, tempo changes with ramps, and time signatures; export each section to its own file.
- Warped, looped and reversed audio clips with Beats, Tones and Re-Pitch modes, fades and clip markers.
- Freeze, bounce in place, consolidate and render selection.
- Session view launch modes, quantization, follow actions and recording into the arrangement.
- Playlist lanes scroll vertically, up to 500 lanes.

### Mixer
- Add, insert, delete and move mixer tracks, up to 999.
- Buses, return buses and VCAs, with grouping and folding.
- A/B comparison on every device; CPU readouts per track and device.

### Plugins, import and export
- CLAP and VST3 plugins open in their own windows; their state is saved with the project and presets; plugins are scanned in separate processes so a broken one cannot crash Ponzik.
- Import FL Studio projects (.flp) with a clear report; offline files can be located and relinked in one go.
- MP3, Ogg Vorbis and FLAC samples; export songs, stems and sections as WAV or FLAC.

### Interface and sounds
- English by default, with a complete French translation.
- A bundled sound library generated on first launch, factory presets for every instrument, and a browser with favorites, tags and sound preview.
- A first-run tour: a beat, a bass and an export in a few minutes.
- Interface size from 75% to 200%, workspace profiles and remappable shortcuts.
- Lower latency and fewer dropouts: multi-core audio engine, real-time priority, and the interface only redraws what changed.
