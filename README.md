# Ponzik

**Le DAW libre. No more gatekeeping.**

Tous les outils de FL Studio, la simplicité et l'ergonomie d'Ableton.
Pour les débutants comme pour les pros. Gratuit, open-source, Windows et Linux.

## Principes

- **Tout fait maison, 100 % Rust, zéro dépendance.** Pas de framework : pilotes
  audio et MIDI, fenêtre, rendu, polices, moteur, DSP, instruments, effets et
  interface sont écrits ici. On ne parle qu'aux API du système.
- **Prise en main de FL Studio, clarté d'Ableton.** Mêmes gestes et raccourcis
  que FL, divulgation progressive : un beat en 30 secondes, toute la puissance
  quand on la cherche.
- **Des visuels, pas des murs de potards.** Courbe d'EQ éditable, courbes de
  transfert, enveloppes et filtres dessinés, formes d'onde, réponses d'effets.

## Ce qu'il y a dedans

- **Vues** : Channel Rack (step sequencer), Piano roll, Playlist (patterns,
  audio, automation), Session (grille de clips façon Ableton), Mixer.
- **Instruments natifs** : Pulse (soustractif), DrumSynth (808/909), Ponzik FM
  (4 opérateurs), Ponzik Wave (wavetable), Sampler, Acid 303, Kick, Découpeur,
  Granulaire, Cordes pincées, Piano électrique.
- **Effets natifs** : EQ, Compresseur, Multibande, Limiteur, Porte, Transitoires,
  Pompe, Réverbe, Shimmer, Délai, Chorus, Phaser, AutoFilter, Saturateur,
  Bitcrusher, Tranche, Transposeur, Image stéréo, Utilitaire.
- **Mixer** : master + 32 pistes, 8 inserts, 4 envois, routage libre sans boucle,
  solo, VU-mètres.
- **Projet** : fichiers JSON, annulation illimitée, export WAV 16/24/32 bits et
  stems, automation, clips audio, boucle sur sélection de temps.
- **Jeu** : clavier d'ordinateur comme piano, contrôleurs MIDI, enregistrement.
- **Démos** : `ponzik` (groove house), `ponzik breakcore` (Amen rave, 180 BPM).

## Architecture

| Crate | Rôle |
|---|---|
| `crates/ponzik-dsp` | Briques DSP sans allocation : oscillateurs, enveloppes, filtres, biquads, retards, dynamique, suréchantillonnage |
| `crates/ponzik-engine` | Moteur temps réel : séquence, mixer, automation, clips audio, Session, instruments et effets natifs, registre |
| `crates/ponzik-project` | Modèle de projet, fichiers JSON, rendu hors ligne, historique, démos |
| `crates/ponzik-audio` | Pilotes audio maison : ALSA (Linux), WASAPI (Windows) |
| `crates/ponzik-midi` | Entrées MIDI maison : ALSA rawmidi (Linux), WinMM (Windows) |
| `crates/ponzik-window` | Fenêtrage maison : protocole X11 direct avec MIT-SHM (Linux, et Wayland via XWayland), Win32 |
| `crates/ponzik-ui` | Rendu 2D maison : rastériseur anti-aliasé, lecteur TrueType (glyf, cmap, GPOS), animations |
| `apps/ponzik` | L'application |

Règle d'or du thread audio : **aucune allocation, aucun verrou, aucun blocage**.
L'interface modifie le projet ; la différence est envoyée au moteur en commandes,
et les objets remplacés reviennent à l'interface pour être libérés.

## Lancer

```sh
cargo run --release                         # interface, démo house
cargo run --release -- breakcore            # interface, démo breakcore
cargo run --release -- render demo.wav      # exporte la démo
cargo run --release -- screenshot ui.png mixer   # capture hors écran
cargo test
```

Sous Linux, il faut `alsa-lib` (`libasound2-dev`).

## Raccourcis (ceux de FL Studio)

| Touche | Effet |
|---|---|
| Ctrl+K | Palette touche-à-tout : toutes les actions en quelques lettres |
| Espace | Lecture / arrêt (retour au marqueur de départ) |
| F5 F6 F7 F9 | Playlist, Channel Rack, Piano roll, Mixer |
| L | Mode pattern / morceau |
| R | Armer l'enregistrement |
| F4 | Nouveau pattern |
| Alt+F8 / Ctrl+F | Navigateur / recherche |
| Ctrl+T | Clavier d'ordinateur musical |
| Ctrl+M | Métronome |
| Ctrl+Z / Ctrl+Y | Annuler / rétablir |
| Ctrl+S / Ctrl+Maj+S / Ctrl+O | Enregistrer / enregistrer sous / ouvrir |
| Ctrl+R | Exporter en WAV |
| Ctrl+C / Ctrl+V / Ctrl+B | Copier / coller / dupliquer (notes, clips) |
| Ctrl+Q | Quantifier |
| Clic droit maintenu | Effacer ce que l'on balaie (pas, notes, clips) |
| Règle : clic / glisser | Marqueur de départ / sélection de temps qui boucle |
| Clic droit sur un potard | Automatiser, valeur par défaut |

## Licence

GPL-3.0-or-later. Ponzik est libre et le restera.
Polices IBM Plex Sans et IBM Plex Mono : SIL Open Font License (`crates/ponzik-ui/fonts/OFL.txt`).
