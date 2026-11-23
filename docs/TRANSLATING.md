# Translating Ponzik

Ponzik's interface is in English by default and ships with French. Anyone can add
another language, or improve an existing one, with a single text file: no
programming, no compiling, no restart.

## Where translations live

Your own translations are `.po` files in the `lang` folder of Ponzik's settings
folder:

| System | Folder |
|---|---|
| Linux | `~/.config/ponzik/lang` (or `$XDG_CONFIG_HOME/ponzik/lang`) |
| Windows | `%APPDATA%\Ponzik\lang` |
| macOS | `~/Library/Application Support/Ponzik/lang` |

Each file is one language, named after its code: `es.po`, `de.po`, `pt_BR.po`.
Ponzik reads them at startup next to the languages it ships with.

## Create a translation

1. Open **Settings** (the menu at the top right, or **Ctrl+K** and type
   "settings"). The **Language** section is at the bottom right.
2. Click **+** (Create a translation) next to the Language title, or press
   **Ctrl+K** and type "Create a translation".
3. Type the language: a code (`es`, `pt_BR`, `fil`), a code and a name
   (`es_MX Español de México`), or the name of a common language (`Spanish`,
   `Deutsch`). Press **Enter**.

Ponzik writes `<code>.po` in your translations folder, switches the interface to
that language and opens the folder in your file manager. The file already lists
every text of the interface, each with an empty translation and the French version
as a hint.

## Edit the file

Open it in any text editor (or a translation tool such as Poedit, Lokalize or
Gtranslator: the format is standard gettext). Each text looks like this:

```po
#. fr: Annuler
msgid "Cancel"
msgstr ""
```

Write your translation between the quotes of `msgstr`:

```po
msgid "Cancel"
msgstr "Cancelar"
```

Then save the file as UTF-8 and click **Reload translations** (the circular arrow
next to the Language title in Settings, or **Ctrl+K**, "Reload translations"). The
interface changes at once.

A few rules:

- **Leave a `msgstr` empty** to keep the English text. You can translate a little at
  a time: the language picker shows how far you are ("Español · 42%").
- **Keep the `{}` fields.** Ponzik puts numbers and names in them: `{}`, `{0}`,
  `{:.1}` (a number with one decimal). Keep the same number of fields as the English
  text; you may move them around. A translation with different fields is ignored.
- **`msgctxt`** tells apart two meanings of the same English word; translate each
  one for its meaning:

  ```po
  msgctxt "drum pad"
  msgid "Open"
  msgstr "Abierto"
  ```

- **Line breaks** are written `\n`, quotes `\"`, backslashes `\\`. Long texts can
  continue on the next lines; the parts are joined:

  ```po
  msgid "Saves every piece as its own WAV file\nin the Samples folder"
  msgstr ""
  "Guarda cada trozo como su propio archivo WAV\n"
  "en la carpeta Samples"
  ```

- Lines starting with `#` are comments. An entry marked `#, fuzzy` (as some tools
  do for guesses) is not used until you remove the mark.
- **Plurals.** Ponzik has a "one" form and an "other" form for counts, written as
  two separate texts (`{} note` and `{} notes`). Translate both; the
  `Plural-Forms` line of the header decides which one a number takes. Entries
  with `msgid_plural` and `msgstr[0]`, `msgstr[1]`… also work.

### The header

The first entry of the file describes the language:

```po
msgid ""
msgstr ""
"Language: es\n"
"Language-Name: Español\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\n"
"X-Decimal-Separator: ,\n"
```

- `Language`: the code of the language. It wins over the file name.
- `Language-Name`: the name shown in the language picker, written in the language
  itself.
- `Plural-Forms`: the standard gettext plural rule. Ponzik fills it in for common
  languages.
- `X-Decimal-Separator`: `,` or `.`, the decimal mark of numbers (`1,5 dB`).

### When something is wrong

Ponzik never refuses a file because of a mistake: it uses every line it can read.
If a file has problems, its language gets a red dot in the picker; point at it to
see how many problems there are and the first one, with its line number (a missing
quote, an unknown line, `{}` fields that differ from the English text…).

## Correct or complete an existing language

To fix a French wording, create a translation with the code `fr`. The file comes
filled with the current French translations: change what you want, delete the rest
if you like, and reload. Your file only overrides the texts it translates; the
others keep the shipped translation.

## After an update of Ponzik

New versions add new texts. Run **Create a translation** again with the same code:
your file is never overwritten, the missing texts are added at its end under
`# --- New strings ---`.

## Share a translation

A translation is just the `.po` file. Send it to anyone: they drop it in their
translations folder (Settings, Language, the folder button opens it) and click
Reload translations.

## Contribute it to Ponzik

To ship your language with Ponzik for everyone:

1. Fork the repository and copy your file to `crates/ponzik-i18n/lang/<code>.po`.
   Files in that folder are built into the application.
2. Check that the header has `Language`, `Language-Name`, `Plural-Forms` and
   `X-Decimal-Separator`, and that the file is UTF-8.
3. Run the checks:

   ```sh
   cargo test -p ponzik-i18n      # the file parses, without problems
   cargo test -p ponzik i18n      # no text that the code no longer uses
   ```

   A partial translation is welcome: what is missing shows in English.
4. Look at the result: `PONZIK_LANG=<code> cargo run --release -- screenshot
   /tmp/shot.png rack` renders the interface in your language. Long words can
   overflow small buttons; prefer short wordings there.
5. Open a pull request. To list the texts your file still lacks, run
   `cargo run -p ponzik-i18n --example missing -- crates/ponzik-i18n/lang/<code>.po apps crates`.

## Limits

- The interface font (IBM Plex Sans) covers Latin, Greek and Cyrillic scripts.
  Languages written in other scripts (Arabic, Hebrew, Chinese, Japanese, Korean,
  Devanagari, Thai…) can already be translated, but their characters show as boxes
  until a font for them is added.
- Text is drawn left to right.

## For developers

- `PONZIK_LANG_DIR=<folder>` replaces the user translations folder (screenshots,
  testing a file without installing it).
- Code: `crates/ponzik-i18n` (`registry.rs` loads and merges catalogs,
  `catalog.rs` parses them, `template.rs` writes templates). The interface lives in
  `apps/ponzik/src/gui/settings.rs` (`language_choice`).
