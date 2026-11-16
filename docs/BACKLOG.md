# Product backlog

November 2026. Benchmarked against FL Studio 2025.2, Ableton Live 12.3, Bitwig
Studio 5.3, Logic Pro 11, Reaper 7, Studio One 7, Cubase 14, and the most widely
used plugins (Serum 2, Vital, Pro-Q 4, Pro-C, Pro-L, Valhalla, Soundtoys, OTT,
Kickstart, Gross Beat, Melodyne, Ozone).

**How to use this document.** Work through the open Wave 2 items in order. Each
row gives the size (**S** ≤ 1 hour, **M** half a day to 2 days, **L** a week or
more), the areas touched (**engine** = `ponzik-engine`, **dsp** = `ponzik-dsp`,
**project** = `ponzik-project`, **ui** = `apps/ponzik/src/gui`, **window** =
`ponzik-window`), what it depends on, and its status (✅ done · 🟡 partial ·
⬜ to do).

**Status.** Wave 1 is complete apart from the permanent help bar (tooltips
already cover every widget). From Wave 2, sidechain, plugin delay compensation
and the Drum Machine have shipped; from Wave 3, the VST3 host and floating device
windows. `FEATURES.md` is up to date with all of this.

---

## 1. Interface rules: Ableton's calm, FL's power

FL Studio is dense: floating windows, every plugin with its own style, lots of
small unlabeled buttons. Live and Bitwig feel calm because they have a single
window, a single visual grammar, and a bottom panel that always shows the details
of the current selection. These are the rules to follow, and to check at every UI
review:

1. **One selection, one detail panel.** Whatever is selected (channel, clip,
   track, note) shows up in the bottom panel (devices or clip). Devices can be
   detached into floating windows, but the docked panel stays the default.
2. **A permanent help bar.** On hover, it shows the name, a one-sentence
   explanation and the shortcut. Every widget must have help text: this is
   Ableton's Info View and FL's hint bar.
3. **Progressive disclosure.** Each device first shows 4 to 8 macros alongside its
   visual, with the rest under "Advanced". Parameter groups (`ParamInfo::group`)
   act as tabs. Nothing hidden is out of reach: the Ctrl+K palette finds
   everything.
4. **One color, one meaning.** The accent color is for selection and playback, red
   for recording and clipping, the channel color for its identity. No decorative
   gradients.
5. **Icons and labels together.** An icon alone is only enough for universal
   gestures (play, record, loop, metronome). Everywhere else, add a word or a
   tooltip.
6. **FL's gestures, the same everywhere.** Left-click places, held right-click
   erases, the wheel adjusts, Ctrl+drag copies, Shift or Ctrl gives fine
   precision, and double-click resets to the default. All of this must behave the
   same way in every view.
7. **Everything from the keyboard.** Every action is in the palette, with its
   shortcut shown. Arrow keys and Enter must be enough to navigate.
8. **No blocking modals.** Undo replaces confirmation: delete right away and show
   "Undone: …" with Ctrl+Z.
9. **Drag and drop everywhere.** From the browser to the rack, the playlist, an
   insert or a pad; from the OS into the window; from one track to another.
10. **Every number has a visual.** An audible parameter must have a curve, a
    waveform or an indicator that moves during playback.
11. **Defaults that sound good.** A freshly inserted device should already do
    something useful, and a new project should be ready to make a beat.
12. **Empty states that guide.** An empty view says in one sentence what to do,
    as the piano roll and the Session view already do.

---

## 2. Wave 1: the next 2 to 3 hours

The best value-for-effort items, almost all of them S. Each one is independent
unless stated otherwise.

| # | Item | Why | Size | Area | Depends on | Status |
|---|---|---|---|---|---|---|
| V1-1 | **Hover help bar**: at the bottom of the window, the name, description and shortcut of the hovered widget (registry descriptions for devices, `ParamInfo` for knobs) | The biggest readability win for beginners; rule 2 | S | ui (widgets.rs, app.rs) | – | 🟡 tooltips everywhere; no permanent bar |
| V1-2 | **Autosave and crash recovery**: writes `~/Musique/Ponzik/.autosave/<name>.ponzik` every 2 minutes if the project changed; at startup, offers to restore the file if it is newer | Losing a project drives a user away for good | S | project (io.rs), ui (compat.rs) | – | ✅ |
| V1-3 | **Project scale**: key and mode (major, minor, dorian, pentatonic…) in `Project`; out-of-scale rows dimmed in the piano roll; "snap to scale" option | The foundation of every harmony tool (Live 12, FL, Logic, Cubase) | S | project, ui (piano.rs) | – | ✅ |
| V1-4 | **One-click chords**: a piano roll tool that places triads, sevenths, sus or power chords on the scale, with inversion on the mouse wheel | Beginners' favorite feature (FL chord stamps, Cubase Chord Pads) | S | ui (piano.rs) | V1-3 (otherwise chromatic chords) | ✅ plus a chord grid with voicings |
| V1-5 | **Humanize and randomize**: adjustable random timing and velocity offsets on the selection (palette and piano roll menu) | Makes a beat less mechanical in one gesture | S | ui (piano.rs) | – | ✅ |
| V1-6 | **Tap tempo and count-in**: a TAP button (average of the last 4 taps); a one-bar count-in on the metronome before recording | Essential for recording and lining up a sample | S | engine (count-in in the transport), ui | – | ✅ |
| V1-7 | **History panel**: list of undo labels; one click returns to that state | `History` already stores the labels; Live, FL and Reaper all have it | S | project (history.rs: label iterator, `goto(n)`), ui | – | ✅ |
| V1-8 | **Drag and drop from the browser**: a WAV goes into a new Sampler channel or a playlist audio clip, an instrument into the rack, an effect into an insert slot | Rule 9; it is the natural gesture in both FL and Ableton | M | ui (app.rs: drag payload, drop targets) | – | ✅ |
| V1-9 | **LUFS metering on the master**: integrated, short-term, momentary, plus true peak (K-weighting = 2 biquads, 4x oversampled peak); shown next to the master meter | All released music targets a LUFS level; Ozone and Pro-L show it | S | dsp (K-weighting), engine (meter.rs), ui (mixer.rs) | – | ✅ |
| V1-10 | **Spectrum analyzer**: in-house radix-2 FFT, curve smoothed to 1/6 octave behind the EQ curve and in a master visual | EQ without a spectrum is tuning blind (Pro-Q, Live's Spectrum) | M | dsp (fft.rs), engine (lock-free analysis buffer), ui (visuals.rs) | – | ✅ |
| V1-11 | **Make unique** and **rename or color via right-click** for patterns and clips | The last ⬜ in the Patterns row; a basic FL gesture | S | project, ui (playlist.rs) | – | ✅ |
| V1-12 | **Project templates**: "Save as template" and "New from template" in the palette (`Modèles/` folder) | Starts a session in 2 seconds; FL, Live and Studio One all have it | S | ui (compat.rs, palette.rs) | – | 🟡 built-in templates (Ctrl+N, palette); no "Save as template" yet |
| V1-13 | **Normalization and dithering on export** (TPDF at 16 bits, peak or target LUFS normalization) | A clean export without going through another program | S | project (render.rs), dsp | V1-9 for the LUFS target | ✅ |
| V1-14 | **Duplicate the time selection** (Ctrl+B on the ruler: everything in the range is copied right after it) | The fastest way to extend an arrangement (Live: Cmd+D) | S | ui (playlist.rs), project | – | ✅ |

What is left in Wave 1: **the permanent help bar (V1-1)** and **"Save as
template" (V1-12)**.

---

## 3. Wave 2: the next few weeks

| # | Item | Why | Size | Area | Depends on | Status |
|---|---|---|---|---|---|---|
| V2-1 | **Sidechain**: every dynamics effect (Compressor, Gate, Pump, Multiband) can listen to another track | House and EDM pumping; nothing replaces it | M | engine (`Effect::process_sidechain`, routing order), ui | – | ✅ key track per insert for the compressor, gate and the new Ducking effect |
| V2-2 | **Plugin delay compensation (PDC)**: `Effect::latency()`, compensated delay on tracks and sends; latency also read from CLAP plugins | The lookahead limiter and CLAP plugins already shift the sound | M | engine (mixer.rs), clap | – | ✅ tracks, sends and sidechain keys; CLAP and VST3 latency; trimmed from exports |
| V2-3 | **Group tracks and VCAs**: a mixer track as a collapsible group, and a VCA fader that drives several tracks without the audio passing through it | Mixing drums or backing vocals (every DAW) | M | project, engine, ui (mixer.rs) | – | ✅ (buses come from the fixed 33 tracks; no nested VCAs, no group pre-listen) |
| V2-4 | **Dynamic mixer tracks**: replace the fixed `TRACKS = 33` with a variable count | FL 2025 goes up to 500 tracks; 33 limits large projects | M | engine, project, ui | – | ⬜ |
| V2-5 | **Device macros and racks**: a chain of stacked effects or instruments, 8 mappable macros with ranges, saved as a preset | The core of Ableton's "calm": 8 knobs instead of 80 | L | engine (effect rack), project, ui | – | ⬜ |
| V2-6 | **Modulators**: LFO, envelope, envelope follower, step, random, patched by dragging onto any parameter, tracks and sends included, with a per-target depth | Bitwig 5, Cubase 14; this is what brings sounds to life | L | engine (per-block modulation), dsp (lfo.rs), ui | V2-5 recommended | ⬜ |
| V2-7 | **MIDI learn**: right-click, "Link to controller", move a CC; the mapping table is saved in the project | A connected MIDI controller is currently only good for notes | S | engine, project, ui | – | ⬜ |
| V2-8 | **Live automation recording** (write, touch and latch modes) | To keep a gesture made by hand | M | engine, ui (automation.rs) | V2-7 helps | ⬜ |
| V2-9 | **Section markers and an arranger track**: verse, chorus…, which can be dragged to move the whole section (Studio One and Cubase Arranger) | Restructure a song in one gesture | M | project, ui (playlist.rs) | – | ⬜ |
| V2-10 | **Tempo and time signature changes**: a global tempo track, rendering and sequencing on a tempo map | Essential as soon as you leave the 4/4 loop | M | engine (sequence.rs), project | – | ⬜ |
| V2-11 | **Audio clip warping**: detected original tempo, time stretching (WSOLA for drums, phase vocoder for tonal sounds), per-clip transposition, warp markers | Fitting a loop to the tempo, the foundation of Live | L | dsp (stretch), engine (clip playback), ui | – | ⬜ |
| V2-12 | **Transient slicing to pads or MIDI**: the Slicer exports its slices into a note pattern, or an audio clip is sliced in the playlist | Flipping breaks (Slicex, Fruity Slicer 2, Simpler Slice) | M | engine (detection already in the Slicer), ui | – | ⬜ |
| V2-13 | **Drum pads** (a 16-pad rack, one sample and one chain per pad, chokes, drop a WAV onto a pad) | Replaces FPC and Drum Rack | M | engine (instrument), ui | V1-8 | 🟡 Drum Pads (`drum_pads`): 16 sample pads with volume, pan, tune, start, attack/decay, reverse, filter, four choke groups, velocity sensitivity and up to four velocity layers; drop a WAV on a pad (Shift: add a layer), pad menu with copy/paste, starter kit from the library or the Drum Machine. Missing: per-pad effect chains and per-pad mixer outputs (the engine has one stereo output per channel) |
| V2-14 | **Take comping**: loop recording, one lane per take, swipe to pick the parts to keep (Reaper 7, Logic) | Recorded vocals and instruments | L | engine (recording), project, ui | – | ⬜ |
| V2-15 | **Freeze, bounce in place, consolidate**: render a track or clip to audio, paste bounced audio (Live 12.3) | Save CPU and commit a sound | M | project (offline render.rs), ui | – | ⬜ |
| V2-16 | **MIDI capture**: a permanent buffer of played notes; Ctrl+Shift+C creates the pattern even without recording | Never lose a good improvised idea again (Live, FL Score logger) | S | engine (note ring buffer), ui | – | ⬜ |
| V2-17 | **MIDI transformations**: arpeggiate, strum, flam, legato, reverse, invert, stretch, glue | FL's piano roll tools and Live 12's Transformations | M | ui (piano.rs), project | V1-3 | ⬜ (real-time arpeggiator available as a note effect) |
| V2-18 | **MIDI generators**: Euclidean, rhythm, scale motif, a bassline that follows the chords (FL 2025's Bassline Generator) | Getting started when you are stuck | M | project (pure generation), ui | V1-3, V1-4 | ⬜ |
| V2-19 | **Per-step probability and ratchets** (ratchets, 1:2 and 3:4 conditions) in the Channel Rack and the piano roll | Living grooves in the style of Elektron, Live, Bitwig Stepwise | M | engine (sequence.rs), project, ui | – | ⬜ |
| V2-20 | **Dropping files from the OS**: XDND on X11, `WM_DROPFILES` on Win32, a new `Event::FileDrop` | Rule 9; grabbing samples from the file manager | M | window, ui | V1-8 | ⬜ |
| V2-21 | **Device A/B**: two states per device and a button to switch between them (Live 12.3, FabFilter plugins) | Comparing settings honestly | S | project, ui (devices.rs) | – | ⬜ |
| V2-22 | **Favorites, tags and sound preview in the browser**: preview on hover, in tempo, stars, category filters | Finding a sound fast is what keeps people on FL | M | ui, engine (preview voice) | – | ⬜ |
| V2-23 | **Clip panel**: gain, adjustable fades, reverse, transposition, loop and color of the selected clip, at the bottom of the screen (Live's Clip View) | Rule 1; fades are currently fixed | M | project, engine, ui | – | ⬜ |
| V2-24 | **Launch modes and follow actions** in the Session (trigger, gate, toggle, legato; next, random, with probability) | Live on stage, generative jams | M | engine (session), project, ui | – | ⬜ |
| V2-25 | **FLAC export** (in-house lossless encoder, fixed LPC prediction) | A free, lossless format, doable without dependencies | M | project | – | ⬜ |
| V2-26 | **Onboarding**: a first launch with 3 guided steps (make a beat, add a bass, export) and callouts on the real widgets | The "a beat in 30 seconds" promise | M | ui | V1-1 | ⬜ (the two essential gestures are shown at startup) |

---

## 4. Wave 3: long-term projects

| # | Item | Why | Size | Area | Depends on | Status |
|---|---|---|---|---|---|---|
| V3-1 | **Global chord track and instruments that follow it**: MIDI and audio clips transposed to match the chord (Logic 11, Studio One, Cubase) | Change the harmony of the whole song in one gesture | L | project, engine | V1-3, V2-17 | ⬜ |
| V3-2 | **Virtual musicians**: a drummer, bassist and keyboardist who play along with the chord track, tuned by complexity and intensity (Logic's Session Players) | A huge help for beginners | L | project (generation), ui | V3-1 | ⬜ |
| V3-3 | **Scratch pads**: an alternative arrangement in the same project (Studio One), multiple arrangements (FL) | Try out a structure without breaking anything | M | project, ui | V2-9 | ⬜ |
| V3-4 | **Project versions**: named snapshots, comparison and rollback, with continuous autosave | Replaces "project_final_v7_REAL.flp" | M | project, ui | V1-2 | ⬜ |
| V3-5 | **Pitch correction**: YIN detection, notes as editable blobs (pitch, vibrato, length, formants), an automatic mode tuned to the scale | Melodyne and Newtone; a must for vocals | L | dsp (PSOLA), engine, ui | V1-3, V2-11 | ⬜ (YIN tuner already in the DSP crate) |
| V3-6 | **Spectral editing**: a clip spectrogram, brush eraser and attenuation, noise reduction | Cleaning up takes (Edison, RX, SpectraLayers) | L | dsp (STFT), ui | V1-10 | ⬜ |
| V3-7 | **Audio to MIDI** (drums, melody, harmony) | Live does it; useful for reworking a sample | L | dsp | V3-5 | ⬜ |
| V3-8 | Local **stem separation** | Live 12.3, FL, Studio One 7 and Cubase 15 all have it. Without dependencies it needs a small hand-written neural network, which makes it the heaviest project | L | dsp | – | ⬜ |
| V3-9 | **Modular grid**: patch modules (oscillator, filter, envelope, logic, delay) to build your own instruments and effects (Bitwig's The Grid, FL's Patcher) | Unlimited power for sound designers | L | engine, ui | V2-6 | ⬜ |
| V3-10 | **MPE**: per-note pressure, glide and timbre, on input, in the piano roll and in the native instruments | Expressive controllers, Meld, Push 3 | L | midi, engine, project, ui | – | ⬜ |
| V3-11 | **Custom actions and command macros**: chain palette actions, name them, give them a shortcut (Reaper actions) | Pro workflows without scripting | M | ui (palette.rs) | – | ⬜ |
| V3-12 | **Region rendering**: one file per section marker, naming templates (Reaper) | Export an EP or a set of loops in one go | S | project (render.rs) | V2-9 | ⬜ |
| V3-13 | **Ableton Link and MIDI clock** (input and output) | Playing along with other machines | M | engine, midi (in-house UDP networking) | – | ⬜ |
| V3-14 | **VST3 and LV2 hosting**, plugins sandboxed in a separate process | Access to the whole plugin ecosystem | L | new crate | – | 🟡 VST3 host shipped (`ponzik-vst3`); no LV2 or sandboxing |
| V3-15 | **DAWproject import** (and partial `.als`) | Bring your projects over from elsewhere | L | project | – | ⬜ |
| V3-16 | **Master recording (resampling)** from the transport bar (Bitwig 5.3) | Keep an improvised jam | S | engine, ui | – | ⬜ |
| V3-17 | **Search everywhere**: the palette also searches presets, samples, channels, tracks and markers, and takes you there | Rule 7 | M | ui (palette.rs) | V2-22 | ⬜ |
| V3-18 | **Detachable windows, multiple monitors** | Pros on two screens | L | window, ui | – | 🟡 devices in stacked floating windows inside the main window; no separate OS windows |

---

## 4b. Ponzik's signature: the Pyramid (once parity is reached)

A creation mode unique to Ponzik, which gives the software its name: you build a
track **layer by layer, from the base up to the tip**.

- **Base**: rhythm and low end (kick, bass, groove), the foundation that holds
  everything up.
- **Middle layers**: harmony (chords, pads), then textures and percussion.
- **Tip**: melody, vocals, ornaments, whatever catches the ear.

Starting ideas:
- a pyramid view where each layer is a stratum (the channels and patterns that
  belong to it); the next layer only opens once the base sounds good, guiding a
  beginner without holding back a pro;
- each layer offers suitable generators (grooves, a bassline that follows the
  chords, progressions, melodies on the scale) and listens to the layers below it
  (key, chords, energy);
- the pyramid "unfolds" into an arrangement: removing or adding layers across
  sections gives you the intro, build-up, drop and break;
- visual: the pyramid lights up layer by layer during playback; a layer can be
  muted or soloed in one click.

Depends on: scale and chords (done), MIDI generators (V2-18), sections (V2-9).

## 5. Native plugins to build

The names are functional; the inspiration is in parentheses and must not appear
in the product.

| Plugin | Function | Inspired by | Wave | Status |
|---|---|---|---|---|
| **Spectrum** | FFT analyzer, comparison of two tracks, LUFS metering | Pro-Q (analyzer), SPAN, Youlean | 1 (V1-9, V1-10) | 🟡 master spectrum and LUFS; no standalone device or track comparison |
| **Dynamic EQ** | The current EQ's bands with a threshold, in dynamic or spectral mode (only the frequencies that exceed it), band solo, freehand curve drawing | Pro-Q 4 (Spectral Dynamics, EQ Sketch) | 2 | ⬜ |
| **Sidechain pump** | The current Pump with a drawn curve and an external input or kick detection | Kickstart, LFOTool | 2 (V2-1) | 🟡 Pump plus a sidechain Ducking effect; no drawn curve |
| **Volume and pitch chopper** | Two curves drawn per bar (volume, playback position), rewinds, slowdowns, rhythm presets | Gross Beat | 2 | ⬜ |
| **Bus compressor** | Glue and character: clean, opto and VCA modes, gain reduction meter with scrolling history | Pro-C 2, Glue | 2 | ✅ |
| **Mastering limiter** | Several algorithms (transparent, punchy, aggressive), true peak, LUFS target, level history | Pro-L 2, Emphasis | 2 | 🟡 three algorithms, true peak, lookahead; no LUFS target or level history |
| **Mastering assistant** | Analyzes the mix and suggests an EQ, multiband, imaging and limiter chain toward a genre target or a reference track, with 4 knobs | Ozone Master Assistant | 3 | ⬜ |
| **Extreme multiband** | An "OTT" mode for the current Multiband with a one-knob preset and per-band visual | OTT | 1 (preset) | ✅ the Multiband is OTT-style (upward and downward, depth control) |
| **Algorithmic reverb** | Hall, plate and diffusion cloud modes, rich modulation, very long tails | ValhallaVintageVerb, Supermassive | 2 | ✅ hall, plate, cloud, freeze |
| **Character delay** | Tape, analog, ping-pong, saturation in the feedback loop, wow and flutter, ducking | EchoBoy, Valhalla Delay | 2 | ✅ tape, analog, digital |
| **Character saturation** | Tube, tape, transistor, rectifier, multiband, with a mix control | Decapitator, Saturn 2 | 2 | ✅ |
| **Animated filter** | Envelope filter, drawn and step LFO, vowel and comb modes | FilterFreak, MicroShift | 2 | ⬜ |
| **Vocoder** | 16 to 32 bands, internal or sidechain carrier, formants | Vocodex, Live's Vocoder | 2 (V2-1) | ⬜ |
| **Pitch corrector** | Real-time automatic tuning to the scale, adjustable speed (the "T-Pain" effect at 0 ms) | Auto-Tune, Pitcher, Melodyne | 3 (V3-5) | ⬜ |
| **Wavetable synth 2** | Ponzik Wave extended: 3 oscillators + sub + noise, spectral, granular and multisample modes, modulation matrix, phase distortion, table import | Serum 2, Vital | 3 (V2-6) | ⬜ |
| **Modular drum synth** | Kick, snare, hi-hat in layers (transient + body + noise), a preset family per machine | Bitwig v8, v9 and v0 | 2 | ✅ Drum Machine (eight layered pads); no per-machine preset families yet |
| **Noise and texture** | Vinyl crackle, tape hiss, block bit loss, wow | RC-20 | 2 | ✅ |
| **Arpeggiator and MIDI effects** | Arpeggio, chords, scale, note echo, random, as MIDI devices placed before the instrument | Live's MIDI effects | 2 (V2-17) | ✅ arpeggiator, chord, scale, random, repeat |

---

## 6. Gap catalog by area

What Ponzik is still missing, and where the backlog covers it. Features Ponzik
already has are not listed here.

- **Composition**: transformations (V2-17), generators (V2-18), probability
  (V2-19), capture (V2-16), chord track (V3-1), virtual musicians (V3-2), MPE
  (V3-10), polyphonic pattern sequencer (Cubase 14, V2-19), microtonality
  (outside the waves).
- **Arrangement**: sections (V2-9), tempo and time signature (V2-10), scratch pads
  and multiple arrangements (V3-3), launch modes and follow actions (V2-24), FL's
  performance mode (part of V2-24).
- **Audio**: warping and stretching (V2-11), slicing (V2-12), comping (V2-14),
  freeze and bounce (V2-15), clip panel and fades (V2-23), pitch correction
  (V3-5), spectral editing (V3-6), audio to MIDI (V3-7), stems (V3-8), resampling
  (V3-16).
- **Mixing**: dynamic tracks (V2-4), mid/side in the EQ
  and compressor (Wave 2, as part of the dynamic EQ), cue and pre-listen (outside
  the waves).
- **Sound design**: macros and racks (V2-5), modulators (V2-6), A/B (V2-21),
  modular grid (V3-9).
- **Workflow and comfort**: permanent help bar (V1-1), "Save as template"
  (V1-12), dropping files from the OS (V2-20), MIDI learn (V2-7), recorded
  automation (V2-8), browser with favorites and preview (V2-22), onboarding
  (V2-26), versions (V3-4), actions (V3-11), search everywhere (V3-17), a smart
  tool (one tool that places, moves and resizes depending on the hovered zone:
  to be checked in every view).
- **Live**: follow actions (V2-24), Link and MIDI clock (V3-13), looper (outside
  the waves).
- **Plugins**: LV2 and sandboxed plugins (V3-14).
- **Export and sharing**: FLAC (V2-25), region rendering (V3-12), DAWproject
  (V3-15). MP3 and OGG need an in-house encoder: to do after FLAC.

---

## Sources

- Ableton Live 12.3: https://www.ableton.com/en/blog/live-12-3-is-coming/ · https://cdm.link/live-12-3-arrives/ · https://www.musicradar.com/music-tech/stem-separation-splice-integration-ableton-is-playing-catch-up-with-live-12-3-but-its-best-new-features-are-the-ones-hidden-beyond-the-headlines
- Ableton's Info View: https://www.ableton.com/en/live-manual/12/first-steps/
- FL Studio 2025: https://www.image-line.com/fl-studio-news/fl-studio-2025-whats-new-2 · https://www.image-line.com/fl-studio-news/history/fl-studio-2025-2 · https://www.attackmagazine.com/news/fl-studio-2025-adds-ai-support-and-workflow-improvements-in-annual-update/
- Bitwig Studio 5 and 5.3: https://musictech.net/news/gear/bitwig-studio-5-update-new-features/ · https://synthanatomy.com/2025/02/bitwig-studio-5-3-new-drum-devices-sequencer-background-recording-and-winter-sale.html
- Logic Pro 11: https://apple.com/newsroom/2024/05/logic-pro-takes-music-making-to-the-next-level-with-new-ai-features · https://www.soundonsound.com/techniques/logic-pro-session-players
- Studio One Pro 7: https://prnewswire.com/news-releases/presonus-introduces-the-daw-for-all-studio-one-pro-7-with-all-new-innovative-production-tools-302270757.html · https://www.soundonsound.com/reviews/presonus-studio-one-pro-7
- Cubase 14: https://www.musicradar.com/music-tech/daws/make-composing-producing-and-mixing-more-inspirational-than-ever-steinberg-launches-cubase-14-with-new-pattern-editor-drum-machine-effects-plugins-modulators-and-more
- Reaper 7: https://www.reaper.fm/userguide/WhatsNewReaper7Summary_r2.pdf · https://www.soundonsound.com/news/cockos-launch-reaper-7
- Serum 2: https://www.kvraudio.com/news/xfer-records-releases-serum-2---free-update-63286 · https://splice.com/blog/serum-2-advanced-features/
- FabFilter Pro-Q 4: https://www.fabfilter.com/news/1733994000/fabfilter-releases-pro-q-4-equalizer-plug-in · https://www.soundonsound.com/reviews/fabfilter-pro-q-4
- iZotope Ozone 11: https://izotope.com/en/products/ozone/features/also-includes/master-assistant-view.html · https://www.soundonsound.com/reviews/izotope-ozone-11-advanced
- Melodyne 5: https://www.pluginboutique.com/products/6442-Melodyne-5-Studio

---

## 7. Wave 4: parity gaps from competitor research

Gaps found in `COMPETITION.md` (November 2026) that no earlier wave covers. Same
columns and size scale as above; **audio** = `ponzik-audio`, **midi** =
`ponzik-midi`. The differentiators listed there (one free edition, Linux first,
instant startup, driver-free vocal recording, workspace profiles, the Pyramid)
should guide the order.

| # | Item | Why | Size | Area | Depends on | Status |
|---|---|---|---|---|---|---|
| V4-1 | **English interface and translation catalog**: every UI string in one table, English and French to start, language picked in the settings | The UI is French only, which blocks most of the world from trying Ponzik | M | ui | – | ✅ English source strings, `ponzik-i18n` with embedded `.po` catalogs (one file per language), complete French, picker in the settings and the palette, `PONZIK_LANG` |
| V4-2 | **macOS port**: Cocoa window, CoreAudio playback and capture, CoreMIDI, then AU hosting | "The same Ponzik on every OS"; a large share of producers are on Macs | L | window, audio, midi, ui | – | ⬜ |
| V4-3 | **Low-latency Windows audio**: WASAPI exclusive mode and small shared buffers (`IAudioClient3`), optional in-house ASIO host (the ASIO SDK is now GPLv3) | Vocals and guitar need about 5 ms round-trip without ASIO4ALL, FL's top beginner pain | M | audio | – | ⬜ |
| V4-4 | **Native PipeWire and JACK backends** on Linux, with transport sync | Linux pros route audio between applications (Bitwig, Ardour, Qtractor do it) | L | audio, engine | – | ⬜ |
| V4-5 | **Recording latency calibration**: loopback measurement, automatic offset of recorded takes | Takes otherwise land late and beginners never know why (Reaper, Cubase) | S | audio, engine, ui | – | ⬜ |
| V4-6 | **Punch in/out and pre-roll** on the ruler | Fix one line of a vocal without redoing the whole take | M | engine, project, ui | – | ⬜ |
| V4-7 | **One-click vocal setup**: input picker, meter with clip warning, monitoring, a ready vocal chain (gate, EQ, de-esser, compressor, reverb send) | Turns FL's most common beginner problem into a 10-second task (Logic, Studio Pro, Nectar) | M | ui, engine (de-esser) | V4-3, V4-5 | ⬜ |
| V4-8 | **Capture Automation**: a buffer of recent parameter moves turned into automation after the fact | Never lose a good gesture (Live 13); the automation twin of MIDI capture | M | engine, ui | V2-8 | ⬜ |
| V4-9 | **Retroactive audio logger**: always-on buffer of the master and armed inputs, "save the last N minutes" | Recover a jam or a vocal idea that was not armed (FL 2026 Audio Logger) | M | engine, ui | – | ⬜ |
| V4-10 | **MIDI output, external instruments and MIDI clock out** (ALSA rawmidi, WinMM out) | Ponzik is input-only: no hardware synths, no syncing drum machines | M | midi, engine, ui | – | ✅ |
| V4-11 | **Control surfaces**: Mackie Control and HUI, controller templates for faders and transport | Studio users expect hardware faders and transport | M | midi, ui | V2-7, V4-10 | ⬜ |
| V4-12 | **Key and chord detection**: from MIDI first, then audio (chromagram), feeding the project scale and chord tools | Find the key of a sample or an idea (Logic 12 Chord ID, Live) | M | dsp, project, ui | V1-3 | ⬜ |
| V4-13 | **Note properties**: slide notes and portamento, per-note pan and fine pitch, CC event lanes | FL's slides and controller lanes are daily tools for 808s and leads | M | engine, project, ui (piano.rs) | – | ⬜ |
| V4-14 | **Channel Rack graph editor and per-row length** (polymeters) | Per-step pan, pitch and polyrhythms without the piano roll (FL, Live 13 Step Sequencer) | M | engine, project, ui (rack.rs) | – | ⬜ |
| V4-15 | **Multi-clip and layered editing** in the piano roll | Edit bass and chords together (Live, Bitwig 6) | M | ui (piano.rs) | – | ⬜ |
| V4-16 | **Groove pool**: extract a groove from MIDI or audio, apply it with an amount | Make programmed parts swing like a sampled break (Live, Logic, Cubase) | M | project, ui | V2-11 for audio | ⬜ |
| V4-17 | **Convolution engine**: partitioned FFT convolution, IR reverb and cabinet loader | Real spaces and guitar cabinets; FEATURES lists "no convolution" | M | dsp, engine | – | ⬜ |
| V4-18 | **Amp and cabinet simulator** with a visual of the amp's tone stack | Guitarists and bassists record direct (Live, Logic, Studio Pro 8) | M | dsp, engine, ui | V4-17 | ⬜ |
| V4-19 | **ARA 2 hosting** | Melodyne and VocAlign inside the timeline (Logic, Cubase, Reaper, Live 13) | L | vst3, engine, ui | V3-14 | ⬜ |
| V4-20 | **Crash-safe plugin scanning**: scan in a child process, quarantine list, rescan button | A broken plugin must never stop Ponzik from starting | M | clap, vst3, ui | – | ⬜ |
| V4-21 | **Multi-core audio graph**: independent tracks processed in parallel on a worker pool, lock-free | The engine runs on one core; large projects need all of them | L | engine | – | ✅ channels and mixer tracks follow the routing DAG on a worker pool (`PONZIK_AUDIO_THREADS`), bit-identical to one thread |
| V4-22 | **Per-track and per-device CPU meter** | Find the plugin that eats the CPU instead of guessing (Reaper, FL) | S | engine, ui | – | ✅ smoothed per track, insert and instrument; readouts in the mixer strip and device header |
| V4-23 | **Multi-output plugins and multiple hardware inputs and outputs** | Drum plugins with separate outs, recording a band, external effects | M | engine, audio, clap, vst3 | – | ⬜ |
| V4-24 | **Manual track delay** (± ms per mixer track) | Fix a late sample or a slow external instrument | S | engine, ui (mixer.rs) | – | ⬜ |
| V4-25 | **Cue and pre-listen bus** on a second output | Preview clips and samples in headphones while the mix plays (Live, Bitwig) | M | engine, audio, ui | V4-23 | ⬜ |
| V4-26 | **Collect all and save**: copy every referenced file into the project folder | Move a project to another machine without missing samples | S | project | – | ⬜ |
| V4-27 | **Free bundled sound library**: drums, loops and presets for every native device | A new user needs sounds in the first minute | M | content, ui | V2-22 | ✅ |
| V4-28 | **Similar-sound search** in the browser (spectral fingerprint per sample) | Find another snare like this one (Live, Logic 13) | M | dsp, ui | V2-22 | ⬜ |
| V4-29 | **Destructive sample editor**: trim, normalize, fade, reverse, loop points, with undo | Clean up a sample without leaving the DAW (FL Edison) | M | project, ui | – | ⬜ |
| V4-30 | **Score and tab view** | Songwriters, students and guitarists read notation and tabs (Cubase, Logic, Studio Pro 8) | L | ui, project | – | ⬜ |
| V4-31 | **Expression maps**: articulations per note mapped to keyswitches or CCs | Orchestral libraries are unusable without them (Cubase, Logic) | M | project, engine, ui | V4-13 | ⬜ |
| V4-32 | **Lossy export**: OGG Vorbis or Opus, then MP3, with in-house encoders | Sharing a demo means a small file | L | project, dsp | V2-25 | ⬜ |
| V4-33 | **Video track**: import, frame-accurate sync, export | Scoring to picture, content creators | L | new crate, ui | – | ⬜ |
| V4-34 | **Surround and immersive mixing** (5.1, 7.1.4 beds, object panner) | Atmos delivery for streaming and film (Logic, Cubase, Reaper) | L | engine, ui | V4-23 | ⬜ |
| V4-35 | **Scripting and extension API**: an in-house script language over the palette actions and the project model | Power users extend the DAW (ReaScript, Ardour Lua, FL Python, Max for Live) | L | new crate, ui | V3-11 | ⬜ |
| V4-36 | **Accessibility**: visible focus everywhere, screen-reader bridge (UI Automation on Windows, AT-SPI on Linux) | Reaper with OSARA is the only real option for blind musicians today | L | window, ui | – | ⬜ |
| V4-37 | **UI scaling and HiDPI** (per-monitor scale, settings slider) | 4K and laptop screens; Cubase 15 made every stock effect scalable | M | window, ui | – | ✅ |
| V4-38 | **Touch and pen input** for pads, faders and drawing | Touch screens and tablets (FL, Ardour 9) | M | window, ui | V4-37 | ⬜ |
| V4-39 | **Looper device** with overdub, undo layer and tempo detection | Live layering for performers and guitarists (Live, Bitwig) | M | engine, ui | – | ⬜ |
| V4-40 | **Microtonal tuning**: Scala files, per-project tuning, piano roll that shows it | Non-Western scales and experimental music (Live, Bitwig, Logic) | M | engine, project, ui | V1-3 | ⬜ |
| V4-41 | **Collaboration**: merge-friendly project bundles first, then shared live sessions | Producers work in pairs; today that means emailing stems (BandLab, Audiotool) | L | project, new crate | V3-4 | ⬜ |
| V4-42 | **Help search in the palette**: "how do I…" answered offline from the help texts, with a button that runs the action | Beginner help without a cloud assistant or subscription (FL Gopher, Logic 13) | S | ui (palette.rs) | V1-1 | ⬜ |
| V4-43 | **FL Studio `.flp` import**: channels, patterns, playlist, mixer, native-device mapping | Ponzik's audience comes from FL; removes the main switching cost | L | project | V3-15 | ✅ tempo, channels (samplers with their WAV, generators → closest native instrument), patterns and steps, playlist pattern/audio clips, mixer levels, routing and obvious effects; an import report lists what was approximated or skipped (plugin settings, automation, layers) |
