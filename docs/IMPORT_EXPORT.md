# Opening and saving other programs' projects

Ponzik opens the projects of several other DAWs and scores in MusicXML, and
saves interchange copies for FL Studio, Ableton Live and REAPER. Open them from
the Open dialog (Ctrl+O), by dropping the file on the window, or from a file
manager or the command line (`ponzik song.rpp`).

An imported project opens as a new, unsaved Ponzik project, with an **import
report**: what was kept as is (mapped), what was converted to the closest thing
Ponzik has (approximated), the media files that are missing on this computer
(offline: they stay in the project and can be located later in one click) and
what was left out (skipped). Reopen it from the command palette with "Show the
import report".

Everything is written in Ponzik itself: the zip, zlib and gzip readers too.
Every importer is tested on small projects written by hand like the program
writes them, on truncated files and on garbage values; the exporters are read
back by the importers in the tests. The reverse-engineered or documented
layouts have not all been checked against files saved by every version of each
program, so a report from a real project that opens badly is welcome.

## Formats

| Format | Extensions | Open | Save a copy |
|---|---|---|---|
| Ponzik | `.ponzik` | yes, everything | yes, everything |
| FL Studio | `.flp` | yes (up to FL 25) | yes |
| Ableton Live | `.als` | yes | yes |
| REAPER | `.rpp` | yes | yes |
| LMMS | `.mmp`, `.mmpz` | yes | no |
| Ardour | `.ardour` | yes (Ardour 5 to 8) | no |
| MusicXML | `.musicxml`, `.mxl`, `.xml` | yes (partwise scores) | no |
| Standard MIDI File | `.mid`, `.midi` | yes, into a pattern | yes, a pattern |
| Studio One | `.song` | not yet | no |
| Bitwig Studio | `.bwproject` | not yet | no |
| Cubase, Logic Pro, Pro Tools, Reason | `.cpr`, `.logicx`, `.ptx`, `.reason` | not yet | no |

Every importer maps instruments to the closest Ponzik instrument: from the
plugin (Serum and Vital become Ponzik Wave, Dexed becomes Ponzik FM, a
sampler stays a sampler with its file), from the General MIDI program, or from
the track's name (Piano, Strings, Drums, Kick…). Instrument and effect settings
are never converted.

## FL Studio (`.flp`)

- Kept: tempo, channels (name, colour, volume, pan, mixer track), samplers
  with their sample, patterns and notes, playlist clips and audio clips, mixer
  tracks (names, levels, routing, sends).
- Approximated: generators and plugins become the closest Ponzik instrument,
  FL's effects the closest Ponzik effect; FL's factory samples are replaced by
  library sounds.
- Left out: automation clips, layer channels, plugin settings.
- Saved copy: tempo, channels (samplers keep their sample, other instruments
  become a 3x Osc), patterns, notes, playlist clips, mixer names, levels and
  routing. Ponzik's devices, effects, automation and the Session view are not kept.

## Ableton Live (`.als`)

- Kept: tempo, time signature, MIDI tracks (name, colour, volume, pan) with
  their arrangement clips (looped or not, offsets), audio clips (file, place,
  length), locators.
- Approximated: Session view clips become patterns, not placed; Live's
  devices pick the closest instrument.
- Left out: warping and the start in an audio file, group and return tracks,
  devices and their settings, automation, routing.
- Saved copy: tempo, time signature, one MIDI track per channel with its notes
  as arrangement clips, markers as locators. Instruments, effects, audio clips,
  automation and routing are not kept.

## REAPER (`.rpp`)

- Kept: tempo, time signature, tempo envelope (changes, linear ramps, time
  signature changes), markers, the loop, tracks (name, colour, volume, pan,
  mute), folders (child tracks routed into their folder track), sends, MIDI
  items as patterns in the playlist (looped or not, start offset, muted), audio
  items (file, place, length, start in the file, item and take volume, fades,
  reversed sections, loop), the active take of each item.
- Approximated: regions become markers at their start; the first instrument
  plugin picks the closest Ponzik instrument (ReaSamplOmatic5000 becomes a
  sampler with its file); items with another play rate play at normal speed;
  fade shapes become linear; colours are read in REAPER's Windows order.
- Left out: effect plugins, automation envelopes, take FX, video items, MIDI
  CCs and pitch bends, stretch markers.
- Saved copy: tempo, tempo changes (ramps included) and time signatures,
  markers, the loop, one track per channel (volume, pan, mute, colour) with a
  MIDI item per playlist clip (patterns unrolled), one track per mixer track
  that plays audio clips (file, place, length, start in the file, gain, fades,
  loop). Instruments, effects, automation and routing are not kept.

## LMMS (`.mmp`, `.mmpz`)

- Kept: tempo, time signature, the loop, instrument tracks (name, colour,
  volume, pan, mute) with their patterns placed in the playlist, each
  Beat/Bassline as one pattern of all the beat channels with its clips, sample
  tracks (file, place, length, start in the file, mute, reverse), the FX mixer
  (channels with their name, volume, mute and routing, sends), the master volume.
- Approximated: LMMS instruments become the closest Ponzik one (Kicker a
  kick, LB302 the 303 bass, AudioFileProcessor a sampler with its sample,
  SF2 Player by its GM program…); LMMS factory samples that are not on this
  computer are replaced by library sounds; step notes last a sixteenth.
- Left out: automation tracks, effects, instrument settings, envelopes,
  arpeggios and chords.

## Ardour (`.ardour`)

- Kept: tempo map (changes, ramps, meters), markers, sections, the loop,
  tracks and buses in the editor's order (name, colour, gain, pan, mute),
  tracks routed into a bus by their output ports, internal sends, MIDI regions
  with their notes from the session's MIDI files (`interchange/<session>/midifiles`),
  audio regions from `interchange/<session>/audiofiles` (file, place, length,
  start in the file, gain, fades), the master gain. Positions in samples
  (Ardour 5, 6) and in superclock or beat time (Ardour 7, 8) are both read.
- Approximated: ranges and cue markers become markers; a MIDI track's first
  plugin picks the closest Ponzik instrument; a region of two mono files
  (a stereo recording) plays its first channel only.
- Left out: plugins and their settings, automation, VCAs, layering of
  overlapping regions (all regions play).

## MusicXML (`.musicxml`, `.mxl`)

- Kept: title, tempo marks, time signatures, one track and channel per part
  (name, volume, pan, instrument from its MIDI program or name; percussion
  parts with their MIDI keys), the notes at their sounding pitch (transposing
  instruments transposed back), chords, voices, ties, dynamics as velocities;
  each part is one pattern placed at the start of the playlist.
- Approximated: repeats and endings are played once (not unrolled); measures
  last as long as their longest part.
- Left out: grace and cue notes, lyrics, articulations, written dynamics,
  the layout of the score; timewise scores.

## Not supported yet

- **Studio One** (`.song`): a zip of XML files, but the note data and its
  layout are not documented; opening one tells you to export MIDI files and
  stems from Studio One.
- **Bitwig Studio** (`.bwproject`), **Cubase** (`.cpr`), **Logic Pro**
  (`.logicx`), **Pro Tools** (`.ptx`), **Reason** (`.reason`): closed binary
  formats. Export MIDI files and stems from them; DAWproject (Bitwig, Studio
  One, Cubase) is planned.
