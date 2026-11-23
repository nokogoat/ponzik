# Feature inventory: FL Studio and Ableton Live

Reference: FL Studio 2025.2 and Ableton Live 12.3 (Suite), as of late 2025.
Each row says where the feature comes from and how far along Ponzik is.

Legend: **FL** = FL Studio · **AB** = Ableton Live · ✅ done in Ponzik · 🟡 partial · ⬜ to do

---

## 1. Views and workflow

| Feature | Where | Ponzik |
|---|---|---|
| Channel Rack: one instrument per channel plus a row of steps | FL | ✅ 16 steps |
| Step sequencer: variable pattern length, swing, per-step velocity, pan and pitch (graph editor) | FL | 🟡 variable length in the engine, swing, wheel sets step velocity; no graph editor |
| Multiple patterns, pattern picker, "make unique", cloning | FL | ✅ |
| Playlist: free tracks mixing pattern, audio and automation clips | FL | 🟡 patterns, audio and automation in separate lanes |
| Multiple arrangements in one project | FL | ⬜ |
| Session view: clip grid, scenes, quantized launching | AB | ✅ 8 × 8 |
| Arrangement view: linear timeline, tracks, recording the Session into the Arrangement | AB | 🟡 playlist |
| Clip View / Device View (clip and effect chain details at the bottom of the screen) | AB | ✅ resizable bottom device panel with a strip of device chips (jump, drag to reorder, bypass, detach, presets), collapsible cards, an effect inserted between two cards, Ctrl+arrows from device to device, keyboard focus (Tab, arrows, Enter); clip panel for the selected clip |
| Performance mode: launch Playlist clips live | FL | ⬜ |
| Browser: samples, presets, plugins, projects, favorites, tags, collections | FL · AB | 🟡 instruments, effects, CLAP, VST3, LV2 and LADSPA plugins (format badge, a folder per format), WAV and FLAC files in a collapsible folder tree, search, favorites, recent items, drag and drop; no presets or collections |
| Similar-sound search in the browser | AB | ⬜ |
| Plugin picker, plugin database | FL | 🟡 CLAP, VST3, LV2 and LADSPA plugins listed in the browser, scanned in child processes with a cache |
| Detachable windows, multiple monitors | FL · AB | 🟡 devices in stacked floating windows inside the main window; no multi-monitor |
| Contextual help bar (hint bar / Info View) | FL · AB | 🟡 tooltips everywhere; no permanent help bar |
| Built-in lessons (Live's Lessons) | AB | ✅ Learn panel: 12 optional hands-on lessons in two series (using Ponzik, first productions), bubbles on the real widgets, steps that end when you do the action, saved progress |
| Customizable themes and colors | FL | ✅ 7 themes (one light), CSS theme files with live reload, visual editor with contrast check ([THEMES.md](THEMES.md)) |
| Markers: sections, loop, time signature, punch in/out | FL · AB | 🟡 start marker, loop over a time selection; no section markers |
| Full undo history | FL · AB | ✅ with a history panel to jump back to any step |
| Metronome, count-in, tap tempo | FL · AB | ✅ |
| Computer keyboard played like a piano | FL · AB | ✅ |
| Tempo and time signature changes within a song | FL · AB | ⬜ |
| Touch support | FL | ⬜ |

## 2. MIDI editing and piano roll

| Feature | Where | Ponzik |
|---|---|---|
| Piano roll: draw, paint, erase, cut, select, mute, zoom | FL · AB | ✅ |
| Note properties: velocity, release, pan, fine pitch, mod X/Y, channel, color | FL | 🟡 velocity lane |
| Slide notes and portamento | FL | ⬜ |
| Ghost notes from other patterns or channels | FL | ✅ |
| Scale highlighting and snapping, key detection | FL · AB | 🟡 project key and scale, out-of-scale rows dimmed, snap to scale; no key detection |
| One-click chords (chord stamps), expressive chords | FL · AB | ✅ chord stamps that follow the scale, chord grid with shapes, inversions, voicings and keyboard preview |
| Tools: arpeggiate, strum, flam, flip, chop, glue, articulate, humanize, quantize | FL | ✅ arpeggiate, strum, flam, legato, staccato, reverse, invert, stretch, glue, quantize (strength, swing), humanize, fit to scale; no chop |
| MIDI Transformations: Arpeggiate, Connect, Ornament, Quantize, Recombine, Span, Strum, Time Warp, Velocity Shaper | AB | 🟡 arpeggiate, connect (legato), ornament (flam), quantize, strum, time stretch, with a live preview |
| MIDI Generators: Rhythm, Seed, Shape, Stacks, Euclidean | AB | ✅ Euclidean, rhythm by style, melody by contour (shape), chord progressions (stacks), bassline following the chords; seeded with re-roll |
| Riff Machine and piano roll Python scripts | FL | ⬜ |
| Note probability and random velocity range | AB | 🟡 chance, pass conditions and ratchets per note and per step; no velocity range |
| MPE editing (per-note pressure, glide, slide) | AB | ⬜ |
| Editing several clips at once | AB | ⬜ |
| MIDI Capture (recover what you played without recording) | AB | ✅ last 5 minutes kept, Ctrl+Shift+C: tempo detected, never quantized |
| Score logger (recover what you played) | FL | ⬜ |
| Tuning systems (microtonal) | AB | ⬜ |
| Event editor and controller lanes | FL · AB | ⬜ |
| MIDI import and export | FL · AB | ✅ MIDI controller input, Standard MIDI File import and export |

## 3. Audio: recording, editing, warping

| Feature | Where | Ponzik |
|---|---|---|
| Audio and MIDI recording, track arming, monitoring | FL · AB | ✅ notes from the keyboard and MIDI, audio on mixer tracks with monitoring |
| Loop takes, take lanes, comping | FL · AB | ⬜ |
| Audio clips: fades, gain, reverse, per-clip pitch and stretch, per-clip editing | FL · AB | 🟡 position, length, waveform; fixed fades |
| Warp: markers, Beats, Tones, Texture, Re-Pitch, Complex, Complex Pro modes | AB | ⬜ |
| High-quality time stretching and pitch shifting | FL · AB | ⬜ |
| Transient detection, slicing to MIDI or a Drum Rack | FL · AB | ⬜ |
| Audio to MIDI conversion (drums, melody, harmony) | AB | 🟡 melody: Voice to Notes plays the selected instrument live from singing or humming (YIN tracker on the input thread, about 12 ms reaction, scale snap, retune bends, chords through note effects, recorded into the pattern) and an audio clip's monophonic line converts to a pattern; no drums or harmony |
| Stem separation (vocals, drums, bass, instruments) | FL · AB | ⬜ |
| Destructive audio editor (Edison: recording, cleanup, cutting, noise reduction) | FL | ⬜ |
| Pitch correction (Newtone, Pitcher) | FL | ⬜ |
| Time correction (NewTime) | FL | ⬜ |
| Groove pool, groove extraction and application | AB | ⬜ |
| Freeze, flatten, consolidate, bounce to a new track, paste bounced audio, group bounce | FL · AB | ✅ freeze channels and mixer tracks, bounce in place or to a new track, consolidate clips, render a time selection to a new track; no flatten |
| Resampling (record the master output) | AB | ⬜ |
| Video in the timeline | AB | ⬜ |

## 4. Native instruments

Proprietary instruments cannot be copied, so Ponzik builds its own equivalents.

### FL Studio

| Instrument | Type | Ponzik equivalent |
|---|---|---|
| 3x Osc, SimSynth, MiniSynth, Sawer | Simple subtractive | 🟡 Pulse |
| Sytrus | FM + additive + subtractive | ✅ Ponzik FM |
| Harmor, Harmless | Additive, image and audio resynthesis | ⬜ |
| FLEX, GMS, Kepler, Kepler Exo | Rompler, modern synths | 🟡 Ponzik Wave |
| Toxic Biohazard, Sakura, PoiZone, Morphine, Ogun | Assorted synths (FM, physical strings, additive) | ⬜ |
| Transistor Bass, BooBass, Fruity Kick, Drumaxx, Fruity DrumSynth Live | 303-style bass, kick, modeled drums | ✅ Acid 303, Kick, DrumSynth |
| FPC, Drumpad | Drum pads | ✅ Drum Machine (eight layered pads, hi-hat choke) |
| DirectWave, channel sampler | Multisample sampler | ✅ Sampler |
| Slicex, Fruity Slicer 2 | Loop slicing | ✅ Slicer |
| Fruity Granulizer | Granular | ✅ Granular |
| FL Keys, Plucked! | Piano, plucked strings | ✅ Electric Piano, Tine Piano, Reed Piano, Tonewheel Organ (with rotary speaker), Plucked Strings |
| Autogun | FM preset player | ⬜ |
| Layer, Patcher | Instrument stacking, modular chains | ⬜ |
| Loop Starter | Generates genre loops in the Channel Rack | ⬜ |

### Ableton Live

| Instrument | Type | Ponzik equivalent |
|---|---|---|
| Analog, Drift | Subtractive | 🟡 Pulse |
| Operator | 4-operator FM | ✅ Ponzik FM |
| Wavetable | Wavetable | ✅ Ponzik Wave |
| Meld | Macro oscillators, MPE | ⬜ |
| Collision, Tension, Electric | Physical modeling (percussion, strings, electric piano) | 🟡 Mallets (modal marimba, vibes, xylophone, glockenspiel, celesta, kalimba, chimes, steel drum), Tine Piano and Reed Piano (modeled pickups), Plucked Strings |
| Simpler, Sampler | Simple and multisample sampler | ✅ Sampler |
| Drum Rack, Drum Sampler, Impulse | Pad-based drums | 🟡 DrumSynth, Drum Machine |
| Granulator III | Granular | ✅ Granular |
| Instrument Rack | Instrument stacking, macros | ⬜ |
| External Instrument, CV Instrument | Hardware control | ⬜ |

## 5. Native effects

| Family | FL Studio | Ableton Live | Ponzik |
|---|---|---|---|
| EQ | Parametric EQ 2, 7 Band EQ | EQ Eight, EQ Three, Channel EQ | ✅ EQ (interactive curve, spectrum behind the master EQ) |
| Compression | Compressor, Multiband Compressor, Maximus, Limiter | Compressor, Glue Compressor, Multiband Dynamics, Limiter, Gate | ✅ compressor, bus compressor (clean, opto, VCA), multiband (OTT-style upward and downward), limiter, gate, ducking |
| Mastering and limiting | Emphasis, Emphasizer, Maximus, Soundgoodizer | Limiter, Glue Compressor | ✅ Mastering Limiter (true peak, lookahead, three algorithms), bus compressor, multiband |
| Transients | Transient Processor | Drum Buss | ✅ Transients |
| Saturation | Fast Dist, Blood Overdrive, Distructor, Hardcore, Squeeze, Waveshaper | Saturator, Roar, Overdrive, Pedal, Amp, Cabinet, Dynamic Tube, Erosion, Redux, Vinyl Distortion | ✅ Saturator, Character Saturation (tube, tape, transistor, rectifier), Bitcrusher, Noise and Texture |
| Reverb | Reeverb 2, LuxeVerb, Convolver | Reverb, Hybrid Reverb, Convolution Reverb | ✅ Reverb, Algorithmic Reverb (hall, plate, cloud, freeze), Shimmer; no convolution |
| Delay | Delay 3, Delay Bank, Multiband Delay | Delay, Echo, Filter Delay, Grain Delay, Align Delay | ✅ tempo-synced Delay, Character Delay (tape, analog, digital) |
| Modulation | Chorus, Vintage Chorus, Flanger, Flangus, Phaser, Vintage Phaser | Chorus-Ensemble, Phaser-Flanger, Auto Pan-Tremolo | ✅ Chorus, Phaser, Pump |
| Filters | Love Philter, Filter, Free Filter | Auto Filter, Spectral Resonator, Resonators, Corpus | ✅ AutoFilter |
| Pitch and frequency | Pitch Shifter, Frequency Shifter, Pitcher, Newtone | Shifter, Auto Shift | ✅ Pitch Shifter |
| Rhythmic effects | Gross Beat, Scratcher | Beat Repeat, Looper | ✅ Slice |
| Spectral | – | Spectral Time | ⬜ |
| Vocoder | Vocodex, Vocoder | Vocoder | ⬜ |
| Stereo and utilities | Stereo Shaper, Stereo Enhancer, Balance, Center, Phase Inverter, Mute 2, Frequency Splitter | Utility, Surround Panner | ✅ Utility (with phase inversion), Stereo Imager |
| Analysis | Wave Candy, Spectroman, dB Meter | Spectrum, Tuner | 🟡 oscilloscope, VU meters, master spectrum and LUFS; tuner in the DSP crate only |
| Modulation controllers | Peak Controller, Formula Controller, X-Y Controller, Envelope Controller, Keyboard Controller | LFO, Envelope Follower, Shaper, Expression Control | ⬜ |
| Effect chains | Patcher, Effector | Audio Effect Rack (parallel chains, 16 macros, variations, device A/B) | ⬜ |
| MIDI effects | – | Arpeggiator, Chord, Note Length, Pitch, Random, Scale, Velocity, Note Echo, Melodic Steps, Rotating Rhythm, MPE Control, CC Control | 🟡 per-channel note effect chain: Arpeggiator, Chord, Scale, Random, Repeat |
| Visuals | ZGameEditor Visualizer, Video Player | – | ⬜ |

## 6. Mixing and routing

| Feature | Where | Ponzik |
|---|---|---|
| Mixer tracks: volume, pan, mute | FL · AB | ✅ |
| Solo, arming, phase inversion, stereo separation | FL · AB | ✅ solo, record arming, phase inversion and width through Utility and Stereo Imager |
| Peak meters | FL · AB | ✅ |
| RMS / LUFS meters | FL · AB | 🟡 RMS on the output, integrated and short-term LUFS on the master |
| Per-track effect inserts (10 slots in FL) | FL · AB | ✅ 8 inserts |
| Dynamic mixer tracks (up to 500 in FL) | FL | ✅ master plus up to 999 tracks: add, insert, delete, move by dragging, route a channel to a new track, compact strips |
| Sends and return tracks | FL · AB | ✅ 4 sends, reverb and delay return buses in one click |
| Free routing from any track to any other | FL | ✅ with loop rejection |
| Track groups, buses and VCAs | FL · AB | ✅ one-gesture buses (Ctrl+G), return buses with an effect ready, foldable groups, VCA faders |
| Sidechain | FL · AB | ✅ key track per insert (compressor, gate, Ducking) |
| Plugin delay compensation (PDC), track delay | FL · AB | 🟡 PDC on tracks, sends and sidechain keys, for native, CLAP, VST3, LV2 and LADSPA effects; no manual track delay |
| Multiple inputs and outputs, multi-output plugins | FL · AB | ⬜ |
| Crossfader, cue (pre-listen) | AB | ⬜ |
| Track and chain presets | FL · AB | ⬜ |

## 7. Automation and modulation

| Feature | Where | Ponzik |
|---|---|---|
| Automation clips with curves (FL) / automation lanes (AB) | FL · AB | ✅ song lanes with curves |
| Live automation recording | FL · AB | ✅ Touch, Latch and Write; transport and per-lane arm; one undo step per pass |
| Clip envelopes (clip-relative modulation) | AB | ⬜ |
| Mappable macros, macro variations, randomization | AB | ⬜ |
| Linking a parameter to a MIDI controller (MIDI learn), keyboard and MIDI mapping mode | FL · AB | 🟡 MIDI learn on every knob, fader, send, macro, plugin parameter and transport button; absolute, relative (encoding detected), toggle, range, pickup; global or per-project links. No keyboard mapping |
| Modulators (LFOs, envelopes, followers) patched to any parameter | FL · AB | ⬜ |

## 8. Live and performance

| Feature | Where | Ponzik |
|---|---|---|
| Quantized clip and scene launching | AB | ✅ to the bar |
| Follow actions (automatic clip chaining) | AB | ⬜ |
| Launch modes (trigger, gate, toggle, repeat), legato | AB | ⬜ |
| Looper | AB | ⬜ |
| Ableton Link sync | AB | ⬜ |
| MIDI clock sync | FL · AB | ⬜ |
| Tempo following from an audio input | AB | ⬜ |

## 9. Plugins, formats, integration

| Feature | Where | Ponzik |
|---|---|---|
| VST2, VST3, AU hosting | FL · AB | 🟡 VST3 instruments and effects; no VST2 or AU |
| CLAP, LV2, LADSPA hosting | – (planned in Ponzik) | ✅ CLAP instruments and effects; LV2 instruments, effects and MIDI effects (atoms, worker, state, presets, embedded X11 and Windows interfaces); LADSPA effects |
| 32/64-bit bridging, plugins sandboxed in a separate process | FL · AB | ⬜ |
| WAV export | FL · AB | ✅ 16-bit, 24-bit, 32-bit float |
| MP3, FLAC, OGG export, dithering, normalization | FL · AB | 🟡 FLAC (16 or 24 bits, songs, stems, sections), peak or LUFS normalization, TPDF dither at 16 bits; no MP3 or OGG |
| Stem export (one track per file) | FL · AB | ✅ |
| Video export | FL · AB | ⬜ |
| MIDI controller scripts (Python in FL, Remote Scripts in AB) | FL · AB | ⬜ |
| Dedicated hardware integration (Push, Move / FL Key, FLkey) | FL · AB | ⬜ |
| Max for Live (device-building environment) | AB | ⬜ |
| Mobile version and mobile project import (FL Studio Mobile, Ableton Note) | FL · AB | ⬜ |
| `.als`, `.flp`, DAWproject import | – (planned in Ponzik) | 🟡 opens `.flp`, `.als`, REAPER `.rpp`, LMMS `.mmp`/`.mmpz`, Ardour `.ardour` and MusicXML `.musicxml`/`.mxl` (tempo map, tracks, MIDI and audio clips, markers, simple routing; devices not converted); saves `.flp`, `.als` and `.rpp` copies; no DAWproject, Studio One or Bitwig yet ([details](IMPORT_EXPORT.md)) |

## 10. Projects and files

| Feature | Where | Ponzik |
|---|---|---|
| Saving and loading projects | FL · AB | ✅ JSON, recent projects, a file dialog (places, path bar, filter, sorting), save as `.flp`, `.als` or `.rpp`, unsaved-changes prompt, reopen the last project or a start screen at launch |
| Autosave, backups | FL · AB | ✅ autosave every two minutes, crash recovery |
| Project templates | FL · AB | ✅ built-in templates (Ctrl+N) |
| "Collect all and save" (copy samples into the project) | FL · AB | ⬜ |
| Sound packs, bundled library | FL · AB | ⬜ |
| Instrument and effect presets | FL · AB | ✅ device presets |

## 11. Assisted tools and services

| Feature | Where | Ponzik |
|---|---|---|
| Built-in AI assistant (Gopher) | FL | ⬜ |
| Genre loop generator (Loop Starter) | FL | ⬜ |
| Stem separation | FL · AB | ⬜ |
| Splice integration (search for samples that fit the project) | AB | ⬜ out of scope: proprietary service |
| FL Cloud: samples, online mastering, distribution | FL | ⬜ out of scope: paid service; a free library could be an alternative |

---

## What Ponzik has today

A real-time engine; in-house ALSA and WASAPI drivers for playback and capture;
MIDI input; in-house windowing and rendering. Channel Rack with swing, piano roll
with scale tools, chord stamps, chord grid, humanize and velocity lane, playlist
(patterns, audio, automation, make unique, time selection duplication, loops that
repeat when their edge is dragged, loop to the end of the song or the selection,
repeat ×2 ×4 ×8, paint brush), a sampler that fits a loop to the pattern in tempo
sync, values typed by hand for every knob, Session
view, a mixer of up to 1,000 tracks (inserts, sends, routing, solo, sidechain, PDC, master
LUFS and spectrum), devices with visuals in stacked floating windows. 12 native
instruments (Pulse, DrumSynth, FM, Wave, Sampler, Acid 303, Kick, Slicer,
Granular, Plucked Strings, Electric Piano, Drum Machine), 26 native effects (EQ,
compressor, bus compressor, limiter, mastering limiter, gate, OTT-style multiband, pump,
ducking, transients, reverb, algorithmic reverb, shimmer, delay,
character delay, chorus, phaser, saturator, character saturation, bitcrusher,
noise and texture, autofilter, slice, pitch shifter, stereo imager, utility) and
5 note effects (arpeggiator, chord, scale, random, repeat). CLAP, VST3, LV2 and LADSPA hosting.
JSON projects with templates, autosave and crash recovery, undo with a history
panel, MIDI file import and export, audio recording, count-in and tap tempo, WAV
export (16/24/32-bit float) and FLAC export (16/24-bit) with normalization and
dither, stems, and freeze, bounce in place and consolidate.
