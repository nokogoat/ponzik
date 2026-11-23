# Themes

Everything you see in Ponzik is a named **token**: colors, corner radii, border
width, shadows, text size, spacing and animation speed. A theme is a small text
file, written in a subset of CSS, that sets some of these tokens. Everything it
does not set comes from a base theme.

You can change a theme in two ways:

- **In the app**: Settings (the ☰ menu, then Settings), tab **Appearance**. Pick a
  theme on the left, a group of tokens in the middle, then a color in the list:
  the color picker (saturation and brightness square, hue strip, opacity strip,
  text field) changes it live in the whole interface. Sliders set the radii,
  border width, shadows, text size and animation speed; three buttons set the
  density. Each token has its own undo button and a reset button (back to the
  inherited value).
- **In your text editor**: the pencil button opens the theme's CSS file. Save it
  and Ponzik updates within half a second. Mistakes are reported with their line
  number (in the editor and in a notification) and never stop the rest of the
  file from applying.

Built-in themes (Ponzik, Ice, Amber) and the examples (Midnight, Paper Light,
Synthwave, High Contrast) are read-only: changing one of their colors makes an
editable copy in your themes folder. The chosen theme is saved in the settings.

## Where theme files live

| System | Folder |
|---|---|
| Linux | `~/.config/ponzik/themes/` (or `$XDG_CONFIG_HOME/ponzik/themes/`) |
| Windows | `%APPDATA%\Ponzik\themes\` |
| macOS | `~/Library/Application Support/Ponzik/themes/` |

Files ending in `.ponzik-theme` or `.css` are listed. To import a theme, drop the
file on the Ponzik window: it is copied into this folder and selected. **Export
to share** writes a copy in `<Music>/Ponzik/Themes/`.

For captures and quick trials, `PONZIK_THEME=<name or path>` selects a theme for
one session, for example:

```sh
PONZIK_THEME=paper-light cargo run --release -- screenshot /tmp/light.png mixer
PONZIK_THEME=~/my-theme.css cargo run --release
```

## File format

```css
/* My theme */
:root {
  --name: "Midnight";
  --base: ponzik;            /* inherit unspecified tokens from a built-in */
  --accent: #22c55e;
  --bg: rgb(10 14 12);
  --radius-md: 10px;
  --font-scale: 1.1;
  --density: compact;
}
.playlist { --lane-alt: #121816; }
.mixer { --meter-hot: hsl(0 80% 60%); }
```

- Only custom properties (`--name: value;`) are read, in `:root` and in the area
  blocks below. Comments `/* … */` go anywhere.
- `--name` is the name shown in the list. `--base` is the theme it inherits
  from: `ponzik`, `glace` (Ice), `ambre` (Amber), or an example (`midnight`,
  `paper-light`, `synthwave`, `high-contrast`). Without `--base`, Ponzik is used.
- **Colors**: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb(10 14 12)`,
  `rgb(10, 14, 12)`, `rgba(255, 0, 0, 0.5)`, `rgb(100% 0% 0% / 50%)`,
  `hsl(142 71% 45%)`, `hsla(240deg, 100%, 50%, 0.5)`, `color-mix(in srgb, #fff 30%, #000)`,
  and `white`, `black`, `transparent` and a few other names.
- **Numbers**: lengths in `px` (or without unit), factors as numbers or
  percentages (`1.1` or `110%`), `--density` as `compact`, `normal` or `comfortable`.
- **Variables**: any property can hold a value used elsewhere with `var()`:
  `--brand: #ff3ea5; --accent: var(--brand);`. `var(--x, fallback)` gives a value
  when `--x` is not declared. A token the file does not declare refers to the
  base theme's value (`var(--bg)`).
- **Areas**: `.rack`, `.piano`, `.playlist`, `.mixer`, `.devices` (the device
  panel), `.browser` (the sidebar) and `.topbar` override any token in that part
  of the interface only. Several areas can share a block: `.mixer, .rack { … }`.

When the visual editor changes a token, it edits that line of the file in place:
your comments and the rest of the file are kept.

## Derived tokens

Some tokens follow others until a theme sets them: `--hairline` is `--text` at
8% opacity, `--tooltip-text` follows `--text`, `--note` follows `--accent`, and so
on (see the "Default" column). Changing a base color is usually enough; set a
derived token only to break the link.

## Contrast check and helpers

The Appearance tab checks the contrast of text on its surfaces against WCAG 2
ratios (4.5:1 for text, 3:1 for secondary text and the accent on panels) and lists
the pairs that fall short; click one to edit its color.

- **Randomize the accent** (dice button): a new accent color, readable saturation
  and lightness.
- **Generate from the accent** (half-disc button): surfaces of the accent's hue,
  text colors lifted to readable contrast, a complementary selection color, in
  light or dark mode depending on the current background. Each generated color
  can be undone on its own.

## Tokens

### Surfaces

| Token | Default (Ponzik) | Used for |
|---|---|---|
| `--bg-deep` | `#090c0c` | Top bar, deepest wells, black keys of small keyboards |
| `--bg` | `#0e1212` | Window background |
| `--panel` | `#131817` | Cards and panels |
| `--strip` | `#171d1c` | Mixer strips, device cards |
| `--raised` | `#1c2322` | Buttons, fields |
| `--raised-hi` | `#26302f` | Hovered and selected controls |
| `--well` | `#0a0d0d` | Inset areas (lanes, meters, graphs) |
| `--hairline` | `--text` at 8% | Separators and outlines |
| `--highlight` | `#ffffff` | Hover brightening, glints |
| `--shadow` | `#000000` | Shadows under menus and windows |

### Text

| Token | Default | Used for |
|---|---|---|
| `--text` | `#e6efec` | Main text |
| `--text-2` | `#98a7a3` | Secondary text |
| `--muted` | `#7a8a86` | Labels, hints |

### Accent

| Token | Default | Used for |
|---|---|---|
| `--accent` | `#3fd45a` | What plays: lit steps, notes, playback, toggles |
| `--accent-deep` | `#23a83a` | Darker accent (gradients, quiet notes) |
| `--accent-hot` | `#c2ffb9` | Brightest accent (selected notes, flashes) |
| `--on-accent` | `#04230b` | Text and icons on the accent |
| `--select` | `#3ccfc4` | Selection, markers |
| `--playhead` | `--select` | Playhead in the piano roll and the playlist |

### Signals

| Token | Default | Used for |
|---|---|---|
| `--negative` | `#e5484d` | Recording, clipping, errors |
| `--warning` | `#f0b44a` | Warnings, latency, theme file problems |
| `--frost` | `#8ee7f2` | Frozen channels and tracks |

### Palettes

| Token | Default | Used for |
|---|---|---|
| `--pattern-1` to `--pattern-8` | `#3fd45a #3ccfc4 #9be15d #2fb6e0 #67e8a4 #c7e85a #5ad1e8 #8ce07a` | Automatic colors of patterns and tracks |
| `--take-1` to `--take-8` | `#3ccfc4 #f0b44a #a98cf0 #9be15d #e878b4 #2fb6e0 #f08a5d #6b7cf0` | Automatic colors of recorded takes |
| `--marker-1` to `--marker-8` | `#3fd45a #3ccfc4 #f0b44a #e878b4 #2fb6e0 #a98cf0 #f08a5d #c7e85a` | Automatic colors of arrangement markers |

Colors chosen by hand for a pattern, track or marker are saved in the project and
do not change with the theme.

### Controls

| Token | Default | Used for |
|---|---|---|
| `--pad-a`, `--pad-b` | `#192020`, `#1f2827` | Step pads, alternating beats |
| `--pad-head` | `#2e3b39` | Step pad of the first beat |
| `--knob-track` | `--raised-hi` | Knob ring |
| `--knob-arc` | `--accent` | Knob value arc |
| `--knob-cap` | `--raised-hi` | Knob body |
| `--knob-cap-hover` | `#34403e` | Knob body under the mouse |
| `--knob-pointer` | `--text` | Knob pointer |
| `--button-hover` | `#3a4644` | Small rack knobs under the mouse |

### Tooltips and toasts

| Token | Default | Used for |
|---|---|---|
| `--tooltip-bg` | `--raised-hi` mixed with 15% `--bg-deep` | Tooltip background |
| `--tooltip-text`, `--tooltip-text-2` | `--text`, `--text-2` | Tooltip lines |
| `--toast-bg` | `--raised-hi` | Notification background |
| `--toast-text` | `--text` | Notification text |
| `--toast-dot` | `--accent` | Notification dot |

### Mixer

| Token | Default | Used for |
|---|---|---|
| `--meter` | `#cfd8d5` | Meter below -6 dB |
| `--meter-hot` | `--accent` | Meter above -6 dB |
| `--meter-clip` | `--negative` | Peak hold at 0 dB |
| `--meter-bg` | `--well` | Meter background |

### Piano roll

| Token | Default | Used for |
|---|---|---|
| `--piano-row` | `--panel` mixed with 35% `--raised` | Rows of white keys |
| `--piano-row-black` | `--well` | Rows of black keys |
| `--piano-grid` | `--hairline` | Grid lines |
| `--note-low`, `--note`, `--note-hot` | `--accent-deep`, `--accent`, `--accent-hot` | Notes, from soft to loud and selected |
| `--key-white`, `--key-black` | `--text` mixed with 12% `--raised-hi`, `--bg-deep` | Keyboard |

### Playlist

| Token | Default | Used for |
|---|---|---|
| `--lane-bg` | `--well` | Lanes background |
| `--lane-alt` | `--panel` at 35% | Every other lane |
| `--playlist-grid` | `--hairline` | Bar and beat lines |
| `--wave` | `--text` | Audio waveforms |

### Shape and motion

| Token | Default | Range | Effect |
|---|---|---|---|
| `--radius-sm` | `4px` | 0 to 32 | Corners that measure 4 px in the default theme (small chips) |
| `--radius-md` | `8px` | 0 to 32 | Corners of 8 px (buttons, rows); 4 to 16 px are interpolated |
| `--radius-lg` | `16px` | 0 to 48 | Corners of 16 px and more (cards, menus) |
| `--border-width` | `1px` | 0 to 4 | Thickness of outlines |
| `--shadow-strength` | `1` | 0 to 3 | Darkness of shadows (0: none) |
| `--font-scale` | `1` | 0.75 to 1.5 | Size of all text, on top of the interface size |
| `--density` | `normal` | `compact` (0.85), `normal` (1), `comfortable` (1.15) | Height of rack rows, browser rows and menu items, width of mixer strips |
| `--animation-speed` | `1` | 0 to 3 | Speed of transitions; `0` turns animations off |

Round shapes (pills, dots, knobs) stay round whatever the radii.

## Examples

The four example themes ship with Ponzik and are in
[`assets/themes/`](../assets/themes/): copy one into your themes folder to start
from it.

A light theme only needs a light set of surfaces and dark text; it is good
practice to also set the tooltip colors and the piano keyboard:

```css
:root {
  --name: "Paper";
  --base: ponzik;
  --bg-deep: #e3e8e5;
  --bg: #eef1ef;
  --panel: #fafbfa;
  --raised: #eaefec;
  --raised-hi: #dde4e0;
  --well: #e6ebe8;
  --text: #16201d;
  --text-2: #46534f;
  --muted: #5f6c68;
  --accent: #1a8532;
  --on-accent: #ffffff;
  --shadow-strength: 0.45;
}
.piano { --key-white: #ffffff; --key-black: #27302d; }
```

An accessible variant of the default theme, without animations and with larger
text:

```css
:root {
  --name: "Calm";
  --base: ponzik;
  --animation-speed: 0;
  --font-scale: 1.15;
  --density: comfortable;
  --border-width: 1.5px;
}
```

A mixer with its own meter colors:

```css
.mixer {
  --meter: #b8c4ff;
  --meter-hot: #ffd319;
  --meter-clip: #ff3e6c;
}
```
