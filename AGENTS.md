# Working on Ponzik

This file is for anyone changing Ponzik's code, human or AI coding assistant
(Claude Code, Codex, Cursor, Aider…). Read it before your first change.

Ponzik is a free, open-source DAW written in 100% Rust with **zero external
crates**. It combines FL Studio's breadth with Ableton Live's clarity, for
beginners and professionals.

## Ground rules

1. **No dependencies.** Do not add crates to any `Cargo.toml`. Everything
   (drivers, windowing, rendering, codecs, JSON, hashing, DSP) is written in this
   repository and talks only to the operating system (FFI to libasound, X11 over a
   socket, Win32/COM, Cocoa through the Objective-C runtime). If you need
   something, write it, with tests. Build tools may use only the Rust standard
   library.
2. **The audio thread is sacred.** Code reached from `Engine::process`,
   `Instrument::render`, `Effect::process`, `NoteFx` and the mixer must not
   allocate, lock, block, do I/O or log. Prepare everything on the interface
   thread and send it with a `Command`; replaced objects come back through the
   garbage queue to be dropped on the interface thread. New real-time code gets a
   counting-allocator test (see `crates/ponzik-engine/tests/*_alloc.rs`).
3. **The project is the source of truth.** The interface edits `Project`
   (`crates/ponzik-project`); `apps/ponzik/src/gui/sync.rs` diffs old and new
   projects and sends the engine commands. Every user edit goes through
   `App::commit(label)` so it can be undone.
4. **Every user-visible string goes through the translation layer**:
   `ponzik_i18n::tr("English text")` or `tr_fmt!("{} tracks", n)`. Add the French
   translation to `crates/ponzik-i18n/lang/fr.po`. The coverage test fails if a
   string is missing a French translation or a catalog keeps a string no longer
   used. Source strings are in English; code comments may be in French or English.
5. **Saved files stay compatible.** Loading is tolerant: new fields are optional,
   unknown or broken values fall back to defaults, ids never change. Add a
   round-trip test when you change the file format (`crates/ponzik-project/src/io.rs`).
6. **Do not run `cargo fmt` on the whole repository.** The code is not uniformly
   rustfmt-formatted; formatting everything produces huge unrelated diffs. Format
   only new files if you like (`max_width = 120`, `use_small_heuristics = "Max"`),
   and match the surrounding style elsewhere.
7. **Finish clean.** `cargo test --workspace` passes and
   `cargo clippy --workspace --all-targets` has **zero warnings**.

## Layout

| Path | What lives there |
|---|---|
| `crates/ponzik-dsp` | Pure DSP blocks, no allocation after construction (filters, delays, FFT, loudness, pitch tracking, stretch, onsets, tempo) |
| `crates/ponzik-engine` | Real-time engine: `engine.rs` (block processing, commands), `sequence.rs`, `mixer.rs`, `pool.rs` (worker threads), `instruments/`, `effects/`, `note_fx/`, `registry.rs`, `codecs/`, `param.rs` |
| `crates/ponzik-project` | `Project` model, `io.rs` (JSON files), `render.rs` (offline), `history.rs`, `scale.rs`, `harmony.rs`, `transform.rs`, `generate.rs`, `flp.rs` (FL Studio import), `library.rs` (sound library), `presets.rs`, demos |
| `crates/ponzik-audio`, `crates/ponzik-midi`, `crates/ponzik-window` | Platform backends, one file per OS (`alsa.rs`/`wasapi.rs`/`coreaudio.rs`, `x11.rs`/`win32.rs`/`cocoa.rs`…) selected with `#[cfg]` |
| `crates/ponzik-ui` | Canvas, rasterizer, text, PNG, animations |
| `crates/ponzik-i18n` | `tr`, `tr_fmt!`, catalogs in `lang/*.po` |
| `crates/ponzik-clap`, `crates/ponzik-vst3` | Plugin hosts (+ test plugins used by their tests) |
| `apps/ponzik/src/main.rs` | CLI (`ponzik`, `breakcore`, `zalgo`, `render`, `screenshot`, `library`, `scan-plugin`) |
| `apps/ponzik/src/gui/` | Immediate-mode interface: `app.rs` (App state, frame, menus), one file per area (`rack.rs`, `piano.rs`, `playlist.rs`, `mixer.rs`, `devices.rs`, `visuals.rs`, `samplelab.rs`, `record.rs`…) |
| `assets/presets/<kind>/*.json` | Factory presets (≥ 6 per native instrument, enforced by a test) |
| `docs/` | `FEATURES.md` (inventory vs FL/Ableton), `BACKLOG.md` (roadmap with status), `COMPETITION.md` |

## Common tasks

### Add an instrument or an effect

1. Write it in `crates/ponzik-engine/src/instruments/` or `effects/`, implementing
   `Instrument` or `Effect` plus `Params` (parameter descriptions in `ParamInfo`:
   English names, units, groups; names are translated through `fr.po`).
2. Register it in `registry.rs` (stable `id`: it is saved in projects).
3. Add factory presets in `assets/presets/<id>/`.
4. Optionally add a visual in `apps/ponzik/src/gui/` (see `visuals.rs` and the
   per-device files such as `grand.rs`, `pads.rs`).
5. Tests: bounded output at extreme settings, behaviour (pitch with the YIN tuner,
   decay times, brightness vs velocity…), and a no-allocation test.

### Add a user action

Add an `Action`, handle it in `App::run_action`, expose it in the command palette
(`gui/palette.rs`) and, if it deserves a key, in `gui/keymap.rs` (remappable).

### Translations

English is the default language. `crates/ponzik-i18n/lang/*.po` are built in;
users also load their own `.po` files at run time from `<config>/lang`, created and
reloaded from Settings > Language. Other shipped catalogs than French may be
partial (missing strings show in English). How translators create, test, share and
contribute a language: [docs/TRANSLATING.md](docs/TRANSLATING.md).

### Change the interface

The interface is immediate mode: each frame redraws from state. Follow the UI rules
in `docs/BACKLOG.md` §1: icons over words, a tooltip on every control (`set_tip`),
FL gestures (hold right-click erases, wheel adjusts, double-click resets),
progressive disclosure, no decorative gradients or glows.
Never hard-code a color, radius or shadow: use the theme tokens of `gui/theme.rs` (see [docs/THEMES.md](docs/THEMES.md)).

## Verify your change

You cannot see the window from a terminal, so Ponzik renders itself off-screen:

```sh
cargo run --release -- screenshot /tmp/shot.png <view> [width height]
# views: rack, piano, playlist, session, mixer, lab; prefixes bc- / zg- use the breakcore / Zalgo demos
```

Then **open the PNG and look at it**. Many `PONZIK_*` environment variables set up
a specific state for a capture (open a popup, a panel, a device, a language…);
they are read in `apps/ponzik/src/gui/mod.rs` (`screenshot`) and
`App::screenshot_*` functions; grep for `PONZIK_` to find them. Useful ones:
`PONZIK_LANG=fr`, `PONZIK_POPUP=<name>`, `PONZIK_CHANNEL=0 PONZIK_INSTRUMENT=<id>`,
`PONZIK_INSERTS="2:compressor"`, `PONZIK_FOCUS_TRACK=<n>`, `PONZIK_SCALE_UI=2`.

Sound can't be heard in a terminal either: measure it. Render with
`ponzik render` or in a test, then check levels, pitch (`ponzik_dsp::tuner`),
loudness (`ponzik_dsp::loudness`), spectra. Instruments have ignored demo tests that
write WAV files to `/tmp` for a human to listen to, e.g.
`cargo test --release -p ponzik-engine --test piano_demo -- --ignored --nocapture`.

Plugin host tests need the test plugins built first:

```sh
cargo build -p ponzik-clap-test-plugin -p ponzik-vst3-test-plugin
cargo test --workspace
cargo clippy --workspace --all-targets
```

`PONZIK_PROFILE=1` prints per-frame timing and allocations; `PONZIK_DEBUG_INPUT=1`
logs input events.

## Commits

- Atomic commits, one topic each, **messages in English**, imperative, prefixed with
  the area: `Engine: …`, `Mixer: …`, `Piano roll: …`, `Docs: …`.
- Keep the tree green at the end of a series (tests and clippy).
- Update `docs/FEATURES.md` / `docs/BACKLOG.md` when you ship a listed item.
