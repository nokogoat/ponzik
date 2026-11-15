# Inventaire des fonctionnalités : FL Studio et Ableton Live

Référence : FL Studio 2025.2 et Ableton Live 12.3 (Suite), état fin 2025.
Chaque ligne indique où vit la fonction et où en est Ponzik.

Légende : **FL** = FL Studio · **AB** = Ableton Live · ✅ fait dans Ponzik · 🟡 ébauche · ⬜ à faire

---

## 1. Vues et organisation du travail

| Fonction | Où | Ponzik |
|---|---|---|
| Channel Rack : un instrument par canal + rangée de pas | FL | ✅ 16 pas |
| Step sequencer : longueur de pattern variable, swing, vélocité, pan, hauteur par pas (graph editor) | FL | 🟡 longueur variable côté moteur, pas de swing ni de graph editor |
| Patterns multiples, sélecteur de pattern, « make unique », clonage | FL | ⬜ |
| Playlist : pistes libres qui mélangent clips de pattern, d'audio et d'automation | FL | ⬜ |
| Arrangements multiples dans un même projet | FL | ⬜ |
| Vue Session : grille de clips, scènes, lancement quantifié | AB | ⬜ |
| Vue Arrangement : timeline linéaire, pistes, enregistrement de la Session vers l'Arrangement | AB | ⬜ |
| Clip View / Device View (détail du clip et de la chaîne d'effets en bas d'écran) | AB | ⬜ |
| Performance mode : lancer les clips de la Playlist en live | FL | ⬜ |
| Navigateur (browser) : samples, presets, plugins, projets, favoris, tags, collections | FL · AB | ⬜ |
| Recherche de sons similaires dans le navigateur | AB | ⬜ |
| Plugin picker, base de données de plugins | FL | ⬜ |
| Fenêtres détachables, multi-écran | FL · AB | ⬜ |
| Barre d'aide contextuelle (hint bar / info view) | FL · AB | ⬜ |
| Thèmes et couleurs personnalisables | FL | ⬜ |
| Marqueurs : sections, boucle, signature rythmique, punch in/out | FL · AB | ⬜ |
| Historique d'annulation complet | FL · AB | ⬜ |
| Métronome, pré-compte, tap tempo | FL · AB | ⬜ |
| Clavier d'ordinateur joué comme un piano | FL · AB | ⬜ |
| Changements de tempo et de signature dans le morceau | FL · AB | ⬜ |
| Support tactile | FL | ⬜ |

## 2. Édition MIDI et piano roll

| Fonction | Où | Ponzik |
|---|---|---|
| Piano roll : dessiner, peindre, effacer, couper, sélectionner, muter, zoom | FL · AB | ⬜ |
| Propriétés de note : vélocité, release, pan, hauteur fine, mod X/Y, canal, couleur | FL | ⬜ |
| Notes glissées (slide) et portamento | FL | ⬜ |
| Notes fantômes des autres patterns ou canaux (ghost notes) | FL | ⬜ |
| Mise en évidence et contrainte à la gamme, détection de tonalité | FL · AB | ⬜ |
| Accords en un clic (chord stamps), accords expressifs | FL · AB | ⬜ |
| Outils : arpéger, strum, flam, flip, chop, glue, articuler, humaniser, quantifier | FL | ⬜ |
| Transformations MIDI : Arpeggiate, Connect, Ornament, Quantize, Recombine, Span, Strum, Time Warp, Velocity Shaper | AB | ⬜ |
| Générateurs MIDI : Rhythm, Seed, Shape, Stacks, Euclidien | AB | ⬜ |
| Riff Machine et scripts Python du piano roll | FL | ⬜ |
| Probabilité de note et plage de vélocité aléatoire | AB | ⬜ |
| Édition MPE (pression, glide, slide par note) | AB | ⬜ |
| Édition de plusieurs clips à la fois | AB | ⬜ |
| Capture MIDI (récupérer ce qu'on a joué sans enregistrer) | AB | ⬜ |
| Score logger (récupérer ce qu'on a joué) | FL | ⬜ |
| Systèmes d'accordage (micro-tonal) | AB | ⬜ |
| Éditeur d'événements et de lanes de contrôleurs | FL · AB | ⬜ |
| Import et export MIDI | FL · AB | ⬜ |

## 3. Audio : enregistrement, édition, warp

| Fonction | Où | Ponzik |
|---|---|---|
| Enregistrement audio et MIDI, armement de piste, monitoring | FL · AB | ⬜ |
| Prises en boucle, lanes de prises, comping | FL · AB | ⬜ |
| Clips audio : fondus, gain, inversion, pitch et stretch par clip, édition par clip | FL · AB | ⬜ |
| Warp : marqueurs, modes Beats, Tones, Texture, Re-Pitch, Complex, Complex Pro | AB | ⬜ |
| Time-stretch et pitch-shift de haute qualité | FL · AB | ⬜ |
| Détection de transitoires, découpe en tranches vers MIDI ou Drum Rack | FL · AB | ⬜ |
| Conversion audio vers MIDI (batterie, mélodie, harmonie) | AB | ⬜ |
| Séparation de stems (voix, batterie, basse, instruments) | FL · AB | ⬜ |
| Éditeur audio destructif (Edison : enregistrement, nettoyage, découpe, réduction de bruit) | FL | ⬜ |
| Correction de hauteur (Newtone, Pitcher) | FL | ⬜ |
| Recalage temporel (NewTime) | FL | ⬜ |
| Groove pool, extraction et application de groove | AB | ⬜ |
| Freeze, flatten, consolider, bounce vers une nouvelle piste, coller l'audio bouncé, bounce de groupes | FL · AB | ⬜ |
| Resampling (enregistrer la sortie du master) | AB | ⬜ |
| Vidéo dans la timeline | AB | ⬜ |

## 4. Instruments natifs

Les instruments propriétaires ne peuvent pas être copiés : Ponzik en fait ses propres équivalents.

### FL Studio

| Instrument | Type | Équivalent Ponzik |
|---|---|---|
| 3x Osc, SimSynth, MiniSynth, Sawer | Soustractif simple | 🟡 Pulse |
| Sytrus | FM + additif + soustractif | ⬜ |
| Harmor, Harmless | Additif, resynthèse d'image et d'audio | ⬜ |
| FLEX, GMS, Kepler, Kepler Exo | Rompler, synthés modernes | ⬜ |
| Toxic Biohazard, Sakura, PoiZone, Morphine, Ogun | Synthés variés (FM, cordes physiques, additif) | ⬜ |
| Transistor Bass, BooBass, Fruity Kick, Drumaxx, Fruity DrumSynth Live | Basse type 303, kick, batterie modélisée | 🟡 DrumSynth |
| FPC, Drumpad | Pads de batterie | ⬜ |
| DirectWave, Sampler de canal | Sampler multi-échantillons | ⬜ |
| Slicex, Fruity Slicer 2 | Découpe de boucles | ⬜ |
| Fruity Granulizer | Granulaire | ⬜ |
| FL Keys, Plucked! | Piano, cordes pincées | ⬜ |
| Autogun | Lecteur de presets FM | ⬜ |
| Layer, Patcher | Empiler des instruments, chaînes modulaires | ⬜ |
| Loop Starter | Génère des boucles par genre dans le Channel Rack | ⬜ |

### Ableton Live

| Instrument | Type | Équivalent Ponzik |
|---|---|---|
| Analog, Drift | Soustractif | 🟡 Pulse |
| Operator | FM 4 opérateurs | ⬜ |
| Wavetable | Wavetable | ⬜ |
| Meld | Macro-oscillateurs, MPE | ⬜ |
| Collision, Tension, Electric | Modélisation physique (percussions, cordes, piano électrique) | ⬜ |
| Simpler, Sampler | Sampler simple et multi-échantillons | ⬜ |
| Drum Rack, Drum Sampler, Impulse | Batterie à pads | 🟡 DrumSynth |
| Granulator III | Granulaire | ⬜ |
| Instrument Rack | Empiler des instruments, macros | ⬜ |
| External Instrument, CV Instrument | Piloter du matériel | ⬜ |

## 5. Effets natifs

| Famille | FL Studio | Ableton Live | Ponzik |
|---|---|---|---|
| EQ | Parametric EQ 2, 7 Band EQ | EQ Eight, EQ Three, Channel EQ | ⬜ |
| Compression | Compressor, Multiband Compressor, Maximus, Limiter | Compressor, Glue Compressor, Multiband Dynamics, Limiter, Gate | ⬜ |
| Mastering et limitation | Emphasis, Emphasizer, Maximus, Soundgoodizer | Limiter, Glue Compressor | ⬜ |
| Transitoires | Transient Processor | Drum Buss | ⬜ |
| Saturation | Fast Dist, Blood Overdrive, Distructor, Hardcore, Squeeze, Waveshaper | Saturator, Roar, Overdrive, Pedal, Amp, Cabinet, Dynamic Tube, Erosion, Redux, Vinyl Distortion | ⬜ |
| Réverbération | Reeverb 2, LuxeVerb, Convolver | Reverb, Hybrid Reverb, Convolution Reverb | ⬜ |
| Délai | Delay 3, Delay Bank, Multiband Delay | Delay, Echo, Filter Delay, Grain Delay, Align Delay | ⬜ |
| Modulation | Chorus, Vintage Chorus, Flanger, Flangus, Phaser, Vintage Phaser | Chorus-Ensemble, Phaser-Flanger, Auto Pan-Tremolo | ⬜ |
| Filtres | Love Philter, Filter, Free Filter | Auto Filter, Spectral Resonator, Resonators, Corpus | ⬜ |
| Hauteur et fréquence | Pitch Shifter, Frequency Shifter, Pitcher, Newtone | Shifter, Auto Shift | ⬜ |
| Effets rythmiques | Gross Beat, Scratcher | Beat Repeat, Looper | ⬜ |
| Spectral | – | Spectral Time | ⬜ |
| Vocodeur | Vocodex, Vocoder | Vocoder | ⬜ |
| Stéréo et utilitaires | Stereo Shaper, Stereo Enhancer, Balance, Center, Phase Inverter, Mute 2, Frequency Splitter | Utility, Surround Panner | 🟡 gain et pan |
| Analyse | Wave Candy, Spectroman, dB Meter | Spectrum, Tuner | 🟡 oscilloscope et VU-mètres |
| Contrôleurs de modulation | Peak Controller, Formula Controller, X-Y Controller, Envelope Controller, Keyboard Controller | LFO, Envelope Follower, Shaper, Expression Control | ⬜ |
| Chaînes d'effets | Patcher, Effector | Audio Effect Rack (chaînes parallèles, 16 macros, variations, A/B d'appareil) | ⬜ |
| Effets MIDI | – | Arpeggiator, Chord, Note Length, Pitch, Random, Scale, Velocity, Note Echo, Melodic Steps, Rotating Rhythm, MPE Control, CC Control | ⬜ |
| Visuels | ZGameEditor Visualizer, Video Player | – | ⬜ |

## 6. Mixage et routage

| Fonction | Où | Ponzik |
|---|---|---|
| Pistes de mixage : volume, pan, mute | FL · AB | ✅ |
| Solo, armement, inversion de phase, séparation stéréo | FL · AB | ⬜ |
| VU-mètres crête | FL · AB | ✅ |
| VU-mètres RMS / LUFS | FL · AB | 🟡 RMS sur la sortie |
| Inserts d'effets par piste (10 slots dans FL) | FL · AB | ⬜ |
| Pistes de mixage dynamiques (jusqu'à 500 dans FL) | FL | ⬜ |
| Envois (sends) et pistes de retour | FL · AB | ⬜ |
| Routage libre de n'importe quelle piste vers n'importe quelle autre | FL | ⬜ |
| Groupes de pistes | FL · AB | ⬜ |
| Sidechain | FL · AB | ⬜ |
| Compensation de latence des plugins (PDC), délai de piste | FL · AB | ⬜ |
| Entrées et sorties multiples, sorties de plugins multiples | FL · AB | ⬜ |
| Crossfader, pré-écoute (cue) | AB | ⬜ |
| Presets de piste et de chaîne | FL · AB | ⬜ |

## 7. Automation et modulation

| Fonction | Où | Ponzik |
|---|---|---|
| Clips d'automation avec courbes (FL) / lanes d'automation (AB) | FL · AB | ⬜ |
| Enregistrement d'automation en direct | FL · AB | ⬜ |
| Enveloppes de clip (modulation relative au clip) | AB | ⬜ |
| Macros mappables, variations de macros, randomisation | AB | ⬜ |
| Lier un paramètre à un contrôleur MIDI (MIDI learn), mode de mapping clavier et MIDI | FL · AB | ⬜ |
| Modulateurs (LFO, enveloppes, suiveurs) branchés sur n'importe quel paramètre | FL · AB | ⬜ |

## 8. Live et performance

| Fonction | Où | Ponzik |
|---|---|---|
| Lancement de clips et de scènes quantifié | AB | ⬜ |
| Follow actions (enchaînements automatiques de clips) | AB | ⬜ |
| Modes de lancement (trigger, gate, toggle, repeat), legato | AB | ⬜ |
| Looper | AB | ⬜ |
| Synchronisation Ableton Link | AB | ⬜ |
| Synchronisation MIDI clock | FL · AB | ⬜ |
| Suivi de tempo à partir d'une entrée audio | AB | ⬜ |

## 9. Plugins, formats, intégration

| Fonction | Où | Ponzik |
|---|---|---|
| Hébergement VST2, VST3, AU | FL · AB | ⬜ |
| Hébergement CLAP, LV2 | – (prévu dans Ponzik) | ⬜ |
| Pont 32/64 bits, plugins isolés dans un processus séparé | FL · AB | ⬜ |
| Export WAV | FL · AB | ✅ 16 bits |
| Export MP3, FLAC, OGG, 24/32 bits, dithering, normalisation | FL · AB | ⬜ |
| Export de stems (une piste par fichier) | FL · AB | ⬜ |
| Export vidéo | FL · AB | ⬜ |
| Scripts de contrôleurs MIDI (Python dans FL, Remote Scripts dans AB) | FL · AB | ⬜ |
| Intégration matérielle dédiée (Push, Move / FL Key, FLkey) | FL · AB | ⬜ |
| Max for Live (environnement de création d'appareils) | AB | ⬜ |
| Version mobile et import de projets mobiles (FL Studio Mobile, Ableton Note) | FL · AB | ⬜ |
| Import `.als`, `.flp`, DAWproject | – (prévu dans Ponzik) | ⬜ |

## 10. Projet et fichiers

| Fonction | Où | Ponzik |
|---|---|---|
| Sauvegarde et chargement de projet | FL · AB | ⬜ |
| Sauvegarde automatique, sauvegardes de secours | FL · AB | ⬜ |
| Modèles de projet (templates) | FL · AB | ⬜ |
| « Tout rassembler et enregistrer » (copier les samples dans le projet) | FL · AB | ⬜ |
| Packs de sons, bibliothèque fournie | FL · AB | ⬜ |
| Presets d'instruments et d'effets | FL · AB | ⬜ |

## 11. Outils assistés et services

| Fonction | Où | Ponzik |
|---|---|---|
| Assistant IA intégré (Gopher) | FL | ⬜ |
| Générateur de boucles par genre (Loop Starter) | FL | ⬜ |
| Séparation de stems | FL · AB | ⬜ |
| Intégration Splice (recherche de samples compatibles avec le projet) | AB | ⬜ hors périmètre : service propriétaire |
| FL Cloud : samples, mastering en ligne, distribution | FL | ⬜ hors périmètre : service payant ; alternative possible avec une bibliothèque libre |

---

## Ce que Ponzik a aujourd'hui

Le moteur temps réel, les pilotes audio ALSA et WASAPI, la fenêtre et le rendu maison, un Channel Rack de 16 pas, un synthé soustractif (Pulse), une batterie synthétisée (DrumSynth), un mixer volume, pan et mute avec VU-mètres, un oscilloscope et l'export WAV.

Sur les quelque 140 fonctions de cet inventaire, Ponzik en couvre une douzaine, en entier ou en ébauche.
