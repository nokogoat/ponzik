# Getting started

1. At launch Ponzik reopens your last project. The first time (or when Settings >
   Workspace > At startup says so) a start screen offers a new project, the
   templates, the house demo and your recent projects. Press **Space** to play.
2. In the **Channel Rack** (F6), click steps to light them. Hold the right mouse
   button and sweep to erase.
3. Drag a sound from the browser (left) onto the rack, or drop a WAV/MP3 from your
   file manager.
4. Press **F7** for the piano roll: click to draw notes, drag their end to make them
   longer. Pick a scale next to the scale button above the notes to stay in key.
5. Press **F5** for the playlist: paint your patterns, drag a loop in and stretch its
   right edge to repeat it.
6. Press **F9** for the mixer, then **Ctrl+R** to export.
7. Press **Ctrl+S**. The first time, a dialog asks where and how: a Ponzik project
   (`.ponzik`, everything kept), or a copy for FL Studio (`.flp`) or Ableton Live
   (`.als`). After that, Ctrl+S saves to the same file without asking;
   **Ctrl+Shift+S** asks again. **Ctrl+O** opens `.ponzik`, `.flp`, `.als` and MIDI
   files, with your recent projects one tab away.

## Find a curve again

A curve drawn on a knob (an instrument's Attack, a filter's Cutoff, a track's
volume) keeps playing even when you no longer see it. To get back to it:

- **On the knob.** An automated knob has a teal dot (hollow when the curve is
  muted). Hover it to see its curves; right-click it for **Show the curve**, which
  opens the piano roll on the right pattern, channel and lane for a pattern curve,
  or the playlist's automation lanes for a song curve. The same menu mutes the
  curve, flattens it to the knob's value or deletes it. While it plays, the knob
  follows the curve.
- **In the piano roll.** Above the lane under the velocities, a chip shows every
  curve drawn in the pattern for this channel, with its shape: click one to show it,
  **Shift+click** to stack it under the lane shown, **x** to delete it, **+** to
  add a lane (pitch bend, a CC, any parameter).
- **Everywhere at once.** Main menu > **Automation list**, or **Ctrl+K** then
  "Automation list", shows every curve of the project: where it lives, what it
  moves, its shape. Click one to jump to it, mute it or delete it. Ctrl+K also
  finds each curve by name ("Show the curve: Attack").

## Shape an envelope

Synths show their envelopes (attack, decay, sustain, release, plus delay and hold
on Pulse, FM, Wave, Sampler and Granular) as a graph above the knobs. Drag a point
sideways for its time, the sustain point up and down for its level, and a segment
up or down to bend it. Double-click resets, right-click automates or types a value.
The moving dot is the last note played.

The tour on first launch walks through this, and **Ctrl+K** finds any action by name.

On a small screen, give the editor room: drag the border of the browser or of the
device panel (drag it to the edge to fold it), or press **Ctrl+Shift+F** to
maximize the view. In the piano roll, the − and + above the keyboard (or Alt+wheel)
make the keys taller. **Ctrl+=** and **Ctrl+−** change the size of the whole
interface. See [Shortcuts](SHORTCUTS.md#layout).
