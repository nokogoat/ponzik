# Ponzik

**Le DAW libre. No more gatekeeping.**

Tous les outils de FL Studio, la simplicité et l'ergonomie d'Ableton.
Pour les débutants comme pour les pros. Gratuit, open-source, Windows et Linux.

## Principes

- **Tout fait maison, 100 % Rust, zéro dépendance.** Pas de framework :
  pilotes audio, moteur, DSP, instruments, effets et interface sont écrits ici.
  On ne parle qu'aux API du système.
- **Simple d'abord, profond ensuite.** Un beat en 30 secondes, et toute la
  puissance accessible quand on la cherche.
- **Compatible avec ton monde.** Plugins VST3 / CLAP / LV2, import de projets
  Ableton (`.als`), FL Studio (`.flp`) et DAWproject.
- **Nos propres instruments.** Inspirés des classiques (FM, wavetable,
  sampler, drum machine…), réécrits nativement.

## Architecture

| Crate | Rôle |
|---|---|
| `crates/ponzik-dsp` | Briques DSP sans allocation : oscillateurs PolyBLEP, ADSR, filtre SVF, bruit |
| `crates/ponzik-engine` | Moteur temps réel : Channel Rack, séquenceur à l'échantillon près, mixage, file de commandes sans verrou, export WAV |
| `crates/ponzik-window` | Fenêtrage maison : protocole X11 parlé directement sur le socket (MIT-SHM, marche aussi sous Wayland via XWayland), Win32 sous Windows |
| `crates/ponzik-ui` | Rendu 2D maison : rastériseur anti-aliasé, rectangles arrondis analytiques, lecteur TrueType (glyf, cmap, crénage GPOS), animations |
| `crates/ponzik-audio` | Pilotes audio maison : ALSA (FFI `libasound`, routé vers PipeWire/PulseAudio) sous Linux, WASAPI (COM fait main, thread MMCSS « Pro Audio ») sous Windows |
| `apps/ponzik` | L'application |

Règle d'or du thread audio : **aucune allocation, aucun verrou, aucun blocage**.
L'interface ne touche jamais le moteur directement, elle lui envoie des `Command`.

### Instruments natifs

- **Pulse** : synthé soustractif polyphonique (2 oscillateurs, filtre, 2 enveloppes)
- **DrumSynth** : kick, snare, clap, hats synthétisés façon 808/909

## Lancer

```sh
cargo run --release              # ouvre l'interface
cargo run --release -- play      # joue le beat de démo dans le terminal
cargo run --release -- render demo.wav 8   # exporte 8 mesures
cargo run --release -- screenshot ui.png   # capture l'interface hors écran
cargo test
```

### Raccourcis

| Geste | Effet |
|---|---|
| Espace | Lecture / stop |
| ↑ ↓ (Maj : pas fin) | Tempo ± 1 BPM |
| Clic, glisser sur les pads | Allume ou éteint les pas, en peignant |
| Clic droit sur un pad | Efface le pas |
| Glisser verticalement sur le BPM, le pan, un fader | Règle la valeur (Maj : réglage fin) |
| Molette | Règle la valeur survolée |
| Ctrl+clic sur un fader ou un pan | Remet à zéro |

Sous Linux, il faut les en-têtes ALSA (`alsa-lib` / `libasound2-dev`).

## Feuille de route

1. ✅ Moteur audio, Channel Rack, instruments de base, export WAV, pilotes ALSA / WASAPI
2. ✅ Interface maison : transport, Channel Rack, mixer avec VU-mètres, oscilloscope
3. Piano roll, réglages des instruments, sauvegarde de projet, Wayland natif, HiDPI
4. Vue Session (clips) + Arrangement
5. Effets natifs : EQ, compresseur, reverb, delay, saturation
6. Sampler, synthé FM, synthé wavetable
7. Hébergement de plugins VST3 / CLAP / LV2
8. Import `.als`, `.flp`, DAWproject

## Licence

GPL-3.0-or-later. Ponzik est libre et le restera.
Polices IBM Plex Sans et IBM Plex Mono : SIL Open Font License (`crates/ponzik-ui/fonts/OFL.txt`).
